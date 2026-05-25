use std::io::{self, Read, Write};

use thiserror::Error;
use vox_core::{
    diagnostics::{Diagnostic, DiagnosticBag, Severity},
    host::{FunctionSpec, PackageManifest, ParameterSpec, Purity, TypeSpec},
    opt::OptimizationLevel,
    source::ModulePath,
    types::{QualifiedTypeName, VoxType},
    value::InlineValue,
};

pub const MAGIC: u32 = 0x5658_5254;
pub const CURRENT_PROTOCOL_VERSION: u16 = 2;
pub const MAX_PAYLOAD_BYTES: u32 = 16 * 1024 * 1024;
pub const DEFAULT_INLINE_VALUE_BYTES: u32 = 1024 * 1024;

pub const FLAG_DIAGNOSTICS: u32 = 0x0000_0001;
pub const FLAG_INLINE_VALUE: u32 = 0x0000_0002;
pub const FLAG_HANDLE_RESULT: u32 = 0x0000_0004;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    Request = 0,
    Success = 1,
    Error = 2,
    Event = 3,
}

impl FrameKind {
    pub fn from_u8(raw: u8) -> Option<Self> {
        Some(match raw {
            0 => Self::Request,
            1 => Self::Success,
            2 => Self::Error,
            3 => Self::Event,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    Hello = 0x01,
    Ping = 0x02,
    OpenSession = 0x03,
    EvaluateSession = 0x04,
    DropSessionItem = 0x05,
    ResetSession = 0x06,
    SnapshotSession = 0x07,
    RestoreSession = 0x08,
    RunSessionScript = 0x09,
    SetSessionXOpt = 0x0a,
    CloseSession = 0x0b,
    ListSessions = 0x0c,
    SetSessionReserved = 0x0d,
    MountLibrary = 0x10,
    UnmountLibrary = 0x11,
    LoadScript = 0x20,
    ReloadScript = 0x21,
    UnloadScript = 0x22,
    SetXOpt = 0x23,
    RunScript = 0x24,
    RetainHandle = 0x30,
    DescribeHandle = 0x31,
    ReleaseHandle = 0x32,
    RefreshEcon = 0x40,
    CacheStats = 0x41,
    ClearCache = 0x42,
    Shutdown = 0x7f,
}

impl Opcode {
    pub fn from_u8(raw: u8) -> Option<Self> {
        Some(match raw {
            0x01 => Self::Hello,
            0x02 => Self::Ping,
            0x03 => Self::OpenSession,
            0x04 => Self::EvaluateSession,
            0x05 => Self::DropSessionItem,
            0x06 => Self::ResetSession,
            0x07 => Self::SnapshotSession,
            0x08 => Self::RestoreSession,
            0x09 => Self::RunSessionScript,
            0x0a => Self::SetSessionXOpt,
            0x0b => Self::CloseSession,
            0x0c => Self::ListSessions,
            0x0d => Self::SetSessionReserved,
            0x10 => Self::MountLibrary,
            0x11 => Self::UnmountLibrary,
            0x20 => Self::LoadScript,
            0x21 => Self::ReloadScript,
            0x22 => Self::UnloadScript,
            0x23 => Self::SetXOpt,
            0x24 => Self::RunScript,
            0x30 => Self::RetainHandle,
            0x31 => Self::DescribeHandle,
            0x32 => Self::ReleaseHandle,
            0x40 => Self::RefreshEcon,
            0x41 => Self::CacheStats,
            0x42 => Self::ClearCache,
            0x7f => Self::Shutdown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    VersionMismatch = 1,
    BadFrame = 2,
    UnsupportedOpcode = 3,
    UnknownLibrary = 4,
    UnknownScript = 5,
    UnknownHandle = 6,
    CompileFailed = 7,
    RuntimeFailed = 8,
    BadArgument = 9,
    PermissionDenied = 10,
}

impl ErrorCode {
    pub fn from_u32(raw: u32) -> Option<Self> {
        Some(match raw {
            1 => Self::VersionMismatch,
            2 => Self::BadFrame,
            3 => Self::UnsupportedOpcode,
            4 => Self::UnknownLibrary,
            5 => Self::UnknownScript,
            6 => Self::UnknownHandle,
            7 => Self::CompileFailed,
            8 => Self::RuntimeFailed,
            9 => Self::BadArgument,
            10 => Self::PermissionDenied,
            _ => return None,
        })
    }

    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    pub version: u16,
    pub kind: FrameKind,
    pub opcode: u8,
    pub flags: u32,
    pub request_id: u32,
    pub target_id: u32,
    pub payload_len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub header: FrameHeader,
    pub payload: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("{0}")]
    Message(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorFrame {
    pub code: ErrorCode,
    pub message: String,
    pub diagnostics: Option<DiagnosticBag>,
}

impl ProtocolError {
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

pub struct PayloadWriter {
    bytes: Vec<u8>,
}

impl PayloadWriter {
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.bytes
    }

    pub fn write_u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn write_u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_f64(&mut self, value: f64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_bytes(&mut self, value: &[u8]) -> Result<(), ProtocolError> {
        let len = u32::try_from(value.len())
            .map_err(|_| ProtocolError::message("payload item exceeds protocol size limit"))?;
        self.write_u32(len);
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    pub fn write_string(&mut self, value: &str) -> Result<(), ProtocolError> {
        self.write_bytes(value.as_bytes())
    }
}

pub struct PayloadReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> PayloadReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    pub fn finish(&self) -> Result<(), ProtocolError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(ProtocolError::message("unexpected trailing payload bytes"))
        }
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.cursor)
    }

    pub fn read_u8(&mut self) -> Result<u8, ProtocolError> {
        let Some(&value) = self.bytes.get(self.cursor) else {
            return Err(ProtocolError::message("unexpected end of payload"));
        };
        self.cursor += 1;
        Ok(value)
    }

    pub fn read_u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    pub fn read_u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    pub fn read_u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    pub fn read_i64(&mut self) -> Result<i64, ProtocolError> {
        Ok(i64::from_le_bytes(self.read_array()?))
    }

    pub fn read_f64(&mut self) -> Result<f64, ProtocolError> {
        Ok(f64::from_le_bytes(self.read_array()?))
    }

    pub fn read_bytes(&mut self) -> Result<Vec<u8>, ProtocolError> {
        let len = self.read_u32()? as usize;
        if self.cursor + len > self.bytes.len() {
            return Err(ProtocolError::message("unexpected end of payload"));
        }
        let value = self.bytes[self.cursor..self.cursor + len].to_vec();
        self.cursor += len;
        Ok(value)
    }

    pub fn read_string(&mut self) -> Result<String, ProtocolError> {
        String::from_utf8(self.read_bytes()?)
            .map_err(|_| ProtocolError::message("string payload is not valid UTF-8"))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], ProtocolError> {
        if self.cursor + N > self.bytes.len() {
            return Err(ProtocolError::message("unexpected end of payload"));
        }

        let mut array = [0; N];
        array.copy_from_slice(&self.bytes[self.cursor..self.cursor + N]);
        self.cursor += N;
        Ok(array)
    }
}

pub fn read_frame(reader: &mut impl Read) -> Result<Option<Frame>, ProtocolError> {
    let mut header = [0_u8; 24];
    let mut read = 0;
    while read < header.len() {
        match reader.read(&mut header[read..])? {
            0 if read == 0 => return Ok(None),
            0 => return Err(ProtocolError::message("unexpected end of frame header")),
            count => read += count,
        }
    }

    let magic = u32::from_le_bytes(header[0..4].try_into().expect("slice has fixed width"));
    if magic != MAGIC {
        return Err(ProtocolError::message("invalid frame magic"));
    }

    let kind = FrameKind::from_u8(header[6])
        .ok_or_else(|| ProtocolError::message("invalid frame kind"))?;
    let payload_len = u32::from_le_bytes(header[20..24].try_into().expect("slice has fixed width"));
    if payload_len > MAX_PAYLOAD_BYTES {
        return Err(ProtocolError::message(
            "payload exceeds protocol size limit",
        ));
    }

    let mut payload = vec![0_u8; payload_len as usize];
    reader.read_exact(&mut payload)?;

    Ok(Some(Frame {
        header: FrameHeader {
            version: u16::from_le_bytes(header[4..6].try_into().expect("slice has fixed width")),
            kind,
            opcode: header[7],
            flags: u32::from_le_bytes(header[8..12].try_into().expect("slice has fixed width")),
            request_id: u32::from_le_bytes(
                header[12..16].try_into().expect("slice has fixed width"),
            ),
            target_id: u32::from_le_bytes(
                header[16..20].try_into().expect("slice has fixed width"),
            ),
            payload_len,
        },
        payload,
    }))
}

pub fn write_frame(writer: &mut impl Write, frame: &Frame) -> Result<(), ProtocolError> {
    let mut header = [0_u8; 24];
    header[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    header[4..6].copy_from_slice(&frame.header.version.to_le_bytes());
    header[6] = frame.header.kind as u8;
    header[7] = frame.header.opcode;
    header[8..12].copy_from_slice(&frame.header.flags.to_le_bytes());
    header[12..16].copy_from_slice(&frame.header.request_id.to_le_bytes());
    header[16..20].copy_from_slice(&frame.header.target_id.to_le_bytes());
    header[20..24].copy_from_slice(&frame.header.payload_len.to_le_bytes());

    writer.write_all(&header)?;
    writer.write_all(&frame.payload)?;
    writer.flush()?;
    Ok(())
}

pub fn success_frame(
    version: u16,
    opcode: Opcode,
    request_id: u32,
    target_id: u32,
    flags: u32,
    payload: Vec<u8>,
) -> Result<Frame, ProtocolError> {
    let payload_len = u32::try_from(payload.len())
        .map_err(|_| ProtocolError::message("payload exceeds protocol size limit"))?;
    Ok(Frame {
        header: FrameHeader {
            version,
            kind: FrameKind::Success,
            opcode: opcode as u8,
            flags,
            request_id,
            target_id,
            payload_len,
        },
        payload,
    })
}

pub fn error_frame(
    version: u16,
    opcode: u8,
    request_id: u32,
    code: ErrorCode,
    message: impl Into<String>,
    diagnostics: Option<DiagnosticBag>,
) -> Result<Frame, ProtocolError> {
    let message = message.into();
    let mut payload = PayloadWriter::new();
    payload.write_u32(code.as_u32());
    payload.write_string(&message)?;
    if let Some(diagnostics) = diagnostics.as_ref() {
        encode_diagnostics(&mut payload, diagnostics)?;
    }
    success_or_error_frame(
        version,
        FrameKind::Error,
        opcode,
        request_id,
        0,
        if diagnostics.is_some() {
            FLAG_DIAGNOSTICS
        } else {
            0
        },
        payload.into_inner(),
    )
}

fn success_or_error_frame(
    version: u16,
    kind: FrameKind,
    opcode: u8,
    request_id: u32,
    target_id: u32,
    flags: u32,
    payload: Vec<u8>,
) -> Result<Frame, ProtocolError> {
    let payload_len = u32::try_from(payload.len())
        .map_err(|_| ProtocolError::message("payload exceeds protocol size limit"))?;
    Ok(Frame {
        header: FrameHeader {
            version,
            kind,
            opcode,
            flags,
            request_id,
            target_id,
            payload_len,
        },
        payload,
    })
}

pub fn decode_error_frame(frame: &Frame) -> Result<ErrorFrame, ProtocolError> {
    let mut payload = PayloadReader::new(&frame.payload);
    let code = ErrorCode::from_u32(payload.read_u32()?)
        .ok_or_else(|| ProtocolError::message("unknown protocol error code"))?;
    let message = payload.read_string()?;
    let diagnostics = if frame.header.flags & FLAG_DIAGNOSTICS != 0 {
        Some(decode_diagnostics(&mut payload)?)
    } else {
        None
    };
    payload.finish()?;
    Ok(ErrorFrame {
        code,
        message,
        diagnostics,
    })
}

pub fn encode_inline_value(
    writer: &mut PayloadWriter,
    value: &InlineValue,
) -> Result<(), ProtocolError> {
    match value {
        InlineValue::Null => writer.write_u8(0x00),
        InlineValue::Bool(value) => {
            writer.write_u8(0x01);
            writer.write_u8(u8::from(*value));
        }
        InlineValue::Int(value) => {
            writer.write_u8(0x02);
            writer.write_i64(*value);
        }
        InlineValue::Float(value) => {
            writer.write_u8(0x03);
            writer.write_f64(*value);
        }
        InlineValue::String(value) => {
            writer.write_u8(0x04);
            writer.write_string(value)?;
        }
        InlineValue::Tuple(values) => {
            writer.write_u8(0x05);
            let len = u32::try_from(values.len())
                .map_err(|_| ProtocolError::message("tuple exceeds protocol size limit"))?;
            writer.write_u32(len);
            for value in values {
                encode_inline_value(writer, value)?;
            }
        }
    }
    Ok(())
}

pub fn decode_inline_value(reader: &mut PayloadReader<'_>) -> Result<InlineValue, ProtocolError> {
    match reader.read_u8()? {
        0x00 => Ok(InlineValue::Null),
        0x01 => Ok(InlineValue::Bool(match reader.read_u8()? {
            0 => false,
            1 => true,
            _ => return Err(ProtocolError::message("invalid boolean payload")),
        })),
        0x02 => Ok(InlineValue::Int(reader.read_i64()?)),
        0x03 => Ok(InlineValue::Float(reader.read_f64()?)),
        0x04 => Ok(InlineValue::String(reader.read_string()?)),
        0x05 => {
            let count = reader.read_u32()? as usize;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(decode_inline_value(reader)?);
            }
            Ok(InlineValue::Tuple(values))
        }
        0x06 => Err(ProtocolError::message(
            "record values are not supported by the current runtime",
        )),
        0x07 => Err(ProtocolError::message(
            "handle values must be decoded by the connection layer",
        )),
        _ => Err(ProtocolError::message("unknown value tag")),
    }
}

pub fn encode_optimization(writer: &mut PayloadWriter, value: OptimizationLevel) {
    writer.write_u8(match value {
        OptimizationLevel::NOpt => 0,
        OptimizationLevel::IOpt => 1,
        OptimizationLevel::SOpt => 2,
    });
}

pub fn decode_optimization(
    reader: &mut PayloadReader<'_>,
) -> Result<OptimizationLevel, ProtocolError> {
    match reader.read_u8()? {
        0 => Ok(OptimizationLevel::NOpt),
        1 => Ok(OptimizationLevel::IOpt),
        2 => Ok(OptimizationLevel::SOpt),
        _ => Err(ProtocolError::message("invalid optimization mode")),
    }
}

pub fn encode_manifest(
    writer: &mut PayloadWriter,
    manifest: &PackageManifest,
) -> Result<(), ProtocolError> {
    writer.write_string(&manifest.package.as_str())?;
    writer.write_u32(
        u32::try_from(manifest.types.len())
            .map_err(|_| ProtocolError::message("type list exceeds protocol size limit"))?,
    );
    for ty in &manifest.types {
        encode_qualified_type_name(writer, &ty.name)?;
    }

    writer.write_u32(
        u32::try_from(manifest.functions.len())
            .map_err(|_| ProtocolError::message("function list exceeds protocol size limit"))?,
    );
    for function in &manifest.functions {
        writer.write_string(&function.name)?;
        writer.write_u32(
            u32::try_from(function.parameters.len()).map_err(|_| {
                ProtocolError::message("parameter list exceeds protocol size limit")
            })?,
        );
        for parameter in &function.parameters {
            writer.write_string(&parameter.name)?;
            encode_vox_type(writer, &parameter.ty)?;
            writer.write_u8(u8::from(parameter.has_default));
        }
        encode_vox_type(writer, &function.return_type)?;
        writer.write_u8(match function.purity {
            Purity::Pure => 0,
            Purity::Evil => 1,
        });
    }
    Ok(())
}

pub fn decode_manifest(reader: &mut PayloadReader<'_>) -> Result<PackageManifest, ProtocolError> {
    let package = ModulePath::parse(&reader.read_string()?)
        .map_err(|diagnostic| ProtocolError::message(diagnostic.message))?;
    let type_count = reader.read_u32()? as usize;
    let mut types = Vec::with_capacity(type_count);
    for _ in 0..type_count {
        types.push(TypeSpec {
            name: decode_qualified_type_name(reader)?,
        });
    }

    let function_count = reader.read_u32()? as usize;
    let mut functions = Vec::with_capacity(function_count);
    for _ in 0..function_count {
        let name = reader.read_string()?;
        let parameter_count = reader.read_u32()? as usize;
        let mut parameters = Vec::with_capacity(parameter_count);
        for _ in 0..parameter_count {
            parameters.push(ParameterSpec {
                name: reader.read_string()?,
                ty: decode_vox_type(reader)?,
                has_default: match reader.read_u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(ProtocolError::message("invalid default-value flag")),
                },
            });
        }
        let return_type = decode_vox_type(reader)?;
        let purity = match reader.read_u8()? {
            0 => Purity::Pure,
            1 => Purity::Evil,
            _ => return Err(ProtocolError::message("invalid purity tag")),
        };
        functions.push(FunctionSpec {
            name,
            parameters,
            return_type,
            purity,
        });
    }

    Ok(PackageManifest {
        package,
        types,
        functions,
    })
}

fn encode_vox_type(writer: &mut PayloadWriter, ty: &VoxType) -> Result<(), ProtocolError> {
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
            writer.write_u32(
                u32::try_from(items.len()).map_err(|_| {
                    ProtocolError::message("tuple type exceeds protocol size limit")
                })?,
            );
            for item in items {
                encode_vox_type(writer, item)?;
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

fn decode_vox_type(reader: &mut PayloadReader<'_>) -> Result<VoxType, ProtocolError> {
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
        0x06 => Ok(VoxType::Nullable(Box::new(decode_vox_type(reader)?))),
        0x07 => Ok(VoxType::DynTrait(decode_qualified_type_name(reader)?)),
        0x08 => Ok(VoxType::Named(decode_qualified_type_name(reader)?)),
        0x09 => Ok(VoxType::TypeParameter(reader.read_string()?)),
        0x0a => Ok(VoxType::OpaqueSurface(reader.read_string()?)),
        _ => Err(ProtocolError::message("unknown type tag")),
    }
}

fn encode_qualified_type_name(
    writer: &mut PayloadWriter,
    name: &QualifiedTypeName,
) -> Result<(), ProtocolError> {
    writer.write_string(&name.module.as_str())?;
    writer.write_string(&name.name)?;
    Ok(())
}

fn decode_qualified_type_name(
    reader: &mut PayloadReader<'_>,
) -> Result<QualifiedTypeName, ProtocolError> {
    let module = ModulePath::parse(&reader.read_string()?)
        .map_err(|diagnostic| ProtocolError::message(diagnostic.message))?;
    let name = reader.read_string()?;
    Ok(QualifiedTypeName { module, name })
}

fn encode_diagnostics(
    writer: &mut PayloadWriter,
    diagnostics: &DiagnosticBag,
) -> Result<(), ProtocolError> {
    let entries = diagnostics.iter().collect::<Vec<_>>();
    writer.write_u32(
        u32::try_from(entries.len())
            .map_err(|_| ProtocolError::message("diagnostic list exceeds protocol size limit"))?,
    );
    for diagnostic in entries {
        writer.write_u8(match diagnostic.severity {
            Severity::Error => 0,
            Severity::Warning => 1,
            Severity::Note => 2,
        });
        writer.write_string("")?;
        writer.write_string(&diagnostic.message)?;
        writer.write_string("")?;
        let start = diagnostic
            .span
            .as_ref()
            .map(|span| span.start.min(u32::MAX as usize) as u32)
            .unwrap_or(0);
        let end = diagnostic
            .span
            .as_ref()
            .map(|span| span.end.min(u32::MAX as usize) as u32)
            .unwrap_or(0);
        writer.write_u32(start);
        writer.write_u32(end);
    }
    Ok(())
}

fn decode_diagnostics(reader: &mut PayloadReader<'_>) -> Result<DiagnosticBag, ProtocolError> {
    let count = reader.read_u32()? as usize;
    let mut diagnostics = Vec::with_capacity(count);
    for _ in 0..count {
        let severity = match reader.read_u8()? {
            0 => Severity::Error,
            1 => Severity::Warning,
            2 => Severity::Note,
            _ => return Err(ProtocolError::message("invalid diagnostic severity")),
        };
        let _code = reader.read_string()?;
        let message = reader.read_string()?;
        let _source_name = reader.read_string()?;
        let start = reader.read_u32()? as usize;
        let end = reader.read_u32()? as usize;
        diagnostics.push(Diagnostic {
            severity,
            message,
            span: (start != 0 || end != 0)
                .then_some(vox_core::diagnostics::TextSpan { start, end }),
        });
    }
    Ok(DiagnosticBag::from(diagnostics))
}
