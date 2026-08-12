use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

use vox_core::{
    diagnostics::DiagnosticBag,
    external_library::{ExternalLibraryHeader, MINIMAL_WASM_MODULE, encode_external_library_file},
    host::{
        FunctionExportKind, FunctionSpec, HostRegistry, PackageManifest, ParameterSpec, Purity,
        ValueSpec,
    },
    ids::ArtifactId,
    opt::{OptimizationLevel, OptimizationSubject},
    plan::{CompiledArtifact, DependencyFingerprint, ExecutablePlan},
    source::{ModuleKind, ModulePath, SourceText},
    tier::LanguageTier,
    types::VoxType,
};

use crate::backend::BackendPipeline;
use crate::frontend::ast::{FunctionDecl, TopLevelItem, ValueDecl, Visibility};
use crate::frontend::{FrontendUnit, analyze_source};
use crate::imports::{ImportResolution, resolve_imports};
use crate::mir::{MirPassFn, check_return_type_inference, lower_mir};
use crate::optimization::{OptimizationPipeline, derive_rankings};
use crate::treewalk::TreewalkScript;

#[derive(Debug, Clone)]
pub struct CompileRequest {
    pub source: SourceText,
    pub optimization: OptimizationLevel,
    pub optimization_overrides: BTreeMap<String, OptimizationLevel>,
    pub host: HostRegistry,
    pub tier: LanguageTier,
}

#[derive(Debug, Clone)]
pub struct CompileResult {
    pub artifact: Option<CompiledArtifact>,
    pub frontend: Option<FrontendUnit>,
    pub treewalk: Option<TreewalkScript>,
    pub diagnostics: DiagnosticBag,
}

#[derive(Debug, Default)]
pub struct Compiler {
    next_artifact_id: AtomicU64,
}

impl Compiler {
    pub fn compile(&self, request: CompileRequest) -> CompileResult {
        self.compile_with_mir_passes(request, &[])
    }

    pub fn compile_with_mir_passes(
        &self,
        request: CompileRequest,
        custom_mir_passes: &[MirPassFn],
    ) -> CompileResult {
        match analyze_source(&request.source) {
            Ok(frontend) => {
                let tier_diagnostics = validate_tier(&frontend, request.tier);
                let treewalk = TreewalkScript::lower(&frontend).ok();
                let pipeline_optimization = request
                    .optimization_overrides
                    .values()
                    .copied()
                    .fold(request.optimization, Ord::max);
                let optimization_rankings = derive_rankings(
                    &frontend,
                    request.optimization,
                    request.tier,
                    &request.optimization_overrides,
                );
                let module_rank = optimization_rankings
                    .iter()
                    .find_map(|ranking| match &ranking.subject {
                        OptimizationSubject::Module => Some(ranking.rank),
                        OptimizationSubject::Function(_) => None,
                    })
                    .expect("module ranking should always be present");
                let mut mir = lower_mir(
                    &frontend,
                    request.optimization,
                    request.tier,
                    &optimization_rankings,
                    build_import_resolution(&frontend, &request.host),
                );
                let return_type_diagnostics = check_return_type_inference(&frontend, &mir);
                let mut optimization_summary =
                    OptimizationPipeline::for_level(pipeline_optimization, request.tier)
                        .run(&mut mir, custom_mir_passes);
                let backend = BackendPipeline::default().lower(&mir);
                optimization_summary.extend(backend.summaries);
                let artifact = CompiledArtifact {
                    id: ArtifactId(self.next_artifact_id.fetch_add(1, Ordering::Relaxed) + 1),
                    module: frontend.header.module.clone(),
                    kind: frontend.header.kind,
                    optimization: request.optimization,
                    tier: request.tier,
                    optimization_rankings,
                    parameters: frontend
                        .parameters
                        .iter()
                        .cloned()
                        .map(|parameter| parameter.into_spec())
                        .collect(),
                    result_type: None,
                    purity: if matches!(
                        frontend.header.kind,
                        vox_core::source::ModuleKind::Script { evil: true }
                    ) {
                        Purity::Evil
                    } else {
                        Purity::Pure
                    },
                    plan: ExecutablePlan::deferred(module_rank)
                        .with_mir(&mir, optimization_summary)
                        .with_wasm(backend.wasm),
                    mir: Some(mir),
                    diagnostics: tier_diagnostics.clone(),
                    dependencies: collect_dependencies(&request),
                    source_revision: request.source.origin.revision,
                };

                CompileResult {
                    artifact: Some(artifact),
                    frontend: Some(frontend),
                    treewalk,
                    diagnostics: {
                        let mut diagnostics = return_type_diagnostics;
                        diagnostics.extend(tier_diagnostics.into_vec());
                        diagnostics
                    },
                }
            }
            Err(diagnostics) => CompileResult {
                artifact: None,
                frontend: None,
                treewalk: None,
                diagnostics,
            },
        }
    }
}

