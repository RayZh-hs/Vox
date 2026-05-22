use std::{
    collections::BTreeMap,
    io,
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::atomic::{AtomicU32, Ordering},
    thread,
    time::Instant,
};

use thiserror::Error;
use vox_core::{
    ids::{ArtifactId, HandleId, LibraryId, SessionId},
    opt::OptimizationLevel,
    source::SourceText,
    value::{InlineValue, RuntimeValue},
};

use crate::{
    CacheClearScope, EmbeddedRunner, RunnerError, Runtime, RuntimeError, RuntimeRunner,
    protocol::{
        CURRENT_PROTOCOL_VERSION, DEFAULT_INLINE_VALUE_BYTES, ErrorCode, FLAG_HANDLE_RESULT,
        FLAG_INLINE_VALUE, Frame, FrameKind, Opcode, PayloadReader, PayloadWriter, ProtocolError,
        decode_manifest, decode_optimization, error_frame, read_frame, success_frame, write_frame,
    },
};

#[derive(Debug, Error)]
pub enum RuntimeServerError {
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone)]
pub struct RuntimeServer {
    runner: EmbeddedRunner,
    started_at: Instant,
    next_instance_id: std::sync::Arc<AtomicU32>,
}

impl Default for RuntimeServer {
    fn default() -> Self {
        Self::new(Runtime::default())
    }
}

impl RuntimeServer {
    pub fn new(runtime: Runtime) -> Self {
        Self::with_runner(EmbeddedRunner::new(runtime))
    }

    pub fn with_runner(runner: EmbeddedRunner) -> Self {
        Self {
            runner,
            started_at: Instant::now(),
            next_instance_id: std::sync::Arc::new(AtomicU32::new(1)),
        }
    }

    pub fn serve_tcp(&self, addr: impl ToSocketAddrs) -> Result<(), RuntimeServerError> {
        let listener = TcpListener::bind(addr)?;
        self.serve_listener(listener)
    }

    pub fn serve_listener(&self, listener: TcpListener) -> Result<(), RuntimeServerError> {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            let instance_id = self.next_instance_id.fetch_add(1, Ordering::Relaxed);
            let runner = self.runner.clone();
            let started_at = self.started_at;
            thread::spawn(move || {
                let mut connection = RuntimeConnection::new(runner, instance_id, started_at);
                let _ = connection.serve(stream);
            });
        }
        Ok(())
    }
}

#[derive(Debug)]
struct RuntimeConnection {
    runner: EmbeddedRunner,
    instance_id: u32,
    started_at: Instant,
    negotiated: Option<NegotiatedProtocol>,
    next_source_revision: u64,
    next_script_id: u32,
    next_handle_id: u32,
    next_library_id: u32,
    scripts: BTreeMap<u32, ArtifactId>,
    libraries: BTreeMap<u32, LibraryId>,
    handles: BTreeMap<u32, HandleLease>,
    local_handle_ids: BTreeMap<HandleId, u32>,
}

#[derive(Debug, Clone, Copy)]
struct NegotiatedProtocol {
    version: u16,
    max_inline_value_bytes: u32,
}

#[derive(Debug, Clone, Copy)]
struct HandleLease {
    actual: HandleId,
    owned_refs: u32,
}

#[derive(Debug)]
struct WireFailure {
    code: ErrorCode,
    message: String,
    fatal: bool,
}

#[derive(Debug)]
struct RequestOutcome {
    frame: Frame,
    close_after: bool,
}

impl RuntimeConnection {
    fn new(runner: EmbeddedRunner, instance_id: u32, started_at: Instant) -> Self {
        Self {
            runner,
            instance_id,
            started_at,
            negotiated: None,
            next_source_revision: 0,
            next_script_id: 1,
            next_handle_id: 1,
            next_library_id: 1,
            scripts: BTreeMap::new(),
            libraries: BTreeMap::new(),
            handles: BTreeMap::new(),
            local_handle_ids: BTreeMap::new(),
        }
    }

    fn serve(&mut self, mut stream: TcpStream) -> Result<(), ProtocolError> {
        loop {
            let maybe_frame = match read_frame(&mut stream) {
                Ok(frame) => frame,
                Err(error) => {
                    let response = error_frame(
                        self.protocol_version(),
                        0,
                        0,
                        ErrorCode::BadFrame,
                        error.to_string(),
                        None,
                    )?;
                    let _ = write_frame(&mut stream, &response);
                    self.cleanup_handles();
                    return Err(error);
                }
            };
            let Some(frame) = maybe_frame else {
                self.cleanup_handles();
                return Ok(());
            };

            let request_id = frame.header.request_id;
            let opcode = frame.header.opcode;
            match self.handle_frame(frame) {
                Ok(outcome) => {
                    write_frame(&mut stream, &outcome.frame)?;
                    if outcome.close_after {
                        self.cleanup_handles();
                        return Ok(());
                    }
                }
                Err(failure) => {
                    let response = error_frame(
                        self.protocol_version(),
                        opcode,
                        request_id,
                        failure.code,
                        failure.message,
                        None,
                    )?;
                    write_frame(&mut stream, &response)?;
                    if failure.fatal {
                        self.cleanup_handles();
                        return Ok(());
                    }
                }
            }
        }
    }

