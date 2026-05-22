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
- creating, opening, and closing interactive sessions;
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

The runtime owns shared compiled artifacts, caches, host library mounts,
handles, and interactive sessions.

An attached client owns only transport-local UI state such as history,
completion menus, and viewport concerns. A runtime session owns shareable
interactive definitions and retained results.

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

When exposed as a daemon, `vox-runtime` uses a compact binary protocol.
The full wire contract is defined in [Protocol](./protocol.md).

Design rules:

- connections attach clients to the runtime, while sessions hold shareable
  interactive state;
- one request yields one response;
- object ids on the wire are integers, not strings;
- large values travel as handles rather than serialized payloads;
- REPL history, completions, and synthetic cell assembly stay out of the
  runtime boundary.

Session rules:

- multiple clients may attach to one session and therefore share bindings,
  definitions, and retained results;
- separate sessions do not implicitly share mutable interactive state;
- closing a client connection should not by itself destroy a durable session;
- if a session binding refers to a large value handle, the session is
  responsible for retaining that handle until the binding is dropped or the
  session closes.

## Interprocess Communication

`vox-runtime` is the IPC hub for Vox tools.

Different programs should collaborate by attaching to the same runtime and then
using runtime sessions, handles, callable references, and published bindings to
exchange data. Clients should not need direct peer-to-peer transfer logic for
ordinary collaboration.

Preferred IPC methods:

- small pure values: copy inline through the protocol;
- large or opaque values: pass runtime-owned handles;
- functions: pass callable references backed by runtime metadata or compiled
  artifacts;
- caches: reuse runtime-owned entries automatically instead of copying cache
  contents between clients.

Cross-runtime movement is a separate concern. It should use explicit export and
import bundles rather than pretending a live runtime handle can be portable.

## Handles

Small values may be returned inline. Large host values must be returned as opaque handles.

The runtime owns handle lifetime. Clients may:

- inspect lightweight metadata;
- release handles explicitly.

The runtime must not serialize large host values by default.

## Host Integration

Host libraries should provide:

- type metadata, including exported fields;
- trait metadata;
- function signatures;
- lowered method functions;
- purity metadata;
- a stable call boundary for execution.

Shared objects are the preferred extension model when cross-language host
support is needed. The plugin boundary is a versioned C ABI.

`vox-runtime` calls compiled host functions through registered adapters. Host
libraries do not ship LLVM IR.

## Invariants

- pure Vox code cannot observe mutation of host values;
- large values keep value semantics through handles;
- `evil` work is explicit;
- `Econ` refresh invalidates dependent pure results;
- unused pure fields of sealed record or tuple producers may be omitted from
  execution entirely;
- `SOpt` may change reuse and scheduling, but not semantics.
