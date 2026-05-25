# External Library Creation

Rust host integration should feel small:

- declare the package once;
- mark Rust structs, traits, and functions as Vox-exportable once;
- let the library collect everything automatically.

An external library is the Rust-side description of one Vox package. In Rust,
the builder type for that concept is `ExternalLibrary`. Ordinary library
authors should not build `PackageManifest`, `TypeSpec`, `FunctionSpec`, or
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

Use a Vox Core derive on each Rust struct that should be visible to Vox:

```rust
use vox_core::external_library::ExternalLibrary;

#[derive(VoxExport)]
struct Image {
    width: i64,
    height: i64,
}
```

This marks `Image` as eligible for export. It does not export anything by
itself yet. The type is pulled into the package automatically when an exported
function or trait method uses it.

### 2. Mark exported traits once

Rust traits cannot use `derive`, so Vox uses a trait attribute for the same
purpose: mark the trait once and let the external library include it
automatically when it is referenced.

```rust
#[vox_trait]
trait Filter {
    #[vox(lowered_by = filter_apply, purity = "pure")]
    fn apply(&self, input: Image) -> Image;
}
```

This contributes trait metadata:

- the Vox trait name;
- the method signature;
- the lowered function symbol that implements the runtime boundary.

Like structs, traits are not registered manually with the `ExternalLibrary`
builder.

### 3. Mark exported functions once

Functions follow the same rule: mark them once on the Rust item and let the
external library include them automatically.

```rust
#[vox_fn(purity = "pure")]
fn blur(input: Image, #[vox(default)] radius: f64) -> Image {
    todo!()
}

#[vox_fn(name = "filter_apply", purity = "pure")]
fn filter_apply(filter: &dyn Filter, input: Image) -> Image {
    filter.apply(input)
}
```

This marks each function as part of the Vox package surface.

### 4. Build the package once

```rust
let manifest = ExternalLibrary::new("image")?
    .build();
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

## End-to-End Example

```rust
use vox_core::external_library::ExternalLibrary;

#[derive(VoxExport)]
struct Image {
    width: i64,
    height: i64,
}

#[vox_trait]
trait Filter {
    #[vox(lowered_by = filter_apply, purity = "pure")]
    fn apply(&self, input: Image) -> Image;
}

#[vox_fn(purity = "pure")]
fn blur(input: Image, #[vox(default)] radius: f64) -> Image {
    todo!()
}

#[vox_fn(name = "filter_apply", purity = "pure")]
fn filter_apply(filter: &dyn Filter, input: Image) -> Image {
    filter.apply(input)
}

let manifest = ExternalLibrary::new("image")?.build();
```

The user overhead is intentionally small:

- one derive per exported struct;
- one attribute per exported trait;
- one attribute per exported function;
- one package name at the `ExternalLibrary` root.

## Automatic Inclusion Rules

`ExternalLibrary` should include an exported Rust struct automatically when:

- an exported function parameter uses it;
- an exported function return type uses it;
- an exported trait method uses it;
- it appears inside a supported container such as `Option<T>` or `Vec<T>`.

`ExternalLibrary` should include an exported Rust trait automatically when:

- an exported function parameter uses `dyn Trait`;
- an exported function return type uses `dyn Trait`;
- the trait itself declares exported methods.

`ExternalLibrary` should include an exported Rust function automatically when:

- it is marked with `#[vox_fn(...)]`;
- it is a lowered function referenced by an exported trait method;
- it belongs to the package being built.

`ExternalLibrary` should follow these references transitively until the
reachable package metadata is complete.

## Trait Methods and Lowered Functions

Traits describe the Vox-facing method surface. Ordinary functions are still the
runtime entry points.

For each exported trait method:

- the method name stays visible in trait metadata;
- the runtime executes the lowered free function named by
  `#[vox(lowered_by = ...)]`;
- that lowered function is included automatically when it is marked with
  `#[vox_fn(...)]`.

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

Most users should stay entirely in ordinary Rust signatures and let Vox infer
the manifest types.

## API Summary

- `ExternalLibrary::new(package)` starts one package export session.
- `#[derive(VoxExport)]` marks a Rust struct as exportable metadata.
- `#[vox_trait]` marks a Rust trait as exportable metadata.
- `#[vox_fn(...)]` describes one exported function.
- `ExternalLibrary::build()` produces the package manifest consumed by the
  runtime.

## Advanced Use

Low-level manifest construction still exists for tooling, code generation, and
unusual edge cases. It is not the normal library-author workflow.

If you are writing an external library by hand, the intended rule is:

- annotate structs, traits, and functions once;
- let `ExternalLibrary` infer the package.
