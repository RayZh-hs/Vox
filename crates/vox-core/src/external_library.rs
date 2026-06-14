use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, io,
};

use crate::{
    host::{
        FieldSpec, FunctionExportKind, FunctionSpec, PackageManifest, ParameterSpec, Purity,
        TraitMethodSpec, TraitSpec, TypeSpec, ValueSpec,
    },
    source::ModulePath,
    types::{QualifiedTypeName, RecordField, VoxType},
};

pub const EXTERNAL_LIBRARY_MAGIC: [u8; 4] = *b"VXLH";
pub const EXTERNAL_LIBRARY_VERSION: u16 = 4;
pub const MINIMAL_WASM_MODULE: &[u8] = b"\0asm\x01\0\0\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalLibraryHeader {
    pub manifest: PackageManifest,
    pub wasm_bytes: Vec<u8>,
    pub metadata: Option<Vec<u8>>,
}

#[derive(Debug)]
pub enum ExternalLibraryFormatError {
    Io(io::Error),
    Message(String),
}

impl fmt::Display for ExternalLibraryFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Message(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ExternalLibraryFormatError {}

impl From<io::Error> for ExternalLibraryFormatError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

// .voxlib binary format (version 4):
// magic(4B) | version(u16) | reserved(u16) | manifest(len+bytes)
// | wasm(len+bytes) | metadata(len+bytes)?

pub fn encode_external_library_file(
    header: &ExternalLibraryHeader,
) -> Result<Vec<u8>, ExternalLibraryFormatError> {
    let mut writer = BinaryWriter::new();
    writer.write_fixed(&EXTERNAL_LIBRARY_MAGIC);
    writer.write_u16(EXTERNAL_LIBRARY_VERSION);
    writer.write_u16(0);
    writer.write_bytes(&encode_package_manifest(&header.manifest)?)?;
    writer.write_bytes(&header.wasm_bytes)?;
    if let Some(metadata) = &header.metadata {
        writer.write_bytes(metadata)?;
    }
    Ok(writer.into_inner())
}

pub fn decode_external_library_file(
    bytes: &[u8],
) -> Result<ExternalLibraryHeader, ExternalLibraryFormatError> {
    let mut reader = BinaryReader::new(bytes);
    let magic = reader.read_fixed::<4>()?;
    if magic != EXTERNAL_LIBRARY_MAGIC {
        return Err(ExternalLibraryFormatError::Message(
            "invalid external library magic".to_owned(),
        ));
    }

    let version = reader.read_u16()?;
    if version != EXTERNAL_LIBRARY_VERSION {
        return Err(ExternalLibraryFormatError::Message(format!(
            "unsupported external library version {version}"
        )));
    }

    let _reserved = reader.read_u16()?;
    let manifest = decode_package_manifest(&reader.read_bytes()?)?;
    let wasm_bytes = reader.read_bytes()?;
    let metadata = if reader.cursor < reader.bytes.len() {
        Some(reader.read_bytes()?)
    } else {
        None
    };
    Ok(ExternalLibraryHeader {
        manifest,
        wasm_bytes,
        metadata,
    })
}

pub fn encode_package_manifest(
    manifest: &PackageManifest,
) -> Result<Vec<u8>, ExternalLibraryFormatError> {
    let mut writer = BinaryWriter::new();
    writer.write_string(&manifest.package.as_str())?;

    writer.write_len(manifest.reexports.len(), "re-export list")?;
    for module in &manifest.reexports {
        writer.write_string(&module.as_str())?;
    }

    writer.write_len(manifest.types.len(), "type list")?;
    for ty in &manifest.types {
        encode_qualified_type_name(&mut writer, &ty.name)?;
        writer.write_len(ty.fields.len(), "field list")?;
        for field in &ty.fields {
            writer.write_string(&field.name)?;
            encode_vox_type(&mut writer, &field.ty)?;
        }
    }

    writer.write_len(manifest.traits.len(), "trait list")?;
    for trait_spec in &manifest.traits {
        encode_qualified_type_name(&mut writer, &trait_spec.name)?;
        writer.write_len(trait_spec.methods.len(), "trait method list")?;
        for method in &trait_spec.methods {
            writer.write_string(&method.name)?;
            writer.write_string(&method.lowered_by)?;
            writer.write_len(method.parameters.len(), "trait method parameter list")?;
            for parameter in &method.parameters {
                encode_parameter_spec(&mut writer, parameter)?;
            }
            encode_vox_type(&mut writer, &method.return_type)?;
            encode_purity(&mut writer, method.purity);
        }
    }

    writer.write_len(manifest.functions.len(), "function list")?;
    for function in &manifest.functions {
        writer.write_string(&function.name)?;
        writer.write_len(function.parameters.len(), "parameter list")?;
        for parameter in &function.parameters {
            encode_parameter_spec(&mut writer, parameter)?;
        }
        encode_vox_type(&mut writer, &function.return_type)?;
        encode_purity(&mut writer, function.purity);
        encode_function_export_kind(&mut writer, &function.export)?;
    }

    writer.write_len(manifest.values.len(), "value list")?;
    for value in &manifest.values {
        writer.write_string(&value.name)?;
        encode_vox_type(&mut writer, &value.ty)?;
        encode_purity(&mut writer, value.purity);
    }

    writer.write_len(manifest.trait_impls.len(), "trait impls")?;
    for (trait_name, impls) in &manifest.trait_impls {
        encode_qualified_type_name(&mut writer, trait_name)?;
        writer.write_len(impls.len(), "trait impl entry")?;
        for struct_name in impls {
            encode_qualified_type_name(&mut writer, struct_name)?;
        }
    }

    Ok(writer.into_inner())
}

pub fn decode_package_manifest(
    bytes: &[u8],
) -> Result<PackageManifest, ExternalLibraryFormatError> {
    let mut reader = BinaryReader::new(bytes);
    let package = ModulePath::parse(&reader.read_string()?)
        .map_err(|diagnostic| ExternalLibraryFormatError::Message(diagnostic.message))?;

    let reexport_count = reader.read_u32()? as usize;
    let mut reexports = Vec::with_capacity(reexport_count);
    for _ in 0..reexport_count {
        reexports.push(
            ModulePath::parse(&reader.read_string()?)
                .map_err(|diagnostic| ExternalLibraryFormatError::Message(diagnostic.message))?,
        );
    }

    let type_count = reader.read_u32()? as usize;
    let mut types = Vec::with_capacity(type_count);
    for _ in 0..type_count {
        let name = decode_qualified_type_name(&mut reader)?;
        let field_count = reader.read_u32()? as usize;
        let mut fields = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            fields.push(FieldSpec {
                name: reader.read_string()?,
                ty: decode_vox_type(&mut reader)?,
            });
        }
        types.push(TypeSpec { name, fields });
    }

    let trait_count = reader.read_u32()? as usize;
    let mut traits = Vec::with_capacity(trait_count);
    for _ in 0..trait_count {
        let name = decode_qualified_type_name(&mut reader)?;
        let method_count = reader.read_u32()? as usize;
        let mut methods = Vec::with_capacity(method_count);
        for _ in 0..method_count {
            let method_name = reader.read_string()?;
            let lowered_by = reader.read_string()?;
            let parameter_count = reader.read_u32()? as usize;
            let mut parameters = Vec::with_capacity(parameter_count);
            for _ in 0..parameter_count {
                parameters.push(decode_parameter_spec(&mut reader)?);
            }
            let return_type = decode_vox_type(&mut reader)?;
            let purity = decode_purity(&mut reader)?;
            methods.push(TraitMethodSpec {
                name: method_name,
                lowered_by,
                parameters,
                return_type,
                purity,
            });
        }
        traits.push(TraitSpec { name, methods });
    }

    let function_count = reader.read_u32()? as usize;
    let mut functions = Vec::with_capacity(function_count);
    for _ in 0..function_count {
        let name = reader.read_string()?;
        let parameter_count = reader.read_u32()? as usize;
        let mut parameters = Vec::with_capacity(parameter_count);
        for _ in 0..parameter_count {
            parameters.push(decode_parameter_spec(&mut reader)?);
        }
        let return_type = decode_vox_type(&mut reader)?;
        let purity = decode_purity(&mut reader)?;
        let export = decode_function_export_kind(&mut reader)?;
        functions.push(FunctionSpec {
            name,
            parameters,
            return_type,
            purity,
            export,
        });
    }

    let value_count = reader.read_u32()? as usize;
    let mut values = Vec::with_capacity(value_count);
    for _ in 0..value_count {
        values.push(ValueSpec {
            name: reader.read_string()?,
            ty: decode_vox_type(&mut reader)?,
            purity: decode_purity(&mut reader)?,
        });
    }

    let trait_impls_count = reader.read_u32()? as usize;
    let mut trait_impls = BTreeMap::new();
    for _ in 0..trait_impls_count {
        let trait_name = decode_qualified_type_name(&mut reader)?;
        let impl_count = reader.read_u32()? as usize;
        let mut impls = BTreeSet::new();
        for _ in 0..impl_count {
            impls.insert(decode_qualified_type_name(&mut reader)?);
        }
        trait_impls.insert(trait_name, impls);
    }

    reader.finish()?;
    Ok(PackageManifest {
        package,
        reexports,
        types,
        traits,
        functions,
        values,
        trait_impls,
    })
}

