# Protocol

The runtime protocol is the binary attach boundary for external instances such
as REPL clients, editors, batch workers, and test harnesses.

This document defines the wire format closely enough to implement both ends of
the connection without an additional schema layer.

## Scope

The protocol exists to:

- attach one client instance to a long-lived `vox-runtime`;
- open or attach that client to a runtime-managed interactive session;
- load, reload, run, and unload script artifacts for that instance;
- move arguments and results across the boundary;
- manage runtime-owned handles for large values;
- expose runtime cache and `Econ` maintenance operations.

The protocol does not model REPL history, completion menus, or client-side
synthetic source assembly. Those stay in the client.

The transport connection and the interactive session are distinct concepts. A
connection is the ordered byte stream used by one attached client. A session is
the durable shareable environment that may later be revisited or shared with
other clients.

## Connection Model

- transport: any ordered byte stream such as a Unix socket or TCP connection;
- endianness: little-endian for all fixed-width integers and floats;
- lifetime: one connection equals one attached client instance, not the entire
  lifetime of a shared interactive session;
- concurrency: the client may pipeline requests and match responses by
  `request_id`;
- isolation: script ids and mutable interactive bindings should ultimately be
  scoped to a runtime session rather than ambiently shared across all clients;
- sharing: library mounts, caches, `Econ` state, and handle storage are owned
  by the runtime and may be shared across connections;
- disconnect: dropping the connection should release connection-owned
  references, but should not destroy a durable shared session by itself.

Later protocol milestones should add explicit session lifecycle operations so
shared interactive state is attached deliberately instead of being inferred from
the socket.

The first frame on every connection must be `HELLO`.

## IPC Model

The runtime protocol is the IPC surface for Vox tools that share one runtime.

Normal same-runtime transfer should use these methods:

- inline copy for small serializable values;
- handle passing for large or opaque runtime-owned values;
- callable references for functions, compiled entry points, and retained
  closures that the runtime can represent safely;
- automatic runtime cache reuse instead of explicit client-to-client cache copy.

Cross-runtime movement is different from same-runtime IPC. It should use
explicit export/import operations and versioned bundles rather than raw handle
reuse.

## Frame Format

Every message begins with this fixed 24-byte header:

```text
offset  size  field
0       4     magic      = 0x56585254  // "VXRT"
4       2     version
6       1     kind
7       1     opcode
8       4     flags
12      4     request_id
16      4     target_id
20      4     payload_len
```

Rules:

- `version` is `0` on the initial `HELLO` request and the selected protocol
  version on every later frame;
- `request_id` is chosen by the client for requests and copied by the server
  into the matching response;
- `target_id` is `0` when the opcode does not act on an existing object;
- `payload_len` may be `0`;
- after the header, exactly `payload_len` bytes follow.

`kind` values:

- `0`: request
- `1`: success response
- `2`: error response
- `3`: event

`flags` are a bitset:

- `0x0000_0001`: payload contains diagnostics
- `0x0000_0002`: payload contains an inline value
- `0x0000_0004`: payload contains a handle result

All other bits are reserved and must be sent as `0`.

## Opcodes

`opcode` is a one-byte enum:

- `0x01`: `HELLO`
- `0x02`: `PING`
- `0x10`: `MOUNT_LIBRARY`
- `0x11`: `UNMOUNT_LIBRARY`
- `0x20`: `LOAD_SCRIPT`
- `0x21`: `RELOAD_SCRIPT`
- `0x22`: `UNLOAD_SCRIPT`
- `0x23`: `SET_XOPT`
- `0x24`: `RUN_SCRIPT`
- `0x30`: `RETAIN_HANDLE`
- `0x31`: `DESCRIBE_HANDLE`
- `0x32`: `RELEASE_HANDLE`
- `0x40`: `REFRESH_ECON`
- `0x41`: `CACHE_STATS`
- `0x42`: `CLEAR_CACHE`
- `0x7f`: `SHUTDOWN`

