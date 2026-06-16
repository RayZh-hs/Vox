use crate::source::ModulePath;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoxType {
    Int,
    UInt,
    Float,
    Bool,
    String,
    List(Box<VoxType>),
    Tuple(Vec<VoxType>),
    Record(Vec<RecordField>),
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
pub struct RecordField {
    pub name: String,
    pub ty: VoxType,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct QualifiedTypeName {
    pub module: ModulePath,
    pub name: String,
}
