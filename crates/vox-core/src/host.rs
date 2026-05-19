use std::collections::BTreeMap;

use crate::{
    source::ModulePath,
    types::{QualifiedTypeName, VoxType},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purity {
    Pure,
    Evil,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterSpec {
    pub name: String,
    pub ty: VoxType,
    pub has_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSpec {
    pub name: String,
    pub parameters: Vec<ParameterSpec>,
    pub return_type: VoxType,
    pub purity: Purity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeSpec {
    pub name: QualifiedTypeName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageManifest {
    pub package: ModulePath,
    pub types: Vec<TypeSpec>,
    pub functions: Vec<FunctionSpec>,
}

#[derive(Debug, Clone, Default)]
pub struct HostRegistry {
    packages: BTreeMap<ModulePath, PackageManifest>,
}

impl HostRegistry {
    pub fn register_package(&mut self, manifest: PackageManifest) -> Option<PackageManifest> {
        self.packages.insert(manifest.package.clone(), manifest)
    }

    pub fn package(&self, module: &ModulePath) -> Option<&PackageManifest> {
        self.packages.get(module)
    }

    pub fn packages(&self) -> impl Iterator<Item = &PackageManifest> {
        self.packages.values()
    }
}
