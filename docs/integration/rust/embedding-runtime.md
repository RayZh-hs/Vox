# Embedding the Runtime

Vox distinguishes between a runtime and a session.

- the runtime owns libraries, compiled artifacts, caches, and handles;
- a session owns interactive state such as imports, definitions, the last
  value shorthand, and any handles retained by those bindings.

This page covers the Rust-side setup path for both embedded and attached
runtime usage.

## Programmatic Setup

In-process setup:

```rust
use vox_runtime::{EmbeddedRunner, InteractiveSession, RuntimeRunner};

let runner = EmbeddedRunner::default();
runner.mount_library(manifest)?;

let mut session = InteractiveSession::new(runner.clone())?;
session.evaluate_submission("import geometry;")?;
```

For a REPL frontend:

```rust
use vox_repl::ReplSession;

let session = ReplSession::with_runner(runner);
```

## Attached Setup

When using a long-lived runtime daemon, clients connect through a runner that
implements the same `RuntimeRunner` trait.

```rust
let runner = RemoteRunner::connect("127.0.0.1:4545")?;
let session = ReplSession::with_runner(runner);
```

The session API does not care whether the runner is embedded or attached.

## Session Semantics

- multiple sessions may talk to one runtime;
- multiple clients may attach to one session and share that session's
  interactive source state;
- separate sessions do not share interactive source state;
- sessions do share mounted libraries, runtime handles, and caches;
- resetting one session does not destroy another session's definitions;
- disconnecting one client does not by itself destroy a durable shared session;
- releasing a handle affects runtime-owned lifetime, not session syntax state.