pub fn validate_tier(frontend: &FrontendUnit, tier: LanguageTier) -> DiagnosticBag {
    let mut diagnostics = DiagnosticBag::default();
    let required = |required: LanguageTier, span: &vox_core::diagnostics::TextSpan, name: &str| {
        if !tier.supports(required) {
            Some(
                vox_core::diagnostics::Diagnostic::error(format!(
                    "{name} requires the {} tier, but this unit is compiled at the {} tier",
                    required.as_str(),
                    tier.as_str()
                ))
                .with_span(span.clone()),
            )
        } else {
            None
        }
    };
    for item in &frontend.syntax.items {
        match item {
            TopLevelItem::Struct(structure) => {
                let needed = if matches!(frontend.header.kind, ModuleKind::Package) {
                    LanguageTier::Dev
                } else {
                    LanguageTier::Eval
                };
                if let Some(error) = required(needed, &structure.span, "struct declarations") {
                    diagnostics.push(error);
                }
            }
            TopLevelItem::Trait(trait_decl) => {
                let needed = if matches!(frontend.header.kind, ModuleKind::Package) {
                    LanguageTier::Dev
                } else {
                    LanguageTier::Eval
                };
                if let Some(error) = required(needed, &trait_decl.span, "trait declarations") {
                    diagnostics.push(error);
                }
            }
            TopLevelItem::Impl(implementation) => {
                if let Some(error) = required(
                    LanguageTier::Dev,
                    &implementation.span,
                    "trait implementations",
                ) {
                    diagnostics.push(error);
                }
            }
            TopLevelItem::Function(function) => {
                if let Some(error) = required(
                    LanguageTier::Script,
                    &function.span,
                    "function declarations",
                ) {
                    diagnostics.push(error);
                }
            }
            TopLevelItem::Value(value)
                if matches!(value.mutability, crate::frontend::ast::Mutability::Var) =>
            {
                if let Some(error) = required(LanguageTier::Script, &value.span, "mutable values") {
                    diagnostics.push(error);
                }
            }
            TopLevelItem::Value(value) => {
                if let Some(error) = required(LanguageTier::Eval, &value.span, "value declarations")
                {
                    diagnostics.push(error);
                }
            }
            TopLevelItem::Import(import) => {
                if let Some(error) = required(LanguageTier::Script, &import.span, "imports") {
                    diagnostics.push(error);
                }
            }
            _ => {}
        }
        match item {
            TopLevelItem::Value(value) => {
                validate_expr_tier(&value.initializer, tier, &mut diagnostics)
            }
            TopLevelItem::Function(function) => {
                validate_expr_tier(&function.body, tier, &mut diagnostics)
            }
            TopLevelItem::Struct(structure) => {
                for method in &structure.methods {
                    validate_expr_tier(&method.body, tier, &mut diagnostics);
                }
            }
            TopLevelItem::Trait(trait_decl) => {
                for method in &trait_decl.methods {
                    if let Some(body) = &method.body {
                        validate_expr_tier(body, tier, &mut diagnostics);
                    }
                }
            }
            TopLevelItem::Impl(implementation) => {
                for method in &implementation.methods {
                    validate_expr_tier(&method.body, tier, &mut diagnostics);
                }
            }
            TopLevelItem::Statement(statement) => {
                validate_block_item_tier(statement, tier, &mut diagnostics)
            }
            TopLevelItem::Param(param) => {
                if let Some(default) = &param.default {
                    validate_expr_tier(default, tier, &mut diagnostics);
                }
            }
            TopLevelItem::Import(_) => {}
        }
    }
    if let Some(result) = &frontend.syntax.result {
        validate_expr_tier(result, tier, &mut diagnostics);
    }
    diagnostics
}

