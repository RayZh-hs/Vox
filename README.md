<p align="center">
  <img src="images/vox-logo.png" alt="Vox logo" width="160">
</p>

<h1 align="center">Vox</h1>

<p align="center">
  A high-performance, data-flow friendly programming language.
</p>

<p align="center">
  <a href="https://rayzh-hs.github.io/Vox/">Documentation</a>
  &middot;
  <a href="https://rayzh-hs.github.io/Vox/language/">Language Guide</a>
  &middot;
  <a href="https://rayzh-hs.github.io/Vox/spec/">Specification</a>
  &middot;
  <a href="https://rayzh-hs.github.io/Vox/system/runtime/protocol.html">Runtime Protocol</a>
</p>

## What is Vox?

Vox is an experimental language for programs that need concise scripting,
predictable execution, and efficient integration with long-lived host runtimes.
It is designed around reusable packages, executable scripts, explicit host
integration, and optimization paths for both interpreted and compiled execution.

The project is currently built as a Rust workspace. The compiler, runtime, REPL,
LSP server, and Rust `.voxlib` authoring SDK are developed together in this
repository.

## Current Scope

The Vox Ecosystem currently includes:

- A beautifully designed programming language with strong static typing, type inference, and modern fluent syntax.
- A runtime for interpreting and executing compiled Vox code;
- A full-featured compiler suite for parsing, semantic analysis, MIR lowering, optimization, and WASM backend;
- A REPL that can run with an embedded runtime or attach to a shared runtime for debugging;
- A feature-rich LSP server for IDE integration, providing diagnostics, hover, go-to-definition, etc.;
- Rust integration crates for authoring external `.voxlib` packages, more languages coming soon.

The Vox language is still under active development. Expect changes to the public surface while Vox is in alpha.

## Quick Start

Clone the repository and build the workspace:

```sh
git clone https://github.com/RayZh-hs/Vox.git
cd Vox
cargo check --workspace
```

Run the REPL with an embedded runtime:

```sh
cargo run -p vox-repl
```

Or start a shared runtime and attach a REPL session to it:

```sh
cargo run -p vox-runtime -- --listen 127.0.0.1:4545
cargo run -p vox-repl -- --connect 127.0.0.1:4545
```

To attach to a named or existing remote session, append `@name` or `@id`:

```sh
cargo run -p vox-repl -- --connect 127.0.0.1:4545@shared --new
```

## Documentation

The main documentation is published with GitHub Pages:

- [Vox Documentation](https://rayzh-hs.github.io/Vox/)
- [Language Guide](https://rayzh-hs.github.io/Vox/language/)
- [Language Specification](https://rayzh-hs.github.io/Vox/spec/)
- [Compiler Notes](https://rayzh-hs.github.io/Vox/system/compiler/)
- [Runtime Notes](https://rayzh-hs.github.io/Vox/system/runtime/)
- [Runtime Protocol](https://rayzh-hs.github.io/Vox/system/runtime/protocol.html)
- [LSP Notes](https://rayzh-hs.github.io/Vox/system/lsp/)
- [Rust Integration](https://rayzh-hs.github.io/Vox/integration/rust/)

The source for the book lives in [`docs/`](docs/). To build or preview it
locally:

```sh
mdbook build docs
mdbook serve docs
```

## Testing

Run the compiler test suite:

```sh
cargo test
```

## Workspace Layout

- [`crates/vox-core`](crates/vox-core): shared language data model, types, MIR,
  builtins, and host registry.
- [`crates/vox-compiler`](crates/vox-compiler): frontend (parsing, semantic
  analysis), MIR lowering, optimization, and WASM backend. Exports a CLI binary
  (`vox-compiler`) for compiling `.vox` sources to `.voxlib` libraries.
- [`crates/vox-runtime`](crates/vox-runtime): long-lived runtime with MIR and
  WASM executors, tree-walk interpreter, package mounting, artifact storage, and
  runtime protocol server. Exports a binary for running a shared runtime server.
- [`crates/vox-repl`](crates/vox-repl): interactive REPL shell for evaluating
  Vox code, with line editing, completions, and type inspection.
- [`crates/vox-lsp`](crates/vox-lsp): language server providing diagnostics,
  hover, go-to-definition, and best-effort matching.
- [`integrations/rust/crates/voxlib-sdk`](integrations/rust/crates/voxlib-sdk):
  Rust SDK for defining external Vox libraries with structs, functions, and
  traits.
- [`integrations/rust/crates/voxlib-macros`](integrations/rust/crates/voxlib-macros):
  procedural macros (`VoxExport`, `vox_fn`) used by the Rust SDK.

## License

Vox is licensed by either:

- [MIT](LICENSE-MIT)
- [Apache-2.0](LICENSE-APACHE)

At your option.