    fn handle_frame(&mut self, frame: Frame) -> Result<RequestOutcome, WireFailure> {
        if frame.header.kind != FrameKind::Request {
            return Err(WireFailure::fatal(
                ErrorCode::BadFrame,
                "server only accepts request frames",
            ));
        }

        let Some(opcode) = Opcode::from_u8(frame.header.opcode) else {
            return Err(WireFailure::recoverable(
                ErrorCode::UnsupportedOpcode,
                "unsupported opcode",
            ));
        };

        if self.negotiated.is_none() && opcode != Opcode::Hello {
            return Err(WireFailure::fatal(
                ErrorCode::BadFrame,
                "HELLO must be the first request on a connection",
            ));
        }

        if opcode == Opcode::Hello {
            return self.handle_hello(frame);
        }

        let Some(protocol) = self.negotiated else {
            return Err(WireFailure::fatal(
                ErrorCode::BadFrame,
                "connection is not negotiated",
            ));
        };

        if frame.header.version != protocol.version {
            return Err(WireFailure::fatal(
                ErrorCode::BadFrame,
                "frame version does not match the negotiated protocol version",
            ));
        }

        let response = match opcode {
            Opcode::Ping => self.handle_ping(frame.header.request_id),
            Opcode::OpenSession => self.handle_open_session(frame),
            Opcode::EvaluateSession => self.handle_evaluate_session(frame),
            Opcode::DropSessionItem => self.handle_drop_session_item(frame),
            Opcode::ResetSession => self.handle_reset_session(frame),
            Opcode::SnapshotSession => self.handle_snapshot_session(frame),
            Opcode::RestoreSession => self.handle_restore_session(frame),
            Opcode::RunSessionScript => self.handle_run_session_script(frame),
            Opcode::SetSessionXOpt => self.handle_set_session_xopt(frame),
            Opcode::MountLibrary => self.handle_mount_library(frame),
            Opcode::UnmountLibrary => Err(WireFailure::recoverable(
                ErrorCode::UnsupportedOpcode,
                "UNMOUNT_LIBRARY is not implemented yet",
            )),
            Opcode::LoadScript => self.handle_load_script(frame),
            Opcode::ReloadScript => self.handle_reload_script(frame),
            Opcode::UnloadScript => self.handle_unload_script(frame),
            Opcode::SetXOpt => self.handle_set_xopt(frame),
            Opcode::RunScript => self.handle_run_script(frame),
            Opcode::RetainHandle => self.handle_retain_handle(frame),
            Opcode::DescribeHandle => self.handle_describe_handle(frame),
            Opcode::ReleaseHandle => self.handle_release_handle(frame),
            Opcode::RefreshEcon => Err(WireFailure::recoverable(
                ErrorCode::UnsupportedOpcode,
                "REFRESH_ECON is not implemented yet",
            )),
            Opcode::CacheStats => self.handle_cache_stats(frame.header.request_id),
            Opcode::ClearCache => self.handle_clear_cache(frame),
            Opcode::Shutdown => Err(WireFailure::recoverable(
                ErrorCode::PermissionDenied,
                "shutdown is not permitted on this server",
            )),
            Opcode::Hello => unreachable!("HELLO is handled before protocol dispatch"),
        }?;

        Ok(RequestOutcome {
            frame: response,
            close_after: false,
        })
    }

