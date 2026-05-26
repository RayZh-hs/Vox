use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt::{self, Write},
    rc::Rc,
};

use vox_compiler::{
    TreewalkScript,
    front_end::ast::{
        Argument, BinaryOp, BlockExpr, BlockItem, CompoundAssignmentOp, Expr, ExprKind,
        FunctionDecl, LambdaParameter, Mutability, ParamDecl, Parameter, QualifiedName, RangeExpr,
        RecordFieldInit, StringLiteral, StringPart, TypeKind, TypeSyntax, UnaryOp, ValueDecl,
    },
};
use vox_core::{
    host::FunctionSpec,
    ids::ArtifactId,
    plan::CompiledArtifact,
    source::ModulePath,
    types::VoxType,
    value::{HandleData, HandleSummary, InlineValue, RuntimeValue},
};

use crate::{
    GenericFunctionHandleSummary, GenericFunctionKey, GenericParameterHandleSummary,
    HostCallArgument, RealizationKey, RealizedFunctionHandleSummary, ReplType, Runtime,
};

pub struct Interpreter<'a> {
    runtime: &'a mut Runtime,
    artifact_id: ArtifactId,
}

impl<'a> Interpreter<'a> {
    pub fn new(runtime: &'a mut Runtime, artifact_id: ArtifactId) -> Self {
        Self {
            runtime,
            artifact_id,
        }
    }

    pub fn run_script(
        &mut self,
        script: &TreewalkScript,
        artifact: &CompiledArtifact,
        arguments: &[RuntimeValue],
    ) -> Result<RuntimeValue, String> {
        let module = Rc::new(ModuleState::new(script, artifact, self.artifact_id));
        let parameter_values = self.bind_script_parameters(module.clone(), arguments)?;
        module.initialize_parameters(parameter_values);
        module.reset_cached_values();

        let mut context = EvalContext::new(self.runtime, module.clone());
        let value = match context.eval_script_result()? {
            Ok(value) => value,
            Err(EvalError::Return(_)) => {
                return Err("`return` may only be used inside a function body".to_owned());
            }
            Err(EvalError::Message(message)) => return Err(message),
        };

        self.into_runtime_value(value)
    }

    fn bind_script_parameters(
        &mut self,
        module: Rc<ModuleState>,
        arguments: &[RuntimeValue],
    ) -> Result<BTreeMap<String, Value>, String> {
        if arguments.len() > module.parameters.len() {
            return Err(format!(
                "script expects at most {} argument(s), but received {}",
                module.parameters.len(),
                arguments.len()
            ));
        }

        let mut values = BTreeMap::new();
        for (index, parameter) in module.parameters.iter().enumerate() {
            let value = if let Some(argument) = arguments.get(index) {
                self.from_runtime_value(argument)?
            } else if let Some(default) = &parameter.default {
                module.initialize_parameters(values.clone());
                let mut context = EvalContext::new(self.runtime, module.clone());
                if !values.is_empty() {
                    context.push_scope(Scope::from_values(values.clone()));
                }
                match context.eval_expr(default) {
                    Ok(value) => value,
                    Err(EvalError::Return(_)) => {
                        return Err(format!(
                            "default value for parameter `{}` attempted to return from a function",
                            parameter.name
                        ));
                    }
                    Err(EvalError::Message(message)) => return Err(message),
                }
            } else {
                return Err(format!(
                    "missing required script parameter `{}`",
                    parameter.name
                ));
            };

            values.insert(parameter.name.clone(), value);
        }

        Ok(values)
    }

    fn from_runtime_value(&self, value: &RuntimeValue) -> Result<Value, String> {
        value_from_runtime_value(self.runtime, value)
    }

    fn into_runtime_value(&mut self, value: Value) -> Result<RuntimeValue, String> {
        runtime_value_from_value(self.runtime, value)
    }
}

#[derive(Clone)]
struct ModuleState {
    artifact_id: ArtifactId,
    optimization: vox_core::opt::OptimizationLevel,
    name: String,
    imports: Vec<ModulePath>,
    parameters: Vec<ParamDecl>,
    result: Option<Expr>,
    values: BTreeMap<String, ValueDecl>,
    functions: BTreeMap<String, FunctionDecl>,
    parameter_values: RefCell<BTreeMap<String, Value>>,
    cached_values: RefCell<BTreeMap<String, CachedTopLevelValue>>,
}

impl ModuleState {
    fn new(script: &TreewalkScript, artifact: &CompiledArtifact, artifact_id: ArtifactId) -> Self {
        let values = script
            .values
            .iter()
            .cloned()
            .map(|value| (value.name.clone(), value))
            .collect::<BTreeMap<_, _>>();
        let functions = script
            .functions
            .iter()
            .cloned()
            .map(|function| (function.name.clone(), function))
            .collect::<BTreeMap<_, _>>();

        Self {
            artifact_id,
            optimization: artifact.optimization,
            name: artifact.module.as_str(),
            imports: script
                .imports
                .iter()
                .map(|import| {
                    ModulePath::parse(&import.module.to_source_string())
                        .expect("parsed import paths should be valid module paths")
                })
                .collect(),
            parameters: script.parameters.clone(),
            result: script.syntax.result.clone(),
            values,
            functions,
            parameter_values: RefCell::new(BTreeMap::new()),
            cached_values: RefCell::new(BTreeMap::new()),
        }
    }

    fn initialize_parameters(&self, values: BTreeMap<String, Value>) {
        *self.parameter_values.borrow_mut() = values;
    }

    fn reset_cached_values(&self) {
        self.cached_values.borrow_mut().clear();
    }

    fn parameter(&self, name: &str) -> Option<Value> {
        self.parameter_values.borrow().get(name).cloned()
    }

    fn resolve_qualified_local_name(&self, name: &QualifiedName) -> Option<String> {
        if name.segments.len() == 1 {
            return Some(name.segments[0].clone());
        }

        let local = name.segments.last()?;
        let module_segments = self.name.split('.').collect::<Vec<_>>();
        if module_segments.len() + 1 != name.segments.len() {
            return None;
        }

        for (expected, actual) in module_segments.iter().zip(name.segments.iter()) {
            if expected != actual {
                return None;
            }
        }

        Some(local.clone())
    }

    fn resolve_function(self: &Rc<Self>, name: &str) -> Option<FunctionValue> {
        self.functions.get(name).cloned().map(|decl| {
            let type_scope = runtime_generic_type_scope(&decl.generic_parameters);
            if decl.generic_parameters.is_empty() {
                FunctionValue::User(UserFunction {
                    name: Some(decl.name.clone()),
                    module: self.clone(),
                    parameters: decl
                        .parameters
                        .into_iter()
                        .map(|parameter| CallableParameter::from_parameter(parameter, &type_scope))
                        .collect(),
                    body: decl.body,
                    captured: BTreeMap::new(),
                })
            } else {
                FunctionValue::Generic(GenericFunction {
                    name: decl.name.clone(),
                    key: GenericFunctionKey {
                        artifact: self.artifact_id,
                        optimization: self.optimization,
                        module: self.name.clone(),
                        function: decl.name.clone(),
                    },
                    generic_parameters: decl
                        .generic_parameters
                        .iter()
                        .map(|parameter| GenericRuntimeParameter {
                            name: parameter.name.clone(),
                            bound: parameter.bound.clone(),
                        })
                        .collect(),
                    parameters: decl
                        .parameters
                        .into_iter()
                        .map(|parameter| CallableParameter::from_parameter(parameter, &type_scope))
                        .collect(),
                    return_type: decl
                        .return_type
                        .as_ref()
                        .map(|ty| runtime_type_from_syntax(ty, &type_scope)),
                    body: decl.body,
                    module: self.clone(),
                })
            }
        })
    }

    fn resolve_imported_host_function(
        &self,
        runtime: &Runtime,
        name: &QualifiedName,
    ) -> Result<Option<HostFunction>, String> {
        for length in (1..name.segments.len()).rev() {
            let package = name.segments[..length].join(".");
            let Some(imported) = self
                .imports
                .iter()
                .find(|candidate| candidate.as_str() == package)
            else {
                continue;
            };
            let Some(manifest) = runtime.package_manifest(imported) else {
                return Err(format!("package `{package}` is not mounted"));
            };

            if name.segments.len() != length + 1 {
                continue;
            }

            let symbol = &name.segments[length];
            if let Some(function) = manifest.functions.iter().find(|item| &item.name == symbol) {
                return Ok(Some(HostFunction::from_spec(imported.clone(), function)));
            }
        }

        Ok(None)
    }

