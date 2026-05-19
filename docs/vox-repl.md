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

The REPL may also offer convenience commands such as:

- show visible bindings;
- show inferred type;
- reload a file;
- run a script with arguments;
- inspect handles.

These commands should map onto compiler or runtime library calls, not expand the runtime protocol.

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
