use crate::{
    diagnostics::DiagnosticBag,
    host::{ParameterSpec, Purity},
    ids::ArtifactId,
    mir::MirModule,
    opt::{OptimizationLevel, OptimizationRank, OptimizationRanking},
    source::{ModuleKind, ModulePath},
    types::VoxType,
    value::InlineValue,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyFingerprint {
    pub subject: String,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanStep {
    BindInlineValue {
        slot: u32,
        value: InlineValue,
    },
    InvokeHost {
        slot: u32,
        callee: String,
        arguments: Vec<u32>,
        purity: Purity,
    },
    ReturnSlot(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutablePlan {
    pub steps: Vec<PlanStep>,
    pub result_type: Option<VoxType>,
    pub optimization_rank: OptimizationRank,
    pub mir_text: Option<String>,
    pub optimization_summary: Vec<String>,
    pub wasm: Option<WasmArtifact>,
}

impl ExecutablePlan {
    pub fn deferred(optimization_rank: OptimizationRank) -> Self {
        Self {
            steps: Vec::new(),
            result_type: None,
            optimization_rank,
            mir_text: None,
            optimization_summary: Vec::new(),
            wasm: None,
        }
    }

    pub fn with_mir(mut self, mir: &MirModule, optimization_summary: Vec<String>) -> Self {
        self.mir_text = Some(mir.to_text());
        self.optimization_summary = optimization_summary;
        self
    }

    pub fn with_wasm(mut self, wasm: Option<WasmArtifact>) -> Self {
        self.wasm = wasm;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmArtifact {
    pub bytes: Vec<u8>,
    pub entry_export: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledArtifact {
    pub id: ArtifactId,
    pub module: ModulePath,
    pub kind: ModuleKind,
    pub optimization: OptimizationLevel,
    pub optimization_rankings: Vec<OptimizationRanking>,
    pub parameters: Vec<ParameterSpec>,
    pub result_type: Option<VoxType>,
    pub purity: Purity,
    pub plan: ExecutablePlan,
    pub mir: Option<MirModule>,
    pub diagnostics: DiagnosticBag,
    pub dependencies: Vec<DependencyFingerprint>,
    pub source_revision: u64,
}
