# 07 Sealed Lowering

This chapter defines how sealed functions are compiled for `SOpt`.

It does not introduce new source-level syntax. It defines an execution-time
lowering contract between the compiler and the runtime.

## 1. Sealed Functions

A function may be marked sealed by the host environment when interactive editing
for that function has finished.

Examples of sealing decisions include:

- an interpreter sealing a function after the user commits the current body;
- a build step sealing package functions for release execution;
- a runtime sealing a script entrypoint before repeated batch execution.

Rules:

- sealing is not a source-language modifier or keyword;
- sealing selects `SOpt` compilation for that function body;
- a sealed function keeps the same source-level semantics as the unsealed form;
- sealing may change caching, scheduling, storage reuse, and partial-result
  lowering;
- a sealed function may still call non-sealed functions, but only sealed bodies
  may rely on the `SOpt` lowering rules in this chapter.

## 2. `SOpt` Guarantees

`SOpt` is allowed to optimize only when the optimization is not observable from
Vox source.

It must preserve:

- ordinary value semantics;
- the result value of every demanded expression;
- the order and presence of `evil` effects;
- host-visible behavior at the runtime boundary.

It may change:

- whether an intermediate aggregate is materialized at all;
- whether a value is internally borrowed or consumed;
- whether storage is reused in place;
- whether unused pure subcomputations are omitted.

## 3. Sealed-Core Annotations

Before lowering to the final executable plan, the compiler should derive a
sealed-core form for every sealed function.

Sealed-core extends ordinary `Vox Core` with the following annotations:

- `last_use`: whether a binding occurrence is the final use of a value in the
  current function body;
- `ownership`: whether a use is compiled as `borrow` or `consume`;
- `demand`: which tuple slots or record fields of a result are required by the
  enclosing context;
- `materialization`: whether a composite value must be built as a full runtime
  object or may stay split into independent field producers;
- `effect_summary`: whether an expression is pure and therefore eligible for
  pruning when undemanded.

These annotations are derived after type checking and purity analysis, and
before final plan lowering.

## 4. Move-Aware Value Lowering

For value reuse, `SOpt` uses last-use analysis.

Rules:

- every non-final use of a value is lowered as `borrow`;
- the final use of a value is lowered as `consume`;
- a `consume` use transfers the right to reuse the current storage internally;
- a `borrow` use must leave the current storage reusable by later code;
- if a value has multiple remaining uses, the runtime must treat it as shared
  even when the implementation stores it as one handle.

This enables the following strategy for aggregates such as lists, records,
buffers, images, and tensors:

- if an update receives a `consume` input and the representation is compatible,
  the runtime may update the existing storage in place;
- if the same input is still live elsewhere, the runtime must preserve value
  semantics by sharing immutable backing storage or by copying only the required
  parts;
- if the representation is small and scalar-like, the compiler may keep it in
  native wasm locals instead of runtime handles.

This realizes move-style reuse without exposing mutation at the source level.

## 5. Demand-Driven Product Lowering

When a pure expression produces a tuple or record, `SOpt` should not treat the
whole product as demanded unless the surrounding code actually needs the whole
value.

Rules:

- demand flows backward from each field access, tuple-slot access, return site,
  and host boundary;
- if only `foo().a` is demanded, only the lowering for `a` is required;
- undemanded pure fields must not be traced, scheduled, or computed;
- a full record or tuple must be materialized only when the entire value is
  demanded as one runtime value;
- demand pruning must stop at an `evil` boundary or any other boundary where
  skipping evaluation would change observable behavior.

Example:

```vox
fun foo(x: Int): {a: Int, b: Int} {
    {
        a = x + 1,
        b = expensive(x),
    }
}

fun bar(x: Int): Int {
    foo(x).a
}
```

In sealed lowering, `bar` demands only field `a` from `foo`.

`SOpt` should therefore lower `foo` into separate field producers or equivalent
projectable plan nodes, and the work for `b` should not run when `bar` is
compiled and executed in sealed form.

## 6. Wasm-Oriented IR Shape

The executable plan for a sealed function should lower into a wasm-oriented IR
with explicit ownership and projection operations before final wasm emission.

This IR should model:

- SSA values for scalars and short-lived temporaries;
- runtime handles for large or opaque values;
- `borrow` and `consume` edges on aggregate uses;
- product nodes whose fields may be lowered independently;
- explicit `project_field` and `project_slot` operations;
- explicit update operations such as record-field update and list-slot update;
- runtime calls for host functions, handle management, and cache interaction.

Recommended lowering rules:

- scalar `Int`, `Float`, and `Bool` values lower directly to wasm locals and
  returns when possible;
- `String` and large aggregates lower to runtime-managed handles in linear
  memory or an equivalent handle table;
- `borrow` lowers to passing an existing handle forward without granting reuse;
- `consume` lowers to passing the handle with no later uses in the current plan,
  allowing the callee or runtime helper to recycle storage;
- product creation lowers to independent field fragments until forced to
  materialize;
- `project_field` on a split product lowers directly to the demanded fragment
  without building the full record first.

The compiler may emit helper intrinsics or imported runtime functions for these
operations. The important requirement is that ownership and demand stay explicit
until after `SOpt` decisions have been made.

## 7. Lowering Pipeline

For sealed functions, the lowering pipeline should be:

1. lower source Vox into ordinary `Vox Core`;
2. infer purity, effect boundaries, and explicit update operations;
3. run liveness and last-use analysis;
4. run backward demand analysis over tuple and record results;
5. rewrite composite producers into split-demand form where legal;
6. mark uses as `borrow` or `consume`;
7. lower sealed-core into wasm-oriented plan nodes;
8. emit final wasm plus runtime imports.

The important ordering constraint is that demand analysis must run before
materialization decisions, and last-use analysis must run before in-place reuse
decisions.

## 8. Runtime Responsibilities

The compiler is responsible for proving when a use is a last use and when a
field is undemanded within the sealed function body.

The runtime is responsible for:

- preserving value semantics when storage reuse is unsafe;
- executing `evil` operations exactly as required;
- honoring the ownership contract of the compiled plan;
- materializing composite runtime values only when the plan requires it.

If the runtime cannot safely apply a requested in-place optimization, it must
fall back to a semantics-preserving shared or copied representation.
