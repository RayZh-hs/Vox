use std::{
    collections::BTreeSet,
    net::{TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use vox_core::{
    host::PackageManifest,
    ids::{ArtifactId, HandleId, LibraryId, SessionId},
    opt::OptimizationLevel,
    source::SourceText,
    value::{HandleData, HandleSummary, RuntimeValue},
};

use crate::{
    CacheStats, HandleDataChunk, OptimizationDump, OptimizationDumpKind, OptimizationSettings,
    OptimizationStatus, RunnerError, RuntimeRunner, SessionOpenMode, SessionOpenRequest,
    SessionSelector, SessionSummary,
    protocol::{
        CURRENT_PROTOCOL_VERSION, DEFAULT_INLINE_VALUE_BYTES, ErrorCode, FLAG_HANDLE_RESULT,
        FLAG_INLINE_VALUE, Frame, FrameKind, Opcode, PayloadReader, PayloadWriter, ProtocolError,
        decode_error_frame, decode_handle_data, decode_inline_value, decode_optimization,
        encode_inline_value, encode_manifest, encode_optimization, read_frame, success_frame,
        write_frame,
    },
};

#[derive(Debug, Clone)]
pub struct RemoteRunner {
    inner: Arc<RemoteRunnerInner>,
}

#[derive(Debug)]
struct RemoteRunnerInner {
    stream: Mutex<TcpStream>,
    request_ids: AtomicU32,
    protocol_version: u16,
    max_payload_bytes: u32,
    state: Mutex<RemoteState>,
}

#[derive(Debug, Clone)]
struct RemoteState {
    default_xopt: OptimizationLevel,
    mounted_manifests: Vec<(LibraryId, PackageManifest)>,
    known_handles: BTreeSet<HandleId>,
}

impl RemoteRunner {
    pub fn connect(addr: impl ToSocketAddrs) -> Result<Self, RunnerError> {
        let mut stream = TcpStream::connect(addr)
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        stream
            .set_nodelay(true)
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;

        let (protocol_version, max_payload_bytes, _max_inline_value_bytes) =
            Self::handshake(&mut stream)?;
        Ok(Self {
            inner: Arc::new(RemoteRunnerInner {
                stream: Mutex::new(stream),
                request_ids: AtomicU32::new(1),
                protocol_version,
                max_payload_bytes,
                state: Mutex::new(RemoteState {
                    default_xopt: OptimizationLevel::IOpt,
                    mounted_manifests: Vec::new(),
                    known_handles: BTreeSet::new(),
                }),
            }),
        })
    }

    pub fn ping(&self) -> Result<u64, RunnerError> {
        let frame = self.invoke(Opcode::Ping, 0, 0, Vec::new())?;
        let mut payload = PayloadReader::new(&frame.payload);
        let uptime = payload.read_u64().map_err(protocol_to_runner)?;
        payload.finish().map_err(protocol_to_runner)?;
        Ok(uptime)
    }

    fn handshake(stream: &mut TcpStream) -> Result<(u16, u32, u32), RunnerError> {
        let mut payload = PayloadWriter::new();
        payload.write_u16(CURRENT_PROTOCOL_VERSION);
        payload.write_u16(CURRENT_PROTOCOL_VERSION);
        payload.write_u32(0);
        payload.write_u32(DEFAULT_INLINE_VALUE_BYTES);
        let request = success_frame(0, Opcode::Hello, 1, 0, 0, payload.into_inner())
            .map_err(protocol_to_runner)?
            .with_kind(FrameKind::Request);
        write_frame(stream, &request).map_err(protocol_to_runner)?;

        let frame = read_frame(stream)
            .map_err(protocol_to_runner)?
            .ok_or_else(|| {
                RunnerError::Unavailable("runtime closed the connection during HELLO".to_owned())
            })?;
        match frame.header.kind {
            FrameKind::Success => {
                let mut payload = PayloadReader::new(&frame.payload);
                let selected_version = payload.read_u16().map_err(protocol_to_runner)?;
                let _reserved = payload.read_u16().map_err(protocol_to_runner)?;
                let _server_caps = payload.read_u32().map_err(protocol_to_runner)?;
                let _instance_id = payload.read_u32().map_err(protocol_to_runner)?;
                let max_payload_bytes = payload.read_u32().map_err(protocol_to_runner)?;
                let max_inline_value_bytes = payload.read_u32().map_err(protocol_to_runner)?;
                payload.finish().map_err(protocol_to_runner)?;
                Ok((selected_version, max_payload_bytes, max_inline_value_bytes))
            }
            FrameKind::Error => Err(protocol_error_to_runner(&frame)?),
            _ => Err(RunnerError::Protocol(
                "runtime replied to HELLO with a non-response frame".to_owned(),
            )),
        }
    }

    fn invoke(
        &self,
        opcode: Opcode,
        target_id: u32,
        flags: u32,
        payload: Vec<u8>,
    ) -> Result<Frame, RunnerError> {
        let payload_len = u32::try_from(payload.len()).map_err(|_| {
            RunnerError::Protocol("request payload exceeds protocol size limit".to_owned())
        })?;
        if payload_len > self.inner.max_payload_bytes {
            return Err(RunnerError::Protocol(
                "request payload exceeds the negotiated runtime limit".to_owned(),
            ));
        }

        let request_id = self.inner.request_ids.fetch_add(1, Ordering::Relaxed);
        let frame = Frame {
            header: crate::protocol::FrameHeader {
                version: self.inner.protocol_version,
                kind: FrameKind::Request,
                opcode: opcode as u8,
                flags,
                request_id,
                target_id,
                payload_len,
            },
            payload,
        };

        let mut stream = self
            .inner
            .stream
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        write_frame(&mut *stream, &frame).map_err(protocol_to_runner)?;

        loop {
            let response = read_frame(&mut *stream)
                .map_err(protocol_to_runner)?
                .ok_or_else(|| {
                    RunnerError::Unavailable("runtime closed the connection".to_owned())
                })?;
            if response.header.kind == FrameKind::Event {
                continue;
            }
            if response.header.request_id != request_id {
                return Err(RunnerError::Protocol(
                    "runtime response request id did not match the outstanding request".to_owned(),
                ));
            }
            if response.header.opcode != opcode as u8 {
                return Err(RunnerError::Protocol(
                    "runtime response opcode did not match the outstanding request".to_owned(),
                ));
            }

            return match response.header.kind {
                FrameKind::Success => Ok(response),
                FrameKind::Error => Err(protocol_error_to_runner(&response)?),
                FrameKind::Request | FrameKind::Event => Err(RunnerError::Protocol(
                    "runtime replied with an unexpected frame kind".to_owned(),
                )),
            };
        }
    }

    fn current_default_xopt(&self) -> Result<OptimizationLevel, RunnerError> {
        self.inner
            .state
            .lock()
            .map(|state| state.default_xopt)
            .map_err(|error| RunnerError::Unavailable(error.to_string()))
    }

    fn update_default_xopt(&self, xopt: OptimizationLevel) -> Result<(), RunnerError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        state.default_xopt = xopt;
        Ok(())
    }

    fn encode_script_payload(
        &self,
        source: SourceText,
        xopt: Option<OptimizationLevel>,
    ) -> Result<Vec<u8>, RunnerError> {
        self.encode_script_payload_with_settings(
            source,
            OptimizationSettings::new(xopt.unwrap_or(self.current_default_xopt()?)),
        )
    }

    fn encode_script_payload_with_settings(
        &self,
        source: SourceText,
        settings: OptimizationSettings,
    ) -> Result<Vec<u8>, RunnerError> {
        let mut payload = PayloadWriter::new();
        payload.write_u8(0);
        encode_optimization(&mut payload, settings.default);
        payload.write_u8(0);
        payload.write_u8(0);
        encode_optimization_settings(&mut payload, &settings).map_err(protocol_to_runner)?;
        payload
            .write_string(source.origin.path.as_str())
            .map_err(protocol_to_runner)?;
        payload
            .write_bytes(source.text.as_bytes())
            .map_err(protocol_to_runner)?;
        Ok(payload.into_inner())
    }

    fn to_wire_id(&self, id: u64, subject: &str) -> Result<u32, RunnerError> {
        u32::try_from(id).map_err(|_| {
            RunnerError::Protocol(format!("{subject} id {id} exceeds the 32-bit wire range"))
        })
    }

    fn decode_runtime_result(&self, frame: Frame) -> Result<RuntimeValue, RunnerError> {
        let mut response = PayloadReader::new(&frame.payload);
        if frame.header.flags & FLAG_HANDLE_RESULT != 0 {
            let handle_id = response.read_u32().map_err(protocol_to_runner)?;
            response.finish().map_err(protocol_to_runner)?;
            let handle = HandleId(handle_id as u64);
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
            state.known_handles.insert(handle);
            Ok(RuntimeValue::Handle(handle))
        } else if frame.header.flags & FLAG_INLINE_VALUE != 0 {
            let value = decode_inline_value(&mut response).map_err(protocol_to_runner)?;
            response.finish().map_err(protocol_to_runner)?;
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
            collect_inline_handles(&value, &mut state.known_handles);
            Ok(RuntimeValue::Inline(value))
        } else if frame.payload.is_empty() {
            Err(RunnerError::Protocol(
                "runtime returned a result frame without a value payload".to_owned(),
            ))
        } else {
            Err(RunnerError::Protocol(
                "runtime returned a malformed result payload".to_owned(),
            ))
        }
    }
}

impl RuntimeRunner for RemoteRunner {
    fn open_session(&self, request: SessionOpenRequest) -> Result<SessionId, RunnerError> {
        let mut payload = PayloadWriter::new();
        payload.write_u8(match request.mode {
            SessionOpenMode::Attach => 0,
            SessionOpenMode::Create => 1,
            SessionOpenMode::AttachOrCreate => 2,
        });
        match request.selector {
            None => {
                payload.write_u8(0);
                payload.write_u8(0);
                payload.write_u8(0);
            }
            Some(SessionSelector::Id(session_id)) => {
                payload.write_u8(1);
                payload.write_u8(0);
                payload.write_u8(0);
                payload.write_u32(self.to_wire_id(session_id.0, "session")?);
            }
            Some(SessionSelector::Name(name)) => {
                payload.write_u8(2);
                payload.write_u8(0);
                payload.write_u8(0);
                payload.write_string(&name).map_err(protocol_to_runner)?;
            }
        }
        let frame = self.invoke(Opcode::OpenSession, 0, 0, payload.into_inner())?;
        let mut response = PayloadReader::new(&frame.payload);
        let session_id = response.read_u32().map_err(protocol_to_runner)?;
        response.finish().map_err(protocol_to_runner)?;
        Ok(SessionId(session_id as u64))
    }

    fn close_session(&self, session: SessionId) -> Result<(), RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let _ = self.invoke(Opcode::CloseSession, target_id, 0, Vec::new())?;
        Ok(())
    }

    fn list_sessions(&self) -> Result<Vec<SessionSummary>, RunnerError> {
        let frame = self.invoke(Opcode::ListSessions, 0, 0, Vec::new())?;
        let mut response = PayloadReader::new(&frame.payload);
        let count = response.read_u32().map_err(protocol_to_runner)? as usize;
        let mut sessions = Vec::with_capacity(count);
        for _ in 0..count {
            let id = SessionId(response.read_u32().map_err(protocol_to_runner)? as u64);
            let has_name = response.read_u8().map_err(protocol_to_runner)? != 0;
            let reserved = response.read_u8().map_err(protocol_to_runner)? != 0;
            let _reserved0 = response.read_u8().map_err(protocol_to_runner)?;
            let _reserved1 = response.read_u8().map_err(protocol_to_runner)?;
            let attached_endpoints = response.read_u64().map_err(protocol_to_runner)?;
            let name = if has_name {
                Some(response.read_string().map_err(protocol_to_runner)?)
            } else {
                None
            };
            sessions.push(SessionSummary {
                id,
                name,
                attached_endpoints,
                reserved,
            });
        }
        response.finish().map_err(protocol_to_runner)?;
        Ok(sessions)
    }

    fn set_session_reserved(&self, session: SessionId, reserved: bool) -> Result<(), RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let mut payload = PayloadWriter::new();
        payload.write_u8(u8::from(reserved));
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_u8(0);
        let _ = self.invoke(
            Opcode::SetSessionReserved,
            target_id,
            0,
            payload.into_inner(),
        )?;
        Ok(())
    }

    fn evaluate_session_submission(
        &self,
        session: SessionId,
        raw: &str,
    ) -> Result<Option<RuntimeValue>, RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let mut payload = PayloadWriter::new();
        payload.write_string(raw).map_err(protocol_to_runner)?;
        let frame = self.invoke(Opcode::EvaluateSession, target_id, 0, payload.into_inner())?;
        if frame.header.flags == 0 && frame.payload.is_empty() {
            return Ok(None);
        }
        self.decode_runtime_result(frame).map(Some)
    }

    fn run_session_script_text(
        &self,
        session: SessionId,
        path: &str,
        raw: &str,
    ) -> Result<RuntimeValue, RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let mut payload = PayloadWriter::new();
        payload.write_string(path).map_err(protocol_to_runner)?;
        payload.write_string(raw).map_err(protocol_to_runner)?;
        let frame = self.invoke(Opcode::RunSessionScript, target_id, 0, payload.into_inner())?;
        self.decode_runtime_result(frame)
    }

    fn drop_session_item(&self, session: SessionId, raw: &str) -> Result<bool, RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let mut payload = PayloadWriter::new();
        payload.write_string(raw).map_err(protocol_to_runner)?;
        let frame = self.invoke(Opcode::DropSessionItem, target_id, 0, payload.into_inner())?;
        let mut response = PayloadReader::new(&frame.payload);
        let removed = response.read_u8().map_err(protocol_to_runner)? != 0;
        response.finish().map_err(protocol_to_runner)?;
        Ok(removed)
    }

    fn reset_session(&self, session: SessionId) -> Result<(), RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let _ = self.invoke(Opcode::ResetSession, target_id, 0, Vec::new())?;
        Ok(())
    }

    fn snapshot_session_source(&self, session: SessionId) -> Result<String, RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let frame = self.invoke(Opcode::SnapshotSession, target_id, 0, Vec::new())?;
        let mut response = PayloadReader::new(&frame.payload);
        let snapshot = response.read_string().map_err(protocol_to_runner)?;
        response.finish().map_err(protocol_to_runner)?;
        Ok(snapshot)
    }

    fn restore_session_snapshot(
        &self,
        session: SessionId,
        label: &str,
        text: &str,
    ) -> Result<(), RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let mut payload = PayloadWriter::new();
        payload.write_string(label).map_err(protocol_to_runner)?;
        payload.write_string(text).map_err(protocol_to_runner)?;
        let _ = self.invoke(Opcode::RestoreSession, target_id, 0, payload.into_inner())?;
        Ok(())
    }

    fn set_session_default_xopt(
        &self,
        session: SessionId,
        xopt: OptimizationLevel,
    ) -> Result<(), RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let mut payload = PayloadWriter::new();
        encode_optimization(&mut payload, xopt);
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_u8(0);
        let _ = self.invoke(Opcode::SetSessionXOpt, target_id, 0, payload.into_inner())?;
        Ok(())
    }

    fn set_session_optimization(
        &self,
        session: SessionId,
        xopt: OptimizationLevel,
        objects: &[String],
    ) -> Result<(), RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let mut payload = PayloadWriter::new();
        encode_optimization(&mut payload, xopt);
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_u8(0);
        write_string_list(&mut payload, objects).map_err(protocol_to_runner)?;
        let _ = self.invoke(Opcode::SetSessionOpt, target_id, 0, payload.into_inner())?;
        Ok(())
    }

    fn session_optimization_status(
        &self,
        session: SessionId,
        object: Option<&str>,
    ) -> Result<Vec<OptimizationStatus>, RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let mut payload = PayloadWriter::new();
        payload.write_u8(u8::from(object.is_some()));
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_u8(0);
        if let Some(object) = object {
            payload.write_string(object).map_err(protocol_to_runner)?;
        }
        let frame = self.invoke(Opcode::GetSessionOpt, target_id, 0, payload.into_inner())?;
        decode_optimization_statuses(&frame.payload)
    }

    fn session_optimization_dump(
        &self,
        session: SessionId,
        object: &str,
        kind: OptimizationDumpKind,
    ) -> Result<Option<OptimizationDump>, RunnerError> {
        let target_id = self.to_wire_id(session.0, "session")?;
        let mut payload = PayloadWriter::new();
        encode_dump_kind(&mut payload, kind);
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_string(object).map_err(protocol_to_runner)?;
        let frame = self.invoke(Opcode::DumpSessionOpt, target_id, 0, payload.into_inner())?;
        decode_optimization_dump(&frame.payload)
    }

    fn mount_library(&self, manifest: PackageManifest) -> Result<LibraryId, RunnerError> {
        let mut manifest_payload = PayloadWriter::new();
        encode_manifest(&mut manifest_payload, &manifest).map_err(protocol_to_runner)?;

        let mut payload = PayloadWriter::new();
        payload.write_u8(1);
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_u8(0);
        payload
            .write_bytes(&manifest_payload.into_inner())
            .map_err(protocol_to_runner)?;

        let frame = self.invoke(Opcode::MountLibrary, 0, 0, payload.into_inner())?;
        let mut response = PayloadReader::new(&frame.payload);
        let library_id = response.read_u32().map_err(protocol_to_runner)?;
        let _revision = response.read_u64().map_err(protocol_to_runner)?;
        response.finish().map_err(protocol_to_runner)?;

        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        let library = LibraryId(library_id as u64);
        state.mounted_manifests.push((library, manifest));
        Ok(library)
    }

    fn unmount_library(&self, library: LibraryId) -> Result<bool, RunnerError> {
        let target_id = self.to_wire_id(library.0, "library")?;
        let _frame = self.invoke(Opcode::UnmountLibrary, target_id, 0, Vec::new())?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        let removed = state
            .mounted_manifests
            .iter()
            .position(|(candidate, _)| *candidate == library)
            .is_some_and(|index| {
                state.mounted_manifests.remove(index);
                true
            });
        Ok(removed)
    }

    fn load_script(
        &self,
        source: SourceText,
        xopt: Option<OptimizationLevel>,
    ) -> Result<ArtifactId, RunnerError> {
        let payload = self.encode_script_payload(source, xopt)?;
        let frame = self.invoke(Opcode::LoadScript, 0, 0, payload)?;
        let mut response = PayloadReader::new(&frame.payload);
        let script_id = response.read_u32().map_err(protocol_to_runner)?;
        let _revision = response.read_u64().map_err(protocol_to_runner)?;
        let _parameter_count = response.read_u32().map_err(protocol_to_runner)?;
        let _result_is_handle_capable = response.read_u8().map_err(protocol_to_runner)?;
        response.finish().map_err(protocol_to_runner)?;
        Ok(ArtifactId(script_id as u64))
    }

    fn load_script_with_settings(
        &self,
        source: SourceText,
        settings: OptimizationSettings,
    ) -> Result<ArtifactId, RunnerError> {
        let payload = self.encode_script_payload_with_settings(source, settings)?;
        let frame = self.invoke(Opcode::LoadScript, 0, 0, payload)?;
        let mut response = PayloadReader::new(&frame.payload);
        let script_id = response.read_u32().map_err(protocol_to_runner)?;
        let _revision = response.read_u64().map_err(protocol_to_runner)?;
        let _parameter_count = response.read_u32().map_err(protocol_to_runner)?;
        let _result_is_handle_capable = response.read_u8().map_err(protocol_to_runner)?;
        response.finish().map_err(protocol_to_runner)?;
        Ok(ArtifactId(script_id as u64))
    }

    fn reload_script(
        &self,
        artifact_id: ArtifactId,
        source: SourceText,
    ) -> Result<(), RunnerError> {
        let target_id = self.to_wire_id(artifact_id.0, "script")?;
        let payload = self.encode_script_payload(source, None)?;
        let frame = self.invoke(Opcode::ReloadScript, target_id, 0, payload)?;
        let mut response = PayloadReader::new(&frame.payload);
        let _revision = response.read_u64().map_err(protocol_to_runner)?;
        let _parameter_count = response.read_u32().map_err(protocol_to_runner)?;
        let _result_is_handle_capable = response.read_u8().map_err(protocol_to_runner)?;
        response.finish().map_err(protocol_to_runner)?;
        Ok(())
    }

    fn reload_script_with_settings(
        &self,
        artifact_id: ArtifactId,
        source: SourceText,
        settings: OptimizationSettings,
    ) -> Result<(), RunnerError> {
        let target_id = self.to_wire_id(artifact_id.0, "script")?;
        let payload = self.encode_script_payload_with_settings(source, settings)?;
        let frame = self.invoke(Opcode::ReloadScript, target_id, 0, payload)?;
        let mut response = PayloadReader::new(&frame.payload);
        let _revision = response.read_u64().map_err(protocol_to_runner)?;
        let _parameter_count = response.read_u32().map_err(protocol_to_runner)?;
        let _result_is_handle_capable = response.read_u8().map_err(protocol_to_runner)?;
        response.finish().map_err(protocol_to_runner)?;
        Ok(())
    }

    fn unload_script(&self, artifact_id: ArtifactId) -> Result<bool, RunnerError> {
        let target_id = self.to_wire_id(artifact_id.0, "script")?;
        let _ = self.invoke(Opcode::UnloadScript, target_id, 0, Vec::new())?;
        Ok(true)
    }

    fn run_script(
        &self,
        artifact_id: ArtifactId,
        arguments: &[RuntimeValue],
    ) -> Result<RuntimeValue, RunnerError> {
        self.run_script_with_xopt(artifact_id, arguments, None)
    }

    fn run_script_with_xopt(
        &self,
        artifact_id: ArtifactId,
        arguments: &[RuntimeValue],
        xopt: Option<OptimizationLevel>,
    ) -> Result<RuntimeValue, RunnerError> {
        let target_id = self.to_wire_id(artifact_id.0, "script")?;
        let mut payload = PayloadWriter::new();
        match xopt {
            Some(xopt) => encode_optimization(&mut payload, xopt),
            None => payload.write_u8(u8::MAX),
        }
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_u32(
            u32::try_from(arguments.len())
                .map_err(|_| RunnerError::Protocol("argument count exceeds u32".to_owned()))?,
        );
        for argument in arguments {
            match argument {
                RuntimeValue::Inline(value) => {
                    encode_inline_value(&mut payload, value).map_err(protocol_to_runner)?;
                }
                RuntimeValue::Handle(handle) => {
                    payload.write_u8(0x07);
                    payload.write_u32(self.to_wire_id(handle.0, "handle")?);
                }
            }
        }

        let frame = self.invoke(Opcode::RunScript, target_id, 0, payload.into_inner())?;
        self.decode_runtime_result(frame)
    }

    fn retain_handle(&self, handle: HandleId) -> Result<bool, RunnerError> {
        let target_id = self.to_wire_id(handle.0, "handle")?;
        let mut payload = PayloadWriter::new();
        payload.write_u32(1);
        let frame = self.invoke(Opcode::RetainHandle, target_id, 0, payload.into_inner())?;
        let mut response = PayloadReader::new(&frame.payload);
        let _handle_id = response.read_u32().map_err(protocol_to_runner)?;
        let _retained_refs = response.read_u32().map_err(protocol_to_runner)?;
        response.finish().map_err(protocol_to_runner)?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        state.known_handles.insert(handle);
        Ok(true)
    }

    fn describe_handle(&self, handle: HandleId) -> Result<Option<HandleSummary>, RunnerError> {
        let target_id = self.to_wire_id(handle.0, "handle")?;
        match self.invoke(Opcode::DescribeHandle, target_id, 0, Vec::new()) {
            Ok(frame) => {
                let mut response = PayloadReader::new(&frame.payload);
                let _handle_id = response.read_u32().map_err(protocol_to_runner)?;
                let type_name = response.read_string().map_err(protocol_to_runner)?;
                let bytes = response.read_u64().map_err(protocol_to_runner)?;
                let _ref_count = response.read_u32().map_err(protocol_to_runner)?;
                let _flags = response.read_u32().map_err(protocol_to_runner)?;
                let summary = if response.remaining() > 0 {
                    response.read_string().map_err(protocol_to_runner)?
                } else {
                    String::new()
                };
                response.finish().map_err(protocol_to_runner)?;
                Ok(Some(HandleSummary {
                    type_name,
                    summary,
                    bytes: (bytes != 0).then_some(bytes),
                }))
            }
            Err(RunnerError::Protocol(message)) if message.contains("unknown handle") => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn read_handle_data(
        &self,
        handle: HandleId,
        offset: u64,
        max_bytes: u32,
    ) -> Result<HandleDataChunk, RunnerError> {
        let target_id = self.to_wire_id(handle.0, "handle")?;
        let mut payload = PayloadWriter::new();
        payload.write_u64(offset);
        payload.write_u32(max_bytes);
        let frame = self.invoke(Opcode::ReadHandleData, target_id, 0, payload.into_inner())?;
        let mut response = PayloadReader::new(&frame.payload);
        let total_bytes = response.read_u64().map_err(protocol_to_runner)?;
        let bytes = response.read_bytes().map_err(protocol_to_runner)?;
        response.finish().map_err(protocol_to_runner)?;
        Ok(HandleDataChunk {
            offset,
            total_bytes,
            bytes,
        })
    }

    fn get_handle_data(&self, handle: HandleId) -> Result<HandleData, RunnerError> {
        let mut offset = 0_u64;
        let mut bytes = Vec::new();
        let max_chunk_bytes = self.inner.max_payload_bytes.saturating_sub(16).max(1);

        loop {
            let chunk = self.read_handle_data(handle, offset, max_chunk_bytes)?;
            if chunk.offset != offset {
                return Err(RunnerError::Protocol(
                    "runtime returned a mismatched handle data chunk".to_owned(),
                ));
            }
            if chunk.bytes.is_empty() && offset < chunk.total_bytes {
                return Err(RunnerError::Protocol(
                    "runtime returned an empty intermediate handle data chunk".to_owned(),
                ));
            }
            offset = offset.saturating_add(chunk.bytes.len() as u64);
            bytes.extend_from_slice(&chunk.bytes);
            if offset >= chunk.total_bytes {
                break;
            }
        }

        decode_handle_data_bytes(&bytes)
    }

    fn release_handle(&self, handle: HandleId) -> Result<bool, RunnerError> {
        let target_id = self.to_wire_id(handle.0, "handle")?;
        let mut payload = PayloadWriter::new();
        payload.write_u32(1);
        let frame = self.invoke(Opcode::ReleaseHandle, target_id, 0, payload.into_inner())?;
        let mut response = PayloadReader::new(&frame.payload);
        let remaining_refs = response.read_u32().map_err(protocol_to_runner)?;
        response.finish().map_err(protocol_to_runner)?;
        if remaining_refs == 0 {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
            state.known_handles.remove(&handle);
        }
        Ok(true)
    }

    fn live_handles(&self) -> Result<Vec<HandleId>, RunnerError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        Ok(state.known_handles.iter().copied().collect())
    }

    fn package_manifests(&self) -> Result<Vec<PackageManifest>, RunnerError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|error| RunnerError::Unavailable(error.to_string()))?;
        Ok(state
            .mounted_manifests
            .iter()
            .map(|(_, manifest)| manifest.clone())
            .collect())
    }

    fn set_default_xopt(&self, xopt: OptimizationLevel) -> Result<(), RunnerError> {
        let mut payload = PayloadWriter::new();
        encode_optimization(&mut payload, xopt);
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_u8(0);
        let _ = self.invoke(Opcode::SetXOpt, 0, 0, payload.into_inner())?;
        self.update_default_xopt(xopt)?;
        Ok(())
    }

    fn optimization_status(
        &self,
        artifact_id: ArtifactId,
        settings: &OptimizationSettings,
    ) -> Result<Vec<OptimizationStatus>, RunnerError> {
        let target_id = self.to_wire_id(artifact_id.0, "script")?;
        let mut payload = PayloadWriter::new();
        encode_optimization_settings(&mut payload, settings).map_err(protocol_to_runner)?;
        let frame = self.invoke(Opcode::GetOpt, target_id, 0, payload.into_inner())?;
        decode_optimization_statuses(&frame.payload)
    }

    fn optimization_dump(
        &self,
        artifact_id: ArtifactId,
        object: &str,
        kind: OptimizationDumpKind,
    ) -> Result<Option<OptimizationDump>, RunnerError> {
        let target_id = self.to_wire_id(artifact_id.0, "script")?;
        let mut payload = PayloadWriter::new();
        encode_dump_kind(&mut payload, kind);
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_string(object).map_err(protocol_to_runner)?;
        let frame = self.invoke(Opcode::DumpOpt, target_id, 0, payload.into_inner())?;
        decode_optimization_dump(&frame.payload)
    }

    fn cache_stats(&self) -> Result<CacheStats, RunnerError> {
        let frame = self.invoke(Opcode::CacheStats, 0, 0, Vec::new())?;
        let mut response = PayloadReader::new(&frame.payload);
        let artifacts = response.read_u64().map_err(protocol_to_runner)? as usize;
        let pure_cache_entries = response.read_u64().map_err(protocol_to_runner)? as usize;
        let pure_cache_bytes = response.read_u64().map_err(protocol_to_runner)?;
        let handles = response.read_u64().map_err(protocol_to_runner)? as usize;
        response.finish().map_err(protocol_to_runner)?;
        Ok(CacheStats {
            artifacts,
            pure_cache_entries,
            pure_cache_bytes,
            handles,
        })
    }

    fn clear_artifacts(&self) -> Result<(), RunnerError> {
        let mut payload = PayloadWriter::new();
        payload.write_u8(1);
        payload.write_u8(0);
        payload.write_u8(0);
        payload.write_u8(0);
        let _ = self.invoke(Opcode::ClearCache, 0, 0, payload.into_inner())?;
        Ok(())
    }
}

