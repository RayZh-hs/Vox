# Runtime

`vox-runtime` is the long-lived execution system for Vox.

It loads compiled Vox plans, executes them, caches pure results, tracks `Econ`
versions, and owns runtime handles for large host values.

Language semantics are defined in [the language overview](../../language/overview.md).

## Role

`vox-runtime` sits below authoring tools and above host libraries.

- `vox-compiler` performs semantic lowering and most IR optimizations.
- `vox-runtime` interprets optimized plans.
- `vox-runtime` owns compiled artifact caches, result caches, handles, and `Econ`.
- `vox-repl` is a separate client that may link the system directly or attach to the runtime process.

This supports two deployment modes:

- in-process: a program links compiler + runtime and ships as one binary;
- out-of-process: a program attaches to a shared runtime daemon.

## Responsibilities

`vox-runtime` is responsible for:

- mounting libraries;
- loading and reloading scripts;
- exposing a uniform runner API for embedded and attached clients;
- selecting `NOpt`, `IOpt`, or `SOpt`;
- executing compiled plans with arguments;
- memoizing pure evaluation;
- running `evil` work on demand;
- managing handles for large host values;
- tracking and refreshing `Econ` snapshots;
- reporting lightweight handle metadata;
- shutting down cleanly.

Attached instances own their own client state. The runtime owns the shared
compiled artifacts, caches, host library mounts, and handle table.

## Execution Model

`vox-runtime` does not interpret source code directly. It executes compiled
plans produced by `vox-compiler`.

The flow is:

1. load libraries and host packages;
2. compile or reload a script through `vox-compiler`;
3. store the compiled artifact;
4. run the compiled plan with arguments;
5. reuse cached pure subresults when valid.

Pure cache validity depends on:

- script revision;
- library revisions;
- optimization mode;
- input identity or hash;
- referenced `Econ` versions.

`evil` evaluation is explicit and never enters the pure cache.

## Optimization Modes

- `NOpt`: correctness only.
- `IOpt`: low latency, stable caches, minimal recompilation.
- `SOpt`: sealed execution, more aggressive storage reuse and scheduling.

Most optimization happens in `vox-compiler`. Runtime-specific reuse decisions,
such as when a large value can be moved or storage can be recycled, happen
inside `vox-runtime` and depend on the selected mode.

The sealed `SOpt` plan is expected to be wasm-oriented:

- scalars stay in wasm locals where possible;
- large values travel as runtime handles;
- aggregate uses are annotated as borrow or consume;
- tuple and record fields may stay split until a full runtime value must be
  materialized.

## Protocol

When exposed as a daemon, `vox-runtime` should use a compact binary protocol.
The full wire contract is defined in [Protocol](./protocol.md).

Design rules:

- one connection represents one attached instance;
- one request yields one response;
- object ids on the wire are integers, not strings;
- large values travel as handles rather than serialized payloads;
- REPL history, completions, and synthetic cell assembly stay out of the
  runtime boundary.

## Handles

Small values may be returned inline. Large host values must be returned as opaque handles.

The runtime owns handle lifetime. Clients may:

- inspect lightweight metadata;
- release handles explicitly.

The runtime must not serialize large host values by default.

## Host Integration

Host libraries should provide:

- type metadata;
- function signatures;
- purity metadata;
- a stable call boundary for execution.

Shared objects are the preferred extension model when cross-language host support is needed. The plugin boundary should be a versioned C ABI.

`vox-runtime` should call compiled host functions through registered adapters. It should not require host libraries to ship LLVM IR.

## Invariants

- pure Vox code cannot observe mutation of host values;
- large values keep value semantics through handles;
- `evil` work is explicit;
- `Econ` refresh invalidates dependent pure results;
- unused pure fields of sealed record or tuple producers may be omitted from
  execution entirely;
- `SOpt` may change reuse and scheduling, but not semantics.
