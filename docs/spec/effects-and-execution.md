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
  `econ`;
- an effectful operation must be executed again when its containing computation
  is evaluated, even if its explicit Vox arguments are unchanged;
- marking a function `evil` does not make every operation inside it uncached.
  Pure subcomputations inside an evil function may still be cached, shared,
  folded, or removed according to their own pure inputs and demand.

## 3. `evil` Scripts

A script header may be either:

- `script path.to.module`; or
- `evil script path.to.module`.

An `evil script` marks the script entrypoint as effectful.

Scripts that omit the header are anonymous pure scripts. They are executable
directly, but they cannot be imported or compiled as libraries. Use a named
`evil script` header when the script entrypoint itself must be effectful.

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

`Econ[T]` exposes one built-in method:

```vox
evil fun update(self: Econ[T]): T
```

`snapshot.update()` re-runs the block captured by the original `econ[T]`
expression, stores the refreshed snapshot in `snapshot`, and returns the new
value.

## 5. Evaluation Model

Package top-level values and function bodies are evaluated on demand.

Scripts are evaluated in source order. A script top-level `val` binds the value
produced at that point in execution. It does not create a live alias to another
binding.

Pure results may be cached.

Effectful computations are never treated as pure cached results.

Pure computations inside an effectful computation remain pure. If an evil
operation produces the same value as in a previous evaluation, downstream pure
work may reuse cached results for that value.

When a package artifact changes, cached package values from the previous
artifact may be discarded. Precise dependency invalidation is an implementation
optimization, not a user-visible semantic rule.

## 6. Value Semantics

Vox uses value semantics.

Consequences:

- passing a value behaves as passing an independent value;
- Vox has no user-visible reference syntax such as `&` or `mut&`;
- host values exposed to pure Vox code must behave immutably.

## 7. Local Mutation

`var` and loop reassignment are local execution conveniences only.

They do not create mutable shared objects or mutable references.

Top-level `var` is valid in scripts because the script top level is an
execution-local scope. Top-level `var` is not valid in packages.
