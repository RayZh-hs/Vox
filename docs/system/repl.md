# Vox REPL

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

Embedded mode is best for a self-contained binary. Client mode is best when multiple tools should share one runtime process and one cache.

## Model

The REPL is not the runtime protocol.

Internally, the REPL may keep a growing synthetic script or module-like state and recompile it incrementally. That is a tool concern, not a runtime concern.

These actions should map onto compiler or runtime library calls, not expand the runtime protocol.

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
- display large values through summaries and previews rather than full serialization.

## Design Rules

- keep REPL concerns out of `vox-runtime`;
- prefer library calls over protocol-only implementations;
- do not make REPL state a protocol concept;
- optimize for fast edit-check-run cycles.
