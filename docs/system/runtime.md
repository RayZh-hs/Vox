# Vox Runtime

`vox-runtime` is the long-lived execution system for Vox.

It loads compiled Vox plans, executes them, caches pure results, tracks `Econ` versions, and owns runtime handles for large host values.

Language semantics are defined in [the language overview](../language/overview.md).

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
- owning interactive session mechanics used by tools such as the REPL;
- selecting `NOpt`, `IOpt`, or `SOpt`;
- executing compiled plans with arguments;
- memoizing pure evaluation;
- running `evil` work on demand;
- managing handles for large host values;
- tracking and refreshing `Econ` snapshots;
- reporting lightweight handle metadata;
- shutting down cleanly.

Interactive sessions are client-local. The runtime process owns shared compiled
artifacts, handles, and caches, while each attached tool owns its own synthetic
script state and reloads only its own artifact slot.

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

The protocol should stay small.

### Framing

Each frame should contain:

- magic;
- protocol version;
- opcode;
- flags;
- request id;
- payload length;
- payload bytes.

Rules:

- one request yields one response;
- events are optional and never replace the response;
- object ids on the wire are integers, not strings;
- large values are always represented by handles.

### Values

Small values may be encoded inline:

- `int`
- `float`
- `bool`
- `string`
- `tuple`
- `null`

Large values must be represented by `handle_id`.

### Operations

The runtime only needs these operations:

- `hello`: handshake and version check.
- `mount_library`: mount a library root or bundle.
- `unmount_library`: remove a mounted library revision.
- `load_script`: compile and store a script artifact.
- `reload_script`: replace a script with a new revision.
- `unload_script`: release a script artifact.
- `set_xopt`: set default `NOpt`, `IOpt`, or `SOpt` for a script.
- `run_script`: execute a compiled script with arguments.
- `describe_handle`: return lightweight metadata for a handle.
- `release_handle`: drop one handle reference.
- `refresh_econ`: refresh an `Econ` snapshot and invalidate dependent cache entries.
- `cache_stats`: report cache counts and size estimates.
- `clear_cache`: clear cache entries by scope.
- `shutdown`: stop the runtime cleanly.

### Request Contents

A request payload should contain only:

- target object id, when required;
- operation-specific arguments;
- optional optimization override for `run_script`;
- source path or source blob for load and reload;
- typed argument values for script parameters.

### Response Contents

A response should contain only what the client needs:

- success or failure;
- created or updated object ids;
- script revision ids;
- inline result value or handle id;
- diagnostics, when relevant;
- lightweight metadata for inspect-style operations.

Do not expose compiler IR, REPL cells, or graph editing state on this boundary.

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