    fn top_level_value(
        self: &Rc<Self>,
        name: &str,
        context: &mut EvalContext,
    ) -> Result<Value, EvalError> {
        let Some(decl) = self.values.get(name).cloned() else {
            return Err(EvalError::Message(format!("unknown name `{name}`")));
        };

        if let Some(cached) = self.cached_values.borrow().get(name) {
            return match cached {
                CachedTopLevelValue::Ready(value) => Ok(value.clone()),
                CachedTopLevelValue::Evaluating => Err(EvalError::Message(format!(
                    "cyclic top-level value dependency while evaluating `{name}`"
                ))),
            };
        }

        self.cached_values
            .borrow_mut()
            .insert(name.to_owned(), CachedTopLevelValue::Evaluating);

        let evaluated = match context.eval_expr(&decl.initializer) {
            Ok(value) => value,
            Err(error) => {
                self.cached_values.borrow_mut().remove(name);
                return Err(error);
            }
        };

        self.cached_values.borrow_mut().insert(
            name.to_owned(),
            CachedTopLevelValue::Ready(evaluated.clone()),
        );

        Ok(evaluated)
    }
}

#[derive(Clone)]
enum CachedTopLevelValue {
    Evaluating,
    Ready(Value),
}

struct EvalContext<'a> {
    runtime: &'a mut Runtime,
    module: Rc<ModuleState>,
    scopes: Vec<Scope>,
}

impl<'a> EvalContext<'a> {
    fn new(runtime: &'a mut Runtime, module: Rc<ModuleState>) -> Self {
        Self {
            runtime,
            module,
            scopes: Vec::new(),
        }
    }

