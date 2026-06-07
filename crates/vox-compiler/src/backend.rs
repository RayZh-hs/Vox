use vox_core::{mir::MirModule, plan::WasmArtifact};

use crate::wasm_backend::{WasmBackend, WasmLowering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackendOutput {
    pub wasm: Option<WasmArtifact>,
    pub summaries: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct BackendPipeline {
    wasm: WasmBackend,
}

impl BackendPipeline {
    pub(crate) fn lower(&self, mir: &MirModule) -> BackendOutput {
        match self.wasm.lower(mir) {
            WasmLowering::Lowered(artifact) => BackendOutput {
                summaries: vec![format!(
                    "backend wasm: emitted {} byte module exporting `{}`",
                    artifact.bytes.len(),
                    artifact.entry_export
                )],
                wasm: Some(artifact),
            },
            WasmLowering::Unsupported(reason) => BackendOutput {
                wasm: None,
                summaries: vec![format!("backend wasm: unsupported MIR shape: {reason}")],
            },
        }
    }
}