fn protocol_to_runner(error: ProtocolError) -> RunnerError {
    RunnerError::Protocol(error.to_string())
}

fn protocol_error_to_runner(frame: &Frame) -> Result<RunnerError, RunnerError> {
    let error = decode_error_frame(frame).map_err(protocol_to_runner)?;
    let message = error.message;
    let session_opcode = matches!(
        Opcode::from_u8(frame.header.opcode),
        Some(
            Opcode::OpenSession
                | Opcode::EvaluateSession
                | Opcode::DropSessionItem
                | Opcode::ResetSession
                | Opcode::SnapshotSession
                | Opcode::RestoreSession
                | Opcode::RunSessionScript
                | Opcode::SetSessionXOpt
                | Opcode::SetSessionOpt
                | Opcode::GetSessionOpt
                | Opcode::DumpSessionOpt
                | Opcode::CloseSession
                | Opcode::ListSessions
                | Opcode::SetSessionReserved
        )
    );
    Ok(match error.code {
        ErrorCode::CompileFailed | ErrorCode::RuntimeFailed if session_opcode => {
            RunnerError::Session(message)
        }
        ErrorCode::BadArgument if session_opcode => RunnerError::Session(message),
        ErrorCode::VersionMismatch
        | ErrorCode::BadFrame
        | ErrorCode::UnsupportedOpcode
        | ErrorCode::BadArgument
        | ErrorCode::PermissionDenied => RunnerError::Protocol(message),
        ErrorCode::UnknownLibrary | ErrorCode::UnknownScript | ErrorCode::UnknownHandle => {
            RunnerError::Protocol(message)
        }
        ErrorCode::CompileFailed => {
            RunnerError::Runtime(crate::RuntimeError::CompilationFailed(message))
        }
        ErrorCode::RuntimeFailed => {
            RunnerError::Runtime(crate::RuntimeError::ExecutionFailed(message))
        }
    })
}

