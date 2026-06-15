# Compiler

`vox-compiler` turns Vox source into compiled artifacts that `vox-runtime` can
load and execute.  It also serves as a standalone CLI tool for producing wasm
and `.voxlib` files.

## CLI Usage

```text
vox-compiler [OPTIONS] FILE
```

Compile a `.vox` source file.  The output format is chosen automatically:
scripts produce raw wasm, and packages produce `.voxlib` artifacts.

| Flag | Description |
|------|-------------|
| `--mount PATH` | Mount a library directory, `.vox`, or `.voxlib` file as a dependency (repeatable) |
| `-o OUTPUT` | Output file path (default: input stem with `.wasm` or `.voxlib`) |
| `--package` | Force `.voxlib` output even for script sources |
| `-h`, `--help` | Show help message |

### Examples

```sh
# Compile a script to wasm
vox-compiler hello.vox                    # → hello.wasm

# Compile a script with library dependencies
vox-compiler --mount ./lib/ app.vox       # → app.wasm

# Compile a package to .voxlib (auto-detected)
vox-compiler mylib.vox                    # → mylib.voxlib

# Force .voxlib output
vox-compiler --package script.vox         # → script.voxlib

# Custom output path
vox-compiler -o out.wasm hello.vox
```

### Auto-detection

The compiler inspects the source to decide the output format:

- Sources starting with `package` → `.voxlib` (compiled via `compile_to_voxlib`)
- All other sources → raw wasm bytes

Use `--package` to override auto-detection and force `.voxlib` output.

### Library Mounting

`--mount PATH` supports:

- `.voxlib` files: decoded and registered in the compilation host registry.
- Directories: scanned for `.voxlib` files (non-recursive).
- `.vox` files: not supported directly; compile to `.voxlib` first.

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

Runtime callers can choose the default optimization level when loading a script,
attach per-object optimization overrides for functions, set a connection or
session default, or request a one-off run override. REPL sessions expose these
controls through `:opt get`, `:opt set`, and `:opt dump`.

When a function override requests a stronger mode than the module default, the
compiler runs the artifact's optimization pipeline at the strongest requested
mode and records the per-object requested mode and rank separately.

## Runtime Contract

The compiled artifact records the requested module optimization level,
per-function override metadata, and the rank chosen for each executable body.
The runtime executes the best representation it supports:

- tree-walk execution remains the fallback for scripts;
- MIR metadata is available for inspection, optimization accounting, and future
  backend lowering;
- wasm lowering will consume optimized MIR directly.

Optimization must not change externally visible behavior. Pure work may be
cached, shared, folded, or removed when unused. Evil calls and other observable
effects remain ordered and are never removed only because their result is unused.
