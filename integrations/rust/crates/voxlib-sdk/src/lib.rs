extern crate self as voxlib_sdk;

pub mod embedded_library {
    pub use crate::external_library::*;
}
pub mod external_export;
pub mod external_library;

pub use voxlib_macros::{vox_fn, vox_trait, vox_trait_impl, VoxExport};