    fn eval_script_result(&mut self) -> Result<Result<Value, EvalError>, String> {
        let Some(result) = self.module.result.clone() else {
            return Ok(Ok(Value::unit()));
        };

        Ok(self.eval_expr(&result))
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, EvalError> {
        match &expr.kind {
            ExprKind::Integer(raw) => {
                raw.replace('_', "")
                    .parse::<i64>()
                    .map(Value::Int)
                    .map_err(|error| {
                        EvalError::Message(format!("invalid integer literal `{raw}`: {error}"))
                    })
            }
            ExprKind::Float(raw) => raw
                .replace('_', "")
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|error| {
                    EvalError::Message(format!("invalid float literal `{raw}`: {error}"))
                }),
            ExprKind::Bool(value) => Ok(Value::Bool(*value)),
            ExprKind::Null => Ok(Value::Null),
            ExprKind::String(literal) => self.eval_string_literal(literal).map(Value::String),
            ExprKind::List(items) => items
                .iter()
                .map(|item| self.eval_expr(item))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            ExprKind::Tuple(items) => items
                .iter()
                .map(|item| self.eval_expr(item))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Tuple),
            ExprKind::Record(fields) => self.eval_record_literal(fields),
            ExprKind::Name(name) => self.resolve_name(name),
            ExprKind::Call { callee, arguments } => self.eval_call(callee, arguments),
            ExprKind::Index { target, index } => self.eval_index(target, index),
            ExprKind::Field { target, name } => {
                let value = self.eval_expr(target)?;
                self.eval_field(value, name)
            }
            ExprKind::SafeField { target, name } => {
                let value = self.eval_expr(target)?;
                if matches!(value, Value::Null) {
                    Ok(Value::Null)
                } else {
                    self.eval_field(value, name)
                }
            }
            ExprKind::NonNull { target } => {
                let value = self.eval_expr(target)?;
                if matches!(value, Value::Null) {
                    Err(EvalError::Message(
                        "non-null assertion failed on `null`".to_owned(),
                    ))
                } else {
                    Ok(value)
                }
            }
            ExprKind::ReceiverCall {
                receiver,
                callee,
                arguments,
            } => self.eval_receiver_call(receiver, callee, arguments),
            ExprKind::Unary { op, expr } => {
                let value = self.eval_expr(expr)?;
                self.eval_unary(*op, value)
            }
            ExprKind::Binary { left, op, right } => self.eval_binary(left, *op, right),
            ExprKind::Range(range) => self.eval_range(range),
            ExprKind::If(expr) => self.eval_if(expr),
            ExprKind::When(expr) => self.eval_when(expr),
            ExprKind::Lambda(lambda) => Ok(Value::Function(Rc::new(FunctionValue::User(
                UserFunction {
                    name: None,
                    module: self.module.clone(),
                    parameters: lambda
                        .parameters
                        .iter()
                        .cloned()
                        .map(CallableParameter::from_lambda_parameter)
                        .collect(),
                    body: (*lambda.body).clone(),
                    captured: self.capture_visible_bindings(),
                },
            )))),
            ExprKind::Block(block) => self.eval_block(block),
            ExprKind::Econ { body, .. } => self
                .eval_block(body)
                .map(|value| Value::Econ(Box::new(value))),
        }
    }

    fn eval_record_literal(&mut self, fields: &[RecordFieldInit]) -> Result<Value, EvalError> {
        let mut record = BTreeMap::new();
        for field in fields {
            if record.contains_key(&field.name) {
                return Err(EvalError::Message(format!(
                    "duplicate record field `{}`",
                    field.name
                )));
            }
            record.insert(field.name.clone(), self.eval_expr(&field.value)?);
        }
        Ok(Value::Record(record))
    }

    fn eval_string_literal(&mut self, literal: &StringLiteral) -> Result<String, EvalError> {
        let mut rendered = String::new();
        for part in &literal.parts {
            match part {
                StringPart::Text(text) => rendered.push_str(text),
                StringPart::Interpolation(expr) => {
                    let value = self.eval_expr(expr)?;
                    rendered.push_str(&value.render());
                }
            }
        }
        Ok(rendered)
    }

    fn resolve_name(&mut self, name: &QualifiedName) -> Result<Value, EvalError> {
        if let Some(local_name) = self.module.resolve_qualified_local_name(name) {
            if let Some(value) = self.lookup_local(&local_name) {
                return Ok(value);
            }

            if let Some(value) = self.module.parameter(&local_name) {
                return Ok(value);
            }

            if let Some(function) = self.module.resolve_function(&local_name) {
                return Ok(Value::Function(Rc::new(function)));
            }

            return self.module.clone().top_level_value(&local_name, self);
        }

        if let Some(function) = self
            .module
            .resolve_imported_host_function(self.runtime, name)
            .map_err(EvalError::Message)?
        {
            return Ok(Value::Function(Rc::new(FunctionValue::Host(function))));
        }

        Err(EvalError::Message(format!(
            "unknown qualified name `{}`",
            name.to_source_string()
        )))
    }

    fn eval_call(&mut self, callee: &Expr, arguments: &[Argument]) -> Result<Value, EvalError> {
        let callee = self.eval_expr(callee)?;
        let Value::Function(function) = callee else {
            return Err(EvalError::Message(
                "attempted to call a non-function value".to_owned(),
            ));
        };

        let evaluated_args = arguments
            .iter()
            .map(|argument| match argument {
                Argument::Positional(expr) => self.eval_expr(expr).map(CallArgument::Positional),
                Argument::Named { name, value, .. } => self
                    .eval_expr(value)
                    .map(|value| CallArgument::Named(name.clone(), value)),
            })
            .collect::<Result<Vec<_>, _>>()?;

        function.call(self.runtime, evaluated_args)
    }

    fn eval_receiver_call(
        &mut self,
        receiver: &Expr,
        callee: &QualifiedName,
        arguments: &[Argument],
    ) -> Result<Value, EvalError> {
        let receiver = self.eval_expr(receiver)?;
        let callee_name = callee.to_source_string();
        let callee = self.resolve_name(callee)?;
        let Value::Function(function) = callee else {
            return Err(EvalError::Message(format!(
                "receiver target `{}` did not resolve to a callable function",
                callee_name
            )));
        };

        let mut evaluated_args = Vec::with_capacity(arguments.len() + 1);
        evaluated_args.push(CallArgument::Positional(receiver));
        for argument in arguments {
            match argument {
                Argument::Positional(expr) => {
                    evaluated_args.push(CallArgument::Positional(self.eval_expr(expr)?));
                }
                Argument::Named { name, value, .. } => {
                    evaluated_args.push(CallArgument::Named(name.clone(), self.eval_expr(value)?));
                }
            }
        }

        function.call(self.runtime, evaluated_args)
    }

    fn eval_index(&mut self, target: &Expr, index: &Expr) -> Result<Value, EvalError> {
        let target = self.eval_expr(target)?;
        let index = self.eval_expr(index)?;
        let Value::Int(index) = index else {
            return Err(EvalError::Message(
                "index expressions require an `Int` index".to_owned(),
            ));
        };
        let index = usize::try_from(index).map_err(|_| {
            EvalError::Message("index expressions require a non-negative index".to_owned())
        })?;

        match target {
            Value::List(items) => items
                .get(index)
                .cloned()
                .ok_or_else(|| EvalError::Message(format!("list index {index} is out of bounds"))),
            Value::Tuple(items) => items
                .get(index)
                .cloned()
                .ok_or_else(|| EvalError::Message(format!("tuple index {index} is out of bounds"))),
            other => Err(EvalError::Message(format!(
                "indexing is not supported for {}",
                other.type_name()
            ))),
        }
    }

    fn eval_field(&self, value: Value, name: &str) -> Result<Value, EvalError> {
        match value {
            Value::Record(fields) => fields.get(name).cloned().ok_or_else(|| {
                EvalError::Message(format!("record does not contain field `{name}`"))
            }),
            other => Err(EvalError::Message(format!(
                "field access is not supported for {}",
                other.type_name()
            ))),
        }
    }

    fn eval_unary(&self, op: UnaryOp, value: Value) -> Result<Value, EvalError> {
        match (op, value) {
            (UnaryOp::Negate, Value::Int(value)) => Ok(Value::Int(-value)),
            (UnaryOp::Negate, Value::Float(value)) => Ok(Value::Float(-value)),
            (UnaryOp::Not, Value::Bool(value)) => Ok(Value::Bool(!value)),
            (UnaryOp::Negate, other) => Err(EvalError::Message(format!(
                "unary `-` is not defined for {}",
                other.type_name()
            ))),
            (UnaryOp::Not, other) => Err(EvalError::Message(format!(
                "unary `!` is not defined for {}",
                other.type_name()
            ))),
        }
    }

    fn eval_binary(&mut self, left: &Expr, op: BinaryOp, right: &Expr) -> Result<Value, EvalError> {
        if matches!(op, BinaryOp::And) {
            let left = self.eval_expr(left)?;
            let left = self.expect_bool(left, "left operand of `&&`")?;
            if !left {
                return Ok(Value::Bool(false));
            }
            let right = self.eval_expr(right)?;
            let right = self.expect_bool(right, "right operand of `&&`")?;
            return Ok(Value::Bool(right));
        }

        if matches!(op, BinaryOp::Or) {
            let left = self.eval_expr(left)?;
            let left = self.expect_bool(left, "left operand of `||`")?;
            if left {
                return Ok(Value::Bool(true));
            }
            let right = self.eval_expr(right)?;
            let right = self.expect_bool(right, "right operand of `||`")?;
            return Ok(Value::Bool(right));
        }

        if matches!(op, BinaryOp::Coalesce) {
            let left = self.eval_expr(left)?;
            if !matches!(left, Value::Null) {
                return Ok(left);
            }
            return self.eval_expr(right);
        }

        let left = self.eval_expr(left)?;
        let right = self.eval_expr(right)?;

        match op {
            BinaryOp::Multiply => self.eval_numeric_arithmetic(left, right, "*"),
            BinaryOp::Divide => self.eval_numeric_arithmetic(left, right, "/"),
            BinaryOp::Remainder => self.eval_numeric_arithmetic(left, right, "%"),
            BinaryOp::Add => self.eval_addition(left, right),
            BinaryOp::Subtract => self.eval_numeric_arithmetic(left, right, "-"),
            BinaryOp::Less => self.eval_comparison(left, right, "<"),
            BinaryOp::LessEqual => self.eval_comparison(left, right, "<="),
            BinaryOp::Greater => self.eval_comparison(left, right, ">"),
            BinaryOp::GreaterEqual => self.eval_comparison(left, right, ">="),
            BinaryOp::Equal => Ok(Value::Bool(left.equals(&right))),
            BinaryOp::NotEqual => Ok(Value::Bool(!left.equals(&right))),
            BinaryOp::And | BinaryOp::Or | BinaryOp::Coalesce => unreachable!(),
        }
    }

    fn eval_addition(&self, left: Value, right: Value) -> Result<Value, EvalError> {
        match (left, right) {
            (Value::Int(left), Value::Int(right)) => Ok(Value::Int(left + right)),
            (Value::Float(left), Value::Float(right)) => Ok(Value::Float(left + right)),
            (Value::Int(left), Value::Float(right)) => Ok(Value::Float(left as f64 + right)),
            (Value::Float(left), Value::Int(right)) => Ok(Value::Float(left + right as f64)),
            (Value::String(left), Value::String(right)) => Ok(Value::String(left + &right)),
            (left, right) => Err(EvalError::Message(format!(
                "binary `+` is not defined for {} and {}",
                left.type_name(),
                right.type_name()
            ))),
        }
    }

    fn eval_numeric_arithmetic(
        &self,
        left: Value,
        right: Value,
        operator: &str,
    ) -> Result<Value, EvalError> {
        match (left, right, operator) {
            (Value::Int(left), Value::Int(right), "*") => Ok(Value::Int(left * right)),
            (Value::Int(_), Value::Int(0), "/") => {
                Err(EvalError::Message("integer division by zero".to_owned()))
            }
            (Value::Int(_), Value::Int(0), "%") => {
                Err(EvalError::Message("integer remainder by zero".to_owned()))
            }
            (Value::Int(left), Value::Int(right), "/") => Ok(Value::Int(left / right)),
            (Value::Int(left), Value::Int(right), "%") => Ok(Value::Int(left % right)),
            (Value::Int(left), Value::Int(right), "-") => Ok(Value::Int(left - right)),
            (Value::Float(left), Value::Float(right), "*") => Ok(Value::Float(left * right)),
            (Value::Float(left), Value::Float(right), "/") => Ok(Value::Float(left / right)),
            (Value::Float(left), Value::Float(right), "%") => Ok(Value::Float(left % right)),
            (Value::Float(left), Value::Float(right), "-") => Ok(Value::Float(left - right)),
            (Value::Int(left), Value::Float(right), "*") => Ok(Value::Float(left as f64 * right)),
            (Value::Int(left), Value::Float(right), "/") => Ok(Value::Float(left as f64 / right)),
            (Value::Int(left), Value::Float(right), "%") => Ok(Value::Float(left as f64 % right)),
            (Value::Int(left), Value::Float(right), "-") => Ok(Value::Float(left as f64 - right)),
            (Value::Float(left), Value::Int(right), "*") => Ok(Value::Float(left * right as f64)),
            (Value::Float(left), Value::Int(right), "/") => Ok(Value::Float(left / right as f64)),
            (Value::Float(left), Value::Int(right), "%") => Ok(Value::Float(left % right as f64)),
            (Value::Float(left), Value::Int(right), "-") => Ok(Value::Float(left - right as f64)),
            (left, right, _) => Err(EvalError::Message(format!(
                "binary `{operator}` is not defined for {} and {}",
                left.type_name(),
                right.type_name()
            ))),
        }
    }

    fn eval_comparison(
        &self,
        left: Value,
        right: Value,
        operator: &str,
    ) -> Result<Value, EvalError> {
        let result = match (&left, &right) {
            (Value::Int(left), Value::Int(right)) => compare_i64(*left, *right, operator),
            (Value::Float(left), Value::Float(right)) => compare_f64(*left, *right, operator),
            (Value::Int(left), Value::Float(right)) => compare_f64(*left as f64, *right, operator),
            (Value::Float(left), Value::Int(right)) => compare_f64(*left, *right as f64, operator),
            (Value::String(left), Value::String(right)) => compare_ord(left, right, operator),
            _ => {
                return Err(EvalError::Message(format!(
                    "binary `{operator}` is not defined for {} and {}",
                    left.type_name(),
                    right.type_name()
                )));
            }
        };

        Ok(Value::Bool(result))
    }

    fn eval_range(&mut self, range: &RangeExpr) -> Result<Value, EvalError> {
        let start = range
            .start
            .as_ref()
            .map(|expr| self.eval_expr(expr))
            .transpose()?;
        let end = range
            .end
            .as_ref()
            .map(|expr| self.eval_expr(expr))
            .transpose()?;

        Ok(Value::Range(RangeValue {
            start: start.map(Box::new),
            end: end.map(Box::new),
            inclusive_end: range.inclusive_end,
        }))
    }

    fn eval_if(&mut self, expr: &vox_compiler::front_end::ast::IfExpr) -> Result<Value, EvalError> {
        for branch in &expr.branches {
            let condition = self.eval_expr(&branch.condition)?;
            if self.expect_bool(condition, "if condition")? {
                return self.eval_block(&branch.body);
            }
        }

        if let Some(else_branch) = &expr.else_branch {
            self.eval_block(else_branch)
        } else {
            Ok(Value::unit())
        }
    }

    fn eval_when(
        &mut self,
        expr: &vox_compiler::front_end::ast::WhenExpr,
    ) -> Result<Value, EvalError> {
        let subject = self.eval_expr(&expr.subject)?;
        for arm in &expr.arms {
            if self.matches_type(&subject, &arm.ty)? {
                self.push_scope(Scope::default());
                if let Some(binding) = &arm.binding {
                    self.define_local(binding.clone(), subject.clone(), false);
                }
                let result = self.eval_expr(&arm.body);
                self.pop_scope();
                return result;
            }
        }

        if let Some(else_arm) = &expr.else_arm {
            self.eval_expr(else_arm)
        } else {
            Err(EvalError::Message(
                "`when` expression did not match any arm".to_owned(),
            ))
        }
    }

    fn eval_block(&mut self, block: &BlockExpr) -> Result<Value, EvalError> {
        self.push_scope(Scope::default());
        for item in &block.items {
            match self.eval_block_item(item) {
                Ok(()) => {}
                Err(error) => {
                    self.pop_scope();
                    return Err(error);
                }
            }
        }

        let value = if let Some(trailing) = &block.trailing {
            self.eval_expr(trailing)
        } else {
            Ok(Value::unit())
        };
        self.pop_scope();
        value
    }

    fn eval_block_item(&mut self, item: &BlockItem) -> Result<(), EvalError> {
        match item {
            BlockItem::LocalValue(value) => {
                let initializer = self.eval_expr(&value.initializer)?;
                self.define_local(
                    value.name.clone(),
                    initializer,
                    matches!(value.mutability, Mutability::Var),
                );
                Ok(())
            }
            BlockItem::Assignment(assignment) => {
                let value = self.eval_expr(&assignment.value)?;
                self.assign_local(&assignment.name, value)
            }
            BlockItem::CompoundAssignment(assignment) => {
                let current = self.lookup_local_binding(&assignment.name)?;
                if !current.mutable {
                    return Err(EvalError::Message(format!(
                        "cannot assign to immutable binding `{}`",
                        assignment.name
                    )));
                }
                let rhs = self.eval_expr(&assignment.value)?;
                let next =
                    self.eval_compound_assignment(current.value.clone(), rhs, assignment.op)?;
                self.assign_local(&assignment.name, next)
            }
            BlockItem::For(statement) => self.eval_for(statement),
            BlockItem::Return(statement) => Err(EvalError::Return(
                statement
                    .value
                    .as_ref()
                    .map(|value| self.eval_expr(value))
                    .transpose()?
                    .unwrap_or_else(Value::unit),
            )),
            BlockItem::Panic(statement) => Err(EvalError::Message(format!(
                "panic: {}",
                self.eval_string_literal(&statement.message)?
            ))),
            BlockItem::Expr(expr) => {
                self.eval_expr(expr)?;
                Ok(())
            }
        }
    }

    fn eval_compound_assignment(
        &self,
        left: Value,
        right: Value,
        op: CompoundAssignmentOp,
    ) -> Result<Value, EvalError> {
        match op {
            CompoundAssignmentOp::Add => self.eval_addition(left, right),
            CompoundAssignmentOp::Subtract => self.eval_numeric_arithmetic(left, right, "-"),
            CompoundAssignmentOp::Multiply => self.eval_numeric_arithmetic(left, right, "*"),
            CompoundAssignmentOp::Divide => self.eval_numeric_arithmetic(left, right, "/"),
            CompoundAssignmentOp::Remainder => self.eval_numeric_arithmetic(left, right, "%"),
        }
    }

    fn eval_for(
        &mut self,
        statement: &vox_compiler::front_end::ast::ForStatement,
    ) -> Result<(), EvalError> {
        let iterable = self.eval_expr(&statement.iterable)?;
        let items = self.expand_iterable(iterable)?;
        for item in items {
            self.push_scope(Scope::default());
            self.define_local(statement.pattern.clone(), item, false);
            let result = self.eval_block(&statement.body);
            self.pop_scope();
            match result {
                Ok(_) => {}
                Err(EvalError::Return(value)) => return Err(EvalError::Return(value)),
                Err(EvalError::Message(message)) => return Err(EvalError::Message(message)),
            }
        }
        Ok(())
    }

    fn expand_iterable(&self, iterable: Value) -> Result<Vec<Value>, EvalError> {
        match iterable {
            Value::List(items) => Ok(items),
            Value::Range(range) => {
                let Some(start) = range.start else {
                    return Err(EvalError::Message(
                        "range iteration requires a lower bound".to_owned(),
                    ));
                };
                let Some(end) = range.end else {
                    return Err(EvalError::Message(
                        "range iteration requires an upper bound".to_owned(),
                    ));
                };

                let Value::Int(start) = *start else {
                    return Err(EvalError::Message(
                        "range iteration currently supports only `Int` bounds".to_owned(),
                    ));
                };
                let Value::Int(end) = *end else {
                    return Err(EvalError::Message(
                        "range iteration currently supports only `Int` bounds".to_owned(),
                    ));
                };

                let last = if range.inclusive_end { end + 1 } else { end };
                Ok((start..last).map(Value::Int).collect())
            }
            other => Err(EvalError::Message(format!(
                "for-loops require a list or finite int range, found {}",
                other.type_name()
            ))),
        }
    }

    fn matches_type(&self, value: &Value, ty: &TypeSyntax) -> Result<bool, EvalError> {
        match &ty.kind {
            TypeKind::Nullable(inner) => {
                if matches!(value, Value::Null) {
                    Ok(true)
                } else {
                    self.matches_type(value, inner)
                }
            }
            TypeKind::Named { name, arguments } => match name.to_source_string().as_str() {
                "Int" => Ok(matches!(value, Value::Int(_))),
                "Float" => Ok(matches!(value, Value::Float(_))),
                "Bool" => Ok(matches!(value, Value::Bool(_))),
                "String" => Ok(matches!(value, Value::String(_))),
                "Unit" => Ok(matches!(value, Value::Tuple(items) if items.is_empty())),
                "List" => match (value, arguments.as_slice()) {
                    (Value::List(items), [item_ty]) => items
                        .iter()
                        .map(|item| self.matches_type(item, item_ty))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|matches| matches.into_iter().all(|matched| matched)),
                    (Value::List(_), _) => Ok(true),
                    _ => Ok(false),
                },
                "Econ" => match (value, arguments.as_slice()) {
                    (Value::Econ(inner), [inner_ty]) => self.matches_type(inner, inner_ty),
                    (Value::Econ(_), _) => Ok(true),
                    _ => Ok(false),
                },
                _ => Ok(false),
            },
            TypeKind::Dyn(_) => Ok(false),
            TypeKind::Grouped(inner) => self.matches_type(value, inner),
            TypeKind::Tuple(items) => match value {
                Value::Tuple(values) => {
                    if values.len() != items.len() {
                        return Ok(false);
                    }
                    for (value, ty) in values.iter().zip(items) {
                        if !self.matches_type(value, ty)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                _ => Ok(false),
            },
            TypeKind::Record(fields) => {
                if fields.is_empty() {
                    return Ok(matches!(value, Value::Tuple(items) if items.is_empty()));
                }

                match value {
                    Value::Record(values) => {
                        for field in fields {
                            let Some(value) = values.get(&field.name) else {
                                return Ok(false);
                            };
                            if !self.matches_type(value, &field.ty)? {
                                return Ok(false);
                            }
                        }
                        Ok(true)
                    }
                    _ => Ok(false),
                }
            }
            TypeKind::Function { .. } => Ok(matches!(value, Value::Function(_))),
        }
    }

    fn expect_bool(&self, value: Value, label: &str) -> Result<bool, EvalError> {
        if let Value::Bool(value) = value {
            Ok(value)
        } else {
            Err(EvalError::Message(format!(
                "{label} must evaluate to `Bool`"
            )))
        }
    }

    fn push_scope(&mut self, scope: Scope) {
        self.scopes.push(scope);
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn define_local(&mut self, name: String, value: Value, mutable: bool) {
        let scope = self
            .scopes
            .last_mut()
            .expect("define_local requires an active scope");
        scope.bindings.insert(name, Binding { value, mutable });
    }

    fn lookup_local(&self, name: &str) -> Option<Value> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.bindings.get(name))
            .map(|binding| binding.value.clone())
    }

    fn lookup_local_binding(&self, name: &str) -> Result<Binding, EvalError> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.bindings.get(name))
            .cloned()
            .ok_or_else(|| EvalError::Message(format!("unknown local binding `{name}`")))
    }

    fn assign_local(&mut self, name: &str, value: Value) -> Result<(), EvalError> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(binding) = scope.bindings.get_mut(name) {
                if !binding.mutable {
                    return Err(EvalError::Message(format!(
                        "cannot assign to immutable binding `{name}`"
                    )));
                }
                binding.value = value;
                return Ok(());
            }
        }

        Err(EvalError::Message(format!(
            "assignment requires a previously declared local `var`, but `{name}` was not found"
        )))
    }

    fn capture_visible_bindings(&self) -> BTreeMap<String, Value> {
        let mut captured = BTreeMap::new();
        for scope in &self.scopes {
            for (name, binding) in &scope.bindings {
                captured.insert(name.clone(), binding.value.clone());
            }
        }
        captured
    }
}

