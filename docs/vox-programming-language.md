# Vox

A high-performance, data-flow compatible programming language designed with modern syntax, optimized for vector processing and parallel execution.

## Vox Syntax Overview

I prefer to demonstrate via example. Here is what Vox syntax looks like:

```vox
// A package turns a Vox script/project into an importable library.
// Package names are stable module identifiers, not filesystem paths.
package voxini.filters;

// Imports can be from other Vox packages or host-injected ones. Traits and types are solely provided by the host.
// Imports are private by default.
import image;
import color;
import math;

// Public imports are re-exported to modules that import this file.
public import geometry;

// Vox can reference host-defined types.
// image.Image, color.Curve, geometry.Point2D, etc. are not defined in Vox.
// They are supplied by host packages through a typed manifest.

// Constants use `val`.
// Vox is value-oriented and pure by default. A `val` cannot be reassigned.
val defaultRadius: Float = 2.0;

// Type annotations are optional when the compiler can infer the type.
val defaultStrength = 0.75;

val weights: List[Float] = [0.25, 0.5, 0.25];   // Lists use bracket syntax.
val range: (Float, Float) = (0.0, 1.0);         // Tuples are lightweight product values.

// Anonymous records are allowed for local configuration.
// They are not named structs and do not define new types.
val gradePreset = {
    exposure = 1.1,
    contrast = 1.25,
    saturation = 0.95
};

// Host constructors and factory functions are called like normal functions.
// Named arguments improve readability and make node transcripts stable.
val origin = geometry.Point2D(x = 0.0, y = 0.0);

// Functions use `fun`.
// Arguments are typed. Return types are written after `:`.
// Blocks return their trailing expression. The keyword `return` is also permitted (as a syntax sugar for authors).
fun distanceFromOrigin(p: geometry.Point2D): Float {
    val dx = p.x - origin.x;
    val dy = p.y - origin.y;

    math.sqrt(dx * dx + dy * dy)
}

// Expression bodies are allowed for small functions.
fun clamp01(x: Float): Float = math.clamp(x, 0.0, 1.0);

// Functions are private by default.
// `public` exposes a function as part of the Vox package/library.
public fun normalizeStrength(strength: Float): Float {
    clamp01(strength)
}

// Generic functions may exist, but Vox does not define traits.
// This function compiles to a single node that can take any numeric type supported by the host.
// Such a node is called a "generic node".
fun mix[T: Numeric](a: T, b: T, t: Float): T {
    a * (1.0 - t) + b * t
}

// Host functions may be called with positional or named arguments.
val mixed = mix(10.0, 20.0, t = 0.25);

// The first argument of a function can be used as a receiver.
// This is only syntax sugar; `input.(image.blur)(radius)` means `image.blur(input, radius)`.
fun preview(input: image.Image): image.Image {
    input.(image.blur)(radius = defaultRadius)
}

// `if` is an expression.
fun radiusForQuality(base: Float, quality: Int): Float {
    if (quality >= 8) {
        base * 2.0
    } else if (quality >= 4) {
        base
    } else {
        base * 0.5
    }
}

// `when` is an expression for finite branching.
// Each line except `else`, if not prefixed with an identifier, is implicitly matched against `it`, which is the expression in "when".
fun qualityLabel(quality: Int): String {
    when (q: quality) {
        in 0..3 -> "draft";
        in 4..7 -> "normal";
        q in 8..10 || q == 10 -> "high";   // If the match pattern has pattern bindings, these are available in the expression body.
        else -> "custom";
    }
}

// Local mutation is allowed only for local calculation.
// It does not imply reference-mutable object semantics.
fun sum(values: List[Float]): Float {
    var total = 0.0;

    for value in values {
        total = total + value;
    }

    total
}

// Lambdas look like that in Scala..
// They are primarily for local transforms and host-provided higher-order functions.
fun soften(values: List[Float]): List[Float] {
    // All these forms are okay:
    // values.map(x -> { x  * 0.9 })
    // values.map(lambda(x: Float) { x * 0.9 }) // Lambdas can capture variables.
    values.map(x -> x * 0.9);
}

// If the function you want to chain is within a package, use a parenthesis to disambiguate the receiver.
fun gradePreview(input: image.Image): image.Image {
    input.(image.blur)(radius = 0.5)
         .(color.expose)(amount = 1.1)
         .(color.saturate)(amount = 0.95)
}

// Non-pure functions are called "evil", see the section on side effects and evil containers below.
evil fun loadTexture(path: String): image.Image {
    image.load(path)
}

public fun cinematicLook(
    input: image.Image,
    exposure: Float = 1.0,
    contrast: Float = 1.2,
    softness: Float = 0.5
): image.Image {
    val blurred = image.blur(
        input,
        radius = softness
    );

    val graded = color.grade(
        blurred,
        exposure = exposure,
        contrast = contrast,
        saturation = 0.95
    );

    // Trailing expressions are returned.
    graded
}

public fun sharpSoft(input: image.Image, amount: Float): image.Image {
    val soft = image.blur(input, radius = 0.75);
    val sharp = image.sharpen(soft, amount = amount);
    sharp
}

// General Vox helpers may be more flexible than graph-compatible transcripts.
// These functions can be inlined, compiled, or used inside expressions,
// but they may not always round-trip into clean visual nodes.
private fun adaptiveAmount(radius: Float, quality: Int): Float {
    val q = math.clamp(quality, 1, 10);

    if (q > 7) {
        radius * 0.25
    } else {
        radius * 0.1
    }
}

// Suppose that ImageFilter is a trait. Prepending `dyn` makes it a type via boxing and dynamic dispatch.
// When using dyn Trait, you may only resort to trait functions
fun runPipeline(input: image.Image, filters: List[dyn ImageFilter]): image.Image {
    filters.fold(input, (img, filter) -> {
        when (filter) {
            // Unless you use the `when` expression to dynamically dispatch on the actual type (or do trait down-casting).
            // Note that this is more expensive since it contains an indirection!
            is image.PlaceholderFilter -> {
                // Throwing does not affect purity, but it terminates the control flow.
                // Vox, by design, does not have error handling constructs, therefore use nullables for recoverable errors.
                // In the node graph, a throw displays the error message on the node and stops the execution of downstream nodes.
                panic "PlaceholderFilter is not a real filter and cannot be applied.";
            }
            else -> filter.apply(img);
        }
    })
}

// There is no reference (&, mut&) in the Vox language, since it adds implicit shared mutability.
// For functions/types injected from host, both ref and sink (move semantic in rust) are supported, and mut ref is considered evil and is generally discouraged since there lacks ways to manipulate such in a graph-based system.

// Top-level expressions are allowed in scripts and libraries. Like functions, they are executed on demand and cached.
val demoInput = image.placeholder(width = 1920, height = 1080);
public val demoOutput = autoEnhance(demoInput, quality = 9);
```

