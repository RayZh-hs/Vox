use crate::{
    diagnostics::DiagnosticBag,
    host::{ParameterSpec, Purity},
    ids::ArtifactId,
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
}

impl ExecutablePlan {
    pub fn deferred(optimization_rank: OptimizationRank) -> Self {
        Self {
            steps: Vec::new(),
            result_type: None,
            optimization_rank,
        }
    }
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
    pub diagnostics: DiagnosticBag,
    pub dependencies: Vec<DependencyFingerprint>,
    pub source_revision: u64,
}