#[derive(Debug, Clone, Default)]
struct Scope {
    bindings: BTreeMap<String, Binding>,
}

impl Scope {
    fn from_values(values: BTreeMap<String, Value>) -> Self {
        let bindings = values
            .into_iter()
            .map(|(name, value)| {
                (
                    name,
                    Binding {
                        value,
                        mutable: false,
                    },
                )
            })
            .collect();
        Self { bindings }
    }
}

#[derive(Debug, Clone)]
struct Binding {
    value: Value,
    mutable: bool,
}

#[derive(Debug, Clone)]
enum FunctionValue {
    User(UserFunction),
    Generic(GenericFunction),
    Host(HostFunction),
}

impl FunctionValue {
    fn call(
        &self,
        runtime: &mut Runtime,
        arguments: Vec<CallArgument>,
    ) -> Result<Value, EvalError> {
        match self {
            Self::User(function) => function.call(runtime, arguments),
            Self::Generic(function) => function.call(runtime, arguments),
            Self::Host(function) => function.call(runtime, arguments),
        }
    }

    fn runtime_type(&self) -> ReplType {
        match self {
            Self::User(function) => ReplType::Function {
                parameters: function
                    .parameters
                    .iter()
                    .map(|parameter| parameter.ty.clone())
                    .collect(),
                result: Box::new(function.return_type()),
            },
            Self::Generic(function) => ReplType::GenericFunction {
                generic_parameters: function
                    .generic_parameters
                    .iter()
                    .map(|parameter| crate::GenericParameterSummary {
                        name: parameter.name.clone(),
                        bound: parameter.bound.clone(),
                    })
                    .collect(),
                parameters: function
                    .parameters
                    .iter()
                    .map(|parameter| parameter.ty.clone())
                    .collect(),
                result: Box::new(function.return_type.clone().unwrap_or_else(|| {
                    ReplType::Unknown(format!("{} return type", function.name))
                })),
            },
            Self::Host(function) => ReplType::Function {
                parameters: function
                    .parameters
                    .iter()
                    .map(|parameter| parameter.ty.clone())
                    .collect(),
                result: Box::new(function.return_type.clone()),
            },
        }
    }
}