fn encode_parameter_spec(
    writer: &mut BinaryWriter,
    parameter: &ParameterSpec,
) -> Result<(), ExternalLibraryFormatError> {
    writer.write_string(&parameter.name)?;
    encode_vox_type(writer, &parameter.ty)?;
    writer.write_u8(u8::from(parameter.has_default));
    Ok(())
}

fn decode_parameter_spec(
    reader: &mut BinaryReader<'_>,
) -> Result<ParameterSpec, ExternalLibraryFormatError> {
    Ok(ParameterSpec {
        name: reader.read_string()?,
        ty: decode_vox_type(reader)?,
        has_default: match reader.read_u8()? {
            0 => false,
            1 => true,
            _ => {
                return Err(ExternalLibraryFormatError::Message(
                    "invalid default-value flag".to_owned(),
                ));
            }
        },
    })
}

fn encode_purity(writer: &mut BinaryWriter, purity: Purity) {
    writer.write_u8(match purity {
        Purity::Pure => 0,
        Purity::Evil => 1,
    });
}

fn decode_purity(reader: &mut BinaryReader<'_>) -> Result<Purity, ExternalLibraryFormatError> {
    match reader.read_u8()? {
        0 => Ok(Purity::Pure),
        1 => Ok(Purity::Evil),
        _ => Err(ExternalLibraryFormatError::Message(
            "invalid purity tag".to_owned(),
        )),
    }
}

