# Overview

Vox is a value-oriented language for reusable packages and executable scripts.
It is pure by default, keeps side effects explicit, and aims to stay readable in
small files.

This page is a quick guide. Use the specification for exact grammar and full
semantic rules.

## What A File Can Be

Every Vox file starts with one header:

- `package demo.math;` for reusable code
- `script demo.main;` for an executable entrypoint
- `evil script demo.main;` for an executable entrypoint that may perform side effects

Packages may be imported by other Vox files. Scripts may declare `param`
inputs and may end with one trailing expression that becomes the script result.
Declarations inside scripts are local to that script.

## Basic Declarations

```vox
package demo.math;

public import math;

val defaultScale = 2.0;

public fun clamp01(x: Float): Float {
    if (x < 0.0) {
        0.0
    } else if (x > 1.0) {
        1.0
    } else {
        x
    }
}

fun scale(x: Float, factor: Float = defaultScale): Float = x * factor;
```

Rules to remember:

- `val` creates an immutable binding.
- `var` allows local reassignment inside a block or script.
- `fun` declares a function.
- `public` exports a package declaration or re-exports an import.
- declarations are private by default.
- function parameters and return types use `name: Type`.
- default argument values use `=`.
- packages are order-independent declaration graphs with top-level `val` and
  `fun` declarations;
- scripts execute in source order, except that script function headers are
  visible throughout the script.

The exact modules, types, and host functions available to `import` depend on
the host application embedding Vox.

## Expressions And Control Flow

Blocks return their last expression, so small functions often read naturally:

```vox
fun sum(values: List[Float]): Float {
    var total = 0.0;

    for (value in values) {
        total += value;
    }

    total
}
```

Common expression forms:

- `if` is an expression.
- `return` is available when an early exit is clearer.
- lambdas use `x -> x * 2` or `(x: Float) -> x * 2`.
- tuples use `(a, b)`.
- lists use `[1, 2, 3]`.
- records use `{ name = "vox", version = 1 }`.
- `value.updated(field = next)` copies an immutable value with selected changes.

```vox
val config = {
    cache: { enabled: true, ttlSeconds: 60 },
    retries: [1, 2, 3],
};

val next = config.updated(cache.ttlSeconds = 120, retries.#1 = 5);
```

## Nullability

Nullable types use `?`:

```vox
fun findUser(id: Int): { name: String }? {
    if (id == 1) {
        { name = "vox" }
    } else {
        null
    }
}

val name = findUser(2)?.name ?: "unknown";
```

Useful operators:

- `?.` accesses a nullable receiver safely.
- `?:` provides a fallback when the left side is `null`.
- `!!` unwraps a nullable value and fails at runtime if it is `null`.

## Effects And `econ`

Pure code is the default. Mark a function `evil` when it performs observable
side effects such as I/O.

```vox
evil fun readText(path: String): String {
    host.readText(path)
}

fun cachedText(path: String): Econ[String] {
    econ[String] {
        readText(path)
    }
}
```

`econ[T] { ... }` is a built-in intrinsic that creates a pure handle to a
cached snapshot of an effectful computation. Pure code can pass the handle
around without re-running the effect.

Runtime support for refreshing `econ` snapshots will be implemented. For now,
the language syntax is available, but refresh tooling should be treated as
incomplete.

## Scripts

Scripts use the same declaration syntax as packages, plus `param` inputs and an
optional trailing result expression.

```vox
script demo.main;

param value: Float;
param factor: Float = 2.0;

fun scale(x: Float): Float = x * factor;

scale(value)
```

Script values and statements are processed in source order:

```vox
script demo.counter;

var b = 1;
val a = b;
b = 2;

a
```

This script returns `1`, because `a` receives the value of `b` at the point
where `a` is declared. It is not a live alias to `b`.

Script functions are visible throughout the script:

```vox
script demo.functions;

val total = even(4) + odd(3);

fun even(value: Int): Int = value;
fun odd(value: Int): Int = value;

total
```

Use scripts for entrypoints and one-off execution. Use packages for code you
want to import elsewhere.

## Documentation Comments

Vox uses `///` for documentation comments, similar to Rust. Doc comments
annotate declarations and are shown in editor hover:

```vox
/// Computes the greatest common divisor of two integers.
/// Uses the Euclidean algorithm.
fun gcd(a: Int, b: Int): Int {
    if (b == 0) {
        a
    } else {
        gcd(b, a % b)
    }
}

/// The default scaling factor.
val defaultScale: Float = 2.0;   /// Applied to all coordinate-transforms.
```

Doc comments come in two forms:
- **Head docstrings**: `///` lines that appear directly before a declaration
  (`val`, `var`, `fun`, `import`, `param`). These describe the declaration they
  precede.
- **Body docstrings**: `///` inside a function body provide additional
  documentation for the function. A `///` on the same line as a value
  declaration (after the `;`) is a body docstring for that value.

A `package` or `script` header may also be preceded by `///` lines to document
the module:

```vox
/// Geometry utilities for 2D and 3D coordinate transforms.
package geo.transform;
```

**Important**: Every `///` comment must annotate either a `val`/`var`, `fun`,
`import`, `param`, or the `package`/`script` header. The language server will
raise a warning if a doc comment is not attached to any declaration.