#[derive(Clone)]
struct UserFunction {
    name: Option<String>,
    module: Rc<ModuleState>,
    parameters: Vec<CallableParameter>,
    body: Expr,
    captured: BTreeMap<String, Value>,
}

impl UserFunction {
    fn call(
        &self,
        runtime: &mut Runtime,
        arguments: Vec<CallArgument>,
    ) -> Result<Value, EvalError> {
        let mut assigned = assign_arguments(
            self.name.as_deref().unwrap_or("<lambda>"),
            &self.parameters,
            arguments,
        )?;

        let mut context = EvalContext::new(runtime, self.module.clone());
        if !self.captured.is_empty() {
            context.push_scope(Scope::from_values(self.captured.clone()));
        }
        context.push_scope(Scope::default());
        for parameter in &self.parameters {
            if !assigned.contains_key(&parameter.name) {
                let Some(default) = &parameter.default else {
                    return Err(EvalError::Message(format!(
                        "missing required parameter `{}` in function `{}`",
                        parameter.name,
                        self.name.as_deref().unwrap_or("<lambda>")
                    )));
                };
                let value = context.eval_expr(default)?;
                assigned.insert(parameter.name.clone(), value);
            }

            let value = assigned
                .get(&parameter.name)
                .cloned()
                .expect("parameter should be assigned after default handling");
            context.define_local(parameter.name.clone(), value, false);
        }

        match context.eval_expr(&self.body) {
            Ok(value) => Ok(value),
            Err(EvalError::Return(value)) => Ok(value),
            Err(EvalError::Message(message)) => Err(EvalError::Message(message)),
        }
    }

    fn return_type(&self) -> ReplType {
        ReplType::Unknown(
            self.name
                .clone()
                .map(|name| format!("{name} return type"))
                .unwrap_or_else(|| "<lambda> return type".to_owned()),
        )
    }
}

#[derive(Clone)]
struct GenericFunction {
    name: String,
    key: GenericFunctionKey,
    generic_parameters: Vec<GenericRuntimeParameter>,
    parameters: Vec<CallableParameter>,
    return_type: Option<ReplType>,
    body: Expr,
    module: Rc<ModuleState>,
}

impl GenericFunction {
    fn call(
        &self,
        runtime: &mut Runtime,
        arguments: Vec<CallArgument>,
    ) -> Result<Value, EvalError> {
        let mut assigned = assign_arguments(&self.name, &self.parameters, arguments)?;
        let mut substitutions = BTreeMap::new();
        let mut context = EvalContext::new(runtime, self.module.clone());
        context.push_scope(Scope::default());
        for parameter in &self.parameters {
            if !assigned.contains_key(&parameter.name) {
                let Some(default) = &parameter.default else {
                    return Err(EvalError::Message(format!(
                        "missing required parameter `{}` in function `{}`",
                        parameter.name, self.name
                    )));
                };
                let value = context.eval_expr(default)?;
                assigned.insert(parameter.name.clone(), value);
            }

            let value = assigned
                .get(&parameter.name)
                .cloned()
                .expect("parameter should be assigned after default handling");
            infer_runtime_type_parameter(
                &parameter.ty,
                &value.runtime_type(),
                &mut substitutions,
                &self.name,
            )?;
            context.define_local(parameter.name.clone(), value, false);
        }

        let mut ordered_types = Vec::with_capacity(self.generic_parameters.len());
        for parameter in &self.generic_parameters {
            let Some(ty) = substitutions.get(&parameter.name).cloned() else {
                return Err(EvalError::Message(format!(
                    "could not infer a concrete type for generic parameter `{}` in function `{}`",
                    parameter.name, self.name
                )));
            };
            if !runtime_type_satisfies_bound(&ty, &parameter.bound) {
                return Err(EvalError::Message(format!(
                    "type `{}` does not satisfy bound `{}` for `{}` in function `{}`",
                    render_runtime_type(&ty),
                    parameter.bound,
                    parameter.name,
                    self.name
                )));
            }
            ordered_types.push(ty);
        }

        let result = match context.eval_expr(&self.body) {
            Ok(value) => Ok(value),
            Err(EvalError::Return(value)) => Ok(value),
            Err(EvalError::Message(message)) => Err(EvalError::Message(message)),
        };
        drop(context);

        if result.is_ok() {
            runtime.record_generic_realization(
                self.key.clone(),
                generic_handle_summary(
                    &self.name,
                    &self.generic_parameters,
                    &self.parameters,
                    &self.return_type,
                ),
                RealizationKey {
                    type_arguments: ordered_types.iter().map(render_runtime_type).collect(),
                },
                realized_handle_summary(
                    &self.name,
                    &self.parameters,
                    &self.return_type,
                    &substitutions,
                ),
            );
        }

        result
    }
}

#[derive(Debug, Clone)]
struct HostFunction {
    package: ModulePath,
    name: String,
    parameters: Vec<HostCallableParameter>,
    return_type: ReplType,
}

impl HostFunction {
    fn from_spec(package: ModulePath, function: &FunctionSpec) -> Self {
        Self {
            package,
            name: function.name.clone(),
            parameters: function
                .parameters
                .iter()
                .map(|parameter| HostCallableParameter {
                    name: parameter.name.clone(),
                    ty: runtime_type_from_host_type(&parameter.ty),
                    has_default: parameter.has_default,
                })
                .collect(),
            return_type: runtime_type_from_host_type(&function.return_type),
        }
    }

