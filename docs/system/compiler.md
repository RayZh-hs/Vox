# Vox Compiler

`vox-compiler` turns Vox source into executable plans for `vox-runtime`.

It owns language understanding. It does not own process lifetime, runtime handles, or result caches.

## Role

`vox-compiler` is responsible for:

- parsing;
- name resolution;
- type checking and inference;
- nullability checking;
- purity propagation;
- desugaring;
- `Vox Core` construction;
- execution plan construction;
- optimization;
- diagnostic reporting.

Its output is a compiled plan plus enough metadata for execution.

## Pipeline

The compiler should stay simple and staged.

1. Parse source into syntax trees.
2. Resolve names against builtins, mounted libraries, and host package metadata.
3. Type-check and infer nullability and purity.
4. Lower surface Vox into `Vox Core`.
5. Optimize `Vox Core` according to `NOpt`, `IOpt`, or `SOpt`.
6. Lower optimized `Vox Core` into a wasm-oriented executable plan.

## Vox Core

`Vox Core` is the lowered form of Vox source files.

It is still Vox-shaped, but with surface syntax removed and semantics made explicit. It should be the main optimization form of the compiler.

`Vox Core` should have:

- resolved names;
- explicit types, nullability, and purity;
- normalized calls;
- SSA-style bindings instead of `var`;
- lowered loops and control-flow sugar;
- explicit dependency information.

## Lowering

Lower early, before execution planning.

This includes:

- receiver syntax to normal calls;
- named argument normalization;
- `var` lowering to SSA-style bindings;
- loop lowering such as `for` to folds;
- nullable operator lowering;
- explicit purity and dependency tracking.

## Optimization

Most optimization belongs in `vox-compiler`, and most of it should run on `Vox Core` rather than on raw surface syntax.

Typical passes include:

- dead code elimination;
- constant folding;
- branch pruning;
- fold simplification;
- common subexpression sharing for pure computations;
- lightweight inlining when it improves the plan.

Mode-specific guidance:

- `NOpt`: only correctness-preserving cleanup.
- `IOpt`: optimize for stable identities and fast rebuilds.
- `SOpt`: spend more effort to produce a leaner execution plan.

Sealed `SOpt` should also run:

- last-use analysis to classify aggregate uses as borrow or consume;
- backward demand analysis over tuple and record outputs;
- split-product lowering so unused pure record fields are never emitted into the
  executable plan;
- update lowering that prefers in-place reuse only for consumed inputs.

After `Vox Core` optimization, the compiler lowers the result into the
wasm-oriented executable plan consumed by `vox-runtime`.

Storage reuse itself is not primarily a compiler concern. The compiler may annotate liveness and last-use information, but the final reuse decision belongs to `vox-runtime`.

## Host Boundary

The compiler type-checks against host metadata. It does not inspect host implementation code.

The host boundary should be based on:

- registered package names;
- type specs;
- function signatures;
- purity metadata.

This keeps the compiler independent from host language details and avoids pushing Vox semantics into Rust generics.

## Output

A compiled script artifact should include:

- script identity and revision;
- parameter list;
- result type;
- purity summary;
- executable plan;
- diagnostics;
- dependency fingerprints needed for cache validation.

## Design Rules

- use `Vox Core` as the main optimization form;
- keep the IR data-oriented and compact;
- prefer IDs and tables over deep generic structures;
- do not expose compiler internals over the runtime protocol;
- optimize for fast incremental rebuilds in `IOpt`;
- treat JIT as optional and later, not foundational.
