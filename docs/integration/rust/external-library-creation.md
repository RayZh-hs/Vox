# External Library Creation

Rust host integration is intentionally small:

- declare the package once;
- mark Rust structs, traits, and functions as Vox-exportable once;
- let the library collect everything automatically.

Rust `.voxlib` authoring lives in `voxlib-sdk`. The lower-level `vox_core`
crate only contains language-neutral Vox manifest, type, value, and `.voxlib`
encoding types.

An external library is the Rust-side description of one Vox package. In Rust,
the builder type for that concept is `ExternalLibrary`. Ordinary library
authors do not build `PackageManifest`, `TypeSpec`, `FunctionSpec`, or
`VoxType` by hand.

## The Model

`ExternalLibrary` is the Rust builder for one external library, which in turn
owns one Vox package.

The package contents are built from two sources:

- Rust items marked for Vox export;
- referenced structs and traits reachable from those exported functions and
  trait methods.

That means:

- structs, traits, and functions are opt-in at the item definition site;
- there are no `export_type::<T>()` or `export_trait::<T>()` calls in normal
  usage;
- there are no `.function(...)` calls in normal usage;
- unused exported items stay out of the final library.

## Recommended Workflow

### 1. Mark exported structs once

Use a Vox SDK derive on each Rust struct that is visible to Vox:

```rust
use voxlib_sdk::{VoxExport, external_library::ExternalLibrary};

#[derive(VoxExport)]
struct Image {
    width: i64,
    height: i64,
}
```

This marks `Image` as eligible for export. It does not export anything by
itself yet. The type is pulled into the package automatically when an exported
function or trait method uses it.

The optional `name` override controls the public Vox name. If omitted, Vox uses
the Rust struct name.

### 2. Mark exported traits once

Rust traits cannot use `derive`, so Vox uses a trait attribute for the same
purpose: mark the trait once and let the external library include it
automatically when it is referenced.

```rust
#[vox_trait]
trait Filter {
    #[vox(lowered_by = filter_apply, pure = true)]
    fn apply(&self, input: Image) -> Image;
}
```

This contributes trait metadata:

- the Vox trait name;
- the method signature;
- the lowered function symbol that implements the runtime boundary.

Like structs, traits are not registered manually with the `ExternalLibrary`
builder.

The optional trait `name` override controls the public Vox trait name. If
omitted, Vox uses the Rust trait name.

### 3. Mark exported functions once

Functions follow the same rule: mark them once on the Rust item and let the
external library include them automatically.

```rust
#[vox_fn(pure = true)]
fn blur(input: Image, #[vox(default)] radius: f64) -> Image {
    todo!()
}

#[vox_fn(name = "filter_apply", pure = true)]
fn filter_apply(filter: &dyn Filter, input: Image) -> Image {
    filter.apply(input)
}
```

This marks each function as part of the Vox package surface.

The optional function `name` override controls the public Vox function name. If
omitted, Vox uses the Rust function name.

`pure` is a boolean attribute: `true` means the function is pure, and `false`
means it is effectful.

### 4. Build the package once

```rust
    let (manifest, _metadata) = ExternalLibrary::new("image")?
        .build()?;
```

That is the full external library declaration.

From the exported Rust items, the external library collects:

- `image.blur`
- `image.filter_apply`
- `image.Image`
- `image.Filter`
- the `Filter.apply` method metadata

No separate type or trait registration step is needed.
No separate function registration step is needed either.

## Docstrings

Every Vox-exported item can carry a docstring. These are collected at build time
and attached to the voxlib as metadata after the wasm module. Libraries with
metadata are called *annotated libraries*, and they are the preferred way to
publish external Vox libraries, since tooling and IDEs can display the
documentation from the voxlib file itself.

Docstrings are sourced from two places, in priority order:

1.  An explicit `#[vox(doc = "...")]` override on the item.
2.  Rust `///` doc comments on the item, if no explicit override is given.

All three macro forms support docstrings:

```rust
/// A pixel buffer with integer dimensions.
#[derive(VoxExport)]
struct Image {
    width: i64,
    height: i64,
}

/// Applies a Gaussian blur to the image.
#[vox_fn(pure = true)]
fn blur(input: Image, #[vox(default)] radius: f64) -> Image {
    todo!()
}

/// Describes a compositional image filter.
#[vox_trait]
trait Filter {
    /// Applies this filter to an input image.
    #[vox(lowered_by = filter_apply, pure = true)]
    fn apply(&self, input: Image) -> Image;
}
```

When `ExternalLibrary::build()` is called, the collected docstrings are returned
alongside the manifest. `ExternalLibrary::generate()` writes them into the
`.voxlib` file automatically.

To override a docstring (e.g. when the Rust doc comment is intended for Rust
consumers but the Vox docstring differs), use the explicit form:

```rust
#[vox_fn(pure = true, doc = "Blurs an image with a Gaussian kernel.")]
fn blur(input: Image, #[vox(default)] radius: f64) -> Image {
    todo!()
}
```

## End-to-End Example

```rust
use voxlib_sdk::{VoxExport, external_library::ExternalLibrary, vox_fn, vox_trait};

#[derive(VoxExport)]
struct Image {
    width: i64,
    height: i64,
}

#[vox_trait]
trait Filter {
    #[vox(lowered_by = filter_apply, pure = true)]
    fn apply(&self, input: Image) -> Image;
}

#[vox_fn(pure = true)]
fn blur(input: Image, #[vox(default)] radius: f64) -> Image {
    todo!()
}

#[vox_fn(name = "filter_apply", pure = true)]
fn filter_apply(filter: &dyn Filter, input: Image) -> Image {
    filter.apply(input)
}

let (manifest, _metadata) = ExternalLibrary::new("image")?.build()?;
```

