# REPL

`vox-repl` is the interactive frontend for evaluating Vox code.

It can either:

- run with an embedded runtime in the same process;
- connect to a separate `vox-runtime` server.

## CLI Arguments

```text
vox-repl [OPTIONS] [SCRIPT] [-- SCRIPT_ARGS...]
```

When run without arguments, `vox-repl` starts an interactive REPL session.
When given a script file, it runs the file and prints the trailing expression
value to stderr.

| Flag | Description |
|------|-------------|
| `-i`, `--interactive` | Drop into REPL after running the script |
| `-s`, `--silent` | Suppress stderr output of trailing expressions |
| `--connect ADDR` | Connect to a remote runtime (`host:port[@session]`) |
| `--new` | Create session if missing (requires `--connect`) |
| `-h`, `--help` | Show help message |

Script arguments after `--` are converted to Vox values and passed as
positional parameters to the script:

| Input | Vox type |
|-------|----------|
| Integer literal | `Int` |
| Float literal | `Float` |
| `true` / `false` | `Bool` |
| `null` | `Null` |
| Everything else | `String` |

### Examples

```sh
# Run a script, printing its result to stderr
vox-repl hello.vox

# Run a script, then drop into the REPL
vox-repl -i hello.vox

# Run silently (no trailing expression output), then REPL
vox-repl -i -s hello.vox

# Pass arguments to a parameterised script
vox-repl greet.vox -- "Alice" 42

# Connect to a remote runtime
vox-repl --connect 127.0.0.1:4545@shared
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

Interactive submissions follow script top-level rules. Values and statements
are processed in submission order. Function headers from the current submission
are visible throughout that submission, and function headers from earlier
successful submissions remain visible in later submissions. When a later
submission redefines a function, that function becomes the active visible
definition for the session.

In practice this means:

- value references must already resolve inside the current session or earlier in
  the same submitted chunk;
- functions may call each other when they are entered in the same compilation
  chunk;
- a value initializer such as `val x = foo() + bar();` may use functions from
  earlier chunks or functions declared in the current chunk;
- if a function signature changes, direct function callers must be resubmitted
  in the same chunk.

Assignments entered at the prompt are statements:

```text
>>> var a = 1;
>>> a = 2;
>>> a
2
```

For coupled changes, prefer `:chunk`, `:edit`, `:run`, or an external file.

## REPL Commands

- `:help` shows the command list.
- `:quit` exits the REPL.
- `:reset` clears the current interactive session state.
- `:clear` clears the terminal screen.
- `:env` prints visible imports, bindings, and functions.
- `:chunk` opens an editor for a new multi-definition chunk.
- `:edit <symbol>...` opens stored definitions together for resubmission as one
  chunk.
- `:snapshot <name>` saves the current session source to a local snapshot file.
- `:restore <name>` replaces the current session state with a snapshot file.
- `:run <file>` runs a Vox script file in the current session context.
- `:show <handle>` prints a lightweight summary for a handle id.
- `:type <expr>` prints the inferred type of an expression.
- `:handles` lists live handles visible through the runtime.
- `:drop <name>` removes a binding or definition from the session.
- `:opt get [object]` prints optimization state for the module and functions.
- `:opt set <mode> [object]...` sets the session default mode, or forces the
  mode for `module` and named functions. Modes are `NOpt`, `IOpt`, and `SOpt`.
- `:opt dump [object]` prints a MIR dump for `module` or a function when an
  optimized artifact exists. Prefix the object with `wasm:` to dump module wasm
  bytes, for example `:opt dump wasm:module`.
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

`:chunk` and `:edit` choose an editor as follows:

- if `VOX_EDITOR=builtin`, use the builtin multiline editor;
- otherwise if `VOX_EDITOR` is set, run that command;
- otherwise if `EDITOR` is set, run that command;
- otherwise fall back to the builtin multiline editor.

The builtin editor is a simple replacement editor:

- it shows the current chunk, if any;
- it asks for the full replacement chunk;
- `.submit` commits the chunk;
- `.cancel` abandons it.

`:opt dump` prints the dump to the REPL by default. If `VOX_VIEWER` is set, the
REPL writes the dump to a temporary read-only text file and runs that viewer
command with the file path.

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
