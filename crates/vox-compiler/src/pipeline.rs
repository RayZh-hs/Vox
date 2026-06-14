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
    source::{ModulePath, SourceText},
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
                let treewalk = TreewalkScript::lower(&frontend).ok();
                let pipeline_optimization = request
                    .optimization_overrides
                    .values()
                    .copied()
                    .fold(request.optimization, Ord::max);
                let optimization_rankings = derive_rankings(
                    &frontend,
                    request.optimization,
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
                    &optimization_rankings,
                    build_import_resolution(&frontend, &request.host),
                );
                let return_type_diagnostics = check_return_type_inference(&frontend, &mir);
                let mut optimization_summary =
                    OptimizationPipeline::for_level(pipeline_optimization)
                        .run(&mut mir, custom_mir_passes);
                let backend = BackendPipeline::default().lower(&mir);
                optimization_summary.extend(backend.summaries);
                let artifact = CompiledArtifact {
                    id: ArtifactId(self.next_artifact_id.fetch_add(1, Ordering::Relaxed) + 1),
                    module: frontend.header.module.clone(),
                    kind: frontend.header.kind,
                    optimization: request.optimization,
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
                    diagnostics: DiagnosticBag::default(),
                    dependencies: collect_dependencies(&request),
                    source_revision: request.source.origin.revision,
                };

                CompileResult {
                    artifact: Some(artifact),
                    frontend: Some(frontend),
                    treewalk,
                    diagnostics: return_type_diagnostics,
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
    let artifact = result
        .artifact
        .ok_or_else(|| result.diagnostics.to_string())?;
    let wasm_bytes = artifact
        .plan
        .wasm
        .as_ref()
        .map(|wasm| wasm.bytes.clone())
        .unwrap_or_else(|| MINIMAL_WASM_MODULE.to_vec());

    let frontend = result
        .frontend
        .as_ref()
        .expect("successful compilation should produce a frontend unit");
    let manifest = package_manifest_from_frontend(frontend);
    let header = ExternalLibraryHeader {
        manifest,
        wasm_bytes,
        metadata: None,
    };
    encode_external_library_file(&header).map_err(|error| error.to_string())
}

pub fn package_manifest_from_frontend(frontend: &FrontendUnit) -> PackageManifest {
    PackageManifest {
        package: frontend.header.module.clone(),
        reexports: public_reexports(frontend),
        types: Vec::new(),
        traits: Vec::new(),
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
        trait_impls: BTreeMap::new(),
    }
}

fn public_reexports(frontend: &FrontendUnit) -> Vec<ModulePath> {
    frontend
        .syntax
        .items
        .iter()
        .filter_map(|item| match item {
            TopLevelItem::Import(import) if matches!(import.visibility, Visibility::Public) => {
                ModulePath::parse(&import.module.to_source_string()).ok()
            }
            _ => None,
        })
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
