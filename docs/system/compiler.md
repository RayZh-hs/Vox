# Compiler

`vox-compiler` turns Vox source into compiled metadata that `vox-runtime` can
load and execute.

This page describes what users can expect from compilation. It does not try to
document internal compiler architecture.

## What The Compiler Does

When you compile a Vox file, the compiler is responsible for:

- reading and parsing the source text;
- reporting syntax errors;
- extracting the module header and script parameters;
- classifying the module as a package, script, or `evil script`;
- assigning optimization rankings for the requested optimization level;
- producing a compiled artifact that `vox-runtime` can store and execute.

For scripts, the compiler also produces a tree-walk form used by the current
runtime.

## What You Get Back

A successful compilation produces:

- the parsed front-end representation of the source;
- a compiled artifact with module identity, parameter metadata, purity, and
  optimization rankings;
- a deferred executable plan;
- for scripts, a tree-walk form used by the runtime interpreter.

If compilation fails, you receive diagnostics instead of an artifact.

## Current Scope

The current compiler is focused on front-end analysis and runtime handoff.

You can rely on it to:

- validate the source file structure;
- parse declarations, expressions, and script parameters;
- preserve enough metadata for runtime execution and tooling;
- classify optimization intent for the module and its functions.

Full executable-plan lowering and broader optimization passes will be
implemented. At the moment, the emitted executable plan is deferred rather than
fully lowered ahead of time.

## Optimization Levels

Compilation accepts the standard Vox optimization levels. From a user perspective:

- `NOpt` is the most conservative setting;
- `IOpt` is intended for interactive work;
- `SOpt` is intended for more aggressively optimized sealed execution once that
  lowering is implemented.

The proposed, not yet implemented, optimization intent for these levels is:

- `NOpt` should stay focused on correctness-preserving cleanup with minimal
  extra work.
- `IOpt` should favors fast rebuilds and stable identities for interactive
  editing. This includes aggressive caching of intermediate results.
- `SOpt` should spend more effort to produce a leaner execution plan for sealed
  workloads. Functions returning Records will be optimized if some of what is returned
  is unused. For values, edits are done in-place if it is no longer necessary to
  preserve the original value (lifetime ends). An optmized execution policy is used
  so that SSA is taken advantage of.

Apart from SSA-specific opts, typical optimization work in this model includes dead code elimination,
constant folding, branch pruning, simplification of derived control-flow forms,
and sharing of repeated pure computations when that improves execution.

For sealed `SOpt` workloads, the compiler is also intended to become more
selective about unused tuple slots, record fields, and other intermediate
materialization. This does not change Vox source semantics. It only changes how
efficiently the runtime can execute the same program.

## Scripts And Packages

Both packages and scripts can be compiled, but the current runtime execution
path is centered on scripts.

- scripts may compile into a tree-walk form for immediate runtime execution;
- packages compile into artifacts and metadata, but package-oriented lowering
  and optimization will continue to expand.

## Diagnostics

Compiler diagnostics are the main way Vox reports source problems.

Users should expect compile-time errors for invalid syntax, malformed file
headers, and other front-end issues discovered during source analysis.
