use std::collections::BTreeMap;

use vox_compiler::front_end::ast::{
    Argument, BinaryOp, BlockExpr, BlockItem, CompilationUnit, CompoundAssignmentOp, Expr,
    ExprKind, LocalValueDecl, Mutability, QualifiedName, TopLevelItem, TypeKind, TypeSyntax,
    UnaryOp,
};
use vox_core::{host::PackageManifest, source::ModulePath};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplType {
    Unit,
    Null,
    Bool,
    Int,
    Float,
    String,
    List(Box<ReplType>),
    Tuple(Vec<ReplType>),
    Nullable(Box<ReplType>),
    DynTrait(String),
    Named {
        name: String,
        arguments: Vec<ReplType>,
    },
    Function {
        parameters: Vec<ReplType>,
        result: Box<ReplType>,
    },
    GenericFunction {
        generic_parameters: Vec<GenericParameterSummary>,
        parameters: Vec<ReplType>,
        result: Box<ReplType>,
    },
    Record(Vec<RecordFieldType>),
    Range(Box<ReplType>),
    TypeParameter {
        name: String,
        bound: Option<String>,
    },
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordFieldType {
    pub name: String,
    pub ty: ReplType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingSummary {
    pub name: String,
    pub ty: ReplType,
    pub mutable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSummary {
    pub name: String,
    pub generic_parameters: Vec<GenericParameterSummary>,
    pub parameters: Vec<CallableParameterSummary>,
    pub return_type: ReplType,
    pub evil: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericParameterSummary {
    pub name: String,
    pub bound: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableParameterSummary {
    pub name: String,
    pub ty: ReplType,
    pub has_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeEnvironment {
    pub imports: Vec<String>,
    pub bindings: Vec<BindingSummary>,
    pub functions: Vec<FunctionSummary>,
    pub result: Option<ReplType>,
}

pub fn infer_environment(
    unit: &CompilationUnit,
    manifests: &[PackageManifest],
) -> Result<TypeEnvironment, String> {
    let mut engine = TypeEngine::new(unit, manifests);
    engine.infer()
}

impl ReplType {
    pub fn render(&self) -> String {
        match self {
            Self::Unit => "Unit".to_owned(),
            Self::Null => "Null".to_owned(),
            Self::Bool => "Bool".to_owned(),
            Self::Int => "Int".to_owned(),
            Self::Float => "Float".to_owned(),
            Self::String => "String".to_owned(),
            Self::List(item) => format!("List[{}]", item.render()),
            Self::Tuple(items) => match items.as_slice() {
                [] => "()".to_owned(),
                [single] => format!("({},)", single.render()),
                _ => format!(
                    "({})",
                    items
                        .iter()
                        .map(ReplType::render)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            },
            Self::Nullable(inner) => format!("{}?", inner.render()),
            Self::DynTrait(name) => format!("dyn {name}"),
            Self::Named { name, arguments } => {
                if arguments.is_empty() {
                    name.clone()
                } else {
                    format!(
                        "{name}[{}]",
                        arguments
                            .iter()
                            .map(ReplType::render)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
            Self::Function { parameters, result } => format!(
                "({}) -> {}",
                parameters
                    .iter()
                    .map(ReplType::render)
                    .collect::<Vec<_>>()
                    .join(", "),
                result.render()
            ),
            Self::GenericFunction {
                generic_parameters,
                parameters,
                result,
            } => format!(
                "[{}] ({}) -> {}",
                generic_parameters
                    .iter()
                    .map(|parameter| format!("{}: {}", parameter.name, parameter.bound))
                    .collect::<Vec<_>>()
                    .join(", "),
                parameters
                    .iter()
                    .map(ReplType::render)
                    .collect::<Vec<_>>()
                    .join(", "),
                result.render()
            ),
            Self::Record(fields) => format!(
                "{{{}}}",
                fields
                    .iter()
                    .map(|field| format!("{}: {}", field.name, field.ty.render()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Range(item) => format!("Range[{}]", item.render()),
            Self::TypeParameter { name, .. } => name.clone(),
            Self::Unknown(name) => name.clone(),
        }
    }

    fn is_assignable_to(&self, target: &Self) -> bool {
        match (self, target) {
            (_, Self::Unknown(_)) | (Self::Unknown(_), _) => true,
            (Self::Null, Self::Nullable(_)) => true,
            (left, right) if left == right => true,
            (Self::Tuple(left), Self::Tuple(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right.iter())
                        .all(|(left, right)| left.is_assignable_to(right))
            }
            (Self::List(left), Self::List(right))
            | (Self::Nullable(left), Self::Nullable(right))
            | (Self::Range(left), Self::Range(right)) => left.is_assignable_to(right),
            (
                Self::Function {
                    parameters: left_params,
                    result: left_result,
                },
                Self::Function {
                    parameters: right_params,
                    result: right_result,
                },
            ) => {
                left_params.len() == right_params.len()
                    && left_params
                        .iter()
                        .zip(right_params.iter())
                        .all(|(left, right)| left.is_assignable_to(right))
                    && left_result.is_assignable_to(right_result)
            }
            (
                Self::GenericFunction {
                    generic_parameters: left_generics,
                    parameters: left_params,
                    result: left_result,
                },
                Self::GenericFunction {
                    generic_parameters: right_generics,
                    parameters: right_params,
                    result: right_result,
                },
            ) => {
                left_generics == right_generics
                    && left_params.len() == right_params.len()
                    && left_params
                        .iter()
                        .zip(right_params.iter())
                        .all(|(left, right)| left.is_assignable_to(right))
                    && left_result.is_assignable_to(right_result)
            }
            (
                Self::Named {
                    name: left_name,
                    arguments: left_args,
                },
                Self::Named {
                    name: right_name,
                    arguments: right_args,
                },
            ) => {
                left_name == right_name
                    && left_args.len() == right_args.len()
                    && left_args
                        .iter()
                        .zip(right_args.iter())
                        .all(|(left, right)| left.is_assignable_to(right))
            }
            (_, Self::DynTrait(name)) if name == "Any" => true,
            (Self::Record(left), Self::Record(right)) => {
                left.len() == right.len()
                    && left.iter().zip(right.iter()).all(|(left, right)| {
                        left.name == right.name && left.ty.is_assignable_to(&right.ty)
                    })
            }
            _ => false,
        }
    }

    fn unify(left: Self, right: Self) -> Self {
        if left == right {
            return left;
        }
        match (&left, &right) {
            (Self::Unknown(_), _) => right,
            (_, Self::Unknown(_)) => left,
            (Self::Null, Self::Nullable(_)) => right,
            (Self::Nullable(_), Self::Null) => left,
            (Self::Tuple(left_items), Self::Tuple(right_items))
                if left_items.len() == right_items.len() =>
            {
                Self::Tuple(
                    left_items
                        .iter()
                        .cloned()
                        .zip(right_items.iter().cloned())
                        .map(|(left, right)| Self::unify(left, right))
                        .collect(),
                )
            }
            (Self::List(left_item), Self::List(right_item)) => Self::List(Box::new(Self::unify(
                (**left_item).clone(),
                (**right_item).clone(),
            ))),
            (
                Self::TypeParameter {
                    name: left_name,
                    bound: left_bound,
                },
                Self::TypeParameter {
                    name: right_name,
                    bound: right_bound,
                },
            ) if left_name == right_name && left_bound == right_bound => left,
            _ => Self::Unknown(format!("{} | {}", left.render(), right.render())),
        }
    }
}

#[derive(Debug, Clone)]
struct ValueInfo {
    name: String,
    ty: ReplType,
    mutable: bool,
}

#[derive(Debug, Clone)]
struct FunctionInfo {
    summary: FunctionSummary,
}

#[derive(Debug, Clone)]
struct ImportedPackage {
    manifest: Option<PackageManifest>,
}

struct TypeEngine<'a> {
    unit: &'a CompilationUnit,
    module: String,
    manifests: BTreeMap<String, PackageManifest>,
    imports: BTreeMap<String, ImportedPackage>,
    values: BTreeMap<String, ValueInfo>,
    functions: BTreeMap<String, FunctionInfo>,
}

impl<'a> TypeEngine<'a> {
    fn new(unit: &'a CompilationUnit, manifests: &'a [PackageManifest]) -> Self {
        Self {
            module: unit.header.module.as_str(),
            unit,
            manifests: manifests
                .iter()
                .cloned()
                .map(|manifest| (manifest.package.as_str(), manifest))
                .collect(),
            imports: BTreeMap::new(),
            values: BTreeMap::new(),
            functions: BTreeMap::new(),
        }
    }

    fn infer(&mut self) -> Result<TypeEnvironment, String> {
        self.prime()?;
        let result = if let Some(expr) = &self.unit.result {
            let mut scope = self.top_level_scope();
            Some(self.infer_expr(expr, &mut scope)?)
        } else {
            None
        };

        Ok(TypeEnvironment {
            imports: self.imports.keys().cloned().collect(),
            bindings: self
                .values
                .values()
                .map(|binding| BindingSummary {
                    name: binding.name.clone(),
                    ty: binding.ty.clone(),
                    mutable: binding.mutable,
                })
                .collect(),
            functions: self
                .functions
                .values()
                .map(|function| function.summary.clone())
                .collect(),
            result,
        })
    }

    fn prime(&mut self) -> Result<(), String> {
        self.collect_imports();
        self.collect_function_headers();
        self.collect_value_placeholders();
        self.infer_function_bodies()?;
        self.infer_value_initializers()?;
        Ok(())
    }

    fn collect_imports(&mut self) {
        for item in &self.unit.items {
            if let TopLevelItem::Import(import) = item {
                let name = import.module.to_source_string();
                self.imports.insert(
                    name.clone(),
                    ImportedPackage {
                        manifest: self.manifests.get(&name).cloned(),
                    },
                );
            }
        }
    }

    fn collect_function_headers(&mut self) {
        for item in &self.unit.items {
            if let TopLevelItem::Function(function) = item {
                self.functions.insert(
                    function.name.clone(),
                    FunctionInfo {
                        summary: FunctionSummary {
                            name: function.name.clone(),
                            generic_parameters: function
                                .generic_parameters
                                .iter()
                                .map(|parameter| GenericParameterSummary {
                                    name: parameter.name.clone(),
                                    bound: parameter.bound.clone(),
                                })
                                .collect(),
                            parameters: function
                                .parameters
                                .iter()
                                .map(|parameter| CallableParameterSummary {
                                    name: parameter.name.clone(),
                                    ty: from_type_syntax(
                                        &parameter.ty,
                                        &generic_parameter_scope(&function.generic_parameters),
                                    ),
                                    has_default: parameter.default.is_some(),
                                })
                                .collect(),
                            return_type: function
                                .return_type
                                .as_ref()
                                .map(|ty| {
                                    from_type_syntax(
                                        ty,
                                        &generic_parameter_scope(&function.generic_parameters),
                                    )
                                })
                                .unwrap_or_else(|| {
                                    ReplType::Unknown(format!("{} return type", function.name))
                                }),
                            evil: function.evil,
                        },
                    },
                );
            }
        }
    }

    fn collect_value_placeholders(&mut self) {
        for item in &self.unit.items {
            if let TopLevelItem::Value(value) = item {
                self.values.insert(
                    value.name.clone(),
                    ValueInfo {
                        name: value.name.clone(),
                        ty: value
                            .ty
                            .as_ref()
                            .map(|ty| from_type_syntax(ty, &BTreeMap::new()))
                            .unwrap_or_else(|| ReplType::Unknown(format!("{} type", value.name))),
                        mutable: matches!(value.mutability, Mutability::Var),
                    },
                );
            }
        }
    }

    fn infer_function_bodies(&mut self) -> Result<(), String> {
        for item in &self.unit.items {
            let TopLevelItem::Function(function) = item else {
                continue;
            };

            let mut scope = self.top_level_scope();
            scope.generic_parameters = generic_parameter_scope(&function.generic_parameters);
            for parameter in &function.parameters {
                scope.values.insert(
                    parameter.name.clone(),
                    LocalBinding {
                        ty: from_type_syntax(&parameter.ty, &scope.generic_parameters),
                        mutable: false,
                    },
                );
            }

            let inferred = self.infer_expr(&function.body, &mut scope)?;
            let return_type = if let Some(explicit) = &function.return_type {
                let explicit = from_type_syntax(explicit, &scope.generic_parameters);
                if !inferred.is_assignable_to(&explicit) {
                    return Err(format!(
                        "function `{}` returns `{}`, which is not assignable to `{}`",
                        function.name,
                        inferred.render(),
                        explicit.render()
                    ));
                }
                explicit
            } else {
                inferred
            };

            if let Some(existing) = self.functions.get_mut(&function.name) {
                existing.summary.return_type = return_type;
            }
        }

        Ok(())
    }

    fn infer_value_initializers(&mut self) -> Result<(), String> {
        for item in &self.unit.items {
            let TopLevelItem::Value(value) = item else {
                continue;
            };

            let mut scope = self.top_level_scope();
            let inferred = self.infer_expr(&value.initializer, &mut scope)?;
            let ty = if let Some(explicit) = &value.ty {
                let explicit = from_type_syntax(explicit, &scope.generic_parameters);
                if !inferred.is_assignable_to(&explicit) {
                    return Err(format!(
                        "value `{}` has initializer type `{}`, which is not assignable to `{}`",
                        value.name,
                        inferred.render(),
                        explicit.render()
                    ));
                }
                explicit
            } else {
                inferred
            };

            if let Some(binding) = self.values.get_mut(&value.name) {
                binding.ty = ty;
            }
        }

        Ok(())
    }

    fn top_level_scope(&self) -> TypeScope {
        let mut scope = TypeScope::default();
        for binding in self.values.values() {
            scope.values.insert(
                binding.name.clone(),
                LocalBinding {
                    ty: binding.ty.clone(),
                    mutable: binding.mutable,
                },
            );
        }
        scope
    }

    fn infer_expr(&self, expr: &Expr, scope: &mut TypeScope) -> Result<ReplType, String> {
        match &expr.kind {
            ExprKind::Integer(_) => Ok(ReplType::Int),
            ExprKind::Float(_) => Ok(ReplType::Float),
            ExprKind::Bool(_) => Ok(ReplType::Bool),
            ExprKind::Null => Ok(ReplType::Null),
            ExprKind::String(_) => Ok(ReplType::String),
            ExprKind::List(items) => {
                let mut item_type = ReplType::Unknown("Unknown".to_owned());
                for item in items {
                    item_type = ReplType::unify(item_type, self.infer_expr(item, scope)?);
                }
                Ok(ReplType::List(Box::new(item_type)))
            }
            ExprKind::Tuple(items) => {
                if items.is_empty() {
                    Ok(ReplType::Unit)
                } else {
                    items
                        .iter()
                        .map(|item| self.infer_expr(item, scope))
                        .collect::<Result<Vec<_>, _>>()
                        .map(ReplType::Tuple)
                }
            }
            ExprKind::Record(fields) => fields
                .iter()
                .map(|field| {
                    self.infer_expr(&field.value, scope)
                        .map(|ty| RecordFieldType {
                            name: field.name.clone(),
                            ty,
                        })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(ReplType::Record),
            ExprKind::Name(name) => self.resolve_name_type(name, scope),
            ExprKind::Call { callee, arguments } => {
                let callee_type = self.infer_expr(callee, scope)?;
                self.infer_call(callee_type, arguments, scope)
            }
            ExprKind::Index { target, .. } => match self.infer_expr(target, scope)? {
                ReplType::List(item) | ReplType::Range(item) => Ok(*item),
                ReplType::Tuple(items) => Ok(items
                    .first()
                    .cloned()
                    .unwrap_or(ReplType::Unknown("Unknown".to_owned()))),
                other => Err(format!("cannot index value of type `{}`", other.render())),
            },
            ExprKind::Field { target, name } => self.infer_field(target, name, scope),
            ExprKind::SafeField { target, name } => {
                let ty = self.infer_field(target, name, scope)?;
                Ok(ReplType::Nullable(Box::new(ty)))
            }
            ExprKind::NonNull { target } => match self.infer_expr(target, scope)? {
                ReplType::Nullable(inner) => Ok(*inner),
                other => Ok(other),
            },
            ExprKind::ReceiverCall {
                receiver,
                callee,
                arguments,
            } => {
                let receiver_type = self.infer_expr(receiver, scope)?;
                let callee_type = self.resolve_name_type(callee, scope)?;
                let mut args = Vec::with_capacity(arguments.len() + 1);
                args.push(CallArgumentType::Positional(receiver_type));
                for argument in arguments {
                    match argument {
                        Argument::Positional(expr) => {
                            args.push(CallArgumentType::Positional(self.infer_expr(expr, scope)?));
                        }
                        Argument::Named { value, .. } => {
                            args.push(CallArgumentType::Named(self.infer_expr(value, scope)?));
                        }
                    }
                }
                self.apply_call(callee_type, args)
            }
            ExprKind::Unary { op, expr } => {
                let ty = self.infer_expr(expr, scope)?;
                match (op, ty) {
                    (UnaryOp::Negate, ReplType::Int) => Ok(ReplType::Int),
                    (UnaryOp::Negate, ReplType::Float) => Ok(ReplType::Float),
                    (UnaryOp::Not, ReplType::Bool) => Ok(ReplType::Bool),
                    (_, other) => Err(format!("operator is not defined for `{}`", other.render())),
                }
            }
            ExprKind::Binary { left, op, right } => self.infer_binary(left, *op, right, scope),
            ExprKind::Range(range) => {
                let start = range
                    .start
                    .as_ref()
                    .map(|expr| self.infer_expr(expr, scope))
                    .transpose()?
                    .unwrap_or_else(|| ReplType::Unknown("Unknown".to_owned()));
                let end = range
                    .end
                    .as_ref()
                    .map(|expr| self.infer_expr(expr, scope))
                    .transpose()?
                    .unwrap_or_else(|| start.clone());
                Ok(ReplType::Range(Box::new(ReplType::unify(start, end))))
            }
            ExprKind::If(expr) => {
                let mut ty = ReplType::Unit;
                for branch in &expr.branches {
                    let condition = self.infer_expr(&branch.condition, scope)?;
                    if condition != ReplType::Bool
                        && condition != ReplType::Unknown("Unknown".to_owned())
                    {
                        return Err("if condition must be Bool".to_owned());
                    }
                    ty = ReplType::unify(ty, self.infer_block(&branch.body, scope)?);
                }
                if let Some(else_branch) = &expr.else_branch {
                    ty = ReplType::unify(ty, self.infer_block(else_branch, scope)?);
                }
                Ok(ty)
            }
            ExprKind::When(expr) => {
                let _subject = self.infer_expr(&expr.subject, scope)?;
                let mut ty = ReplType::Unknown("Unknown".to_owned());
                for arm in &expr.arms {
                    let mut nested = scope.clone();
                    if let Some(binding) = &arm.binding {
                        nested.values.insert(
                            binding.clone(),
                            LocalBinding {
                                ty: from_type_syntax(&arm.ty, &nested.generic_parameters),
                                mutable: false,
                            },
                        );
                    }
                    ty = ReplType::unify(ty, self.infer_expr(&arm.body, &mut nested)?);
                }
                if let Some(else_arm) = &expr.else_arm {
                    ty = ReplType::unify(ty, self.infer_expr(else_arm, scope)?);
                }
                Ok(ty)
            }
            ExprKind::Lambda(lambda) => {
                let mut nested = scope.clone();
                let mut parameters = Vec::new();
                for parameter in &lambda.parameters {
                    let ty = parameter
                        .ty
                        .as_ref()
                        .map(|ty| from_type_syntax(ty, &nested.generic_parameters))
                        .unwrap_or_else(|| ReplType::Unknown(parameter.name.clone()));
                    nested.values.insert(
                        parameter.name.clone(),
                        LocalBinding {
                            ty: ty.clone(),
                            mutable: false,
                        },
                    );
                    parameters.push(ty);
                }
                let result = self.infer_expr(&lambda.body, &mut nested)?;
                Ok(ReplType::Function {
                    parameters,
                    result: Box::new(result),
                })
            }
            ExprKind::Block(block) => self.infer_block(block, scope),
            ExprKind::Econ { ty, .. } => Ok(ReplType::Named {
                name: "Econ".to_owned(),
                arguments: vec![from_type_syntax(ty, &scope.generic_parameters)],
            }),
        }
    }

    fn infer_block(&self, block: &BlockExpr, scope: &mut TypeScope) -> Result<ReplType, String> {
        let mut nested = scope.clone();
        for item in &block.items {
            match item {
                BlockItem::LocalValue(value) => self.infer_local_value(value, &mut nested)?,
                BlockItem::Assignment(assignment) => {
                    let current = nested
                        .values
                        .get(&assignment.name)
                        .cloned()
                        .ok_or_else(|| format!("unknown local `{}`", assignment.name))?;
                    if !current.mutable {
                        return Err(format!(
                            "cannot assign to immutable local `{}`",
                            assignment.name
                        ));
                    }
                    let next = self.infer_expr(&assignment.value, &mut nested)?;
                    if !next.is_assignable_to(&current.ty) {
                        return Err(format!(
                            "cannot assign `{}` to `{}`",
                            next.render(),
                            current.ty.render()
                        ));
                    }
                }
                BlockItem::CompoundAssignment(assignment) => {
                    let current = nested
                        .values
                        .get(&assignment.name)
                        .cloned()
                        .ok_or_else(|| format!("unknown local `{}`", assignment.name))?;
                    if !current.mutable {
                        return Err(format!(
                            "cannot assign to immutable local `{}`",
                            assignment.name
                        ));
                    }
                    let rhs = self.infer_expr(&assignment.value, &mut nested)?;
                    self.infer_compound_assignment(&current.ty, &rhs, assignment.op)?;
                }
                BlockItem::For(statement) => {
                    let iterable = self.infer_expr(&statement.iterable, &mut nested)?;
                    let element = match iterable {
                        ReplType::List(item) | ReplType::Range(item) => *item,
                        other => {
                            return Err(format!(
                                "for-loop requires an iterable list or range, found `{}`",
                                other.render()
                            ));
                        }
                    };
                    let mut loop_scope = nested.clone();
                    loop_scope.values.insert(
                        statement.pattern.clone(),
                        LocalBinding {
                            ty: element,
                            mutable: false,
                        },
                    );
                    self.infer_block(&statement.body, &mut loop_scope)?;
                }
                BlockItem::Return(statement) => {
                    return Ok(statement
                        .value
                        .as_ref()
                        .map(|value| self.infer_expr(value, &mut nested))
                        .transpose()?
                        .unwrap_or(ReplType::Unit));
                }
                BlockItem::Panic(_) => return Ok(ReplType::Unknown("Never".to_owned())),
                BlockItem::Expr(expr) => {
                    self.infer_expr(expr, &mut nested)?;
                }
            }
        }

        block
            .trailing
            .as_ref()
            .map(|expr| self.infer_expr(expr, &mut nested))
            .transpose()
            .map(|value| value.unwrap_or(ReplType::Unit))
    }

    fn infer_local_value(
        &self,
        value: &LocalValueDecl,
        scope: &mut TypeScope,
    ) -> Result<(), String> {
        let inferred = self.infer_expr(&value.initializer, scope)?;
        let ty = if let Some(explicit) = &value.ty {
            let explicit = from_type_syntax(explicit, &scope.generic_parameters);
            if !inferred.is_assignable_to(&explicit) {
                return Err(format!(
                    "local `{}` has initializer type `{}`, which is not assignable to `{}`",
                    value.name,
                    inferred.render(),
                    explicit.render()
                ));
            }
            explicit
        } else {
            inferred
        };
        scope.values.insert(
            value.name.clone(),
            LocalBinding {
                ty,
                mutable: matches!(value.mutability, Mutability::Var),
            },
        );
        Ok(())
    }

    fn infer_binary(
        &self,
        left: &Expr,
        op: BinaryOp,
        right: &Expr,
        scope: &mut TypeScope,
    ) -> Result<ReplType, String> {
        let left = self.infer_expr(left, scope)?;
        let right = self.infer_expr(right, scope)?;
        match op {
            BinaryOp::Add => addition_result(&left, &right).ok_or_else(|| {
                format!(
                    "operator `+` is not defined for `{}` and `{}`",
                    left.render(),
                    right.render()
                )
            }),
            BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide | BinaryOp::Remainder => {
                numeric_result(&left, &right)
            }
            BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
            | BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::And
            | BinaryOp::Or => Ok(ReplType::Bool),
            BinaryOp::Coalesce => match left {
                ReplType::Nullable(inner) => Ok(ReplType::unify(*inner, right)),
                other => Ok(ReplType::unify(other, right)),
            },
        }
    }

    fn infer_field(
        &self,
        target: &Expr,
        name: &str,
        scope: &mut TypeScope,
    ) -> Result<ReplType, String> {
        match self.infer_expr(target, scope)? {
            ReplType::Record(fields) => fields
                .into_iter()
                .find(|field| field.name == name)
                .map(|field| field.ty)
                .ok_or_else(|| format!("record has no field `{name}`")),
            ReplType::Nullable(inner) => match *inner {
                ReplType::Record(fields) => fields
                    .into_iter()
                    .find(|field| field.name == name)
                    .map(|field| ReplType::Nullable(Box::new(field.ty)))
                    .ok_or_else(|| format!("record has no field `{name}`")),
                other => Err(format!("cannot access field on `{}`", other.render())),
            },
            other => Err(format!("cannot access field on `{}`", other.render())),
        }
    }

    fn infer_compound_assignment(
        &self,
        left: &ReplType,
        right: &ReplType,
        op: CompoundAssignmentOp,
    ) -> Result<(), String> {
        match op {
            CompoundAssignmentOp::Add => {
                self.infer_compound_add(left, right)?;
                Ok(())
            }
            CompoundAssignmentOp::Subtract
            | CompoundAssignmentOp::Multiply
            | CompoundAssignmentOp::Divide
            | CompoundAssignmentOp::Remainder => {
                numeric_result(left, right)?;
                Ok(())
            }
        }
    }

    fn infer_compound_add(&self, left: &ReplType, right: &ReplType) -> Result<(), String> {
        addition_result(left, right).map(|_| ()).ok_or_else(|| {
            format!(
                "operator `+=` is not defined for `{}` and `{}`",
                left.render(),
                right.render()
            )
        })
    }

    fn infer_call(
        &self,
        callee_type: ReplType,
        arguments: &[Argument],
        scope: &mut TypeScope,
    ) -> Result<ReplType, String> {
        let arguments = arguments
            .iter()
            .map(|argument| match argument {
                Argument::Positional(expr) => self
                    .infer_expr(expr, scope)
                    .map(CallArgumentType::Positional),
                Argument::Named { value, .. } => {
                    self.infer_expr(value, scope).map(CallArgumentType::Named)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.apply_call(callee_type, arguments)
    }

    fn apply_call(
        &self,
        callee_type: ReplType,
        arguments: Vec<CallArgumentType>,
    ) -> Result<ReplType, String> {
        let (generic_parameters, parameters, result) = match callee_type {
            ReplType::Function { parameters, result } => (Vec::new(), parameters, result),
            ReplType::GenericFunction {
                generic_parameters,
                parameters,
                result,
            } => (generic_parameters, parameters, result),
            _ => return Err("attempted to call a non-function expression".to_owned()),
        };

        let mut assigned = vec![false; parameters.len()];
        let mut next_positional = 0usize;
        let mut substitutions = BTreeMap::new();

        for argument in arguments {
            match argument {
                CallArgumentType::Positional(ty) => {
                    while next_positional < parameters.len() && assigned[next_positional] {
                        next_positional += 1;
                    }
                    let Some(parameter) = parameters.get(next_positional) else {
                        return Err("too many positional arguments".to_owned());
                    };
                    if !self.argument_matches_parameter(
                        parameter,
                        &ty,
                        &mut substitutions,
                        !generic_parameters.is_empty(),
                    )? {
                        return Err(format!(
                            "argument of type `{}` is not assignable to `{}`",
                            ty.render(),
                            parameter.render()
                        ));
                    }
                    assigned[next_positional] = true;
                    next_positional += 1;
                }
                CallArgumentType::Named(ty) => {
                    if let Some((index, parameter)) = parameters
                        .iter()
                        .enumerate()
                        .find(|(index, _)| !assigned[*index])
                    {
                        if !self.argument_matches_parameter(
                            parameter,
                            &ty,
                            &mut substitutions,
                            !generic_parameters.is_empty(),
                        )? {
                            return Err(format!(
                                "argument of type `{}` is not assignable to `{}`",
                                ty.render(),
                                parameter.render()
                            ));
                        }
                        assigned[index] = true;
                    }
                }
            }
        }

        if generic_parameters.is_empty() {
            return Ok(*result);
        }

        let mut resolved = BTreeMap::new();
        for parameter in &generic_parameters {
            let Some(ty) = substitutions.get(&parameter.name).cloned() else {
                return Err(format!(
                    "could not infer a concrete type for generic parameter `{}`",
                    parameter.name
                ));
            };
            if !type_satisfies_bound(&ty, &parameter.bound) {
                return Err(format!(
                    "argument type `{}` does not satisfy bound `{}` for `{}`",
                    ty.render(),
                    parameter.bound,
                    parameter.name
                ));
            }
            resolved.insert(parameter.name.clone(), ty);
        }

        Ok(substitute_repl_type(&result, &resolved))
    }

    fn argument_matches_parameter(
        &self,
        parameter: &ReplType,
        argument: &ReplType,
        substitutions: &mut BTreeMap<String, ReplType>,
        allow_generic_inference: bool,
    ) -> Result<bool, String> {
        if !allow_generic_inference {
            return Ok(argument.is_assignable_to(parameter));
        }

        match_generic_parameter(parameter, argument, substitutions)
    }

    fn resolve_name_type(
        &self,
        name: &QualifiedName,
        scope: &TypeScope,
    ) -> Result<ReplType, String> {
        if name.segments.len() == 1 {
            let local = &name.segments[0];
            if let Some(binding) = scope.values.get(local) {
                return Ok(binding.ty.clone());
            }
            if let Some(function) = self.functions.get(local) {
                return Ok(self.function_repl_type(&function.summary));
            }
            return Err(format!("unknown name `{local}`"));
        }

        if let Some(local) = self.resolve_local_qualified_name(name) {
            if let Some(binding) = scope.values.get(&local) {
                return Ok(binding.ty.clone());
            }
            if let Some(function) = self.functions.get(&local) {
                return Ok(self.function_repl_type(&function.summary));
            }
        }

        self.resolve_imported_name(name)
    }

    fn resolve_local_qualified_name(&self, name: &QualifiedName) -> Option<String> {
        let module = self.module.split('.').collect::<Vec<_>>();
        if name.segments.len() != module.len() + 1 {
            return None;
        }
        for (expected, actual) in module.iter().zip(name.segments.iter()) {
            if expected != actual {
                return None;
            }
        }
        name.segments.last().cloned()
    }

    fn resolve_imported_name(&self, name: &QualifiedName) -> Result<ReplType, String> {
        for length in (1..name.segments.len()).rev() {
            let package = name.segments[..length].join(".");
            let Some(imported) = self.imports.get(&package) else {
                continue;
            };
            let Some(manifest) = &imported.manifest else {
                return Err(format!("package `{package}` is not mounted"));
            };

            if name.segments.len() == length + 1 {
                let symbol = &name.segments[length];
                if let Some(function) = manifest
                    .functions
                    .iter()
                    .find(|function| &function.name == symbol)
                {
                    return Ok(ReplType::Function {
                        parameters: function
                            .parameters
                            .iter()
                            .map(|parameter| from_vox_host_type(&parameter.ty))
                            .collect(),
                        result: Box::new(from_vox_host_type(&function.return_type)),
                    });
                }
                if manifest.types.iter().any(|ty| ty.name.name == *symbol) {
                    return Ok(ReplType::Named {
                        name: format!("{package}.{symbol}"),
                        arguments: Vec::new(),
                    });
                }
            }
        }

        Err(format!(
            "unknown qualified name `{}`",
            name.to_source_string()
        ))
    }

    fn function_repl_type(&self, summary: &FunctionSummary) -> ReplType {
        let parameters = summary
            .parameters
            .iter()
            .map(|parameter| parameter.ty.clone())
            .collect();
        let result = Box::new(summary.return_type.clone());
        if summary.generic_parameters.is_empty() {
            ReplType::Function { parameters, result }
        } else {
            ReplType::GenericFunction {
                generic_parameters: summary.generic_parameters.clone(),
                parameters,
                result,
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TypeScope {
    values: BTreeMap<String, LocalBinding>,
    generic_parameters: BTreeMap<String, GenericParameterSummary>,
}

#[derive(Debug, Clone)]
struct LocalBinding {
    ty: ReplType,
    mutable: bool,
}

enum CallArgumentType {
    Positional(ReplType),
    Named(ReplType),
}

fn addition_result(left: &ReplType, right: &ReplType) -> Option<ReplType> {
    match (left, right) {
        (ReplType::Int, ReplType::Int) => Some(ReplType::Int),
        (ReplType::Float, ReplType::Float)
        | (ReplType::Float, ReplType::Int)
        | (ReplType::Int, ReplType::Float) => Some(ReplType::Float),
        (ReplType::String, ReplType::String) => Some(ReplType::String),
        (
            ReplType::TypeParameter {
                name: left_name,
                bound: Some(left_bound),
            },
            ReplType::TypeParameter {
                name: right_name,
                bound: Some(right_bound),
            },
        ) if left_name == right_name
            && left_bound == right_bound
            && bound_allows_numeric(left_bound) =>
        {
            Some(left.clone())
        }
        _ => None,
    }
}

fn numeric_result(left: &ReplType, right: &ReplType) -> Result<ReplType, String> {
    match (left, right) {
        (ReplType::Int, ReplType::Int) => Ok(ReplType::Int),
        (ReplType::Float, ReplType::Float)
        | (ReplType::Float, ReplType::Int)
        | (ReplType::Int, ReplType::Float) => Ok(ReplType::Float),
        (
            ReplType::TypeParameter {
                name: left_name,
                bound: Some(left_bound),
            },
            ReplType::TypeParameter {
                name: right_name,
                bound: Some(right_bound),
            },
        ) if left_name == right_name
            && left_bound == right_bound
            && bound_allows_numeric(left_bound) =>
        {
            Ok(left.clone())
        }
        _ => Err(format!(
            "numeric operator is not defined for `{}` and `{}`",
            left.render(),
            right.render()
        )),
    }
}

fn from_type_syntax(
    ty: &TypeSyntax,
    generic_parameters: &BTreeMap<String, GenericParameterSummary>,
) -> ReplType {
    match &ty.kind {
        TypeKind::Function { parameters, result } => ReplType::Function {
            parameters: parameters
                .iter()
                .map(|parameter| from_type_syntax(parameter, generic_parameters))
                .collect(),
            result: Box::new(from_type_syntax(result, generic_parameters)),
        },
        TypeKind::Nullable(inner) => {
            ReplType::Nullable(Box::new(from_type_syntax(inner, generic_parameters)))
        }
        TypeKind::Named { name, arguments } => {
            let raw = name.to_source_string();
            match raw.as_str() {
                "Int" => ReplType::Int,
                "Float" => ReplType::Float,
                "Bool" => ReplType::Bool,
                "String" => ReplType::String,
                "Unit" => ReplType::Unit,
                "List" if arguments.len() == 1 => ReplType::List(Box::new(from_type_syntax(
                    &arguments[0],
                    generic_parameters,
                ))),
                _ if arguments.is_empty() => generic_parameters
                    .get(&raw)
                    .map(|parameter| ReplType::TypeParameter {
                        name: parameter.name.clone(),
                        bound: Some(parameter.bound.clone()),
                    })
                    .unwrap_or_else(|| ReplType::Named {
                        name: raw,
                        arguments: Vec::new(),
                    }),
                _ => ReplType::Named {
                    name: raw,
                    arguments: arguments
                        .iter()
                        .map(|argument| from_type_syntax(argument, generic_parameters))
                        .collect(),
                },
            }
        }
        TypeKind::Dyn(name) => ReplType::DynTrait(name.to_source_string()),
        TypeKind::Grouped(inner) => from_type_syntax(inner, generic_parameters),
        TypeKind::Tuple(items) => {
            if items.is_empty() {
                ReplType::Unit
            } else {
                ReplType::Tuple(
                    items
                        .iter()
                        .map(|item| from_type_syntax(item, generic_parameters))
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
                        .map(|field| RecordFieldType {
                            name: field.name.clone(),
                            ty: from_type_syntax(&field.ty, generic_parameters),
                        })
                        .collect(),
                )
            }
        }
    }
}

fn from_vox_host_type(ty: &vox_core::types::VoxType) -> ReplType {
    match ty {
        vox_core::types::VoxType::Int => ReplType::Int,
        vox_core::types::VoxType::Float => ReplType::Float,
        vox_core::types::VoxType::Bool => ReplType::Bool,
        vox_core::types::VoxType::String => ReplType::String,
        vox_core::types::VoxType::List(item) => ReplType::List(Box::new(from_vox_host_type(item))),
        vox_core::types::VoxType::Tuple(items) => {
            if items.is_empty() {
                ReplType::Unit
            } else {
                ReplType::Tuple(items.iter().map(from_vox_host_type).collect())
            }
        }
        vox_core::types::VoxType::Record(fields) => ReplType::Record(
            fields
                .iter()
                .map(|field| RecordFieldType {
                    name: field.name.clone(),
                    ty: from_vox_host_type(&field.ty),
                })
                .collect(),
        ),
        vox_core::types::VoxType::Nullable(inner) => {
            ReplType::Nullable(Box::new(from_vox_host_type(inner)))
        }
        vox_core::types::VoxType::DynTrait(name) => {
            ReplType::DynTrait(format!("{}.{}", name.module.as_str(), name.name))
        }
        vox_core::types::VoxType::Named(name) => ReplType::Named {
            name: format!("{}.{}", name.module.as_str(), name.name),
            arguments: Vec::new(),
        },
        vox_core::types::VoxType::TypeParameter(name) => ReplType::TypeParameter {
            name: name.clone(),
            bound: None,
        },
        vox_core::types::VoxType::OpaqueSurface(raw) => ReplType::Unknown(raw.clone()),
    }
}

fn generic_parameter_scope(
    parameters: &[vox_compiler::front_end::ast::GenericParameter],
) -> BTreeMap<String, GenericParameterSummary> {
    parameters
        .iter()
        .map(|parameter| {
            (
                parameter.name.clone(),
                GenericParameterSummary {
                    name: parameter.name.clone(),
                    bound: parameter.bound.clone(),
                },
            )
        })
        .collect()
}

fn match_generic_parameter(
    expected: &ReplType,
    actual: &ReplType,
    substitutions: &mut BTreeMap<String, ReplType>,
) -> Result<bool, String> {
    match expected {
        ReplType::TypeParameter { name, bound } => {
            if let Some(existing) = substitutions.get(name) {
                return Ok(actual.is_assignable_to(existing) && existing.is_assignable_to(actual));
            }
            if let Some(bound) = bound {
                if !type_satisfies_bound(actual, bound) {
                    return Ok(false);
                }
            }
            substitutions.insert(name.clone(), actual.clone());
            Ok(true)
        }
        ReplType::List(expected_item) => {
            let ReplType::List(actual_item) = actual else {
                return Ok(false);
            };
            match_generic_parameter(expected_item, actual_item, substitutions)
        }
        ReplType::Nullable(expected_inner) => {
            let ReplType::Nullable(actual_inner) = actual else {
                return Ok(false);
            };
            match_generic_parameter(expected_inner, actual_inner, substitutions)
        }
        ReplType::Tuple(expected_items) => {
            let ReplType::Tuple(actual_items) = actual else {
                return Ok(false);
            };
            if expected_items.len() != actual_items.len() {
                return Ok(false);
            }
            for (expected, actual) in expected_items.iter().zip(actual_items.iter()) {
                if !match_generic_parameter(expected, actual, substitutions)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        ReplType::Record(expected_fields) => {
            let ReplType::Record(actual_fields) = actual else {
                return Ok(false);
            };
            if expected_fields.len() != actual_fields.len() {
                return Ok(false);
            }
            for (expected, actual) in expected_fields.iter().zip(actual_fields.iter()) {
                if expected.name != actual.name
                    || !match_generic_parameter(&expected.ty, &actual.ty, substitutions)?
                {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        ReplType::Range(expected_item) => {
            let ReplType::Range(actual_item) = actual else {
                return Ok(false);
            };
            match_generic_parameter(expected_item, actual_item, substitutions)
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
                return Ok(false);
            };
            if expected_name != actual_name || expected_arguments.len() != actual_arguments.len() {
                return Ok(false);
            }
            for (expected, actual) in expected_arguments.iter().zip(actual_arguments.iter()) {
                if !match_generic_parameter(expected, actual, substitutions)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        ReplType::Function { parameters, result } => {
            let ReplType::Function {
                parameters: actual_parameters,
                result: actual_result,
            } = actual
            else {
                return Ok(false);
            };
            if parameters.len() != actual_parameters.len() {
                return Ok(false);
            }
            for (expected, actual) in parameters.iter().zip(actual_parameters.iter()) {
                if !match_generic_parameter(expected, actual, substitutions)? {
                    return Ok(false);
                }
            }
            match_generic_parameter(result, actual_result, substitutions)
        }
        _ => Ok(actual.is_assignable_to(expected)),
    }
}

fn substitute_repl_type(ty: &ReplType, substitutions: &BTreeMap<String, ReplType>) -> ReplType {
    match ty {
        ReplType::List(item) => ReplType::List(Box::new(substitute_repl_type(item, substitutions))),
        ReplType::Tuple(items) => ReplType::Tuple(
            items
                .iter()
                .map(|item| substitute_repl_type(item, substitutions))
                .collect(),
        ),
        ReplType::Nullable(inner) => {
            ReplType::Nullable(Box::new(substitute_repl_type(inner, substitutions)))
        }
        ReplType::Named { name, arguments } => ReplType::Named {
            name: name.clone(),
            arguments: arguments
                .iter()
                .map(|argument| substitute_repl_type(argument, substitutions))
                .collect(),
        },
        ReplType::Function { parameters, result } => ReplType::Function {
            parameters: parameters
                .iter()
                .map(|parameter| substitute_repl_type(parameter, substitutions))
                .collect(),
            result: Box::new(substitute_repl_type(result, substitutions)),
        },
        ReplType::Record(fields) => ReplType::Record(
            fields
                .iter()
                .map(|field| RecordFieldType {
                    name: field.name.clone(),
                    ty: substitute_repl_type(&field.ty, substitutions),
                })
                .collect(),
        ),
        ReplType::Range(item) => {
            ReplType::Range(Box::new(substitute_repl_type(item, substitutions)))
        }
        ReplType::TypeParameter { name, .. } => substitutions
            .get(name)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        _ => ty.clone(),
    }
}

fn bound_allows_numeric(bound: &str) -> bool {
    matches!(bound, "Numeric")
}

fn type_satisfies_bound(ty: &ReplType, bound: &str) -> bool {
    match bound {
        "Any" => true,
        "Numeric" => matches!(ty, ReplType::Int | ReplType::Float),
        _ => true,
    }
}

pub fn language_keywords() -> Vec<String> {
    [
        "as", "dyn", "econ", "else", "evil", "false", "for", "fun", "if", "import", "in", "is",
        "null", "package", "panic", "param", "private", "public", "return", "script", "true",
        "val", "var", "when",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

pub fn extend_manifest_symbols(symbols: &mut Vec<String>, manifest: &PackageManifest) {
    let package = manifest.package.as_str();
    symbols.push(package.clone());

    for function in &manifest.functions {
        symbols.push(format!("{package}.{}", function.name));
    }

    for ty in &manifest.types {
        symbols.push(format!("{package}.{}", ty.name.name));
    }
}

#[allow(dead_code)]
fn _module_path(raw: &str) -> ModulePath {
    ModulePath::parse(raw).expect("module path")
}