The user overhead is intentionally small:

- one derive per exported struct;
- one attribute per exported trait;
- one attribute per exported function;
- one package name at the `ExternalLibrary` root.

## Generated Artifact Format

`ExternalLibrary::generate(wasm_bytes)` produces a single `.voxlib` file
containing:

- a header (magic, version, reserved);
- the package manifest (types, traits, functions, trait impls);
- the embedded wasm module;
- optionally, an annotated metadata section at the end.

The resulting `GeneratedExternalLibrary` can be written to disk via
`write_to_dir(dir)`.

For tooling or codegen that needs raw bytes without wasm, use
`ExternalLibrary::build()` which returns `(PackageManifest, Vec<u8>)` — the
manifest plus serialized docstring metadata.

## Exported Names

By default, Vox exports each declaration under its Rust name:

- structs use the Rust struct name;
- traits use the Rust trait name;
- trait methods use the Rust method name;
- functions use the Rust function name.

When a public Vox name needs to differ from the Rust item name, use the same
`name = "..."` override on each exported declaration kind:

- structs via `#[vox(name = "...")]` next to `#[derive(VoxExport)]`;
- traits via `#[vox_trait(name = "...")]`;
- trait methods via `#[vox(name = "...", lowered_by = ...)]`;
- functions via `#[vox_fn(name = "...")]`.

For example:

```rust
#[derive(VoxExport)]
#[vox(name = "Bitmap")]
struct Image {
    width: i64,
    height: i64,
}

#[vox_trait(name = "PixelFilter")]
trait Filter {
    #[vox(name = "run", lowered_by = filter_apply, pure = true)]
    fn apply(&self, input: Image) -> Image;
}

#[vox_fn(name = "gaussian_blur", pure = true)]
fn blur(input: Image, #[vox(default)] radius: f64) -> Image {
    todo!()
}
```

## Automatic Inclusion Rules

`ExternalLibrary` includes an exported Rust struct automatically when:

- an exported function parameter uses it;
- an exported function return type uses it;
- an exported trait method uses it;
- it appears inside a supported container such as `Option<T>` or `Vec<T>`.

`ExternalLibrary` includes an exported Rust trait automatically when:

- an exported function parameter uses `dyn Trait`;
- an exported function return type uses `dyn Trait`;
- the trait itself declares exported methods.

`ExternalLibrary` includes an exported Rust function automatically when:

- it is marked with `#[vox_fn(...)]`;
- it is a lowered function referenced by an exported trait method;
- it belongs to the package being built.

`ExternalLibrary` follows these references transitively until the
reachable package metadata is complete.

## Trait Methods and Lowered Functions

Traits describe the Vox-facing method surface. Ordinary functions are still the
runtime entry points.

For each exported trait method:

- the Vox method name defaults to the Rust method name and may be overridden
  with `name = "..."`;
- the runtime executes the lowered free function named by
  `#[vox(lowered_by = ...)]`;
- that lowered function is included automatically when it is marked with
  `#[vox_fn(...)]`.

The two method fields serve different roles:

- `name` controls the public Vox method name;
- `lowered_by` names the Rust free function that implements the runtime
  boundary.

This keeps the API simple:

- traits stay declarative;
- functions stay executable;
- the runtime only needs one callable host boundary.

## Type Mapping

Rust types map to Vox types automatically for the common cases:

- `i64`, `i32`, `u32`, `usize` -> `Int`
- `f64`, `f32` -> `Float`
- `bool` -> `Bool`
- `String`, `&str` -> `String`
- `Option<T>` -> `T?`
- `Vec<T>` -> `List[T]`
- `(A, B, ...)` -> tuple
- exported Rust structs -> qualified named Vox types
- exported Rust traits behind `dyn` -> qualified dynamic trait types

Most users stay entirely in ordinary Rust signatures and let Vox infer
the manifest types.

When a host function needs to return multiple named values, prefer a record
return type instead of inventing a temporary exported struct. Structs remain
single named values in the Vox type system; records are the anonymous product
type for "these named fields together".

For the common path, expose the Rust function normally and override only the
Vox-facing return type when Rust cannot express the anonymous record directly:

```rust
#[vox_fn(
    pure = true,
    return_type = "{ shadows: Int, mids: Int, highlights: Int }"
)]
fn histogram(image: Image) -> HistogramSummary {
    todo!()
}
```

In low-level manifest code, the equivalent shape is `VoxType::Record(...)`.

## API Summary

- `ExternalLibrary::new(package)` starts one package export session.
- `#[derive(VoxExport)]` marks a Rust struct as exportable metadata.
- `#[vox(name = "...")]` optionally overrides the exported name of a struct or
  trait method.
- `#[vox(doc = "...")]` optionally overrides the documented description of any
  exported item.
- `#[vox_trait(...)]` marks a Rust trait as exportable metadata and may
  override its exported name.
- `#[vox_fn(...)]` describes one exported function and may override its
  exported name, purity, Vox return type, or docstring.
- `ExternalLibrary::build()` returns `(PackageManifest, Vec<u8>)` — the package
  manifest and serialized docstring metadata.
- `ExternalLibrary::generate(wasm_bytes)` produces a `GeneratedExternalLibrary`
  containing the complete `.voxlib` bytes, which can be written to disk with
  `write_to_dir(dir)`.

## Advanced Use

Low-level manifest construction still exists for tooling, code generation, and
unusual edge cases. It is not the normal library-author workflow.

If you are writing an external library by hand, the intended rule is:

- annotate structs, traits, and functions once;
- let `ExternalLibrary` infer the package.