fn decode_handle_data_bytes(bytes: &[u8]) -> Result<HandleData, RunnerError> {
    let mut reader = PayloadReader::new(bytes);
    let value = decode_handle_data(&mut reader).map_err(protocol_to_runner)?;
    reader.finish().map_err(protocol_to_runner)?;
    Ok(value)
}

fn collect_inline_handles(value: &vox_core::value::InlineValue, handles: &mut BTreeSet<HandleId>) {
    match value {
        vox_core::value::InlineValue::Handle(handle) => {
            handles.insert(*handle);
        }
        vox_core::value::InlineValue::Tuple(values) => {
            for value in values {
                collect_inline_handles(value, handles);
            }
        }
        vox_core::value::InlineValue::Record(fields) => {
            for value in fields.values() {
                collect_inline_handles(value, handles);
            }
        }
        vox_core::value::InlineValue::Null
        | vox_core::value::InlineValue::Bool(_)
        | vox_core::value::InlineValue::Int(_)
        | vox_core::value::InlineValue::Float(_)
        | vox_core::value::InlineValue::String(_) => {}
    }
}

fn write_string_list(writer: &mut PayloadWriter, values: &[String]) -> Result<(), ProtocolError> {
    writer.write_u32(
        u32::try_from(values.len())
            .map_err(|_| ProtocolError::message("string list exceeds protocol range"))?,
    );
    for value in values {
        writer.write_string(value)?;
    }
    Ok(())
}

