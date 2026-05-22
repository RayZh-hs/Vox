# External Library Creation

Rust host integration centers on an external library. An external library is
the Rust-side description of one Vox package.

It contains:

- the package name;
- exported structs and their fields;
- exported traits;
- exported functions;
- lowered trait methods, represented as ordinary functions with the receiver as
  the first parameter;
- purity and default-parameter metadata.

Ordinary callers do not assemble `PackageManifest`, `TypeSpec`, `FunctionSpec`,
or `VoxType` by hand. Those remain the low-level representation for tooling,
tests, and transport.

## Design Rules

- one package is created by one `EmbeddedLibrary`;
- Rust types are declared once, next to the Rust item they describe;
- library creation collects exported items and produces the manifest
  automatically;
- method calls are metadata only; the runtime consumes lowered free functions;
- the host boundary stays data-oriented and serializable.

## Preferred API

The preferred surface is derive-first:

```rust
use vox_core::embedded_library::{EmbeddedLibrary, Purity};

#[derive(VoxExport)]
#[vox(package = "image")]
struct Image {
    width: i64,
    height: i64,
}

#[vox_trait(package = "image")]
trait Filter {
    #[vox(exported_as = "filter_apply", purity = "pure")]
    fn apply(&self, input: Image) -> Image;
}

#[vox_fn(package = "image", purity = "pure")]
fn blur(input: Image, radius: f64) -> Image {
    todo!()
}

let manifest = EmbeddedLibrary::new("image")?
    .export_type::<Image>()
    .export_trait::<dyn Filter>()
    .export_fn(blur)
    .build();
```

The caller names the package once and registers the Rust items once. The
manifest is inferred from the exported Rust definitions.

## Representation Rules

### Structs

Exporting a Rust struct produces:

- one Vox type name, qualified by the package;
- one ordered field list;
- zero or more implemented traits.

Example:

```rust
#[derive(VoxExport)]
#[vox(package = "image")]
struct Image {
    width: i64,
    height: i64,
}
```

This exports the following Vox-facing metadata:

- type: `image.Image`
- fields:
  - `width: Int`
  - `height: Int`

The Rust field types map to Vox types automatically. The export site does not
repeat `Int`, `Float`, or other `VoxType` constructors.

### Traits

Exporting a Rust trait produces:

- one Vox trait name, qualified by the package;
- one method list for documentation and type checking;
- one lowered function per method for runtime use.

Method lowering is fixed:

- the receiver becomes the first parameter;
- the lowered function is stored in the package function table;
- the trait record keeps the method name and the lowered function symbol.

Example:

```rust
#[vox_trait(package = "image")]
trait Filter {
    #[vox(exported_as = "filter_apply", purity = "pure")]
    fn apply(&self, input: Image) -> Image;
}
```

This exports:

- trait: `image.Filter`
- method: `apply(input: image.Image) -> image.Image`
- lowered function:
  `filter_apply(receiver: dyn image.Filter, input: image.Image) -> image.Image`

The lowered function is the executable boundary. The trait metadata exists so
the compiler, REPL, and teaching material can talk about trait members without
inventing a second host ABI.

### Functions

Exporting a Rust function produces:

- one Vox function symbol in the package;
- ordered parameters with names, types, and defaultability;
- one return type;
- one purity flag.

Example:

```rust
#[vox_fn(package = "image", purity = "pure")]
fn blur(input: Image, #[vox(default)] radius: f64) -> Image {
    todo!()
}
```

This exports:

- function: `image.blur`
- parameters:
  - `input: image.Image`
  - `radius: Float`, defaultable
- return type: `image.Image`
- purity: `Pure`

Parameter names come from the Rust item definition. That is why the preferred
surface uses an attribute macro for exported functions instead of a bare
function pointer plus a manual manifest entry.

## Type Mapping

The Rust-to-Vox mapping is fixed for the standard scalar and container forms:

- `i64`, `i32`, `u32`, `usize` -> `Int`
- `f64`, `f32` -> `Float`
- `bool` -> `Bool`
- `String`, `&str` -> `String`
- `Option<T>` -> `T?`
- `Vec<T>` -> `List[T]`
- `(A, B, ...)` -> tuple
- exported Rust structs -> qualified named Vox types
- exported Rust traits behind `dyn` -> qualified dynamic trait types

The low-level API may still construct `VoxType` directly for cases that do not
map cleanly from ordinary Rust types.

## Manual API

Derive macros are the default path. A manual descriptor API exists for code
generation, unusual generic cases, and environments that cannot use proc
macros.

```rust
use vox_core::embedded_library::{
    EmbeddedLibrary, FunctionBuilder, TraitBuilder, TypeBuilder,
    TypeExport, TraitExport,
};

struct Image {
    width: i64,
    height: i64,
}

impl TypeExport for Image {
    fn describe(builder: TypeBuilder<Self>) -> TypeBuilder<Self> {
        builder
            .field("width", |value| value.width)
            .field("height", |value| value.height)
    }
}

trait Filter {
    fn apply(&self, input: Image) -> Image;
}

impl TraitExport for dyn Filter {
    fn describe(builder: TraitBuilder<Self>) -> TraitBuilder<Self> {
        builder.method("apply", |method| {
            method
                .exported_as("filter_apply")
                .receiver()
                .param::<Image>("input")
                .returns::<Image>()
                .purity(Purity::Pure)
        })
    }
}

let manifest = EmbeddedLibrary::new("image")?
    .export_type::<Image>()
    .export_trait::<dyn Filter>()
    .export(
        FunctionBuilder::new("blur", blur)
            .param("input")
            .param("radius")
            .defaultable("radius")
            .purity(Purity::Pure),
    )?
    .build();
```

The manual API still infers Rust-to-Vox types from accessors and function
signatures. It exists to supply names and edge-case metadata, not to rebuild
the full manifest by hand.

## Public API Summary

- `EmbeddedLibrary::new(package)` creates one package export session.
- `EmbeddedLibrary::export_type::<T>()` registers one exported Rust struct.
- `EmbeddedLibrary::export_trait::<dyn T>()` registers one exported Rust trait.
- `EmbeddedLibrary::export_fn(f)` registers one macro-described function item.
- `EmbeddedLibrary::export(descriptor)` registers one manual descriptor.
- `EmbeddedLibrary::build()` returns the package manifest consumed by the
  runtime.

## Runtime Boundary

External library creation defines metadata. It does not define transport,
storage, or execution policy.

The resulting manifest is consumed by:

- `vox-compiler` for imported names and type checking;
- `vox-runtime` for mounted package metadata;
- `vox-repl` for completion and inspection.

That keeps host-language ergonomics in the export layer and keeps runtime
execution on a compact manifest format.
