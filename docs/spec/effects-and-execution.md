# Effects and Execution

This chapter defines purity, `evil`, `econ`, and the source-level execution
rules visible to Vox users.

## 1. Purity

Vox is pure by default.

A pure computation:

- may be cached;
- may be shared;
- may not perform observable side effects.

A computation that performs observable effects must be marked `evil`.

## 2. `evil` Functions

A function declaration may be prefixed with `evil`.

```ebnf
EvilFunctionDecl
  ::= VisibilityModifier? "evil" "fun" Identifier GenericParameterClause?
      "(" ParameterList? ")" ReturnTypeAnnotation? FunctionBody
```

Rules:

- an `evil fun` may perform host-visible effects such as I/O;
- purity is contagious: a function that directly performs or depends on an
  effectful computation is `evil`;
- pure code may call only pure computations unless the effect is mediated by
  `econ`.

## 3. `evil` Scripts

A script header may be either:

- `script path.to.module`; or
- `evil script path.to.module`.

An `evil script` marks the script entrypoint as effectful.

## 4. `econ`

`econ` is a built-in intrinsic that creates a pure handle to a cached snapshot
of an effectful computation.

```ebnf
EconExpr
  ::= "econ" "[" Type "]" BlockExpr
```

Example:

```vox
fun loadConfig(path: String): Econ[String] {
    econ[String] {
        readFile(path)
    }
}
```

Semantics:

- constructing or refreshing an `econ` snapshot is effectful;
- reading from an existing snapshot is pure;
- pure computations that depend on an `Econ[T]` depend on the snapshot version,
  not on re-running the effect.

`econ` does not use a Rust-style `!` sigil. It is a compiler-known intrinsic
with dedicated syntax.

Runtime support for refreshing `econ` snapshots will be implemented. The
surface syntax already exists.

## 5. Evaluation Model

Top-level values, function bodies, and scripts are evaluated on demand.

Pure results may be cached.

Effectful computations are never treated as pure cached results.

## 6. Value Semantics

Vox uses value semantics.

Consequences:

- passing a value behaves as passing an independent value;
- Vox has no user-visible reference syntax such as `&` or `mut&`;
- host values exposed to pure Vox code must behave immutably.

## 7. Local Mutation

`var` and loop reassignment are local conveniences only.

They do not create mutable shared objects or mutable references.