fn encode_optimization_settings(
    writer: &mut PayloadWriter,
    settings: &OptimizationSettings,
) -> Result<(), ProtocolError> {
    encode_optimization(writer, settings.default);
    writer.write_u8(0);
    writer.write_u8(0);
    writer.write_u8(0);
    writer.write_u32(
        u32::try_from(settings.overrides.len())
            .map_err(|_| ProtocolError::message("optimization override count exceeds u32"))?,
    );
    for (object, xopt) in &settings.overrides {
        writer.write_string(object)?;
        encode_optimization(writer, *xopt);
        writer.write_u8(0);
        writer.write_u8(0);
        writer.write_u8(0);
    }
    Ok(())
}

fn encode_dump_kind(writer: &mut PayloadWriter, kind: OptimizationDumpKind) {
    writer.write_u8(match kind {
        OptimizationDumpKind::Mir => 0,
        OptimizationDumpKind::Wasm => 1,
    });
}

fn decode_dump_kind(raw: u8) -> Result<OptimizationDumpKind, RunnerError> {
    match raw {
        0 => Ok(OptimizationDumpKind::Mir),
        1 => Ok(OptimizationDumpKind::Wasm),
        _ => Err(RunnerError::Protocol(
            "invalid optimization dump kind".to_owned(),
        )),
    }
}

