extern crate self as vox_core;

pub mod diagnostics;
pub mod external_export;
pub mod external_library;
pub mod host;
pub mod ids;
pub mod mir;
pub mod opt;
pub mod plan;
pub mod source;
pub mod types;
pub mod value;

pub use vox_core_macros::{VoxExport, vox_fn, vox_trait};