## Nullables

Vox has built-in support for nullability. Types can be annotated with `?` to indicate that they may be null. The compiler enforces null safety, preventing null dereference errors at compile time.

```vox
val users: List[User] = fetchUsersFromDatabase();
fun findUser(id: String): User? {
    users.find(user -> user.id == id)
}
```

---

```
val list = List[Big]{...}
```

---

Use `?.` to chain nullable expressions. Use `?:` to provide a default value when an expression is null.

```vox
val username: String = findUser("SomeRandomUser")?.name ?: "Unknown User";
```

The `!!` operator acts as an unwrap, asserting that the value is not null. If the value is null, it throws a runtime error.

```vox
val user: User = findUser("SomeRandomUser")!!;
```

## Side Effects and Evil Containers

Vox is derived from functional programming, and side effects/causes are explicit and controlled. For a function to have side effects, it must be annotated as being `evil`. Evil functions can perform IO operations, and they will be executed without caching. Evil functions are contagious, meaning that any function that calls an evil function is also evil.

To "break the evil spell", you can use an "evil container (econ for short)" on side causes. `Econ[T]` is a pure handle to a cached snapshot of an effectful computation. Pure code may depend on the snapshot, but only evil code or the host may refresh the snapshot.

```vox
package voxini.utils;

import std.econ;    // provides the Econ type
import std.file;

// An evil function that reads a file and returns its content as a string.
evil fun readFile(path: String): String {
    std.file.read(path)
}

// The same function wrapped in an econ to prevent eval contamination.
fun getConfigEcon(path: String): Econ[String] {
    // `econ` is a macro that wraps the effectful computation and returns an Econ key object.
    econ[String] {
        readFile(path)
    }
}

fun getConfig(key: Econ[String]): String? {
    // Accessing the value of an econ is pure and does not trigger the effect.
    key.get()
}

evil fun refreshConfig(key: Econ[String]) {
    key.refresh();
}
```