    fn call(
        &self,
        runtime: &mut Runtime,
        arguments: Vec<CallArgument>,
    ) -> Result<Value, EvalError> {
        let assigned = assign_host_arguments(&self.qualified_name(), &self.parameters, arguments)?;
        let assigned = assigned
            .into_iter()
            .map(|argument| {
                Ok(HostCallArgument {
                    name: argument.name,
                    value: argument
                        .value
                        .map(|value| runtime_value_from_value(runtime, value))
                        .transpose()
                        .map_err(EvalError::Message)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let value = runtime
            .invoke_host_function(&self.package, &self.name, &assigned)
            .map_err(EvalError::Message)?;
        value_from_runtime_value(runtime, &value).map_err(EvalError::Message)
    }

    fn qualified_name(&self) -> String {
        format!("{}.{}", self.package.as_str(), self.name)
    }
}

#[derive(Clone)]
struct CallableParameter {
    name: String,
    ty: ReplType,
    default: Option<Expr>,
}

impl CallableParameter {
    fn from_parameter(parameter: Parameter, generic_parameters: &BTreeMap<String, String>) -> Self {
        Self {
            name: parameter.name,
            ty: runtime_type_from_syntax(&parameter.ty, generic_parameters),
            default: parameter.default,
        }
    }

    fn from_lambda_parameter(parameter: LambdaParameter) -> Self {
        let name = parameter.name;
        Self {
            name: name.clone(),
            ty: parameter
                .ty
                .as_ref()
                .map(|ty| runtime_type_from_syntax(ty, &BTreeMap::new()))
                .unwrap_or_else(|| ReplType::Unknown(name)),
            default: None,
        }
    }
}

#[derive(Debug, Clone)]
struct HostCallableParameter {
    name: String,
    ty: ReplType,
    has_default: bool,
}

struct AssignedHostArgument {
    name: String,
    value: Option<Value>,
}

#[derive(Clone)]
struct GenericRuntimeParameter {
    name: String,
    bound: String,
}

enum CallArgument {
    Positional(Value),
    Named(String, Value),
}

#[derive(Debug, Clone)]
enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Handle(vox_core::ids::HandleId, HandleSummary),
    List(Vec<Value>),
    Tuple(Vec<Value>),
    Record(BTreeMap<String, Value>),
    Range(RangeValue),
    Function(Rc<FunctionValue>),
    Econ(Box<Value>),
}

impl Value {
    fn unit() -> Self {
        Self::Tuple(Vec::new())
    }

    fn from_inline(value: InlineValue) -> Self {
        match value {
            InlineValue::Int(value) => Self::Int(value),
            InlineValue::Float(value) => Self::Float(value),
            InlineValue::Bool(value) => Self::Bool(value),
            InlineValue::String(value) => Self::String(value),
            InlineValue::Tuple(values) => {
                Self::Tuple(values.into_iter().map(Self::from_inline).collect())
            }
            InlineValue::Record(fields) => Self::Record(
                fields
                    .into_iter()
                    .map(|(name, value)| (name, Self::from_inline(value)))
                    .collect(),
            ),
            InlineValue::Null => Self::Null,
        }
    }

    fn from_handle_data(value: HandleData) -> Self {
        match value {
            HandleData::Null => Self::Null,
            HandleData::Bool(value) => Self::Bool(value),
            HandleData::Int(value) => Self::Int(value),
            HandleData::Float(value) => Self::Float(value),
            HandleData::String(value) => Self::String(value),
            HandleData::List(values) => {
                Self::List(values.into_iter().map(Self::from_handle_data).collect())
            }
            HandleData::Tuple(values) => {
                Self::Tuple(values.into_iter().map(Self::from_handle_data).collect())
            }
            HandleData::Record(fields) => Self::Record(
                fields
                    .into_iter()
                    .map(|(name, value)| (name, Self::from_handle_data(value)))
                    .collect(),
            ),
        }
    }

    fn to_inline(&self) -> Option<InlineValue> {
        match self {
            Self::Null => Some(InlineValue::Null),
            Self::Bool(value) => Some(InlineValue::Bool(*value)),
            Self::Int(value) => Some(InlineValue::Int(*value)),
            Self::Float(value) => Some(InlineValue::Float(*value)),
            Self::String(value) => Some(InlineValue::String(value.clone())),
            Self::Handle(_, _) => None,
            Self::Tuple(values) => values
                .iter()
                .map(Value::to_inline)
                .collect::<Option<Vec<_>>>()
                .map(InlineValue::Tuple),
            Self::Record(fields) => fields
                .iter()
                .map(|(name, value)| Some((name.clone(), value.to_inline()?)))
                .collect::<Option<BTreeMap<_, _>>>()
                .map(InlineValue::Record),
            Self::List(_) | Self::Range(_) | Self::Function(_) | Self::Econ(_) => None,
        }
    }

    fn to_handle_data(&self) -> Option<HandleData> {
        match self {
            Self::Null => Some(HandleData::Null),
            Self::Bool(value) => Some(HandleData::Bool(*value)),
            Self::Int(value) => Some(HandleData::Int(*value)),
            Self::Float(value) => Some(HandleData::Float(*value)),
            Self::String(value) => Some(HandleData::String(value.clone())),
            Self::Handle(_, _) => None,
            Self::List(values) => values
                .iter()
                .map(Value::to_handle_data)
                .collect::<Option<Vec<_>>>()
                .map(HandleData::List),
            Self::Tuple(values) => values
                .iter()
                .map(Value::to_handle_data)
                .collect::<Option<Vec<_>>>()
                .map(HandleData::Tuple),
            Self::Record(fields) => fields
                .iter()
                .map(|(name, value)| Some((name.clone(), value.to_handle_data()?)))
                .collect::<Option<BTreeMap<_, _>>>()
                .map(HandleData::Record),
            Self::Range(_) | Self::Function(_) | Self::Econ(_) => None,
        }
    }

    fn equals(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::Int(left), Self::Int(right)) => left == right,
            (Self::Float(left), Self::Float(right)) => left == right,
            (Self::Int(left), Self::Float(right)) => (*left as f64) == *right,
            (Self::Float(left), Self::Int(right)) => *left == (*right as f64),
            (Self::String(left), Self::String(right)) => left == right,
            (Self::Handle(left, _), Self::Handle(right, _)) => left == right,
            (Self::List(left), Self::List(right)) | (Self::Tuple(left), Self::Tuple(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right.iter())
                        .all(|(left, right)| left.equals(right))
            }
            (Self::Record(left), Self::Record(right)) => {
                left.len() == right.len()
                    && left.iter().all(|(name, value)| {
                        right
                            .get(name)
                            .map(|other| value.equals(other))
                            .unwrap_or(false)
                    })
            }
            (Self::Range(left), Self::Range(right)) => left.equals(right),
            (Self::Econ(left), Self::Econ(right)) => left.equals(right),
            (Self::Function(left), Self::Function(right)) => Rc::ptr_eq(left, right),
            _ => false,
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "Null",
            Self::Bool(_) => "Bool",
            Self::Int(_) => "Int",
            Self::Float(_) => "Float",
            Self::String(_) => "String",
            Self::Handle(_, _) => "Handle",
            Self::List(_) => "List",
            Self::Tuple(_) => "Tuple",
            Self::Record(_) => "Record",
            Self::Range(_) => "Range",
            Self::Function(_) => "Function",
            Self::Econ(_) => "Econ",
        }
    }

    fn render(&self) -> String {
        match self {
            Self::Null => "null".to_owned(),
            Self::Bool(value) => value.to_string(),
            Self::Int(value) => value.to_string(),
            Self::Float(value) => value.to_string(),
            Self::String(value) => value.clone(),
            Self::Handle(handle, summary) => {
                format!("<{} handle {}>", summary.type_name, handle.0)
            }
            Self::List(values) => render_delimited("[", "]", values),
            Self::Tuple(values) => match values.as_slice() {
                [] => "()".to_owned(),
                [single] => format!("({},)", single.render()),
                _ => render_delimited("(", ")", values),
            },
            Self::Record(fields) => {
                let entries = fields
                    .iter()
                    .map(|(name, value)| format!("{name} = {}", value.render()))
                    .collect::<Vec<_>>();
                format!("{{{}}}", entries.join(", "))
            }
            Self::Range(range) => range.render(),
            Self::Function(function) => match &**function {
                FunctionValue::User(function) => format!(
                    "<function {}>",
                    function.name.as_deref().unwrap_or("<lambda>")
                ),
                FunctionValue::Generic(function) => {
                    format!("<generic function {}>", function.name)
                }
                FunctionValue::Host(function) => {
                    format!("<host function {}>", function.qualified_name())
                }
            },
            Self::Econ(value) => format!("econ({})", value.render()),
        }
    }

    fn summary(&self) -> HandleSummary {
        if let Self::Handle(_, summary) = self {
            return summary.clone();
        }
        HandleSummary {
            type_name: self.type_name().to_owned(),
            summary: self.render(),
            bytes: None,
        }
    }

    fn runtime_type(&self) -> ReplType {
        match self {
            Self::Null => ReplType::Null,
            Self::Bool(_) => ReplType::Bool,
            Self::Int(_) => ReplType::Int,
            Self::Float(_) => ReplType::Float,
            Self::String(_) => ReplType::String,
            Self::Handle(_, summary) => ReplType::Named {
                name: summary.type_name.clone(),
                arguments: Vec::new(),
            },
            Self::List(items) => ReplType::List(Box::new(
                items
                    .first()
                    .map(Value::runtime_type)
                    .unwrap_or_else(|| ReplType::Unknown("Unknown".to_owned())),
            )),
            Self::Tuple(items) => {
                if items.is_empty() {
                    ReplType::Unit
                } else {
                    ReplType::Tuple(items.iter().map(Value::runtime_type).collect())
                }
            }
            Self::Record(fields) => ReplType::Record(
                fields
                    .iter()
                    .map(|(name, value)| crate::RecordFieldType {
                        name: name.clone(),
                        ty: value.runtime_type(),
                    })
                    .collect(),
            ),
            Self::Range(range) => ReplType::Range(Box::new(
                range
                    .start
                    .as_ref()
                    .or(range.end.as_ref())
                    .map(|value| value.runtime_type())
                    .unwrap_or_else(|| ReplType::Unknown("Unknown".to_owned())),
            )),
            Self::Function(function) => function.runtime_type(),
            Self::Econ(value) => ReplType::Named {
                name: "Econ".to_owned(),
                arguments: vec![value.runtime_type()],
            },
        }
    }
}

#[derive(Debug, Clone)]
struct RangeValue {
    start: Option<Box<Value>>,
    end: Option<Box<Value>>,
    inclusive_end: bool,
}

impl RangeValue {
    fn equals(&self, other: &Self) -> bool {
        self.inclusive_end == other.inclusive_end
            && self
                .start
                .as_ref()
                .map(|value| &**value)
                .zip(other.start.as_ref().map(|value| &**value))
                .map(|(left, right)| left.equals(right))
                .unwrap_or(self.start.is_none() && other.start.is_none())
            && self
                .end
                .as_ref()
                .map(|value| &**value)
                .zip(other.end.as_ref().map(|value| &**value))
                .map(|(left, right)| left.equals(right))
                .unwrap_or(self.end.is_none() && other.end.is_none())
    }

