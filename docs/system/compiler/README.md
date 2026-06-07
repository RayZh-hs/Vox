# Compiler

`vox-compiler` turns Vox source into compiled metadata that `vox-runtime` can
load and execute.

## Pipeline

Compilation runs through these phases:

1. Parse source into the frontend syntax tree.
2. Analyze the source: resolve imports, declarations, names, calls, types,
   purity, captures, and script parameters.
3. Lower executable bodies into [MIR](mir/README.md).
4. Analyze and optimize MIR according to the requested optimization level.
5. Produce a compiled artifact for the runtime.

The current runtime still executes scripts through the tree-walk path. MIR is
compiled into the artifact as the optimization and backend handoff layer, so it
can be inspected and later lowered to wasm without changing source semantics.

## Compilation Result

A successful compilation produces:

- the parsed frontend representation;
- a compiled artifact with module identity, parameter metadata, purity, and
  optimization rankings;
- MIR for executable script, function, and initializer bodies;
- an executable plan carrying MIR inspection text and optimization metadata;
- for scripts, tree-walk data for the current interpreter.

If compilation fails, the compiler returns diagnostics instead of an artifact.

## Optimization Levels

`NOpt` keeps compilation conservative:

- preserve source-shaped MIR;
- run required control-flow cleanup;
- compute binding versions and lifetimes;
- avoid expensive demand analysis.

`IOpt` is the default for interactive work:

- keep stable body, binding, block, and value identities where possible;
- cache active pure values and lowered body metadata;
- fold cheap constants;
- simplify local control flow.

`SOpt` is for sealed execution:

- run full lifetime and demand analysis;
- remove dead pure computations;
- prune unused tuple slots and record fields when demand proves they are unused;
- allow copy-on-write and slot reuse when a value's lifetime has ended;
- prepare compact MIR for backend lowering.

Runtime callers can choose the optimization level when loading a script, set a
connection or session default, or request a one-off run override. REPL sessions
use their own default and do not need to expose every sealed optimization
control.

## Runtime Contract

The compiled artifact records the requested optimization level and the rank
chosen for each executable body. The runtime executes the best representation it
supports:

- tree-walk execution remains the fallback for scripts;
- MIR metadata is available for inspection, optimization accounting, and future
  backend lowering;
- wasm lowering will consume optimized MIR directly.

Optimization must not change externally visible behavior. Pure work may be
cached, shared, folded, or removed when unused. Evil calls and other observable
effects remain ordered and are never removed only because their result is unused.
