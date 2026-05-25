# REPL

`vox-repl` is the interactive frontend for evaluating Vox code.

It can either:

- run with an embedded runtime in the same process;
- connect to a separate `vox-runtime` server.

## Starting a REPL

Start an embedded REPL thus:

```sh
cargo run -p vox-repl
```

You can also start a shared runtime first, then attach the REPL to it:

```sh
cargo run -p vox-runtime -- --listen 127.0.0.1:4545
cargo run -p vox-repl -- --connect 127.0.0.1:4545
```

To attach to an existing remote session, append `@session`:

```sh
cargo run -p vox-repl -- --connect 127.0.0.1:4545@shared
cargo run -p vox-repl -- --connect 127.0.0.1:4545@12
```

Use `--new` with a named target to create it if it does not already exist:

```sh
cargo run -p vox-repl -- --connect 127.0.0.1:4545@shared --new
```

## What the REPL Owns

The REPL owns terminal interaction only:

- line editing;
- history;
- completion UI;
- command parsing;
- snapshot files.

The runtime owns execution and interactive state:

- bindings;
- functions;
- imports;
- the `$` last value;
- retained handles.

## Current Session Behavior

Every `ReplSession` opens one runtime session and evaluates all user input
inside it.

The current CLI behavior is:

- embedded mode opens a fresh anonymous session;
- `--connect host:port` opens a fresh anonymous session on the remote runtime;
- `--connect host:port@name` attaches to an existing named session;
- `--connect host:port@id` attaches to an existing session by id;
- `--connect host:port@name --new` attaches to that named session or creates it
  if it does not exist.

Session ids are numeric. If the token after `@` is all digits, the REPL treats
it as a session id.

## Entering Code

Any line that does not start with `:` is treated as Vox code.

Examples:

```text
>>> val x = 4;
>>> x + 1
5
>>> :type x
Int
```

The REPL preserves prior successful state when a later submission fails.

## REPL Commands

- `:help` shows the command list.
- `:quit` exits the REPL.
- `:reset` clears the current interactive session state.
- `:clear` clears the terminal screen.
- `:env` prints visible imports, bindings, and functions.
- `:snapshot <name>` saves the current session source to a local snapshot file.
- `:restore <name>` replaces the current session state with a snapshot file.
- `:run <file>` runs a Vox script file in the current session context.
- `:show <handle>` prints a lightweight summary for a handle id.
- `:type <expr>` prints the inferred type of an expression.
- `:handles` lists live handles visible through the runtime.
- `:drop <name>` removes a binding or definition from the session.
- `:xopt <mode>` sets the session default optimization mode to `NOpt`, `IOpt`,
  or `SOpt`.
- `:session connect <id-or-name>` switches to an existing session.
- `:session new [name]` creates a fresh anonymous or named session and switches
  to it.
- `:session reserve` toggles whether the current session is kept when its
  endpoint count reaches zero.
- `:session list` shows all live sessions, their ids, attachment counts, and
  reserve status.

## Shorthands and Editing

- `$` refers to the last evaluated value.
- Arrow keys move through input history.
- `Tab` completes commands, snapshot names, handles, and visible symbols.
- `Ctrl+C` interrupts the current input line.
- `Ctrl+D` exits the REPL.

## Snapshot Files

Snapshots are stored locally by the REPL process:

- Unix-like systems: `/tmp/vox-repl/snapshots`
- Windows: `%APPDATA%\\vox-repl\\snapshots`

`:snapshot` writes `<name>.vox`.

`:restore` loads that file and replaces the current session state.

This is the current user-facing way to move source-defined interactive state
from one session to another.

## Sharing Data

Different users often mean different things by "share", so the rules are:

- Same session: shared bindings, shared functions, shared `$`, shared retained
  handles.
- Different sessions on the same runtime: separate interactive state.
- Different runtimes: completely separate state.
- A session with zero attached endpoints is recycled unless it has been marked
  as reserved.

From the REPL CLI, the practical sharing options are:

- attach multiple REPLs or tools to the same reserved or still-live session by
  name or id;
- use `:session list` to discover session ids and names;
- use snapshot and restore to copy session source between separate sessions.

There is no REPL command that directly copies a live binding or handle from one
session into another session.
