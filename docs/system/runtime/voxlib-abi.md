# Voxlib Package ABI

`.voxlib` format version 6 uses package ABI version 1. The ABI is shared by
packages compiled from Vox and packages produced by another language, including
Rust. A file-backed library is self-contained: its manifest describes the public
surface and its wasm module implements that surface.

The file header stores the package ABI version after the file-format version.
Mounting rejects a file when either version is unsupported, the wasm is invalid,
or an executable manifest entry has no compatible wasm export.

## Package exports

For every manifest function named `name`, wasm exports:

```text
vox:function:name
```

For every manifest value named `name`, wasm exports a zero-argument initializer:

```text
vox:value:name
```

Each Vox parameter is represented by two wasm parameters, `(i32 tag, i64 data)`.
Every function and value initializer returns `(i32 tag, i64 data)`. Function
exports therefore have this shape:

```text
(tag0, data0, tag1, data1, ...) -> (result_tag, result_data)
```

The tags are:

| Tag | Value |
|---:|---|
| 0 | `Int` |
| 1 | `Float`, with the `f64` bits in `data` |
| 2 | `Bool` |
| 3 | `String` |
| 4 | tuple |
| 5 | record |
| 6 | list |
| 7 | opaque runtime handle |
| 8 | `Null` |
| 9 | `UInt` |
| 10 | closure, internal only |

Strings and compound values crossing a standalone package boundary use offsets
in the shared wasm memory. Their layouts are:

```text
String: [u32 byte_len][u8 bytes...]
Tuple:  [u32 count][(i32 tag, i32 padding, i64 data)...]
List:   [u32 count][(i32 tag, i32 padding, i64 data)...]
Record: [u32 field_count][(u32 name_len, u8 name..., i32 tag, i64 data)...]
```

An export has its full manifest arity. Calls across a package boundary must
supply concrete values for every parameter. Default expressions are not stored
in the manifest, so an omitted external default cannot be reconstructed by the
runtime.

## Runtime imports and memory

An executable package imports these entries in order. `vox.memory` is an
unshared 32-bit memory with a minimum no greater than 256 pages and no declared
maximum, so the runtime's 256-page memory satisfies it:

```text
vox.memory
vox.__vox_op(i32, i32, i32, i32, i32, i32)
vox.__vox_host(i32, i32, i32, i32, i32)
```

It re-exports the imported memory as `memory` and exports a mutable `i32` global
named `__vox_heap_top`. `vox.__vox_op` provides runtime operations that cannot be
lowered locally. `vox.__vox_host` invokes a fully qualified function from a
mounted dependency.

The runtime compiles and validates a mounted wasm module once, then creates a
fresh instance for each exported call or value evaluation. Mutable wasm globals
therefore do not persist between calls.
Persistent external state must live behind runtime handles or host operations.
Package values are lazy: mounting does not invoke value exports.

## Rust-produced packages

`voxlib_sdk::ExternalLibrary::generate` accepts wasm implementing this same ABI.
It validates the wasm module, required imports, base exports, and manifest export
names before producing the file. The runtime repeats validation and additionally
checks every exported function signature.

Rust annotations still provide the manifest and in-process handler inventory.
When producing a portable `.voxlib`, the supplied Rust-generated wasm must also
provide the ABI exports above; arbitrary wasm bytes are rejected.

## Remote mounting

Remote runners send the complete `.voxlib` bytes with `MOUNT_LIBRARY` source
kind 2. The server validates and retains the same bundle used by embedded
runtimes. Source kind 1 remains available for manifest-only programmatic host
libraries whose implementations are registered in the runtime process.
