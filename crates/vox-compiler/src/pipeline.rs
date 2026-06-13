use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

use vox_core::{
    diagnostics::DiagnosticBag,
    external_library::{ExternalLibraryHeader, MINIMAL_WASM_MODULE, encode_external_library_file},
    host::{HostRegistry, PackageManifest, Purity},
    ids::ArtifactId,
    opt::{OptimizationLevel, OptimizationSubject},
    plan::{CompiledArtifact, DependencyFingerprint, ExecutablePlan},
    source::SourceText,
};

use crate::backend::BackendPipeline;
use crate::frontend::{FrontendUnit, analyze_source};
use crate::frontend::ast::TopLevelItem;
use crate::imports::{resolve_imports, ImportResolution};
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

    let manifest = PackageManifest {
        package: artifact.module.clone(),
        types: Vec::new(),
        traits: Vec::new(),
        functions: Vec::new(),
        trait_impls: BTreeMap::new(),
    };
    let header = ExternalLibraryHeader {
        manifest,
        wasm_bytes,
    };
    encode_external_library_file(&header).map_err(|error| error.to_string())
}