fn decode_optimization_statuses(bytes: &[u8]) -> Result<Vec<OptimizationStatus>, RunnerError> {
    let mut reader = PayloadReader::new(bytes);
    let count = reader.read_u32().map_err(protocol_to_runner)? as usize;
    let mut statuses = Vec::with_capacity(count);
    for _ in 0..count {
        let object = reader.read_string().map_err(protocol_to_runner)?;
        let requested = decode_optimization(&mut reader).map_err(protocol_to_runner)?;
        let rank = decode_optimization_rank(reader.read_u8().map_err(protocol_to_runner)?)?;
        let has_artifact = reader.read_u8().map_err(protocol_to_runner)? != 0;
        let mir_available = reader.read_u8().map_err(protocol_to_runner)? != 0;
        let wasm_available = reader.read_u8().map_err(protocol_to_runner)? != 0;
        let artifact = if has_artifact {
            Some(ArtifactId(
                reader.read_u32().map_err(protocol_to_runner)? as u64
            ))
        } else {
            None
        };
        let runtime_note = match reader.read_string().map_err(protocol_to_runner)? {
            note if note.is_empty() => None,
            note => Some(note),
        };
        statuses.push(OptimizationStatus {
            object,
            requested,
            rank,
            artifact,
            mir_available,
            wasm_available,
            runtime_note,
        });
    }
    reader.finish().map_err(protocol_to_runner)?;
    Ok(statuses)
}

