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

Vox currently includes:

- a source frontend for package and script units;
- semantic checks for names, imports, public values, functions, and package
  manifests;
- runtime mounting for `.vox` packages and `.voxlib` libraries;
- a REPL that can run with an embedded runtime or attach to a shared runtime;
- a binary runtime protocol for external clients;
- an LSP server for editor diagnostics;
- Rust integration crates for authoring external `.voxlib` packages.

The parser, type system, optimizer, and runtime are still under active
development. Expect the public surface to change while Vox is in alpha.

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

## Workspace Layout

- [`crates/vox-core`](crates/vox-core): shared language and runtime data model.
- [`crates/vox-compiler`](crates/vox-compiler): frontend, semantic analysis,
  package manifests, and backend lowering.
- [`crates/vox-runtime`](crates/vox-runtime): long-lived runtime state, package
  mounting, artifact storage, host calls, and runtime protocol support.
- [`crates/vox-repl`](crates/vox-repl): interactive shell for evaluating Vox
  code.
- [`crates/vox-lsp`](crates/vox-lsp): language server for editor diagnostics.
- [`integrations/rust/crates/voxlib-sdk`](integrations/rust/crates/voxlib-sdk):
  Rust SDK for defining external Vox libraries.
- [`integrations/rust/crates/voxlib-macros`](integrations/rust/crates/voxlib-macros):
  procedural macros used by the Rust SDK.

## License

Vox is licensed by either:

- [MIT](LICENSE-MIT)
- [Apache-2.0](LICENSE-APACHE)

At your option.