The server must reject unknown opcodes with `ERR_UNSUPPORTED_OPCODE`.

## Primitive Encodings

The protocol uses only these primitive encodings:

- `u8`, `u16`, `u32`, `u64`
- `i64`
- `f64`
- `bytes`: `u32 len` followed by `len` raw bytes
- `string`: `bytes` containing UTF-8

There is no map or self-describing object envelope at the frame level.

## Value Encoding

Arguments and inline results use the `Value` encoding below:

```text
tag: u8
payload: tag-specific
```

Tags:

- `0x00`: `null`
- `0x01`: `bool` followed by `u8` (`0` or `1`)
- `0x02`: `int` followed by `i64`
- `0x03`: `float` followed by `f64`
- `0x04`: `string`
- `0x05`: `tuple` followed by `u32 count`, then `count` encoded values
- `0x06`: `record` followed by `u32 field_count`, then repeated `string name`
  plus encoded value
- `0x07`: `handle` followed by `u32 handle_id`

Encoding rules:

- values smaller than the negotiated inline limit should be sent inline;
- large host values must be returned as `handle`;
- a client may send a previously received `handle` value back as an argument;
- when a result does not fit the inline limit, the runtime should prefer
  returning a `handle` over copying the value into the response.
- inline values are copy-transferred, not shared by later mutation.

The protocol deliberately avoids textual field names outside inline records.

Function transfer rules:

- functions should normally cross the process boundary as callable references,
  not raw executable blobs;
- top-level functions and compiled script entry points should be addressable by
  runtime-issued callable ids or by symbol plus revision metadata;
- closures may only be transferred when the runtime can retain their captured
  environment safely as a runtime-owned callable object.

## Diagnostics

Compilation and runtime failures may carry diagnostics. A diagnostic block is:

```text
u32 count
repeat count times:
  u8 severity        // 0=error, 1=warning, 2=note
  string code
  string message
  string source_name // empty when unavailable
  u32 start_byte
  u32 end_byte
```

Diagnostics are optional on success and recommended on compile failures.

## Handshake

`HELLO` is mandatory and must be the first request.

The `HELLO` request must use header `version = 0`. The `HELLO` response must
return the selected version both in the header and in the payload.

Request payload:

```text
u16 min_version
u16 max_version
u32 client_caps
u32 max_inline_value_bytes
```

Response payload:

```text
u16 selected_version
u16 reserved
u32 server_caps
u32 instance_id
u32 max_payload_bytes
u32 max_inline_value_bytes
```

Handshake rules:

- the server selects one version within the requested range;
- if there is no overlap, the server replies with `ERR_VERSION_MISMATCH`;
- `instance_id` identifies the attached instance in logs and metrics only;
- both sides must honor the smaller of the client and server inline limits.

## Operation Payloads

This section defines the exact payload for each opcode. `target_id` in the
frame header identifies the object being acted on when required.

### `PING`

Request payload: empty.

Success response payload:

```text
u64 runtime_uptime_ms
```

### `MOUNT_LIBRARY`

Request payload:

```text
u8 source_kind       // 0=filesystem path, 1=manifest bytes, 2=bundle bytes
u8 reserved[3]
bytes source
```

Success response payload:

```text
u32 library_id
u64 library_revision
```

### `UNMOUNT_LIBRARY`

`target_id` is `library_id`.

Request payload: empty.

Success response payload: empty.

### `LOAD_SCRIPT`

`target_id` must be `0`.

Request payload:

```text
u8 source_kind       // 0=source text, 1=precompiled artifact
u8 default_xopt      // 0=NOpt, 1=IOpt, 2=SOpt
u8 reserved[2]
string logical_path
bytes source
```

Success response payload:

```text
u32 script_id
u64 script_revision
u32 parameter_count
u8 result_is_handle_capable
```

If compilation produces diagnostics but still yields a runnable artifact, the
server may return success with the diagnostics flag set.

### `RELOAD_SCRIPT`

`target_id` is `script_id`.

Request payload matches `LOAD_SCRIPT`.

