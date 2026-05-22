# Type Bindings

`vox-core` should present a small frontside for Rust authors who want to expose
host types to Vox.

## Goal

The binding author should describe:

- package name;
- structs and their fields;
- traits and their methods;
- free functions, when needed;
- purity and defaultability.

They do not need to assemble low-level manifest pieces by hand unless they
are doing advanced tooling work.

## Frontside

The user-facing surface should read like a concise schema builder:

```rust
use vox_core::bindings::{Package, Pure, Ty, field, method, param};

let manifest = Package::new("image")?
    .struct_("Image", [field("width", Ty::int()), field("height", Ty::int())])
    .trait_("Filter", [method(
        "apply",
        [param("input", Ty::named("image.Image")?)],
        Ty::named("image.Image")?,
        Pure,
    )])
    .build();
```

This frontside is intentionally thin.

- `Package` owns one package manifest.
- `Ty` is the short type vocabulary used at the binding boundary.
- `Pure` and `Evil` stay explicit.
- advanced users may still drop to the lower-level manifest structs.

## Design Rules

- one obvious path for simple bindings;
- zero protocol knowledge required to author bindings;
- low allocation and no hidden global registry;
- the produced manifest remains the single runtime input;
- lower-level manifest structs remain available for tooling and tests.

## Runtime Boundary

Bindings are data, not behavior.

The binding layer defines what exists at the host boundary. Runtime processes,
REPLs, and external loaders consume the resulting manifest. That separation
keeps authoring simple and keeps transport concerns out of `vox-core`.