    fn handle_hello(&mut self, frame: Frame) -> Result<RequestOutcome, WireFailure> {
        if self.negotiated.is_some() {
            return Err(WireFailure::fatal(
                ErrorCode::BadFrame,
                "HELLO may only be sent once per connection",
            ));
        }
        if frame.header.version != 0 {
            return Err(WireFailure::fatal(
                ErrorCode::BadFrame,
                "HELLO requests must use version 0",
            ));
        }

        let mut payload = PayloadReader::new(&frame.payload);
        let min_version = payload.read_u16().map_err(WireFailure::bad_argument)?;
        let max_version = payload.read_u16().map_err(WireFailure::bad_argument)?;
        let _client_caps = payload.read_u32().map_err(WireFailure::bad_argument)?;
        let client_inline_limit = payload.read_u32().map_err(WireFailure::bad_argument)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        if min_version > CURRENT_PROTOCOL_VERSION || max_version < CURRENT_PROTOCOL_VERSION {
            return Err(WireFailure {
                code: ErrorCode::VersionMismatch,
                message: format!(
                    "client requested versions {min_version}..={max_version}, but server supports {}",
                    CURRENT_PROTOCOL_VERSION
                ),
                fatal: true,
            });
        }

        let negotiated = NegotiatedProtocol {
            version: CURRENT_PROTOCOL_VERSION,
            max_inline_value_bytes: client_inline_limit.min(DEFAULT_INLINE_VALUE_BYTES),
        };
        self.negotiated = Some(negotiated);

        let mut response = PayloadWriter::new();
        response.write_u16(CURRENT_PROTOCOL_VERSION);
        response.write_u16(0);
        response.write_u32(0);
        response.write_u32(self.instance_id);
        response.write_u32(crate::protocol::MAX_PAYLOAD_BYTES);
        response.write_u32(negotiated.max_inline_value_bytes);

        Ok(RequestOutcome {
            frame: success_frame(
                CURRENT_PROTOCOL_VERSION,
                Opcode::Hello,
                frame.header.request_id,
                0,
                0,
                response.into_inner(),
            )
            .map_err(WireFailure::bad_argument)?,
            close_after: false,
        })
    }