fn decode_optimization_rank(
    raw: u8,
) -> Result<Option<vox_core::opt::OptimizationRank>, RunnerError> {
    use vox_core::opt::OptimizationRank;

    match raw {
        0 => Ok(None),
        1 => Ok(Some(OptimizationRank::Baseline)),
        2 => Ok(Some(OptimizationRank::Interactive)),
        3 => Ok(Some(OptimizationRank::SealedOwnership)),
        4 => Ok(Some(OptimizationRank::SealedDemand)),
        5 => Ok(Some(OptimizationRank::SealedMaterialization)),
        _ => Err(RunnerError::Protocol(
            "invalid optimization rank".to_owned(),
        )),
    }
}

fn decode_optimization_dump(bytes: &[u8]) -> Result<Option<OptimizationDump>, RunnerError> {
    let mut reader = PayloadReader::new(bytes);
    let present = reader.read_u8().map_err(protocol_to_runner)? != 0;
    reader.read_u8().map_err(protocol_to_runner)?;
    reader.read_u8().map_err(protocol_to_runner)?;
    reader.read_u8().map_err(protocol_to_runner)?;
    if !present {
        reader.finish().map_err(protocol_to_runner)?;
        return Ok(None);
    }
    let kind = decode_dump_kind(reader.read_u8().map_err(protocol_to_runner)?)?;
    reader.read_u8().map_err(protocol_to_runner)?;
    reader.read_u8().map_err(protocol_to_runner)?;
    reader.read_u8().map_err(protocol_to_runner)?;
    let object = reader.read_string().map_err(protocol_to_runner)?;
    let text = reader.read_string().map_err(protocol_to_runner)?;
    reader.finish().map_err(protocol_to_runner)?;
    Ok(Some(OptimizationDump { object, kind, text }))
}

trait FrameExt {
    fn with_kind(self, kind: FrameKind) -> Self;
}

impl FrameExt for Frame {
    fn with_kind(mut self, kind: FrameKind) -> Self {
        self.header.kind = kind;
        self
    }
}
