use crate::{
    diagnostics::Diagnostic,
    host::PackageManifest,
    source::ModulePath,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalLibrary {
    manifest: PackageManifest,
}

impl ExternalLibrary {
    pub fn new(package: &str) -> Result<Self, Diagnostic> {
        Ok(Self {
            manifest: PackageManifest {
                package: ModulePath::parse(package)?,
                types: Vec::new(),
                functions: Vec::new(),
            },
        })
    }

    pub fn package(&self) -> &ModulePath {
        &self.manifest.package
    }

    pub fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }

    pub fn manifest_mut(&mut self) -> &mut PackageManifest {
        &mut self.manifest
    }

    pub fn build(self) -> PackageManifest {
        self.manifest
    }
}
