# REPL

`vox-repl` is the human-facing interactive tool for Vox.

It is separate from both `vox-compiler` and `vox-runtime`.

## Role

The REPL exists for interactive authoring, quick evaluation, and debugging.

It should feel simple:

- enter code;
- inspect values and types;
- reload code quickly;
- keep useful state across commands.

## Architecture

`vox-repl` may run in two modes:

- embedded mode: link compiler + runtime directly into one process;
- client mode: attach to a `vox-runtime` daemon.

Embedded mode is best for a self-contained binary. Client mode is best when
multiple tools should share one runtime process, one cache, and optionally one
interactive session.

## Model

The REPL is not the runtime protocol.

The REPL may present the user with a growing synthetic script or module-like
state, but the durable interactive environment should live in a runtime-managed
session rather than only inside the REPL process.

These actions should map onto runtime-owned session and runner calls, not
expand the runtime protocol with REPL-only UI details or reimplement parsing
logic in the REPL.

## Commands

Everything that does not start with `:` is treated as Vox input.

The REPL should support a small command set:

### General Commands

- `:help`: show available commands.
- `:quit`: exit the REPL.

### Environment Manipulation

- `:reset`: clear interactive state.
- `:clear`: clear the screen.
- `:env`: show visible imports, bindings, and functions.
- `:snapshot <name>`: save the current state under a name for later retrieval, overwriting existing snapshots if exists. snapshots are stored under `/tmp/vox-repl/snapshots` on Unix-like machines, and `%APPDATA%\\vox-repl\\snapshots` on Windows by default.
- `:restore <name>`: restore a previously saved snapshot by name, replacing the current state. if the snapshot does not exist, an error message is shown.
- `:drop <name>`: remove a binding or definition from interactive state.

### Running Scripts

- `:run <file>`: run a script file in the current state.

### Inspection

- `:show <handle | handle-id>`: show lightweight metadata for a handle.
- `:type <expr>`: show the inferred type of an expression.
- `:handles`: list live handles visible to the REPL session.

## Shorthands

The REPL supports the following shorthands:

- `$`: Shorthand for the last evaluated object.
- Arrow keys: navigate command history.
- Tab: auto-complete commands and identifiers.
- Ctrl+C: interrupt long-running evaluations.
- Ctrl+D: exit the REPL.

## Example

```text
>>> import math;
>>> val x = 4.0;
>>> math.sqrt(x)
2.0
>>> :type $
Float
```

## Behavior

The REPL should:

- surface diagnostics clearly;
- preserve prior successful state when a new input fails;
- make `IOpt` the default interactive mode;
- allow switching to `SOpt` for final execution checks;
- reconnect cleanly to a named session when requested;
- make session sharing explicit rather than ambient;
- display large values through summaries and previews rather than full serialization.

## Design Rules

- keep REPL concerns out of `vox-runtime`;
- keep parser, session, and artifact-lifetime mechanics in `vox-runtime`;
- prefer one runtime API that works both in-process and over a future runner protocol;
- treat the runtime session, not the raw socket connection, as the shareable
  unit of interactive state;
- do not make REPL history, completion menus, or other UI state a protocol concept;
- optimize for fast edit-check-run cycles.