fn encode_function_export_kind(
    writer: &mut BinaryWriter,
    export: &FunctionExportKind,
) -> Result<(), ExternalLibraryFormatError> {
    match export {
        FunctionExportKind::Function => writer.write_u8(0),
        FunctionExportKind::LoweredTraitMethod {
            trait_name,
            method_name,
        } => {
            writer.write_u8(1);
            encode_qualified_type_name(writer, trait_name)?;
            writer.write_string(method_name)?;
        }
    }
    Ok(())
}

fn decode_function_export_kind(
    reader: &mut BinaryReader<'_>,
) -> Result<FunctionExportKind, ExternalLibraryFormatError> {
    match reader.read_u8()? {
        0 => Ok(FunctionExportKind::Function),
        1 => Ok(FunctionExportKind::LoweredTraitMethod {
            trait_name: decode_qualified_type_name(reader)?,
            method_name: reader.read_string()?,
        }),
        _ => Err(ExternalLibraryFormatError::Message(
            "invalid function export tag".to_owned(),
        )),
    }
}

fn encode_vox_type(
    writer: &mut BinaryWriter,
    ty: &VoxType,
) -> Result<(), ExternalLibraryFormatError> {
    match ty {
        VoxType::Int => writer.write_u8(0x00),
        VoxType::Float => writer.write_u8(0x01),
        VoxType::Bool => writer.write_u8(0x02),
        VoxType::String => writer.write_u8(0x03),
        VoxType::List(item) => {
            writer.write_u8(0x04);
            encode_vox_type(writer, item)?;
        }
        VoxType::Tuple(items) => {
            writer.write_u8(0x05);
            writer.write_len(items.len(), "tuple type")?;
            for item in items {
                encode_vox_type(writer, item)?;
            }
        }
        VoxType::Record(fields) => {
            writer.write_u8(0x0b);
            writer.write_len(fields.len(), "record type")?;
            for field in fields {
                writer.write_string(&field.name)?;
                encode_vox_type(writer, &field.ty)?;
            }
        }
        VoxType::Nullable(inner) => {
            writer.write_u8(0x06);
            encode_vox_type(writer, inner)?;
        }
        VoxType::DynTrait(name) => {
            writer.write_u8(0x07);
            encode_qualified_type_name(writer, name)?;
        }
        VoxType::Named(name) => {
            writer.write_u8(0x08);
            encode_qualified_type_name(writer, name)?;
        }
        VoxType::TypeParameter(name) => {
            writer.write_u8(0x09);
            writer.write_string(name)?;
        }
        VoxType::OpaqueSurface(raw) => {
            writer.write_u8(0x0a);
            writer.write_string(raw)?;
        }
    }
    Ok(())
}

