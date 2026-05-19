use crate::{
    diagnostics::DiagnosticBag,
    host::{ParameterSpec, Purity},
    ids::ArtifactId,
    opt::OptimizationLevel,
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
}

impl ExecutablePlan {
    pub fn deferred() -> Self {
        Self {
            steps: Vec::new(),
            result_type: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledArtifact {
    pub id: ArtifactId,
    pub module: ModulePath,
    pub kind: ModuleKind,
    pub optimization: OptimizationLevel,
    pub parameters: Vec<ParameterSpec>,
    pub result_type: Option<VoxType>,
    pub purity: Purity,
    pub plan: ExecutablePlan,
    pub diagnostics: DiagnosticBag,
    pub dependencies: Vec<DependencyFingerprint>,
    pub source_revision: u64,
}
