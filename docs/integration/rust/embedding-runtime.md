# Embedding the Runtime

This page shows how Rust code uses the Vox runtime directly.

The important split is:

- `RuntimeRunner` gives you a transport-neutral way to talk to a runtime;
- `InteractiveSession` gives you an interactive workspace inside that runtime.

The same session API works with both embedded and remote runners.

## Embedded Use

Create a runtime in-process and open a fresh anonymous session:

```rust
use vox_runtime::{EmbeddedRunner, InteractiveSession};

let runner = EmbeddedRunner::default();
let mut session = InteractiveSession::new(runner.clone())?;

session.eval("val answer = 42;")?;
let value = session.eval("answer")?;
```

Use this when one program owns the runtime and does not need a separate server
process.

## Remote Use

Connect to a long-lived runtime server:

```rust
use vox_runtime::{InteractiveSession, RemoteRunner};

let runner = RemoteRunner::connect("127.0.0.1:4545")?;
let mut session = InteractiveSession::new(runner)?;
```

This opens a fresh anonymous session on that runtime.

## Named Sessions

Named sessions are the mechanism for shared interactive state.

```rust
use vox_runtime::{InteractiveSession, RemoteRunner, SessionSelector};

let runner = RemoteRunner::connect("127.0.0.1:4545")?;
let mut shared = InteractiveSession::named(runner, "shared")?;

shared.eval("val answer = 42;")?;
```

If another client opens `"shared"` on the same runtime, it attaches to the same
interactive workspace and can read or extend those bindings.

To attach to an existing session by id instead of by name:

```rust
use vox_core::ids::SessionId;
use vox_runtime::{InteractiveSession, RemoteRunner, SessionSelector};

let runner = RemoteRunner::connect("127.0.0.1:4545")?;
let mut session = InteractiveSession::attach(runner, SessionSelector::Id(SessionId(12)))?;
```

## Session Rules

- `InteractiveSession::new(...)` creates a fresh anonymous session.
- `InteractiveSession::named(..., "name")` reopens the same session when the
  name already exists.
- `InteractiveSession::attach(..., selector)` attaches to an existing session
  and fails when it does not exist.
- `InteractiveSession::create_named(..., "name")` creates a fresh named
  session and fails when the name already exists.
- Different session names are isolated from one another.
- Sessions on the same runtime still share runtime-level resources such as
  mounted libraries and live handles.
- An unreserved session is recycled when its attached endpoint count reaches
  zero.

## Sharing Data

Choose the sharing model based on what you need:

- Shared interactive workspace: use one named session.
- Isolated workspaces with copied source state: export session source with
  `snapshot_source()` and import it with `restore_snapshot_source()`.
- Handle-backed large values: keep the handle, then fetch serializable data
  later with `get_handle_data()` or stream it in chunks with
  `read_handle_data()`.

There is currently no higher-level API that copies a live binding directly from
one session into another separate session.

## Reading Handle-Backed Data

Large serializable values may cross the runtime boundary as handles. You can
materialize the value later without re-running the original submission.

```rust
use vox_core::value::HandleData;
use vox_core::value::RuntimeValue;
use vox_runtime::{EmbeddedRunner, InteractiveSession};

let runner = EmbeddedRunner::default();
let mut session = InteractiveSession::new(runner)?;

let result = session.eval("[40, 41, 42]")?.expect("list result");
let RuntimeValue::Handle(handle) = result else {
    panic!("expected a handle-backed list");
};

let data = session.get_handle_data(handle)?;
assert_eq!(
    data,
    HandleData::List(vec![
        HandleData::Int(40),
        HandleData::Int(41),
        HandleData::Int(42),
    ])
);
```

For very large payloads, read incrementally:

```rust
let first_chunk = session.read_handle_data(handle, 0, 64 * 1024)?;
```

`get_handle_data()` is the eager convenience API. `read_handle_data()` is the
chunked API that remote clients can use to stay within protocol payload limits.
