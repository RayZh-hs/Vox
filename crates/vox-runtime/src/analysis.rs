use std::collections::{BTreeMap, BTreeSet};

use vox_compiler::front_end::ast::{
    Argument, BinaryOp, BlockExpr, BlockItem, CompilationUnit, CompoundAssignmentOp, EconIntrinsic,
    Expr, ExprKind, FunctionDecl, IntrinsicExpr, LocalValueDecl, Mutability, QualifiedName,
    StringPart, TopLevelItem, TypeKind, TypeSyntax, UnaryOp, UpdatedIntrinsic, UpdatedPathSegment,
    ValueDecl,
};
use vox_core::{
    host::PackageManifest,
    source::{ModuleKind, ModulePath},
};

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
            (left, Self::Nullable(right)) => left.is_assignable_to(right),
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
        self.collect_imports()?;
        self.collect_function_headers();
        match self.unit.header.kind {
            ModuleKind::Package => {
                self.collect_value_placeholders();
                self.infer_function_bodies()?;
                self.infer_value_initializers()?;
            }
            ModuleKind::Script { .. } => {
                self.collect_parameter_bindings();
                self.infer_parameter_defaults()?;
                self.infer_script_items()?;
            }
        }
        Ok(())
    }

    fn collect_imports(&mut self) -> Result<(), String> {
        for item in &self.unit.items {
            if let TopLevelItem::Import(import) = item {
                let name = import.module.to_source_string();
                let manifest = self
                    .manifests
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| format!("imported package `{name}` is not mounted"))?;
                self.imports.insert(
                    name.clone(),
                    ImportedPackage {
                        manifest: Some(manifest),
                    },
                );
            }
        }
        Ok(())
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
            match item {
                TopLevelItem::Param(parameter) => {
                    self.values.insert(
                        parameter.name.clone(),
                        ValueInfo {
                            name: parameter.name.clone(),
                            ty: from_type_syntax(&parameter.ty, &BTreeMap::new()),
                            mutable: false,
                        },
                    );
                }
                TopLevelItem::Value(value) => {
                    self.values.insert(
                        value.name.clone(),
                        ValueInfo {
                            name: value.name.clone(),
                            ty: value
                                .ty
                                .as_ref()
                                .map(|ty| from_type_syntax(ty, &BTreeMap::new()))
                                .unwrap_or_else(|| {
                                    ReplType::Unknown(format!("{} type", value.name))
                                }),
                            mutable: matches!(value.mutability, Mutability::Var),
                        },
                    );
                }
                TopLevelItem::Import(_) | TopLevelItem::Function(_) => {}
                TopLevelItem::Statement(_) => {}
            }
        }
    }

    fn collect_parameter_bindings(&mut self) {
        for item in &self.unit.items {
            let TopLevelItem::Param(parameter) = item else {
                continue;
            };
            self.values.insert(
                parameter.name.clone(),
                ValueInfo {
                    name: parameter.name.clone(),
                    ty: from_type_syntax(&parameter.ty, &BTreeMap::new()),
                    mutable: false,
                },
            );
        }
    }

    fn infer_parameter_defaults(&self) -> Result<(), String> {
        for item in &self.unit.items {
            let TopLevelItem::Param(parameter) = item else {
                continue;
            };
            let Some(default) = &parameter.default else {
                continue;
            };

            let mut scope = self.top_level_scope();
            let inferred = self.infer_expr(default, &mut scope)?;
            let explicit = from_type_syntax(&parameter.ty, &scope.generic_parameters);
            if !inferred.is_assignable_to(&explicit) {
                return Err(format!(
                    "parameter `{}` has default type `{}`, which is not assignable to `{}`",
                    parameter.name,
                    inferred.render(),
                    explicit.render()
                ));
            }
        }

        Ok(())
    }

    fn infer_function_bodies(&mut self) -> Result<(), String> {
        for item in &self.unit.items {
            let TopLevelItem::Function(function) = item else {
                continue;
            };

            let mut scope = self.top_level_scope();
            self.infer_function_body(function, &mut scope)?;
        }

        Ok(())
    }

    fn infer_value_initializers(&mut self) -> Result<(), String> {
        for item in &self.unit.items {
            let TopLevelItem::Value(value) = item else {
                continue;
            };

            let mut scope = self.top_level_scope();
            let ty = self.infer_value_initializer(value, &mut scope)?;

            if let Some(binding) = self.values.get_mut(&value.name) {
                binding.ty = ty;
            }
        }

        Ok(())
    }

    fn infer_script_items(&mut self) -> Result<(), String> {
        let mut scope = self.top_level_scope();
        let mut finalized_values = BTreeMap::<String, BTreeSet<String>>::new();
        for item in &self.unit.items {
            match item {
                TopLevelItem::Import(_) | TopLevelItem::Param(_) => {}
                TopLevelItem::Function(function) => {
                    let captured = self.infer_function_body(function, &mut scope.clone())?;
                    for name in captured {
                        finalized_values
                            .entry(name)
                            .or_default()
                            .insert(function.name.clone());
                    }
                }
                TopLevelItem::Value(value) => {
                    if let Some(functions) = finalized_values.get(&value.name) {
                        return Err(captured_rebind_error(&value.name, functions));
                    }
                    let ty = self.infer_value_initializer(value, &mut scope)?;
                    let binding = LocalBinding {
                        ty: ty.clone(),
                        mutable: matches!(value.mutability, Mutability::Var),
                    };
                    scope.values.insert(value.name.clone(), binding);
                    self.values.insert(
                        value.name.clone(),
                        ValueInfo {
                            name: value.name.clone(),
                            ty,
                            mutable: matches!(value.mutability, Mutability::Var),
                        },
                    );
                }
                TopLevelItem::Statement(statement) => {
                    self.infer_script_statement(statement, &mut scope)?;
                }
            }
        }
        Ok(())
    }

    fn infer_function_body(
        &mut self,
        function: &FunctionDecl,
        scope: &mut TypeScope,
    ) -> Result<BTreeSet<String>, String> {
        let captured_values = self.validate_function_captures(function, scope)?;
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

        let inferred = self.infer_expr(&function.body, scope)?;
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
        Ok(captured_values)
    }

    fn validate_function_captures(
        &self,
        function: &FunctionDecl,
        scope: &TypeScope,
    ) -> Result<BTreeSet<String>, String> {
        let parameter_names = function
            .parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<BTreeSet<_>>();
        let captures = CaptureNameCollector::new(&self.module, &scope.values, parameter_names)
            .collect_function(function);
        for name in &captures {
            let Some(binding) = scope.values.get(name) else {
                continue;
            };
            if binding.mutable {
                return Err(format!(
                    "function `{}` cannot capture mutable binding `{}`; bind it to a `val` first",
                    function.name, name
                ));
            }
        }
        Ok(captures)
    }

    fn infer_value_initializer(
        &self,
        value: &ValueDecl,
        scope: &mut TypeScope,
    ) -> Result<ReplType, String> {
        let inferred = self.infer_expr(&value.initializer, scope)?;
        if let Some(explicit) = &value.ty {
            let explicit = from_type_syntax(explicit, &scope.generic_parameters);
            if !inferred.is_assignable_to(&explicit) {
                return Err(format!(
                    "value `{}` has initializer type `{}`, which is not assignable to `{}`",
                    value.name,
                    inferred.render(),
                    explicit.render()
                ));
            }
            Ok(explicit)
        } else {
            Ok(inferred)
        }
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
            ExprKind::Intrinsic(intrinsic) => self.infer_intrinsic(intrinsic, scope),
            ExprKind::Index { target, .. } => match self.infer_expr(target, scope)? {
                ReplType::List(item) | ReplType::Range(item) => Ok(*item),
                ReplType::Tuple(items) => Ok(items
                    .first()
                    .cloned()
                    .unwrap_or(ReplType::Unknown("Unknown".to_owned()))),
                other => Err(format!("cannot index value of type `{}`", other.render())),
            },
            ExprKind::Field { target, name } => {
                if let Some(qualified) = expr_as_qualified_name(expr) {
                    if let Ok(ty) = self.resolve_name_type(&qualified, scope) {
                        return Ok(ty);
                    }
                }
                self.infer_field(target, name, scope)
            }
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
                let parameter_names = lambda
                    .parameters
                    .iter()
                    .map(|parameter| parameter.name.clone())
                    .collect::<BTreeSet<_>>();
                let captures =
                    CaptureNameCollector::new(&self.module, &nested.values, parameter_names)
                        .collect(&lambda.body);
                for name in captures {
                    let Some(binding) = nested.values.get(&name) else {
                        continue;
                    };
                    if binding.mutable {
                        return Err(format!(
                            "lambda cannot capture mutable binding `{name}`; bind it to a `val` first"
                        ));
                    }
                }
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
        }
    }

    fn infer_intrinsic(
        &self,
        intrinsic: &IntrinsicExpr,
        scope: &mut TypeScope,
    ) -> Result<ReplType, String> {
        match intrinsic {
            IntrinsicExpr::Updated(updated) => self.infer_updated(updated, scope),
            IntrinsicExpr::Econ(econ) => self.infer_econ(econ, scope),
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

    fn infer_script_statement(
        &self,
        statement: &BlockItem,
        scope: &mut TypeScope,
    ) -> Result<(), String> {
        match statement {
            BlockItem::LocalValue(value) => self.infer_local_value(value, scope),
            BlockItem::Assignment(assignment) => {
                let current = scope
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
                let next = self.infer_expr(&assignment.value, scope)?;
                if !next.is_assignable_to(&current.ty) {
                    return Err(format!(
                        "cannot assign `{}` to `{}`",
                        next.render(),
                        current.ty.render()
                    ));
                }
                Ok(())
            }
            BlockItem::CompoundAssignment(assignment) => {
                let current = scope
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
                let rhs = self.infer_expr(&assignment.value, scope)?;
                self.infer_compound_assignment(&current.ty, &rhs, assignment.op)
            }
            BlockItem::For(statement) => {
                let iterable = self.infer_expr(&statement.iterable, scope)?;
                let element = match iterable {
                    ReplType::List(item) | ReplType::Range(item) => *item,
                    other => {
                        return Err(format!(
                            "for-loop requires an iterable list or range, found `{}`",
                            other.render()
                        ));
                    }
                };
                let mut loop_scope = scope.clone();
                loop_scope.values.insert(
                    statement.pattern.clone(),
                    LocalBinding {
                        ty: element,
                        mutable: false,
                    },
                );
                self.infer_block(&statement.body, &mut loop_scope)?;
                Ok(())
            }
            BlockItem::Return(_) => {
                Err("`return` may only be used inside a function body".to_owned())
            }
            BlockItem::Panic(_) => Ok(()),
            BlockItem::Expr(expr) => {
                self.infer_expr(expr, scope)?;
                Ok(())
            }
        }
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

    fn infer_updated(
        &self,
        updated: &UpdatedIntrinsic,
        scope: &mut TypeScope,
    ) -> Result<ReplType, String> {
        let updates = &updated.updates;
        if updates.is_empty() {
            return Err("`updated` requires at least one field assignment".to_owned());
        }

        let target_type = self.infer_expr(&updated.target, scope)?;
        let mut seen = BTreeSet::new();

        for update in updates {
            let path = render_updated_path(&update.path);
            if !seen.insert(path.clone()) {
                return Err(format!("updated path `{path}` was provided more than once"));
            }

            let expected = self.updated_path_type(&target_type, &update.path)?;
            let actual = self.infer_expr(&update.value, scope)?;
            if !actual.is_assignable_to(&expected) {
                return Err(format!(
                    "cannot assign `{}` to updated path `{}` of type `{}`",
                    actual.render(),
                    path,
                    expected.render()
                ));
            }
        }

        Ok(target_type)
    }

    fn infer_econ(&self, econ: &EconIntrinsic, scope: &TypeScope) -> Result<ReplType, String> {
        Ok(ReplType::Named {
            name: "Econ".to_owned(),
            arguments: vec![from_type_syntax(&econ.ty, &scope.generic_parameters)],
        })
    }

    fn updated_path_type(
        &self,
        target: &ReplType,
        path: &[UpdatedPathSegment],
    ) -> Result<ReplType, String> {
        let Some((segment, rest)) = path.split_first() else {
            return Err("updated path cannot be empty".to_owned());
        };

        match (target, segment) {
            (ReplType::Record(fields), UpdatedPathSegment::Field(name)) => {
                let field = fields
                    .iter()
                    .find(|field| field.name == *name)
                    .ok_or_else(|| format!("record has no field `{name}`"))?;
                if rest.is_empty() {
                    Ok(field.ty.clone())
                } else {
                    self.updated_path_type(&field.ty, rest)
                }
            }
            (ReplType::Record(_), UpdatedPathSegment::Index(index)) => Err(format!(
                "record updates require a field name, found `#{index}`"
            )),
            (ReplType::Tuple(items), UpdatedPathSegment::Index(index)) => {
                let field = items
                    .get(*index)
                    .ok_or_else(|| format!("tuple index {index} is out of bounds"))?;
                if rest.is_empty() {
                    Ok(field.clone())
                } else {
                    self.updated_path_type(field, rest)
                }
            }
            (ReplType::Tuple(_), UpdatedPathSegment::Field(name)) => Err(format!(
                "tuple updates require an index like `#0`, found `{name}`"
            )),
            (ReplType::List(item), UpdatedPathSegment::Index(_)) => {
                if rest.is_empty() {
                    Ok((**item).clone())
                } else {
                    self.updated_path_type(item, rest)
                }
            }
            (ReplType::List(_), UpdatedPathSegment::Field(name)) => Err(format!(
                "list updates require an index like `#0`, found `{name}`"
            )),
            (ReplType::Unknown(_), _) => Ok(target.clone()),
            (other, _) => Err(format!("updated is not supported for `{}`", other.render())),
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
            if scope.generic_parameters.contains_key(local) {
                return Err(type_name_used_as_value_error(local, "type parameter"));
            }
            if let Some(kind) = predefined_type_name_kind(local) {
                return Err(type_name_used_as_value_error(local, kind));
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
                    return Err(type_name_used_as_value_error(
                        &format!("{package}.{symbol}"),
                        "type",
                    ));
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

struct CaptureNameCollector<'a> {
    module_segments: Vec<String>,
    visible: &'a BTreeMap<String, LocalBinding>,
    scopes: Vec<BTreeSet<String>>,
    captures: BTreeSet<String>,
}

impl<'a> CaptureNameCollector<'a> {
    fn new(
        module: &str,
        visible: &'a BTreeMap<String, LocalBinding>,
        parameters: BTreeSet<String>,
    ) -> Self {
        Self {
            module_segments: module.split('.').map(str::to_owned).collect(),
            visible,
            scopes: vec![parameters],
            captures: BTreeSet::new(),
        }
    }

    fn collect_function(mut self, function: &FunctionDecl) -> BTreeSet<String> {
        for parameter in &function.parameters {
            if let Some(default) = &parameter.default {
                self.visit_expr(default);
            }
        }
        self.visit_expr(&function.body);
        self.captures
    }

    fn collect(mut self, expr: &Expr) -> BTreeSet<String> {
        self.visit_expr(expr);
        self.captures
    }

    fn push_scope(&mut self) {
        self.scopes.push(BTreeSet::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn bind_name(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned());
        }
    }

    fn is_shadowed(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|scope| scope.contains(name))
    }

    fn capture_name(&mut self, name: &QualifiedName) {
        let local = match name.segments.as_slice() {
            [local] => local,
            segments
                if segments.len() == self.module_segments.len() + 1
                    && segments[..self.module_segments.len()]
                        .iter()
                        .zip(self.module_segments.iter())
                        .all(|(left, right)| left == right) =>
            {
                segments.last().expect("qualified name has a local segment")
            }
            _ => return,
        };

        if !self.is_shadowed(local) && self.visible.contains_key(local) {
            self.captures.insert(local.clone());
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Integer(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Null => {}
            ExprKind::Name(name) => self.capture_name(name),
            ExprKind::String(literal) => {
                for part in &literal.parts {
                    if let StringPart::Interpolation(expr) = part {
                        self.visit_expr(expr);
                    }
                }
            }
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for item in items {
                    self.visit_expr(item);
                }
            }
            ExprKind::Record(fields) => {
                for field in fields {
                    self.visit_expr(&field.value);
                }
            }
            ExprKind::Call { callee, arguments } => {
                self.visit_expr(callee);
                for argument in arguments {
                    self.visit_argument(argument);
                }
            }
            ExprKind::Intrinsic(intrinsic) => self.visit_intrinsic(intrinsic),
            ExprKind::Index { target, index } => {
                self.visit_expr(target);
                self.visit_expr(index);
            }
            ExprKind::Field { target, .. }
            | ExprKind::SafeField { target, .. }
            | ExprKind::NonNull { target } => self.visit_expr(target),
            ExprKind::ReceiverCall {
                receiver,
                callee,
                arguments,
            } => {
                self.visit_expr(receiver);
                self.capture_name(callee);
                for argument in arguments {
                    self.visit_argument(argument);
                }
            }
            ExprKind::Unary { expr, .. } => self.visit_expr(expr),
            ExprKind::Binary { left, right, .. } => {
                self.visit_expr(left);
                self.visit_expr(right);
            }
            ExprKind::Range(range) => {
                if let Some(start) = &range.start {
                    self.visit_expr(start);
                }
                if let Some(end) = &range.end {
                    self.visit_expr(end);
                }
            }
            ExprKind::If(expr) => {
                for branch in &expr.branches {
                    self.visit_expr(&branch.condition);
                    self.visit_block(&branch.body);
                }
                if let Some(else_branch) = &expr.else_branch {
                    self.visit_block(else_branch);
                }
            }
            ExprKind::When(expr) => {
                self.visit_expr(&expr.subject);
                for arm in &expr.arms {
                    self.push_scope();
                    if let Some(binding) = &arm.binding {
                        self.bind_name(binding);
                    }
                    self.visit_expr(&arm.body);
                    self.pop_scope();
                }
                if let Some(else_arm) = &expr.else_arm {
                    self.visit_expr(else_arm);
                }
            }
            ExprKind::Lambda(lambda) => {
                self.push_scope();
                for parameter in &lambda.parameters {
                    self.bind_name(&parameter.name);
                }
                self.visit_expr(&lambda.body);
                self.pop_scope();
            }
            ExprKind::Block(block) => self.visit_block(block),
        }
    }

    fn visit_argument(&mut self, argument: &Argument) {
        match argument {
            Argument::Positional(expr) => self.visit_expr(expr),
            Argument::Named { value, .. } => self.visit_expr(value),
        }
    }

    fn visit_intrinsic(&mut self, intrinsic: &IntrinsicExpr) {
        match intrinsic {
            IntrinsicExpr::Updated(updated) => {
                self.visit_expr(&updated.target);
                for update in &updated.updates {
                    self.visit_expr(&update.value);
                }
            }
            IntrinsicExpr::Econ(econ) => self.visit_block(&econ.body),
        }
    }

    fn visit_block(&mut self, block: &BlockExpr) {
        self.push_scope();
        self.visit_block_contents(block);
        self.pop_scope();
    }

    fn visit_block_contents(&mut self, block: &BlockExpr) {
        for item in &block.items {
            match item {
                BlockItem::LocalValue(value) => {
                    self.visit_expr(&value.initializer);
                    self.bind_name(&value.name);
                }
                BlockItem::Assignment(assignment) => {
                    self.capture_name(&QualifiedName {
                        segments: vec![assignment.name.clone()],
                        span: assignment.span.clone(),
                    });
                    self.visit_expr(&assignment.value);
                }
                BlockItem::CompoundAssignment(assignment) => {
                    self.capture_name(&QualifiedName {
                        segments: vec![assignment.name.clone()],
                        span: assignment.span.clone(),
                    });
                    self.visit_expr(&assignment.value);
                }
                BlockItem::For(statement) => {
                    self.visit_expr(&statement.iterable);
                    self.push_scope();
                    self.bind_name(&statement.pattern);
                    self.visit_block_contents(&statement.body);
                    self.pop_scope();
                }
                BlockItem::Return(statement) => {
                    if let Some(value) = &statement.value {
                        self.visit_expr(value);
                    }
                }
                BlockItem::Panic(statement) => {
                    for part in &statement.message.parts {
                        if let StringPart::Interpolation(expr) = part {
                            self.visit_expr(expr);
                        }
                    }
                }
                BlockItem::Expr(expr) => self.visit_expr(expr),
            }
        }
        if let Some(trailing) = &block.trailing {
            self.visit_expr(trailing);
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

fn captured_rebind_error(name: &str, functions: &BTreeSet<String>) -> String {
    let functions = render_function_set(functions);
    format!("cannot rebind `{name}` because it is captured by {functions} in this scope")
}

fn render_function_set(functions: &BTreeSet<String>) -> String {
    let names = functions
        .iter()
        .map(|function| format!("`{function}`"))
        .collect::<Vec<_>>();
    match names.as_slice() {
        [single] => format!("function {single}"),
        _ => format!("functions {}", names.join(", ")),
    }
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

fn render_updated_path(path: &[UpdatedPathSegment]) -> String {
    path.iter()
        .map(|segment| match segment {
            UpdatedPathSegment::Field(name) => name.clone(),
            UpdatedPathSegment::Index(index) => format!("#{index}"),
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn expr_as_qualified_name(expr: &Expr) -> Option<QualifiedName> {
    match &expr.kind {
        ExprKind::Name(name) => Some(name.clone()),
        ExprKind::Field { target, name } => {
            let mut qualified = expr_as_qualified_name(target)?;
            qualified.segments.push(name.clone());
            qualified.span = expr.span.clone();
            Some(qualified)
        }
        _ => None,
    }
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
        ReplType::Nullable(expected_inner) => match actual {
            ReplType::Null => Ok(true),
            ReplType::Nullable(actual_inner) => {
                match_generic_parameter(expected_inner, actual_inner, substitutions)
            }
            actual => match_generic_parameter(expected_inner, actual, substitutions),
        },
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

fn predefined_type_name_kind(name: &str) -> Option<&'static str> {
    match name {
        "Int" | "Float" | "Bool" | "String" | "Unit" => Some("type"),
        "List" | "Econ" => Some("type constructor"),
        _ => None,
    }
}

fn type_name_used_as_value_error(name: &str, kind: &str) -> String {
    format!(
        "`{name}` is a {kind}, not a value; use it only in type positions such as annotations or `when ... is` arms"
    )
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
