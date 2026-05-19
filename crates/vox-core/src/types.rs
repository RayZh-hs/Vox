use crate::source::ModulePath;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoxType {
    Int,
    Float,
    Bool,
    String,
    List(Box<VoxType>),
    Tuple(Vec<VoxType>),
    Nullable(Box<VoxType>),
    DynTrait(QualifiedTypeName),
    Named(QualifiedTypeName),
    TypeParameter(String),
    OpaqueSurface(String),
}

impl VoxType {
    pub fn opaque_surface(raw: impl Into<String>) -> Self {
        Self::OpaqueSurface(raw.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedTypeName {
    pub module: ModulePath,
    pub name: String,
}
