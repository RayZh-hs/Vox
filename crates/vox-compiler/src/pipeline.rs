use std::sync::atomic::{AtomicU64, Ordering};

use vox_core::{
    diagnostics::DiagnosticBag,
    host::{HostRegistry, Purity},
    ids::ArtifactId,
    opt::{OptimizationLevel, OptimizationSubject},
    plan::{CompiledArtifact, DependencyFingerprint, ExecutablePlan},
    source::SourceText,
};

use crate::front_end::{FrontEndUnit, analyze_source};
use crate::mir::{MirPassFn, lower_and_optimize_mir};
use crate::optimization::derive_rankings;
use crate::treewalk::TreewalkScript;

#[derive(Debug, Clone)]
pub struct CompileRequest {
    pub source: SourceText,
    pub optimization: OptimizationLevel,
    pub host: HostRegistry,
}

#[derive(Debug, Clone)]
pub struct CompileResult {
    pub artifact: Option<CompiledArtifact>,
    pub front_end: Option<FrontEndUnit>,
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
            Ok(front_end) => {
                let treewalk = TreewalkScript::lower(&front_end).ok();
                let optimization_rankings = derive_rankings(&front_end, request.optimization);
                let module_rank = optimization_rankings
                    .iter()
                    .find_map(|ranking| match &ranking.subject {
                        OptimizationSubject::Module => Some(ranking.rank),
                        OptimizationSubject::Function(_) => None,
                    })
                    .expect("module ranking should always be present");
                let (mir, optimization_summary) = lower_and_optimize_mir(
                    &front_end,
                    request.optimization,
                    &optimization_rankings,
                    custom_mir_passes,
                );
                let artifact = CompiledArtifact {
                    id: ArtifactId(self.next_artifact_id.fetch_add(1, Ordering::Relaxed) + 1),
                    module: front_end.header.module.clone(),
                    kind: front_end.header.kind,
                    optimization: request.optimization,
                    optimization_rankings,
                    parameters: front_end
                        .parameters
                        .iter()
                        .cloned()
                        .map(|parameter| parameter.into_spec())
                        .collect(),
                    result_type: None,
                    purity: if matches!(
                        front_end.header.kind,
                        vox_core::source::ModuleKind::Script { evil: true }
                    ) {
                        Purity::Evil
                    } else {
                        Purity::Pure
                    },
                    plan: ExecutablePlan::deferred(module_rank)
                        .with_mir(&mir, optimization_summary),
                    mir: Some(mir),
                    diagnostics: DiagnosticBag::default(),
                    dependencies: collect_dependencies(&request),
                    source_revision: request.source.origin.revision,
                };

                CompileResult {
                    artifact: Some(artifact),
                    front_end: Some(front_end),
                    treewalk,
                    diagnostics: DiagnosticBag::default(),
                }
            }
            Err(diagnostics) => CompileResult {
                artifact: None,
                front_end: None,
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
