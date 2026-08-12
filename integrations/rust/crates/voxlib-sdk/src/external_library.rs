use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use vox_core::{
    diagnostics::Diagnostic,
    external_library::{
        ExternalLibraryFormatError, ExternalLibraryHeader, VOXLIB_HEAP_TOP_EXPORT_NAME,
        VOXLIB_HOST_IMPORT_NAME, VOXLIB_MEMORY_EXPORT_NAME, VOXLIB_MEMORY_IMPORT_MODULE,
        VOXLIB_MEMORY_IMPORT_NAME, VOXLIB_OPERATION_IMPORT_NAME, encode_external_library_file,
        voxlib_function_export, voxlib_value_export,
    },
    host::PackageManifest,
    source::ModulePath,
};

use crate::external_export::{
    collect_registered_docstrings, extend_manifest_with_registered_exports,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalLibrary {
    manifest: PackageManifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedExternalLibrary {
    header: ExternalLibraryHeader,
    library_bytes: Vec<u8>,
    library_file_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedExternalLibraryFiles {
    pub library_path: PathBuf,
}

impl ExternalLibrary {
    pub fn new(package: &str) -> Result<Self, Diagnostic> {
        Ok(Self {
            manifest: PackageManifest {
                package: ModulePath::parse(package)?,
                reexports: Vec::new(),
                types: Vec::new(),
                traits: Vec::new(),
                functions: Vec::new(),
                values: Vec::new(),
                trait_impls: BTreeMap::new(),
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

    pub fn build(self) -> Result<(PackageManifest, Vec<u8>), String> {
        let manifest = extend_manifest_with_registered_exports(self.manifest)?;
        let docstrings = collect_registered_docstrings();
        let metadata = encode_docstring_metadata(&docstrings);
        Ok((manifest, metadata))
    }

    pub fn generate(
        self,
        wasm_bytes: impl Into<Vec<u8>>,
    ) -> Result<GeneratedExternalLibrary, ExternalLibraryFormatError> {
        let (manifest, metadata) = self.build().map_err(ExternalLibraryFormatError::Message)?;
        let wasm_bytes = wasm_bytes.into();
        validate_package_wasm(&manifest, &wasm_bytes)?;
        let header = ExternalLibraryHeader {
            manifest,
            wasm_bytes,
            metadata: if metadata.is_empty() {
                None
            } else {
                Some(metadata)
            },
        };
        let library_bytes = encode_external_library_file(&header)?;
        let library_file_name = format!("{}.voxlib", header.manifest.package.as_str());
        Ok(GeneratedExternalLibrary {
            header,
            library_bytes,
            library_file_name,
        })
    }
}

impl GeneratedExternalLibrary {
    pub fn header(&self) -> &ExternalLibraryHeader {
        &self.header
    }

    pub fn library_bytes(&self) -> &[u8] {
        &self.library_bytes
    }

    pub fn library_file_name(&self) -> &str {
        &self.library_file_name
    }

    pub fn wasm_bytes(&self) -> &[u8] {
        &self.header.wasm_bytes
    }

    pub fn write_to_dir(
        &self,
        dir: impl AsRef<Path>,
    ) -> Result<GeneratedExternalLibraryFiles, ExternalLibraryFormatError> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir)?;

        let library_path = dir.join(self.library_file_name());
        fs::write(&library_path, &self.library_bytes)?;

        Ok(GeneratedExternalLibraryFiles { library_path })
    }
}

fn validate_package_wasm(
    manifest: &PackageManifest,
    wasm_bytes: &[u8],
) -> Result<(), ExternalLibraryFormatError> {
    wasmparser::Validator::new()
        .validate_all(wasm_bytes)
        .map_err(|error| {
            ExternalLibraryFormatError::Message(format!("invalid package wasm: {error}"))
        })?;
    if manifest.functions.is_empty() && manifest.values.is_empty() {
        return Ok(());
    }

    let mut imports = Vec::new();
    let mut function_exports = BTreeSet::new();
    let mut memory_exported = false;
    let mut heap_top_exported = false;
    for payload in wasmparser::Parser::new(0).parse_all(wasm_bytes) {
        match payload.map_err(|error| ExternalLibraryFormatError::Message(error.to_string()))? {
            wasmparser::Payload::ImportSection(section) => {
                for import in section {
                    let import = import
                        .map_err(|error| ExternalLibraryFormatError::Message(error.to_string()))?;
                    imports.push((import.module.to_owned(), import.name.to_owned()));
                }
            }
            wasmparser::Payload::ExportSection(section) => {
                for export in section {
                    let export = export
                        .map_err(|error| ExternalLibraryFormatError::Message(error.to_string()))?;
                    match export.kind {
                        wasmparser::ExternalKind::Func => {
                            function_exports.insert(export.name.to_owned());
                        }
                        wasmparser::ExternalKind::Memory
                            if export.name == VOXLIB_MEMORY_EXPORT_NAME =>
                        {
                            memory_exported = true;
                        }
                        wasmparser::ExternalKind::Global
                            if export.name == VOXLIB_HEAP_TOP_EXPORT_NAME =>
                        {
                            heap_top_exported = true;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    let required_imports = [
        (VOXLIB_MEMORY_IMPORT_MODULE, VOXLIB_MEMORY_IMPORT_NAME),
        (VOXLIB_MEMORY_IMPORT_MODULE, VOXLIB_OPERATION_IMPORT_NAME),
        (VOXLIB_MEMORY_IMPORT_MODULE, VOXLIB_HOST_IMPORT_NAME),
    ];
    if imports.len() != required_imports.len()
        || imports
            .iter()
            .zip(required_imports)
            .any(|(actual, expected)| actual.0 != expected.0 || actual.1 != expected.1)
    {
        return Err(ExternalLibraryFormatError::Message(
            "package wasm must import `vox.memory`, `vox.__vox_op`, and `vox.__vox_host` in that order"
                .to_owned(),
        ));
    }
    if !memory_exported || !heap_top_exported {
        return Err(ExternalLibraryFormatError::Message(format!(
            "package wasm must export `{VOXLIB_MEMORY_EXPORT_NAME}` memory and `{VOXLIB_HEAP_TOP_EXPORT_NAME}` global"
        )));
    }
    for function in &manifest.functions {
        let export = voxlib_function_export(&function.name);
        if !function_exports.contains(&export) {
            return Err(ExternalLibraryFormatError::Message(format!(
                "package wasm is missing function export `{export}`"
            )));
        }
    }
    for value in &manifest.values {
        let export = voxlib_value_export(&value.name);
        if !function_exports.contains(&export) {
            return Err(ExternalLibraryFormatError::Message(format!(
                "package wasm is missing value export `{export}`"
            )));
        }
    }
    Ok(())
}

fn encode_docstring_metadata(docstrings: &BTreeMap<String, String>) -> Vec<u8> {
    if docstrings.is_empty() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    write_len(&mut bytes, docstrings.len());
    for (name, doc) in docstrings {
        write_string(&mut bytes, name);
        write_string(&mut bytes, doc);
    }
    bytes
}

fn write_string(bytes: &mut Vec<u8>, value: &str) {
    write_len(bytes, value.len());
    bytes.extend_from_slice(value.as_bytes());
}

fn write_len(bytes: &mut Vec<u8>, len: usize) {
    let len = u32::try_from(len).expect("docstring metadata exceeds 32-bit size limit");
    bytes.extend_from_slice(&len.to_le_bytes());
}