fn require_tier(
    required: LanguageTier,
    tier: LanguageTier,
    span: &vox_core::diagnostics::TextSpan,
    feature: &str,
    diagnostics: &mut DiagnosticBag,
) {
    if !tier.supports(required) {
        diagnostics.push(
            vox_core::diagnostics::Diagnostic::error(format!(
                "{feature} requires the {} tier, but this unit is compiled at the {} tier",
                required.as_str(),
                tier.as_str()
            ))
            .with_span(span.clone()),
        );
    }
}

fn validate_expr_tier(
    expr: &crate::frontend::ast::Expr,
    tier: LanguageTier,
    diagnostics: &mut DiagnosticBag,
) {
    use crate::frontend::ast::ExprKind;
    match &expr.kind {
        ExprKind::If(if_expr) => {
            for branch in &if_expr.branches {
                validate_expr_tier(&branch.condition, tier, diagnostics);
                for item in &branch.body.items {
                    validate_block_item_tier(item, tier, diagnostics);
                }
                if let Some(trailing) = &branch.body.trailing {
                    validate_expr_tier(trailing, tier, diagnostics);
                }
            }
            if let Some(body) = &if_expr.else_branch {
                for item in &body.items {
                    validate_block_item_tier(item, tier, diagnostics);
                }
                if let Some(trailing) = &body.trailing {
                    validate_expr_tier(trailing, tier, diagnostics);
                }
            }
        }
        ExprKind::Block(block) => {
            require_tier(
                LanguageTier::Eval,
                tier,
                &expr.span,
                "block expressions",
                diagnostics,
            );
            for item in &block.items {
                validate_block_item_tier(item, tier, diagnostics);
            }
            if let Some(trailing) = &block.trailing {
                validate_expr_tier(trailing, tier, diagnostics);
            }
        }
        ExprKind::For(for_expr) => {
            require_tier(LanguageTier::Eval, tier, &expr.span, "loops", diagnostics);
            if let Some(init) = &for_expr.init {
                validate_block_item_tier(init, tier, diagnostics);
            }
            match &for_expr.header {
                crate::frontend::ast::ForHeader::In { iterable, .. } => {
                    validate_expr_tier(iterable, tier, diagnostics)
                }
                crate::frontend::ast::ForHeader::Condition(condition) => {
                    validate_expr_tier(condition, tier, diagnostics)
                }
            }
            for item in &for_expr.body.items {
                validate_block_item_tier(item, tier, diagnostics);
            }
            if let Some(trailing) = &for_expr.body.trailing {
                validate_expr_tier(trailing, tier, diagnostics);
            }
        }
        ExprKind::When(when_expr) => {
            require_tier(
                LanguageTier::Eval,
                tier,
                &expr.span,
                "when expressions",
                diagnostics,
            );
            validate_expr_tier(&when_expr.subject, tier, diagnostics);
            for arm in &when_expr.arms {
                validate_expr_tier(&arm.body, tier, diagnostics);
            }
            if let Some(else_arm) = &when_expr.else_arm {
                validate_expr_tier(else_arm, tier, diagnostics);
            }
        }
        ExprKind::Intrinsic(crate::frontend::ast::IntrinsicExpr::Econ(econ)) => {
            require_tier(
                LanguageTier::Eval,
                tier,
                &expr.span,
                "econ expressions",
                diagnostics,
            );
            for item in &econ.body.items {
                validate_block_item_tier(item, tier, diagnostics);
            }
            if let Some(trailing) = &econ.body.trailing {
                validate_expr_tier(trailing, tier, diagnostics);
            }
        }
        ExprKind::Intrinsic(crate::frontend::ast::IntrinsicExpr::Updated(updated)) => {
            validate_expr_tier(&updated.target, tier, diagnostics);
            for update in &updated.updates {
                validate_expr_tier(&update.value, tier, diagnostics);
            }
        }
        ExprKind::Call { callee, arguments } => {
            validate_expr_tier(callee, tier, diagnostics);
            for argument in arguments {
                match argument {
                    crate::frontend::ast::Argument::Positional(expr)
                    | crate::frontend::ast::Argument::Named { value: expr, .. } => {
                        validate_expr_tier(expr, tier, diagnostics)
                    }
                }
            }
        }
        ExprKind::Index { target, index } => {
            validate_expr_tier(target, tier, diagnostics);
            validate_expr_tier(index, tier, diagnostics);
        }
        ExprKind::Field { target, .. }
        | ExprKind::SafeField { target, .. }
        | ExprKind::NonNull { target } => validate_expr_tier(target, tier, diagnostics),
        ExprKind::ReceiverCall {
            receiver,
            arguments,
            ..
        } => {
            validate_expr_tier(receiver, tier, diagnostics);
            for argument in arguments {
                match argument {
                    crate::frontend::ast::Argument::Positional(expr)
                    | crate::frontend::ast::Argument::Named { value: expr, .. } => {
                        validate_expr_tier(expr, tier, diagnostics)
                    }
                }
            }
        }
        ExprKind::Unary { expr, .. } => validate_expr_tier(expr, tier, diagnostics),
        ExprKind::Binary { left, right, .. } => {
            validate_expr_tier(left, tier, diagnostics);
            validate_expr_tier(right, tier, diagnostics);
        }
        ExprKind::Range(range) => {
            if let Some(start) = &range.start {
                validate_expr_tier(start, tier, diagnostics);
            }
            if let Some(end) = &range.end {
                validate_expr_tier(end, tier, diagnostics);
            }
        }
        ExprKind::Lambda(lambda) => validate_expr_tier(&lambda.body, tier, diagnostics),
        ExprKind::List(items) | ExprKind::Tuple(items) => {
            for item in items {
                validate_expr_tier(item, tier, diagnostics);
            }
        }
        ExprKind::Record(fields) => {
            for field in fields {
                validate_expr_tier(&field.value, tier, diagnostics);
            }
        }
        ExprKind::Integer(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Null
        | ExprKind::String(_)
        | ExprKind::Name(_) => {}
    }
}

fn validate_block_item_tier(
    item: &crate::frontend::ast::BlockItem,
    tier: LanguageTier,
    diagnostics: &mut DiagnosticBag,
) {
    use crate::frontend::ast::BlockItem;
    match item {
        BlockItem::LocalValue(value) => {
            require_tier(
                LanguageTier::Eval,
                tier,
                &value.span,
                "local values",
                diagnostics,
            );
            if matches!(value.mutability, crate::frontend::ast::Mutability::Var) {
                require_tier(
                    LanguageTier::Script,
                    tier,
                    &value.span,
                    "mutable values",
                    diagnostics,
                );
            }
            validate_expr_tier(&value.initializer, tier, diagnostics);
        }
        BlockItem::Assignment(statement) => {
            require_tier(
                LanguageTier::Script,
                tier,
                &statement.span,
                "assignments",
                diagnostics,
            );
            validate_expr_tier(&statement.value, tier, diagnostics);
        }
        BlockItem::CompoundAssignment(statement) => {
            require_tier(
                LanguageTier::Script,
                tier,
                &statement.span,
                "assignments",
                diagnostics,
            );
            validate_expr_tier(&statement.value, tier, diagnostics);
        }
        BlockItem::Panic(statement) => require_tier(
            LanguageTier::Eval,
            tier,
            &statement.span,
            "panic statements",
            diagnostics,
        ),
        BlockItem::Return(statement) => {
            if let Some(value) = &statement.value {
                validate_expr_tier(value, tier, diagnostics);
            }
        }
        BlockItem::BlockStatement(expr) | BlockItem::Expr(expr) => {
            validate_expr_tier(expr, tier, diagnostics)
        }
        BlockItem::Break(_) | BlockItem::Continue(_) => require_tier(
            LanguageTier::Eval,
            tier,
            &item_span(item),
            "loop control",
            diagnostics,
        ),
    }
}

fn item_span(item: &crate::frontend::ast::BlockItem) -> vox_core::diagnostics::TextSpan {
    match item {
        crate::frontend::ast::BlockItem::LocalValue(value) => value.span.clone(),
        crate::frontend::ast::BlockItem::Assignment(statement) => statement.span.clone(),
        crate::frontend::ast::BlockItem::CompoundAssignment(statement) => statement.span.clone(),
        crate::frontend::ast::BlockItem::Return(statement) => statement.span.clone(),
        crate::frontend::ast::BlockItem::Panic(statement) => statement.span.clone(),
        crate::frontend::ast::BlockItem::Break(statement) => statement.span.clone(),
        crate::frontend::ast::BlockItem::Continue(statement) => statement.span.clone(),
        crate::frontend::ast::BlockItem::BlockStatement(expr)
        | crate::frontend::ast::BlockItem::Expr(expr) => expr.span.clone(),
    }
}

fn collect_dependencies(request: &CompileRequest) -> Vec<DependencyFingerprint> {
    request
        .host
        .packages()
        .map(|package| DependencyFingerprint {
            subject: package.package.as_str(),
            revision: request.source.origin.revision,
        })
        .collect()
}

fn build_import_resolution(frontend: &FrontendUnit, host: &HostRegistry) -> ImportResolution {
    let imports: Vec<_> = frontend
        .syntax
        .items
        .iter()
        .filter_map(|item| match item {
            TopLevelItem::Import(import) => Some(import.clone()),
            _ => None,
        })
        .collect();
    resolve_imports(&imports, host)
}

pub fn compile_to_voxlib(request: CompileRequest) -> Result<Vec<u8>, String> {
    let result = Compiler::default().compile(request);
    if result.diagnostics.has_errors() {
        return Err(result.diagnostics.to_string());
    }
    let artifact = result
        .artifact
        .ok_or_else(|| result.diagnostics.to_string())?;
    let frontend = result
        .frontend
        .as_ref()
        .expect("successful compilation should produce a frontend unit");
    let manifest = package_manifest_from_frontend(frontend)?;
    let wasm_bytes = match artifact.plan.wasm.as_ref() {
        Some(wasm) => wasm.bytes.clone(),
        None if manifest.functions.is_empty() && manifest.values.is_empty() => {
            MINIMAL_WASM_MODULE.to_vec()
        }
        None => {
            return Err(format!(
                "package `{}` has executable exports but could not be lowered to the .voxlib wasm ABI: {}",
                manifest.package.as_str(),
                artifact.plan.optimization_summary.join("; ")
            ));
        }
    };
    let header = ExternalLibraryHeader {
        manifest,
        wasm_bytes,
        metadata: None,
    };
    encode_external_library_file(&header).map_err(|error| error.to_string())
}

pub fn package_manifest_from_frontend(frontend: &FrontendUnit) -> Result<PackageManifest, String> {
    if !matches!(frontend.header.kind, ModuleKind::Package) {
        return Err(if frontend.header.anonymous {
            "anonymous scripts cannot be compiled as importable libraries".to_owned()
        } else {
            "script files cannot be compiled as importable libraries".to_owned()
        });
    }

    Ok(surface_manifest_from_frontend(frontend))
}

pub fn surface_manifest_from_frontend(frontend: &FrontendUnit) -> PackageManifest {
    PackageManifest {
        package: frontend.header.module.clone(),
        reexports: public_reexports(frontend),
        types: frontend
            .syntax
            .items
            .iter()
            .filter_map(|item| match item {
                TopLevelItem::Struct(structure)
                    if matches!(structure.visibility, Visibility::Public) =>
                {
                    Some(vox_core::host::TypeSpec {
                        name: vox_core::types::QualifiedTypeName {
                            module: frontend.header.module.clone(),
                            name: structure.name.clone(),
                        },
                        fields: structure
                            .fields
                            .iter()
                            .filter(|field| matches!(field.visibility, Visibility::Public))
                            .map(|field| vox_core::host::FieldSpec {
                                name: field.name.clone(),
                                ty: VoxType::opaque_surface(field.ty.to_source_string()),
                            })
                            .collect(),
                    })
                }
                _ => None,
            })
            .collect(),
        traits: frontend
            .syntax
            .items
            .iter()
            .filter_map(|item| match item {
                TopLevelItem::Trait(trait_decl)
                    if matches!(trait_decl.visibility, Visibility::Public) =>
                {
                    Some(vox_core::host::TraitSpec {
                        name: vox_core::types::QualifiedTypeName {
                            module: frontend.header.module.clone(),
                            name: trait_decl.name.clone(),
                        },
                        fields: trait_decl
                            .fields
                            .iter()
                            .filter(|field| matches!(field.visibility, Visibility::Public))
                            .map(|field| vox_core::host::FieldSpec {
                                name: field.name.clone(),
                                ty: VoxType::opaque_surface(field.ty.to_source_string()),
                            })
                            .collect(),
                        methods: trait_decl
                            .methods
                            .iter()
                            .filter(|method| matches!(method.visibility, Visibility::Public))
                            .map(|method| vox_core::host::TraitMethodSpec {
                                name: method.name.clone(),
                                lowered_by: format!("{}.{}", trait_decl.name, method.name),
                                parameters: method
                                    .parameters
                                    .iter()
                                    .skip(if method.associated { 0 } else { 1 })
                                    .map(|parameter| ParameterSpec {
                                        name: parameter.name.clone(),
                                        ty: VoxType::opaque_surface(
                                            parameter.ty.to_source_string(),
                                        ),
                                        has_default: parameter.default.is_some(),
                                    })
                                    .collect(),
                                return_type: method
                                    .return_type
                                    .as_ref()
                                    .map(|ty| VoxType::opaque_surface(ty.to_source_string()))
                                    .unwrap_or_else(|| {
                                        VoxType::opaque_surface(format!(
                                            "{} return type",
                                            method.name
                                        ))
                                    }),
                                purity: if method.evil {
                                    Purity::Evil
                                } else {
                                    Purity::Pure
                                },
                            })
                            .collect(),
                    })
                }
                _ => None,
            })
            .collect(),
        functions: frontend
            .syntax
            .items
            .iter()
            .filter_map(|item| match item {
                TopLevelItem::Function(function)
                    if matches!(function.visibility, Visibility::Public) =>
                {
                    Some(function_spec_from_decl(function))
                }
                _ => None,
            })
            .collect(),
        values: frontend
            .syntax
            .items
            .iter()
            .filter_map(|item| match item {
                TopLevelItem::Value(value) if matches!(value.visibility, Visibility::Public) => {
                    Some(value_spec_from_decl(value))
                }
                _ => None,
            })
            .collect(),
        trait_impls: frontend
            .syntax
            .items
            .iter()
            .filter_map(|item| match item {
                TopLevelItem::Impl(implementation) => {
                    let trait_name = vox_core::types::QualifiedTypeName {
                        module: frontend.header.module.clone(),
                        name: implementation.trait_name.to_source_string(),
                    };
                    let struct_name = vox_core::types::QualifiedTypeName {
                        module: frontend.header.module.clone(),
                        name: implementation.struct_name.to_source_string(),
                    };
                    Some((trait_name, struct_name))
                }
                _ => None,
            })
            .fold(BTreeMap::new(), |mut impls, (trait_name, struct_name)| {
                impls.entry(trait_name).or_default().insert(struct_name);
                impls
            }),
    }
}

fn public_reexports(frontend: &FrontendUnit) -> Vec<ModulePath> {
    frontend
        .syntax
        .items
        .iter()
        .filter_map(|item| match item {
            TopLevelItem::Import(import) if matches!(import.visibility, Visibility::Public) => {
                Some(import.expanded())
            }
            _ => None,
        })
        .flatten()
        .filter_map(|import| ModulePath::parse(&import.module.to_source_string()).ok())
        .collect()
}

fn function_spec_from_decl(function: &FunctionDecl) -> FunctionSpec {
    FunctionSpec {
        name: function.name.clone(),
        parameters: function
            .parameters
            .iter()
            .map(|parameter| ParameterSpec {
                name: parameter.name.clone(),
                ty: VoxType::opaque_surface(parameter.ty.to_source_string()),
                has_default: parameter.default.is_some(),
            })
            .collect(),
        return_type: function
            .return_type
            .as_ref()
            .map(|ty| VoxType::opaque_surface(ty.to_source_string()))
            .unwrap_or_else(|| VoxType::opaque_surface(format!("{} return type", function.name))),
        purity: if function.evil {
            Purity::Evil
        } else {
            Purity::Pure
        },
        export: FunctionExportKind::Function,
    }
}

fn value_spec_from_decl(value: &ValueDecl) -> ValueSpec {
    ValueSpec {
        name: value.name.clone(),
        ty: value
            .ty
            .as_ref()
            .map(|ty| VoxType::opaque_surface(ty.to_source_string()))
            .unwrap_or_else(|| VoxType::opaque_surface(format!("{} type", value.name))),
        purity: Purity::Pure,
    }
}
