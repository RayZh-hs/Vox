# Vox

Rust workspace scaffold for the Vox compiler, runtime, and REPL described in `docs/`.

## Workspace Layout

- `crates/vox-core`: shared language/runtime data model.
- `crates/vox-compiler`: staged compiler entry points and source front-end.
- `crates/vox-runtime`: long-lived runtime state, artifact storage, and handle lifecycle.
- `crates/vox-repl`: interactive command parsing and REPL session model.

## Current Scope

This scaffold establishes the crate boundaries, core types, and a minimal end-to-end control flow for:

- classifying Vox source units as packages or scripts;
- extracting script parameters at the surface level;
- compiling source into a typed artifact shell;
- loading artifacts into a runtime;
- driving that runtime from a REPL session model;
- attaching external clients to a long-lived runtime over the binary protocol.

The full parser, type checker, and the remaining `Vox Core` IR/runtime features
are still intentionally incremental rather than being filled with low-value
placeholder code.

## Documentation

Project documentation is published as an `mdBook` rooted at `docs/`.

Use:

```sh
mdbook build docs
mdbook serve docs
```

The book groups:

- language overview material under `docs/language/`;
- the normative language specification under `docs/spec/`;
- system design notes under `docs/system/`.
