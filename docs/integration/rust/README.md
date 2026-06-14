# Rust

Rust is the first host-language integration target for Vox.

This section covers the practical Rust-facing APIs:

1. [Embedding the Runtime](./embedding-runtime.md)
2. [External Library Creation](./external-library-creation.md)

Use `vox_runtime` for embedding or connecting to a runtime. Use `voxlib-sdk`
for authoring Rust-backed `.voxlib` packages with `VoxExport`, `vox_fn`,
`vox_trait`, `vox_trait_impl`, and `ExternalLibrary`.