Success response payload:

```text
u64 script_revision
u32 parameter_count
u8 result_is_handle_capable
```

### `UNLOAD_SCRIPT`

`target_id` is `script_id`.

Request payload: empty.

Success response payload: empty.

### `SET_XOPT`

`target_id` must be `0`.

Request payload:

```text
u8 default_xopt      // 0=NOpt, 1=IOpt, 2=SOpt
u8 reserved[3]
```

Success response payload: empty.

Current runtime behavior:

- `SET_XOPT` updates the connection default used by later `LOAD_SCRIPT` and
  `RELOAD_SCRIPT` requests;
- `RUN_SCRIPT` currently supports only `xopt_override = 255`, because
  execution uses the optimization mode compiled into the loaded artifact.

### `RUN_SCRIPT`

`target_id` is `script_id`.

Request payload:

```text
u8 xopt_override     // 255=use script default, else 0/1/2
u8 reserved[3]
u32 arg_count
Value args[arg_count]
```

Success response payload:

- when the result is inline: one encoded `Value`;
- when the result is large: `u32 handle_id`.

The server must return exactly one result value.

### `RETAIN_HANDLE`

`target_id` is `handle_id`.

Request payload:

```text
u32 extra_refs
```

Success response payload:

```text
u32 handle_id
u32 retained_refs
```

### `DESCRIBE_HANDLE`

`target_id` is `handle_id`.

Request payload: empty.

Success response payload:

```text
u32 handle_id
string type_name
u64 approx_size_bytes
u32 ref_count
u32 handle_flags
string summary
```

`handle_flags` should currently use:

- `0x0000_0001`: pure-serializable
- `0x0000_0002`: externally pinned

### `RELEASE_HANDLE`

`target_id` is `handle_id`.

Request payload:

```text
u32 release_refs     // usually 1
```

Success response payload:

```text
u32 remaining_refs
```

### `REFRESH_ECON`

Request payload:

```text
string econ_key
```

Success response payload:

```text
u64 econ_version
u64 invalidated_cache_entries
```

### `CACHE_STATS`

Request payload: empty.

Success response payload:

```text
u64 artifact_entries
u64 pure_cache_entries
u64 pure_cache_bytes
u64 live_handles
```

### `CLEAR_CACHE`

Request payload:

```text
u8 scope             // 0=all, 1=artifacts, 2=pure-cache
u8 reserved[3]
```

Success response payload:

```text
u64 cleared_entries
```

### `SHUTDOWN`

Request payload: empty.

Success response payload: empty.

Only privileged clients should be allowed to issue this opcode.

## Error Model

An error response uses `kind = 2` and this payload:

```text
u32 error_code
string message
optional diagnostic block
```

Recommended error codes:

- `1`: `ERR_VERSION_MISMATCH`
- `2`: `ERR_BAD_FRAME`
- `3`: `ERR_UNSUPPORTED_OPCODE`
- `4`: `ERR_UNKNOWN_LIBRARY`
- `5`: `ERR_UNKNOWN_SCRIPT`
- `6`: `ERR_UNKNOWN_HANDLE`
- `7`: `ERR_COMPILE_FAILED`
- `8`: `ERR_RUNTIME_FAILED`
- `9`: `ERR_BAD_ARGUMENT`
- `10`: `ERR_PERMISSION_DENIED`

`ERR_BAD_FRAME` should be treated as fatal to the connection.

## Events

Events are optional and never replace the required response to a request.

If implemented, supported events should be:

- `0x80`: `HANDLE_DROPPED` with payload `u32 handle_id`
- `0x81`: `ECON_INVALIDATED` with payload `string econ_key` plus `u64 version`

Clients must ignore unknown event opcodes.

## Performance Rules

- keep the header fixed-width and branch-light to parse;
- do not use JSON, text keys, or per-message schema negotiation;
- prefer integer ids and handle passing over value copying;
- allow request pipelining on one connection;
- keep script ownership connection-local so disconnect cleanup is constant-time.