    fn render(&self) -> String {
        let mut rendered = String::new();
        if let Some(start) = &self.start {
            rendered.push_str(&start.render());
        }
        rendered.push_str(if self.inclusive_end { "..=" } else { ".." });
        if let Some(end) = &self.end {
            rendered.push_str(&end.render());
        }
        rendered
    }
}

#[derive(Debug, Clone)]
enum EvalError {
    Message(String),
    Return(Value),
}

fn assign_arguments(
    function_name: &str,
    parameters: &[CallableParameter],
    arguments: Vec<CallArgument>,
) -> Result<BTreeMap<String, Value>, EvalError> {
    let mut assigned = BTreeMap::new();
    let mut next_positional = 0usize;

    for argument in arguments {
        match argument {
            CallArgument::Positional(value) => {
                while next_positional < parameters.len()
                    && assigned.contains_key(&parameters[next_positional].name)
                {
                    next_positional += 1;
                }
                let Some(parameter) = parameters.get(next_positional) else {
                    return Err(EvalError::Message(format!(
                        "function `{function_name}` received too many positional arguments"
                    )));
                };
                assigned.insert(parameter.name.clone(), value);
                next_positional += 1;
            }
            CallArgument::Named(name, value) => {
                if !parameters.iter().any(|parameter| parameter.name == name) {
                    return Err(EvalError::Message(format!(
                        "function `{function_name}` does not have a parameter named `{name}`"
                    )));
                }
                if assigned.insert(name.clone(), value).is_some() {
                    return Err(EvalError::Message(format!(
                        "parameter `{name}` was provided more than once"
                    )));
                }
            }
        }
    }

    Ok(assigned)
}

fn assign_host_arguments(
    function_name: &str,
    parameters: &[HostCallableParameter],
    arguments: Vec<CallArgument>,
) -> Result<Vec<AssignedHostArgument>, EvalError> {
    let mut assigned = BTreeMap::new();
    let mut next_positional = 0usize;

    for argument in arguments {
        match argument {
            CallArgument::Positional(value) => {
                while next_positional < parameters.len()
                    && assigned.contains_key(&parameters[next_positional].name)
                {
                    next_positional += 1;
                }
                let Some(parameter) = parameters.get(next_positional) else {
                    return Err(EvalError::Message(format!(
                        "function `{function_name}` received too many positional arguments"
                    )));
                };
                assigned.insert(parameter.name.clone(), value);
                next_positional += 1;
            }
            CallArgument::Named(name, value) => {
                if !parameters.iter().any(|parameter| parameter.name == name) {
                    return Err(EvalError::Message(format!(
                        "function `{function_name}` does not have a parameter named `{name}`"
                    )));
                }
                if assigned.insert(name.clone(), value).is_some() {
                    return Err(EvalError::Message(format!(
                        "parameter `{name}` was provided more than once"
                    )));
                }
            }
        }
    }

    let mut ordered = Vec::with_capacity(parameters.len());
    for parameter in parameters {
        if let Some(value) = assigned.remove(&parameter.name) {
            ordered.push(AssignedHostArgument {
                name: parameter.name.clone(),
                value: Some(value),
            });
            continue;
        }

        if parameter.has_default {
            ordered.push(AssignedHostArgument {
                name: parameter.name.clone(),
                value: None,
            });
            continue;
        }

        return Err(EvalError::Message(format!(
            "missing required parameter `{}` in function `{function_name}`",
            parameter.name
        )));
    }

    Ok(ordered)
}

fn runtime_generic_type_scope(
    parameters: &[vox_compiler::front_end::ast::GenericParameter],
) -> BTreeMap<String, String> {
    parameters
        .iter()
        .map(|parameter| (parameter.name.clone(), parameter.bound.clone()))
        .collect()
}

fn runtime_type_from_syntax(
    ty: &TypeSyntax,
    generic_parameters: &BTreeMap<String, String>,
) -> ReplType {
    match &ty.kind {
        TypeKind::Function { parameters, result } => ReplType::Function {
            parameters: parameters
                .iter()
                .map(|parameter| runtime_type_from_syntax(parameter, generic_parameters))
                .collect(),
            result: Box::new(runtime_type_from_syntax(result, generic_parameters)),
        },
        TypeKind::Nullable(inner) => ReplType::Nullable(Box::new(runtime_type_from_syntax(
            inner,
            generic_parameters,
        ))),
        TypeKind::Named { name, arguments } => {
            let raw = name.to_source_string();
            match raw.as_str() {
                "Int" => ReplType::Int,
                "Float" => ReplType::Float,
                "Bool" => ReplType::Bool,
                "String" => ReplType::String,
                "Unit" => ReplType::Unit,
                "List" if arguments.len() == 1 => ReplType::List(Box::new(
                    runtime_type_from_syntax(&arguments[0], generic_parameters),
                )),
                _ if arguments.is_empty() => generic_parameters
                    .get(&raw)
                    .map(|bound| ReplType::TypeParameter {
                        name: raw.clone(),
                        bound: Some(bound.clone()),
                    })
                    .unwrap_or_else(|| ReplType::Named {
                        name: raw,
                        arguments: Vec::new(),
                    }),
                _ => ReplType::Named {
                    name: raw,
                    arguments: arguments
                        .iter()
                        .map(|argument| runtime_type_from_syntax(argument, generic_parameters))
                        .collect(),
                },
            }
        }
        TypeKind::Dyn(name) => ReplType::DynTrait(name.to_source_string()),
        TypeKind::Grouped(inner) => runtime_type_from_syntax(inner, generic_parameters),
        TypeKind::Tuple(items) => {
            if items.is_empty() {
                ReplType::Unit
            } else {
                ReplType::Tuple(
                    items
                        .iter()
                        .map(|item| runtime_type_from_syntax(item, generic_parameters))
                        .collect(),
                )
            }
        }
        TypeKind::Record(fields) => {
            if fields.is_empty() {
                ReplType::Unit
            } else {
                ReplType::Record(
                    fields
                        .iter()
                        .map(|field| crate::RecordFieldType {
                            name: field.name.clone(),
                            ty: runtime_type_from_syntax(&field.ty, generic_parameters),
                        })
                        .collect(),
                )
            }
        }
    }
}

fn runtime_type_from_host_type(ty: &VoxType) -> ReplType {
    match ty {
        VoxType::Int => ReplType::Int,
        VoxType::Float => ReplType::Float,
        VoxType::Bool => ReplType::Bool,
        VoxType::String => ReplType::String,
        VoxType::List(item) => ReplType::List(Box::new(runtime_type_from_host_type(item))),
        VoxType::Tuple(items) => {
            if items.is_empty() {
                ReplType::Unit
            } else {
                ReplType::Tuple(items.iter().map(runtime_type_from_host_type).collect())
            }
        }
        VoxType::Record(fields) => {
            if fields.is_empty() {
                ReplType::Unit
            } else {
                ReplType::Record(
                    fields
                        .iter()
                        .map(|field| crate::RecordFieldType {
                            name: field.name.clone(),
                            ty: runtime_type_from_host_type(&field.ty),
                        })
                        .collect(),
                )
            }
        }
        VoxType::Nullable(inner) => {
            ReplType::Nullable(Box::new(runtime_type_from_host_type(inner)))
        }
        VoxType::DynTrait(name) => {
            ReplType::DynTrait(format!("{}.{}", name.module.as_str(), name.name))
        }
        VoxType::Named(name) => ReplType::Named {
            name: format!("{}.{}", name.module.as_str(), name.name),
            arguments: Vec::new(),
        },
        VoxType::TypeParameter(name) => ReplType::TypeParameter {
            name: name.clone(),
            bound: None,
        },
        VoxType::OpaqueSurface(name) => ReplType::Unknown(name.clone()),
    }
}

fn value_from_runtime_value(runtime: &Runtime, value: &RuntimeValue) -> Result<Value, String> {
    match value {
        RuntimeValue::Inline(value) => Ok(Value::from_inline(value.clone())),
        RuntimeValue::Handle(handle) => {
            if let Some(bytes) = runtime.handle_data(*handle) {
                let mut reader = crate::protocol::PayloadReader::new(bytes);
                let data = crate::protocol::decode_handle_data(&mut reader).map_err(|error| {
                    format!("failed to decode handle {} data: {error}", handle.0)
                })?;
                reader.finish().map_err(|error| {
                    format!("failed to decode handle {} data: {error}", handle.0)
                })?;
                return Ok(Value::from_handle_data(data));
            }

            let summary = runtime
                .describe_handle(*handle)
                .ok_or_else(|| format!("unknown handle {}", handle.0))?;
            Ok(Value::Handle(*handle, summary))
        }
    }
}

fn runtime_value_from_value(runtime: &mut Runtime, value: Value) -> Result<RuntimeValue, String> {
    if let Some(inline) = value.to_inline() {
        return Ok(RuntimeValue::Inline(inline));
    }

    if let Value::Handle(handle, _) = &value {
        return Ok(RuntimeValue::Handle(*handle));
    }

    if let Value::Function(function) = &value {
        if let FunctionValue::Generic(generic) = &**function {
            let handle = runtime.materialize_generic_handle(
                generic.key.clone(),
                generic_handle_summary(
                    &generic.name,
                    &generic.generic_parameters,
                    &generic.parameters,
                    &generic.return_type,
                ),
            );
            return Ok(RuntimeValue::Handle(handle));
        }
    }

    let summary = value.summary();
    let handle = if let Some(data) = value.to_handle_data() {
        runtime.allocate_serializable_handle(summary, data)
    } else {
        runtime.allocate_handle(summary)
    };
    Ok(RuntimeValue::Handle(handle))
}

fn infer_runtime_type_parameter(
    expected: &ReplType,
    actual: &ReplType,
    substitutions: &mut BTreeMap<String, ReplType>,
    function_name: &str,
) -> Result<(), EvalError> {
    match expected {
        ReplType::TypeParameter { name, bound } => {
            if let Some(existing) = substitutions.get(name) {
                if existing != actual {
                    return Err(EvalError::Message(format!(
                        "generic parameter `{name}` in function `{function_name}` was inferred as both `{}` and `{}`",
                        render_runtime_type(existing),
                        render_runtime_type(actual)
                    )));
                }
            } else {
                if let Some(bound) = bound {
                    if !runtime_type_satisfies_bound(actual, bound) {
                        return Err(EvalError::Message(format!(
                            "type `{}` does not satisfy bound `{}` for `{name}` in function `{function_name}`",
                            render_runtime_type(actual),
                            bound
                        )));
                    }
                }
                substitutions.insert(name.clone(), actual.clone());
            }
            Ok(())
        }
        ReplType::List(expected_item) => {
            let ReplType::List(actual_item) = actual else {
                return Ok(());
            };
            infer_runtime_type_parameter(expected_item, actual_item, substitutions, function_name)
        }
        ReplType::Tuple(expected_items) => {
            let ReplType::Tuple(actual_items) = actual else {
                return Ok(());
            };
            for (expected, actual) in expected_items.iter().zip(actual_items.iter()) {
                infer_runtime_type_parameter(expected, actual, substitutions, function_name)?;
            }
            Ok(())
        }
        ReplType::Record(expected_fields) => {
            let ReplType::Record(actual_fields) = actual else {
                return Ok(());
            };
            for (expected, actual) in expected_fields.iter().zip(actual_fields.iter()) {
                if expected.name == actual.name {
                    infer_runtime_type_parameter(
                        &expected.ty,
                        &actual.ty,
                        substitutions,
                        function_name,
                    )?;
                }
            }
            Ok(())
        }
        ReplType::Nullable(expected_inner) => {
            let ReplType::Nullable(actual_inner) = actual else {
                return Ok(());
            };
            infer_runtime_type_parameter(expected_inner, actual_inner, substitutions, function_name)
        }
        ReplType::Named {
            name: expected_name,
            arguments: expected_arguments,
        } => {
            let ReplType::Named {
                name: actual_name,
                arguments: actual_arguments,
            } = actual
            else {
                return Ok(());
            };
            if expected_name == actual_name {
                for (expected, actual) in expected_arguments.iter().zip(actual_arguments.iter()) {
                    infer_runtime_type_parameter(expected, actual, substitutions, function_name)?;
                }
            }
            Ok(())
        }
        ReplType::Range(expected_item) => {
            let ReplType::Range(actual_item) = actual else {
                return Ok(());
            };
            infer_runtime_type_parameter(expected_item, actual_item, substitutions, function_name)
        }
        ReplType::Function {
            parameters: expected_parameters,
            result: expected_result,
        } => {
            let ReplType::Function {
                parameters: actual_parameters,
                result: actual_result,
            } = actual
            else {
                return Ok(());
            };
            for (expected, actual) in expected_parameters.iter().zip(actual_parameters.iter()) {
                infer_runtime_type_parameter(expected, actual, substitutions, function_name)?;
            }
            infer_runtime_type_parameter(
                expected_result,
                actual_result,
                substitutions,
                function_name,
            )
        }
        _ => Ok(()),
    }
}

fn render_runtime_type(ty: &ReplType) -> String {
    ty.render()
}

fn runtime_type_satisfies_bound(ty: &ReplType, bound: &str) -> bool {
    match bound {
        "Any" => true,
        "Numeric" => matches!(ty, ReplType::Int | ReplType::Float),
        _ => true,
    }
}

fn generic_handle_summary(
    name: &str,
    generic_parameters: &[GenericRuntimeParameter],
    parameters: &[CallableParameter],
    return_type: &Option<ReplType>,
) -> GenericFunctionHandleSummary {
    GenericFunctionHandleSummary {
        name: name.to_owned(),
        generic_parameters: generic_parameters
            .iter()
            .map(|parameter| GenericParameterHandleSummary {
                name: parameter.name.clone(),
                bound: parameter.bound.clone(),
            })
            .collect(),
        parameters: parameters
            .iter()
            .map(|parameter| render_runtime_type(&parameter.ty))
            .collect(),
        return_type: return_type
            .as_ref()
            .map(render_runtime_type)
            .unwrap_or_else(|| "Unknown".to_owned()),
    }
}

fn realized_handle_summary(
    name: &str,
    parameters: &[CallableParameter],
    return_type: &Option<ReplType>,
    substitutions: &BTreeMap<String, ReplType>,
) -> RealizedFunctionHandleSummary {
    RealizedFunctionHandleSummary {
        name: name.to_owned(),
        parameters: parameters
            .iter()
            .map(|parameter| {
                render_runtime_type(&substitute_runtime_type(&parameter.ty, substitutions))
            })
            .collect(),
        return_type: return_type
            .as_ref()
            .map(|ty| render_runtime_type(&substitute_runtime_type(ty, substitutions)))
            .unwrap_or_else(|| "Unknown".to_owned()),
    }
}

fn substitute_runtime_type(ty: &ReplType, substitutions: &BTreeMap<String, ReplType>) -> ReplType {
    match ty {
        ReplType::List(item) => {
            ReplType::List(Box::new(substitute_runtime_type(item, substitutions)))
        }
        ReplType::Tuple(items) => ReplType::Tuple(
            items
                .iter()
                .map(|item| substitute_runtime_type(item, substitutions))
                .collect(),
        ),
        ReplType::Nullable(inner) => {
            ReplType::Nullable(Box::new(substitute_runtime_type(inner, substitutions)))
        }
        ReplType::Named { name, arguments } => ReplType::Named {
            name: name.clone(),
            arguments: arguments
                .iter()
                .map(|argument| substitute_runtime_type(argument, substitutions))
                .collect(),
        },
        ReplType::Function { parameters, result } => ReplType::Function {
            parameters: parameters
                .iter()
                .map(|parameter| substitute_runtime_type(parameter, substitutions))
                .collect(),
            result: Box::new(substitute_runtime_type(result, substitutions)),
        },
        ReplType::Record(fields) => ReplType::Record(
            fields
                .iter()
                .map(|field| crate::RecordFieldType {
                    name: field.name.clone(),
                    ty: substitute_runtime_type(&field.ty, substitutions),
                })
                .collect(),
        ),
        ReplType::Range(item) => {
            ReplType::Range(Box::new(substitute_runtime_type(item, substitutions)))
        }
        ReplType::TypeParameter { name, .. } => substitutions
            .get(name)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        _ => ty.clone(),
    }
}

fn compare_i64(left: i64, right: i64, operator: &str) -> bool {
    match operator {
        "<" => left < right,
        "<=" => left <= right,
        ">" => left > right,
        ">=" => left >= right,
        _ => unreachable!(),
    }
}

fn compare_f64(left: f64, right: f64, operator: &str) -> bool {
    match operator {
        "<" => left < right,
        "<=" => left <= right,
        ">" => left > right,
        ">=" => left >= right,
        _ => unreachable!(),
    }
}

fn compare_ord(left: &str, right: &str, operator: &str) -> bool {
    match operator {
        "<" => left < right,
        "<=" => left <= right,
        ">" => left > right,
        ">=" => left >= right,
        _ => unreachable!(),
    }
}

fn render_delimited(prefix: &str, suffix: &str, values: &[Value]) -> String {
    let mut rendered = String::new();
    rendered.push_str(prefix);
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            rendered.push_str(", ");
        }
        let _ = write!(rendered, "{}", value.render());
    }
    rendered.push_str(suffix);
    rendered
}

impl fmt::Debug for UserFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserFunction")
            .field("name", &self.name)
            .field("parameters", &self.parameters.len())
            .finish()
    }
}

impl fmt::Debug for GenericFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GenericFunction")
            .field("name", &self.name)
            .field("generic_parameters", &self.generic_parameters.len())
            .field("parameters", &self.parameters.len())
            .finish()
    }
}