fn decode_vox_type(reader: &mut BinaryReader<'_>) -> Result<VoxType, ExternalLibraryFormatError> {
    match reader.read_u8()? {
        0x00 => Ok(VoxType::Int),
        0x01 => Ok(VoxType::Float),
        0x02 => Ok(VoxType::Bool),
        0x03 => Ok(VoxType::String),
        0x04 => Ok(VoxType::List(Box::new(decode_vox_type(reader)?))),
        0x05 => {
            let count = reader.read_u32()? as usize;
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(decode_vox_type(reader)?);
            }
            Ok(VoxType::Tuple(items))
        }
        0x0b => {
            let count = reader.read_u32()? as usize;
            let mut fields = Vec::with_capacity(count);
            for _ in 0..count {
                fields.push(RecordField {
                    name: reader.read_string()?,
                    ty: decode_vox_type(reader)?,
                });
            }
            Ok(VoxType::Record(fields))
        }
        0x06 => Ok(VoxType::Nullable(Box::new(decode_vox_type(reader)?))),
        0x07 => Ok(VoxType::DynTrait(decode_qualified_type_name(reader)?)),
        0x08 => Ok(VoxType::Named(decode_qualified_type_name(reader)?)),
        0x09 => Ok(VoxType::TypeParameter(reader.read_string()?)),
        0x0a => Ok(VoxType::OpaqueSurface(reader.read_string()?)),
        _ => Err(ExternalLibraryFormatError::Message(
            "unknown type tag".to_owned(),
        )),
    }
}

fn encode_qualified_type_name(
    writer: &mut BinaryWriter,
    name: &QualifiedTypeName,
) -> Result<(), ExternalLibraryFormatError> {
    writer.write_string(&name.module.as_str())?;
    writer.write_string(&name.name)?;
    Ok(())
}

fn decode_qualified_type_name(
    reader: &mut BinaryReader<'_>,
) -> Result<QualifiedTypeName, ExternalLibraryFormatError> {
    let module = ModulePath::parse(&reader.read_string()?)
        .map_err(|diagnostic| ExternalLibraryFormatError::Message(diagnostic.message))?;
    let name = reader.read_string()?;
    Ok(QualifiedTypeName { module, name })
}

struct BinaryWriter {
    bytes: Vec<u8>,
}

impl BinaryWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }

    fn write_fixed(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn write_u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn write_u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn write_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), ExternalLibraryFormatError> {
        self.write_len(bytes.len(), "byte string")?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn write_string(&mut self, value: &str) -> Result<(), ExternalLibraryFormatError> {
        self.write_bytes(value.as_bytes())
    }

    fn write_len(&mut self, len: usize, subject: &str) -> Result<(), ExternalLibraryFormatError> {
        let len = u32::try_from(len).map_err(|_| {
            ExternalLibraryFormatError::Message(format!("{subject} exceeds 32-bit size limit"))
        })?;
        self.write_u32(len);
        Ok(())
    }
}

struct BinaryReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> BinaryReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn finish(&self) -> Result<(), ExternalLibraryFormatError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(ExternalLibraryFormatError::Message(
                "unexpected trailing bytes".to_owned(),
            ))
        }
    }

    fn read_fixed<const N: usize>(&mut self) -> Result<[u8; N], ExternalLibraryFormatError> {
        let bytes = self.read_exact(N)?;
        let mut out = [0; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    fn read_u8(&mut self) -> Result<u8, ExternalLibraryFormatError> {
        Ok(self.read_exact(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16, ExternalLibraryFormatError> {
        let bytes = self.read_exact(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, ExternalLibraryFormatError> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_bytes(&mut self) -> Result<Vec<u8>, ExternalLibraryFormatError> {
        let len = self.read_u32()? as usize;
        Ok(self.read_exact(len)?.to_vec())
    }

    fn read_string(&mut self) -> Result<String, ExternalLibraryFormatError> {
        String::from_utf8(self.read_bytes()?).map_err(|_| {
            ExternalLibraryFormatError::Message("string payload is not valid UTF-8".to_owned())
        })
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], ExternalLibraryFormatError> {
        let end = self.cursor.saturating_add(len);
        let Some(bytes) = self.bytes.get(self.cursor..end) else {
            return Err(ExternalLibraryFormatError::Message(
                "unexpected end of bytes".to_owned(),
            ));
        };
        self.cursor = end;
        Ok(bytes)
    }
}
