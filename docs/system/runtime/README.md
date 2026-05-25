# Runtime

`vox-runtime` is the long-lived execution service for Vox.

It owns:

- mounted host libraries;
- compiled script artifacts;
- runtime handles for large values;
- interactive sessions.

It can run in-process through `EmbeddedRunner` or as a TCP server through the
`vox-runtime` binary.

## Start the Runtime

Run a shared runtime server with:

```sh
cargo run -p vox-runtime -- --listen 127.0.0.1:4545
```

`--listen` is optional. If omitted, the server listens on `127.0.0.1:4545`.

The runtime prints the address it bound to and then waits for client
connections.

## Connect a Client

The REPL can connect to that runtime with:

```sh
cargo run -p vox-repl -- --connect 127.0.0.1:4545
```

Programs can also connect directly through `RemoteRunner`.

To attach to a specific session from the REPL:

```sh
cargo run -p vox-repl -- --connect 127.0.0.1:4545@shared
cargo run -p vox-repl -- --connect 127.0.0.1:4545@12
```

Use `--new` with a named target to create it when missing:

```sh
cargo run -p vox-repl -- --connect 127.0.0.1:4545@shared --new
```

## Runtime and Session

The runtime and the session are different objects.

- The runtime is the shared process. It stores libraries, compiled artifacts,
  live handles, and caches.
- A session is an interactive workspace inside that runtime. It stores imports,
  definitions, the last-value binding behind `$`, and any handles retained by
  those bindings.

When a client opens a session, all later evaluation happens inside that
session.

## Session Kinds

The runtime supports two kinds of sessions:

- Anonymous session: always creates a new interactive workspace.
- Named session: reopens the same interactive workspace when another client
  uses the same name.

Named sessions are how multiple clients share one interactive environment.

Sessions also have two lifecycle states:

- attached: one or more client endpoints are currently using the session;
- reserved: the session is kept even when the attached endpoint count reaches
  zero.

An unreserved session is recycled as soon as its attached endpoint count drops
to zero.

## Programmatic Session Use

Embedded use:

```rust
use vox_runtime::{EmbeddedRunner, InteractiveSession};

let runner = EmbeddedRunner::default();
let mut session = InteractiveSession::new(runner)?;
session.evaluate_submission("val answer = 42;")?;
```

Remote use with a shared named session:

```rust
use vox_runtime::{InteractiveSession, RemoteRunner};

let runner = RemoteRunner::connect("127.0.0.1:4545")?;
let mut session = InteractiveSession::named(runner, "shared")?;
session.evaluate_submission("val answer = 42;")?;
```

If another client opens `"shared"` on the same runtime, it sees the same
interactive state.

## What Is Shared

Clients attached to the same runtime share:

- mounted libraries;
- compiled artifacts;
- the runtime handle store;
- runtime-wide caches;
- any interactive state inside the same named session.

Clients in different sessions do not share:

- bindings;
- function definitions entered interactively;
- the `$` value;
- `:reset` effects.

## Sharing Data

There are two supported ways to share data today.

### 1. Share one named session

If multiple clients should see the same bindings and definitions, they must
attach to the same named session. This is the direct sharing model.

Anonymous sessions can also be shared by id while they are still live, or after
they have been marked as reserved.

### 2. Copy source state between sessions

If the sessions must stay separate, copy the session source with snapshot and
restore operations. In the REPL this is exposed as `:snapshot` and `:restore`.

This copies source-defined interactive state. It does not move a live session
binding from one session to another inside the runtime.

## Handles

Large values cross the runtime boundary as handles instead of full serialized
payloads.

Clients can:

- receive a handle as an evaluation result;
- inspect a handle summary;
- retain or release a handle through the runner API.

The runtime owns actual handle lifetime and storage.

## Session Management

At the API and protocol level, clients can:

- create anonymous sessions;
- attach to sessions by id;
- attach to sessions by name;
- create named sessions on demand;
- list live sessions;
- mark a session as reserved or unreserved.

The REPL exposes these through `--connect host:port@session`, `--new`, and the
`:session` command family.