    fn handle_ping(&self, request_id: u32) -> Result<Frame, WireFailure> {
        let mut payload = PayloadWriter::new();
        payload.write_u64(self.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64);
        success_frame(
            self.protocol_version(),
            Opcode::Ping,
            request_id,
            0,
            0,
            payload.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_open_session(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let mut payload = PayloadReader::new(&frame.payload);
        let session_kind = payload.read_u8().map_err(WireFailure::bad_argument)?;
        self.read_reserved(&mut payload, 3)?;
        let name = match session_kind {
            0 => None,
            1 => Some(payload.read_string().map_err(WireFailure::bad_argument)?),
            _ => {
                return Err(WireFailure::recoverable(
                    ErrorCode::BadArgument,
                    "invalid interactive session kind",
                ));
            }
        };
        payload.finish().map_err(WireFailure::bad_argument)?;

        let session_id = self
            .runner
            .open_session(name.as_deref())
            .map_err(WireFailure::from_runner)?;
        let session_wire_id = u32::try_from(session_id.0).map_err(|_| {
            WireFailure::recoverable(
                ErrorCode::RuntimeFailed,
                "session id exceeds the 32-bit protocol range",
            )
        })?;

        let mut response = PayloadWriter::new();
        response.write_u32(session_wire_id);
        success_frame(
            self.protocol_version(),
            Opcode::OpenSession,
            frame.header.request_id,
            session_wire_id,
            0,
            response.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_evaluate_session(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let session_id = self.resolve_session(frame.header.target_id)?;
        let mut payload = PayloadReader::new(&frame.payload);
        let raw = payload.read_string().map_err(WireFailure::bad_argument)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        let result = self
            .runner
            .evaluate_session_submission(session_id, &raw)
            .map_err(WireFailure::from_runner)?;
        match result {
            Some(value) => self.encode_value_result(
                Opcode::EvaluateSession,
                frame.header.request_id,
                frame.header.target_id,
                value,
            ),
            None => success_frame(
                self.protocol_version(),
                Opcode::EvaluateSession,
                frame.header.request_id,
                frame.header.target_id,
                0,
                Vec::new(),
            )
            .map_err(WireFailure::bad_argument),
        }
    }

    fn handle_drop_session_item(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let session_id = self.resolve_session(frame.header.target_id)?;
        let mut payload = PayloadReader::new(&frame.payload);
        let raw = payload.read_string().map_err(WireFailure::bad_argument)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        let removed = self
            .runner
            .drop_session_item(session_id, &raw)
            .map_err(WireFailure::from_runner)?;
        let mut response = PayloadWriter::new();
        response.write_u8(u8::from(removed));
        success_frame(
            self.protocol_version(),
            Opcode::DropSessionItem,
            frame.header.request_id,
            frame.header.target_id,
            0,
            response.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_reset_session(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let session_id = self.resolve_session(frame.header.target_id)?;
        self.runner
            .reset_session(session_id)
            .map_err(WireFailure::from_runner)?;
        success_frame(
            self.protocol_version(),
            Opcode::ResetSession,
            frame.header.request_id,
            frame.header.target_id,
            0,
            Vec::new(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_snapshot_session(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let session_id = self.resolve_session(frame.header.target_id)?;
        let snapshot = self
            .runner
            .snapshot_session_source(session_id)
            .map_err(WireFailure::from_runner)?;
        let mut response = PayloadWriter::new();
        response
            .write_string(&snapshot)
            .map_err(WireFailure::bad_argument)?;
        success_frame(
            self.protocol_version(),
            Opcode::SnapshotSession,
            frame.header.request_id,
            frame.header.target_id,
            0,
            response.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_restore_session(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let session_id = self.resolve_session(frame.header.target_id)?;
        let mut payload = PayloadReader::new(&frame.payload);
        let label = payload.read_string().map_err(WireFailure::bad_argument)?;
        let text = payload.read_string().map_err(WireFailure::bad_argument)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        self.runner
            .restore_session_snapshot(session_id, &label, &text)
            .map_err(WireFailure::from_runner)?;
        success_frame(
            self.protocol_version(),
            Opcode::RestoreSession,
            frame.header.request_id,
            frame.header.target_id,
            0,
            Vec::new(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_run_session_script(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let session_id = self.resolve_session(frame.header.target_id)?;
        let mut payload = PayloadReader::new(&frame.payload);
        let path = payload.read_string().map_err(WireFailure::bad_argument)?;
        let raw = payload.read_string().map_err(WireFailure::bad_argument)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        let value = self
            .runner
            .run_session_script_text(session_id, &path, &raw)
            .map_err(WireFailure::from_runner)?;
        self.encode_value_result(
            Opcode::RunSessionScript,
            frame.header.request_id,
            frame.header.target_id,
            value,
        )
    }

    fn handle_set_session_xopt(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let session_id = self.resolve_session(frame.header.target_id)?;
        let mut payload = PayloadReader::new(&frame.payload);
        let xopt = decode_optimization(&mut payload).map_err(WireFailure::bad_argument)?;
        self.read_reserved(&mut payload, 3)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        self.runner
            .set_session_default_xopt(session_id, xopt)
            .map_err(WireFailure::from_runner)?;
        success_frame(
            self.protocol_version(),
            Opcode::SetSessionXOpt,
            frame.header.request_id,
            frame.header.target_id,
            0,
            Vec::new(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_mount_library(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let mut payload = PayloadReader::new(&frame.payload);
        let source_kind = payload.read_u8().map_err(WireFailure::bad_argument)?;
        self.read_reserved(&mut payload, 3)?;
        let source = payload.read_bytes().map_err(WireFailure::bad_argument)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        if source_kind != 1 {
            return Err(WireFailure::recoverable(
                ErrorCode::UnsupportedOpcode,
                "only manifest-byte library mounts are implemented",
            ));
        }

        let mut manifest_payload = PayloadReader::new(&source);
        let manifest = decode_manifest(&mut manifest_payload).map_err(WireFailure::bad_argument)?;
        manifest_payload.finish().map_err(WireFailure::bad_argument)?;

        let library = self
            .runner
            .with_runtime(|runtime| {
                let actual_id = runtime.mount_library(manifest);
                let mounted = runtime
                    .library(actual_id)
                    .cloned()
                    .ok_or_else(|| RunnerError::Unavailable("mounted library was not found".to_owned()))?;
                Ok(mounted)
            })
            .map_err(WireFailure::from_runner)?;

        let local_id = self.allocate_library_id(library.id);
        let mut response = PayloadWriter::new();
        response.write_u32(local_id);
        response.write_u64(library.revision);
        success_frame(
            self.protocol_version(),
            Opcode::MountLibrary,
            frame.header.request_id,
            local_id,
            0,
            response.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_load_script(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let (source, xopt) = self.decode_script_source(&frame.payload)?;
        let (artifact_id, source_revision, parameter_count) = self
            .runner
            .with_runtime(|runtime| {
                let artifact_id = runtime.load_script(source, Some(xopt))?;
                let artifact = runtime
                    .artifact(artifact_id)
                    .ok_or(RuntimeError::MissingArtifact(artifact_id))?;
                let parameter_count = u32::try_from(artifact.parameters.len())
                    .map_err(|_| RunnerError::Protocol("script parameter count exceeds u32".to_owned()))?;
                Ok((artifact_id, artifact.source_revision, parameter_count))
            })
            .map_err(WireFailure::from_runner)?;

        let local_id = self.allocate_script_id(artifact_id);
        let mut response = PayloadWriter::new();
        response.write_u32(local_id);
        response.write_u64(source_revision);
        response.write_u32(parameter_count);
        response.write_u8(1);
        success_frame(
            self.protocol_version(),
            Opcode::LoadScript,
            frame.header.request_id,
            local_id,
            0,
            response.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_reload_script(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let artifact_id = self.resolve_script(frame.header.target_id)?;
        let (source, _xopt) = self.decode_script_source(&frame.payload)?;
        let (source_revision, parameter_count) = self
            .runner
            .with_runtime(|runtime| {
                runtime.reload_script(artifact_id, source)?;
                let artifact = runtime
                    .artifact(artifact_id)
                    .ok_or(RuntimeError::MissingArtifact(artifact_id))?;
                let parameter_count = u32::try_from(artifact.parameters.len())
                    .map_err(|_| RunnerError::Protocol("script parameter count exceeds u32".to_owned()))?;
                Ok((artifact.source_revision, parameter_count))
            })
            .map_err(WireFailure::from_runner)?;

        let mut response = PayloadWriter::new();
        response.write_u64(source_revision);
        response.write_u32(parameter_count);
        response.write_u8(1);
        success_frame(
            self.protocol_version(),
            Opcode::ReloadScript,
            frame.header.request_id,
            frame.header.target_id,
            0,
            response.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_unload_script(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let artifact_id = self.resolve_script(frame.header.target_id)?;
        self.runner
            .unload_script(artifact_id)
            .map_err(WireFailure::from_runner)?;
        self.scripts.remove(&frame.header.target_id);
        success_frame(
            self.protocol_version(),
            Opcode::UnloadScript,
            frame.header.request_id,
            frame.header.target_id,
            0,
            Vec::new(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_set_xopt(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        if frame.header.target_id != 0 {
            return Err(WireFailure::recoverable(
                ErrorCode::BadArgument,
                "SET_XOPT currently applies to the connection default and requires target_id = 0",
            ));
        }

        let mut payload = PayloadReader::new(&frame.payload);
        let xopt = decode_optimization(&mut payload).map_err(WireFailure::bad_argument)?;
        self.read_reserved(&mut payload, 3)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        self.runner
            .set_default_xopt(xopt)
            .map_err(WireFailure::from_runner)?;
        success_frame(
            self.protocol_version(),
            Opcode::SetXOpt,
            frame.header.request_id,
            0,
            0,
            Vec::new(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_run_script(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let artifact_id = self.resolve_script(frame.header.target_id)?;
        let mut payload = PayloadReader::new(&frame.payload);
        let xopt_override = payload.read_u8().map_err(WireFailure::bad_argument)?;
        self.read_reserved(&mut payload, 3)?;
        if xopt_override != u8::MAX {
            return Err(WireFailure::recoverable(
                ErrorCode::UnsupportedOpcode,
                "RUN_SCRIPT optimization overrides are not implemented yet",
            ));
        }

        let argument_count = payload.read_u32().map_err(WireFailure::bad_argument)? as usize;
        let mut arguments = Vec::with_capacity(argument_count);
        for _ in 0..argument_count {
            arguments.push(self.decode_runtime_value(&mut payload)?);
        }
        payload.finish().map_err(WireFailure::bad_argument)?;

        let result = self
            .runner
            .run_script(artifact_id, &arguments)
            .map_err(WireFailure::from_runner)?;
        self.encode_value_result(
            Opcode::RunScript,
            frame.header.request_id,
            frame.header.target_id,
            result,
        )
    }

    fn handle_retain_handle(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let local_id = frame.header.target_id;
        let mut payload = PayloadReader::new(&frame.payload);
        let extra_refs = payload.read_u32().map_err(WireFailure::bad_argument)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        let actual = self.resolve_handle(local_id)?;
        for _ in 0..extra_refs {
            self.runner
                .retain_handle(actual)
                .map_err(WireFailure::from_runner)?;
        }

        let lease = self
            .handles
            .get_mut(&local_id)
            .ok_or_else(|| WireFailure::recoverable(ErrorCode::UnknownHandle, "unknown handle"))?;
        lease.owned_refs = lease.owned_refs.saturating_add(extra_refs);

        let mut response = PayloadWriter::new();
        response.write_u32(local_id);
        response.write_u32(lease.owned_refs);
        success_frame(
            self.protocol_version(),
            Opcode::RetainHandle,
            frame.header.request_id,
            local_id,
            0,
            response.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_describe_handle(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let actual = self.resolve_handle(frame.header.target_id)?;
        let metadata = self
            .runner
            .with_runtime(|runtime| {
                Ok(runtime.handle_metadata(actual))
            })
            .map_err(WireFailure::from_runner)?
            .ok_or_else(|| WireFailure::recoverable(ErrorCode::UnknownHandle, "unknown handle"))?;

        let mut response = PayloadWriter::new();
        response.write_u32(frame.header.target_id);
        response.write_string(&metadata.summary.type_name)
            .map_err(WireFailure::bad_argument)?;
        response.write_u64(metadata.summary.bytes.unwrap_or(0));
        response.write_u32(metadata.ref_count);
        response.write_u32(metadata.flags);
        response
            .write_string(&metadata.summary.summary)
            .map_err(WireFailure::bad_argument)?;
        success_frame(
            self.protocol_version(),
            Opcode::DescribeHandle,
            frame.header.request_id,
            frame.header.target_id,
            0,
            response.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_release_handle(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let local_id = frame.header.target_id;
        let mut payload = PayloadReader::new(&frame.payload);
        let release_refs = payload.read_u32().map_err(WireFailure::bad_argument)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        let lease = self
            .handles
            .get(&local_id)
            .copied()
            .ok_or_else(|| WireFailure::recoverable(ErrorCode::UnknownHandle, "unknown handle"))?;
        if release_refs > lease.owned_refs {
            return Err(WireFailure::recoverable(
                ErrorCode::BadArgument,
                "cannot release more handle references than this connection owns",
            ));
        }

        for _ in 0..release_refs {
            self.runner
                .release_handle(lease.actual)
                .map_err(WireFailure::from_runner)?;
        }

        let remaining_refs = lease.owned_refs - release_refs;
        if remaining_refs == 0 {
            self.handles.remove(&local_id);
            self.local_handle_ids.remove(&lease.actual);
        } else if let Some(lease) = self.handles.get_mut(&local_id) {
            lease.owned_refs = remaining_refs;
        }

        let mut response = PayloadWriter::new();
        response.write_u32(remaining_refs);
        success_frame(
            self.protocol_version(),
            Opcode::ReleaseHandle,
            frame.header.request_id,
            local_id,
            0,
            response.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_cache_stats(&self, request_id: u32) -> Result<Frame, WireFailure> {
        let stats = self.runner.cache_stats().map_err(WireFailure::from_runner)?;
        let mut response = PayloadWriter::new();
        response.write_u64(stats.artifacts as u64);
        response.write_u64(stats.pure_cache_entries as u64);
        response.write_u64(stats.pure_cache_bytes);
        response.write_u64(stats.handles as u64);
        success_frame(
            self.protocol_version(),
            Opcode::CacheStats,
            request_id,
            0,
            0,
            response.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn handle_clear_cache(&mut self, frame: Frame) -> Result<Frame, WireFailure> {
        let mut payload = PayloadReader::new(&frame.payload);
        let scope = match payload.read_u8().map_err(WireFailure::bad_argument)? {
            0 => CacheClearScope::All,
            1 => CacheClearScope::Artifacts,
            2 => CacheClearScope::PureCache,
            _ => return Err(WireFailure::recoverable(ErrorCode::BadArgument, "invalid cache scope")),
        };
        self.read_reserved(&mut payload, 3)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        let cleared_entries = self
            .runner
            .with_runtime(|runtime| Ok(runtime.clear_cache(scope)))
            .map_err(WireFailure::from_runner)?;
        let mut response = PayloadWriter::new();
        response.write_u64(cleared_entries);
        success_frame(
            self.protocol_version(),
            Opcode::ClearCache,
            frame.header.request_id,
            0,
            0,
            response.into_inner(),
        )
        .map_err(WireFailure::bad_argument)
    }

    fn decode_script_source(
        &mut self,
        payload_bytes: &[u8],
    ) -> Result<(SourceText, OptimizationLevel), WireFailure> {
        let mut payload = PayloadReader::new(payload_bytes);
        let source_kind = payload.read_u8().map_err(WireFailure::bad_argument)?;
        let xopt = decode_optimization(&mut payload).map_err(WireFailure::bad_argument)?;
        self.read_reserved(&mut payload, 2)?;
        let logical_path = payload.read_string().map_err(WireFailure::bad_argument)?;
        let source = payload.read_bytes().map_err(WireFailure::bad_argument)?;
        payload.finish().map_err(WireFailure::bad_argument)?;

        if source_kind != 0 {
            return Err(WireFailure::recoverable(
                ErrorCode::UnsupportedOpcode,
                "only source-text script payloads are implemented",
            ));
        }

        let source = String::from_utf8(source)
            .map_err(|_| WireFailure::recoverable(ErrorCode::BadArgument, "script source is not valid UTF-8"))?;
        self.next_source_revision += 1;
        Ok((
            SourceText::new(logical_path, self.next_source_revision, source),
            xopt,
        ))
    }

    fn decode_runtime_value(
        &self,
        payload: &mut PayloadReader<'_>,
    ) -> Result<RuntimeValue, WireFailure> {
        match payload.read_u8().map_err(WireFailure::bad_argument)? {
            0x00 => Ok(RuntimeValue::Inline(InlineValue::Null)),
            0x01 => Ok(RuntimeValue::Inline(InlineValue::Bool(
                match payload.read_u8().map_err(WireFailure::bad_argument)? {
                    0 => false,
                    1 => true,
                    _ => {
                        return Err(WireFailure::recoverable(
                            ErrorCode::BadArgument,
                            "invalid boolean value",
                        ));
                    }
                },
            ))),
            0x02 => Ok(RuntimeValue::Inline(InlineValue::Int(
                payload.read_i64().map_err(WireFailure::bad_argument)?,
            ))),
            0x03 => Ok(RuntimeValue::Inline(InlineValue::Float(
                payload.read_f64().map_err(WireFailure::bad_argument)?,
            ))),
            0x04 => Ok(RuntimeValue::Inline(InlineValue::String(
                payload.read_string().map_err(WireFailure::bad_argument)?,
            ))),
            0x05 => {
                let count = payload.read_u32().map_err(WireFailure::bad_argument)? as usize;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    let RuntimeValue::Inline(value) = self.decode_runtime_value(payload)? else {
                        return Err(WireFailure::recoverable(
                            ErrorCode::BadArgument,
                            "handle values are not supported inside tuples",
                        ));
                    };
                    values.push(value);
                }
                Ok(RuntimeValue::Inline(InlineValue::Tuple(values)))
            }
            0x07 => {
                let local_handle = payload.read_u32().map_err(WireFailure::bad_argument)?;
                Ok(RuntimeValue::Handle(self.resolve_handle(local_handle)?))
            }
            0x06 => Err(WireFailure::recoverable(
                ErrorCode::BadArgument,
                "record arguments are not supported by the current runtime",
            )),
            _ => Err(WireFailure::recoverable(
                ErrorCode::BadArgument,
                "unknown value tag",
            )),
        }
    }

    fn encode_value_result(
        &mut self,
        opcode: Opcode,
        request_id: u32,
        target_id: u32,
        value: RuntimeValue,
    ) -> Result<Frame, WireFailure> {
        match value {
            RuntimeValue::Inline(value) => {
                let mut payload = PayloadWriter::new();
                crate::protocol::encode_inline_value(&mut payload, &value)
                    .map_err(WireFailure::bad_argument)?;
                success_frame(
                    self.protocol_version(),
                    opcode,
                    request_id,
                    target_id,
                    FLAG_INLINE_VALUE,
                    payload.into_inner(),
                )
                .map_err(WireFailure::bad_argument)
            }
            RuntimeValue::Handle(actual) => {
                let local_handle = self.allocate_handle(actual);
                let mut payload = PayloadWriter::new();
                payload.write_u32(local_handle);
                success_frame(
                    self.protocol_version(),
                    opcode,
                    request_id,
                    target_id,
                    FLAG_HANDLE_RESULT,
                    payload.into_inner(),
                )
                .map_err(WireFailure::bad_argument)
            }
        }
    }

    fn allocate_script_id(&mut self, actual: ArtifactId) -> u32 {
        let id = self.next_script_id;
        self.next_script_id = self.next_script_id.saturating_add(1);
        self.scripts.insert(id, actual);
        id
    }

    fn allocate_library_id(&mut self, actual: LibraryId) -> u32 {
        let id = self.next_library_id;
        self.next_library_id = self.next_library_id.saturating_add(1);
        self.libraries.insert(id, actual);
        id
    }

    fn allocate_handle(&mut self, actual: HandleId) -> u32 {
        if let Some(&local) = self.local_handle_ids.get(&actual) {
            if let Some(lease) = self.handles.get_mut(&local) {
                lease.owned_refs = lease.owned_refs.saturating_add(1);
            }
            return local;
        }

        let id = self.next_handle_id;
        self.next_handle_id = self.next_handle_id.saturating_add(1);
        self.handles.insert(
            id,
            HandleLease {
                actual,
                owned_refs: 1,
            },
        );
        self.local_handle_ids.insert(actual, id);
        id
    }

    fn resolve_script(&self, local_id: u32) -> Result<ArtifactId, WireFailure> {
        self.scripts
            .get(&local_id)
            .copied()
            .ok_or_else(|| WireFailure::recoverable(ErrorCode::UnknownScript, "unknown script"))
    }

    fn resolve_session(&self, session_id: u32) -> Result<SessionId, WireFailure> {
        if session_id == 0 {
            return Err(WireFailure::recoverable(
                ErrorCode::BadArgument,
                "interactive session target_id must not be zero",
            ));
        }
        Ok(SessionId(session_id as u64))
    }

    fn resolve_handle(&self, local_id: u32) -> Result<HandleId, WireFailure> {
        self.handles
            .get(&local_id)
            .map(|lease| lease.actual)
            .ok_or_else(|| WireFailure::recoverable(ErrorCode::UnknownHandle, "unknown handle"))
    }

    fn cleanup_handles(&mut self) {
        for lease in self.handles.values().copied() {
            for _ in 0..lease.owned_refs {
                let _ = self.runner.release_handle(lease.actual);
            }
        }
        self.handles.clear();
        self.local_handle_ids.clear();
        self.scripts.clear();
    }

    fn protocol_version(&self) -> u16 {
        self.negotiated
            .map(|protocol| protocol.version)
            .unwrap_or(CURRENT_PROTOCOL_VERSION)
    }

    fn read_reserved(
        &self,
        payload: &mut PayloadReader<'_>,
        count: usize,
    ) -> Result<(), WireFailure> {
        for _ in 0..count {
            let _ = payload.read_u8().map_err(WireFailure::bad_argument)?;
        }
        Ok(())
    }
}

impl WireFailure {
    fn recoverable(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            fatal: false,
        }
    }

    fn fatal(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            fatal: true,
        }
    }

    fn bad_argument(error: ProtocolError) -> Self {
        Self::recoverable(ErrorCode::BadArgument, error.to_string())
    }

    fn from_runner(error: RunnerError) -> Self {
        match error {
            RunnerError::Runtime(RuntimeError::CompilationFailed(message)) => Self::recoverable(
                ErrorCode::CompileFailed,
                message,
            ),
            RunnerError::Runtime(RuntimeError::MissingArtifact(_))
            | RunnerError::Runtime(RuntimeError::NotAScript(_)) => {
                Self::recoverable(ErrorCode::UnknownScript, error.to_string())
            }
            RunnerError::Runtime(RuntimeError::ExecutionNotImplemented(_))
            | RunnerError::Runtime(RuntimeError::ExecutionFailed(_)) => {
                Self::recoverable(ErrorCode::RuntimeFailed, error.to_string())
            }
            RunnerError::Unavailable(message) => Self::fatal(ErrorCode::RuntimeFailed, message),
            RunnerError::Protocol(message) => Self::recoverable(ErrorCode::BadFrame, message),
            RunnerError::Session(message) => Self::recoverable(ErrorCode::RuntimeFailed, message),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::TcpListener,
        sync::atomic::Ordering,
        thread,
        time::Instant,
    };

    use vox_core::value::{InlineValue, RuntimeValue};

    use crate::{InteractiveSession, RemoteRunner, RuntimeServer};

    use super::RuntimeConnection;

    #[test]
    fn remote_clients_share_named_sessions_across_connections() {
        let (addr, server_thread) = spawn_test_server(3);

        let shared_addr = addr.to_string();
        let runner_one = RemoteRunner::connect(shared_addr.as_str()).expect("first client connects");
        let mut first = InteractiveSession::named(runner_one, "shared")
            .expect("shared session should open");
        assert!(
            first
                .evaluate_submission("val numbers = [39, 41];")
                .expect("first client should seed the session")
                .is_none()
        );
        let closure = first
            .evaluate_submission("() -> numbers[1] + 1")
            .expect("first client should store a closure")
            .expect("closure should produce a result");
        assert!(
            matches!(closure, RuntimeValue::Handle(_)),
            "remote closures should cross the protocol as handles"
        );
        drop(first);

        let runner_two = RemoteRunner::connect(shared_addr.as_str()).expect("second client connects");
        let mut second = InteractiveSession::named(runner_two, "shared")
            .expect("second client should attach to the same session");
        assert_runtime_int(
            second
                .evaluate_submission("$()")
                .expect("shared last value should survive reconnect")
                .expect("closure call should return a value"),
            42,
        );
        assert!(
            second
                .evaluate_submission("val answer = numbers[1] + 1;")
                .expect("second client should mutate shared state")
                .is_none()
        );
        drop(second);

        let runner_three =
            RemoteRunner::connect(shared_addr.as_str()).expect("third client connects");
        let mut isolated = InteractiveSession::named(runner_three, "isolated")
            .expect("isolated session should open");
        let error = isolated
            .evaluate_submission("answer")
            .expect_err("separate sessions must not see each other's bindings");
        assert!(
            error.to_string().contains("answer"),
            "unexpected isolated-session error: {error}"
        );
        drop(isolated);

        server_thread.join().expect("test server should stop cleanly");
    }

    fn spawn_test_server(expected_connections: usize) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let addr = listener.local_addr().expect("listener should expose an address");
        let server = RuntimeServer::default();

        let handle = thread::spawn(move || {
            let started_at = Instant::now();
            for _ in 0..expected_connections {
                let (stream, _) = listener.accept().expect("connection should be accepted");
                let instance_id = server.next_instance_id.fetch_add(1, Ordering::Relaxed);
                let mut connection =
                    RuntimeConnection::new(server.runner.clone(), instance_id, started_at);
                connection.serve(stream).expect("connection should complete");
            }
        });

        (addr, handle)
    }

    fn assert_runtime_int(value: RuntimeValue, expected: i64) {
        match value {
            RuntimeValue::Inline(InlineValue::Int(actual)) => assert_eq!(actual, expected),
            other => panic!("expected inline int {expected}, got {other:?}"),
        }
    }
}