## Vox Scripts and Libraries

Vox scripts and libraries share a same system of syntax.

A file prefixed with `package` is a Vox package. It can be imported by other Vox files.

Packages may not have side effects. They are not permitted to have return values or trailing expressions.

```vox
package voxini.buildings;

import geometry;

fun wall(height: Float, width: Float): geometry.Geometry {
    geometry.rectangle(width = width, height = height)
}
```

A file prefixed with `(evil) script` is a Vox script. It can be executed as standalone programs or imported as functions by other Vox files.

Scripts can have side effects and return values (via trailing expressions or returns). They are executed on demand and cached, just like functions.
Functions and values defined in a script are private to that script and cannot be imported by other files. The main purpose of a script is to define an entrypoint for execution or document expressions, not to provide reusable code.

Scripts can use the `param` keyword to define input parameters like that in a function(node). When executed via the REPL, necessary parameters must be provided as arguments.

```vox
script voxini.demo;

import image;
param input: image.Image = image.placeholder(width = 1920, height = 1080);

input.(image.blur)(radius = 0.5)
     .(color.expose)(amount = 1.1)
     .(color.saturate)(amount = 0.95)
```

## Desugaring Variables

By design, Vox is SSA. The appearance of keyword `var` and variable reassignment is syntax sugar. At the beginning of the compilation phase all var assignments are transformed thus:

- Each `var` declaration is transformed into a `val` declaration.
- Each assignment to a `var` is transformed into a new `val` declaration with a unique name.
- Loop variables are desugared into functional folds. For example, the following code:

```
var x = 0;
for (i in 0..10) {
    x = x + i;
}
```

This is desugared into:

```
val x = range(0, 10).fold(0, (acc, i) -> acc + i);
```

## Copy, Ref, and Move Semantics

Vox has value semantics, not eager-copy semantics. Assigning or passing a value behaves as if an independent value was passed, but the runtime does not need to physically copy large data.

Small values such as `Int`, `Float`, `Bool`, and small tuples are copied directly.

Large values such as images, meshes, buffers, and tensors are passed as immutable handles. Copying one of these values copies only the handle, not the underlying data. The backing storage may be shared because Vox code cannot mutate it observably.

Vox does not expose references such as `&` or `mut&`. Host-provided large objects must behave as immutable values from Vox’s point of view. If a host operation needs mutation, it must either create a new value or mutate in place only when the runtime can prove the old value is no longer observable.

Optimization behavior is determined by the compilation mode of a function.

### No Opt (NOpt)

This mode performs no optimization beyond what is required for correctness. Large values may still be represented as handles, since that is part of the runtime representation rather than an optimization.

### Interactive Opt (IOpt)

When a user is editing a function through a node graph, the function is compiled in IOpt mode. IOpt favors fast incremental feedback. Intermediate graph results are cached and shared so that unchanged nodes do not need to be recomputed. Move-style buffer reuse is avoided because previous intermediate values may still be needed for previews, debugging, or node reuse. The results of pure functions when given input values are cached and shared, and cache aging is slowed to maximize cache hit during undo/redo.

Large values are still passed as immutable handles.

### Sealed Opt (SOpt)

When a function is no longer being edited, it may be compiled in SOpt mode. SOpt favors execution performance over editability. The compiler may use liveness analysis to internally move, consume, or reuse storage for values that are no longer needed. If a value is still needed later, the runtime must preserve value semantics by sharing immutable storage, copying only when necessary.

Move semantics in SOpt are aggressively used in SOpt.
