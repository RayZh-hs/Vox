# Compiler

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

The compiler should also assign an optimization rank to each compiled body.
Ranks capture how far lowering may proceed for that body, from baseline cleanup
through sealed ownership, demand, and materialization-aware lowering.

Sealed `SOpt` should also run:

- last-use analysis to classify aggregate uses as borrow or consume;
- backward demand analysis over tuple and record outputs;
- split-product lowering so unused pure record fields are never emitted into the
  executable plan;
- update lowering that prefers in-place reuse only for consumed inputs.

After `Vox Core` optimization, the compiler lowers the result into the
wasm-oriented executable plan consumed by `vox-runtime`.

Storage reuse itself is not primarily a compiler concern. The compiler may annotate liveness and last-use information, but the final reuse decision belongs to `vox-runtime`.

## Sealed Lowering

Sealed lowering is not part of the Vox source language. It is the execution
strategy used when the host marks a function body as sealed and requests
`SOpt`.

Sealing is a deployment or tooling decision:

- a REPL or editor may seal a function once editing has stabilized;
- a build may seal package functions for release execution;
- a runtime may seal a script entrypoint before repeated batch execution.

Sealing does not change source-level semantics. It only allows the compiler to
emit a more ownership-aware and demand-aware plan.

### Guarantees

Sealed lowering must preserve:

- ordinary value semantics;
- the result of every demanded expression;
- the order and presence of `evil` effects;
- host-visible behavior at the runtime boundary.

It may change:

- whether an intermediate aggregate is materialized at all;
- whether a use is lowered as borrow or consume;
- whether storage is reused in place;
- whether unused pure subcomputations are omitted.

### Derived Annotations

Before final plan lowering, the compiler should derive sealed-core annotations
for each sealed body:

- `last_use`: whether a use is final within the current body;
- `ownership`: whether a use lowers as `borrow` or `consume`;
- `demand`: which tuple slots or record fields are actually required;
- `materialization`: whether a composite must become one runtime value;
- `effect_summary`: whether an expression is pure enough to prune when
  undemanded.

### Required Analyses

Sealed `SOpt` should run:

- last-use analysis to classify aggregate uses as borrow or consume;
- backward demand analysis over tuple and record outputs;
- split-product lowering so unused pure fields are never emitted into the plan;
- update lowering that prefers in-place reuse only for consumed inputs.

This enables move-style internal reuse without exposing mutation in source Vox.

### Executable Plan Shape

The sealed executable plan should keep ownership and projection explicit until
after `SOpt` decisions are complete.

The plan should model:

- SSA values for scalars and short-lived temporaries;
- runtime handles for large or opaque values;
- explicit `borrow` and `consume` edges on aggregate uses;
- product nodes whose fields may remain split;
- explicit `project_field` and `project_slot` operations;
- update operations that can reuse storage when the input was consumed.

Recommended lowering rules:

- scalars stay in wasm locals when possible;
- large aggregates and opaque host values lower to runtime handles;
- `borrow` passes an existing handle forward without granting reuse;
- `consume` passes a handle with no later uses in the current body;
- product construction stays split until a full runtime value is required.

### Lowering Order

For sealed functions, the compiler pipeline should be:

1. lower source Vox into ordinary `Vox Core`;
2. infer purity, effect boundaries, and explicit update operations;
3. run liveness and last-use analysis;
4. run backward demand analysis over tuple and record results;
5. rewrite composite producers into split-demand form where legal;
6. mark uses as `borrow` or `consume`;
7. lower sealed-core into the wasm-oriented executable plan;
8. emit final wasm plus runtime imports.

Demand analysis must run before materialization decisions, and last-use
analysis must run before in-place reuse decisions.

### Runtime Contract

The compiler proves which uses are last uses and which fields are undemanded
inside the sealed body. The runtime remains responsible for:

- preserving value semantics when reuse is unsafe;
- executing `evil` operations exactly as required;
- honoring the ownership contract carried by the plan;
- materializing composite runtime values only when the plan requires it.

If the runtime cannot safely apply a requested in-place optimization, it must
fall back to a semantics-preserving shared or copied representation.

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
- optimization ranks for the module body and declared functions;
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
