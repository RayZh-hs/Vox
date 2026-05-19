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

- `:help`: show available commands.
- `:quit`: exit the REPL.
- `:reset`: clear interactive state.
- `:list`: show visible imports, bindings, and functions.
- `:type <expr>`: show the inferred type of an expression.
- `:purity <expr>`: show whether an expression is pure or `evil`.
- `:load <file>`: load a Vox file into the current state, checking for conflicts ahead of time to prevent partial state updates.
- `:reload`: reload the last loaded file.
- `:run [name] [args...]`: run the current script or a named script with arguments.
- `:handles`: list live handles visible to the REPL session.
- `:show <handle | handle-id>`: show lightweight metadata for a handle.
- `:drop <name>`: remove a binding or definition from interactive state.

## Shorthands

The REPL supports the following shorthands:

- `$`: Shorthand for the last evaluated object.

## Example

```text
>>> import math;
>>> val x = 4.0;
>>> math.sqrt(x)
2.0
>>> :type $
Float
>>> :xopt SOpt
>>> :load demo.vox
>>> :run
<image.Image handle=12>
>>> :show 12
image.Image 1920x1080
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
