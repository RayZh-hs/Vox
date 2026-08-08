# Proposal: Tiered Language Support

Currently, Vox has a single language layer, like most programming langauges. However, as a primarily embeddded language and due to its node-flow optimized nature, there are already many aspects in the langauge dedicated for it to be run on a small subset of features.

This proposal aims at two things:

1. To formalize a "tiered" language support, where in each higher tier new language features are made available to the user and the guarantees of which give rise to more aggressive optimizations.
2. To raise the upper-limit of the language to allow defining structs and traits in the native vox langauge, without needing external library bindings.

## Formal Definition of Tiers

Vox will support these tiers of language features:

| Tier | Description   | Features |
|------|---------------|----------|
| 0    | Inline Tier   | Access to visible functions, operators, values, struct, trait, creation of inline if statements (inline tier within both blocks). |
| 1    | Eval Tier     | Inline Tier + creation of blocks, values (val), structs, traits, loops and when statements, panic statements. |
| 2    | Script Tier   | Eval Tier + creation of functions, variables (var), importing of libraries and scripts as functions. |
| 3    | Dev Tier      | Script Tier + creation of libraries, definition of traits, structs and implmentation of traits for structs. |
| 4    | Debug Tier    | Dev Tier + access to private fields, functions, values, structs, and debugger mixins (:commands). |

Each tier is a superset of the previous tier, meaning that all features of a lower tier are available in higher tiers. For tiers 0, 1, 2, the language is designed to be an embedded language, and all the structs and traits can be injected into the host applciation at startup. In tier 3, the language is designed to be a full programming language, used for creating libraries and extend host applications. Tier 4 is designed for debugging and testing, and should not be used in production code.

## Vox Struct and Traits

Vox structs and traits resemble those of Rust.

## Structs

Vox structs are defined as follows:

```vox
struct Point {
    val x: Int;
    val y: Int;
    private val id: Int;

    struct fun new(x: Int, y: Int): Point {
        Point(x, y, 0)
    }

    fun id(): Int = self.id;
    private fun privateFun(): Int = self.id;
}
```

By default, all fields and functions are public, but they can be made private by using the `private` keyword. Structs can contain "struct functions" which are functions that are called on the struct itself, for instance `Point.new`. This needs to be added to external libraries as well.

Structs can contain mutable fields (variables):

```
struct MutatableStruct {
    var x: Int;

    // Mutating a variable within a struct is evil.
    evil fun setX(newX: Int) {
        self.x = newX;
    }
}
```

Reading and writing these fields are evil operations.

## Traits

Traits are defined as follows:

```vox
trait Drawable {
    val color: Color;

    struct fun new(color: Color): Drawable;
    fun draw(): PixelSeq;
    evil fun drawInContext(context: Context);
}
```

Traits can contain public fields as well as public functions (both struct functions and instance functions).

## Implementations

The "visible field" of a file consists of itself and all the public fields it imports. An implementation of a trait succeeds ONLY IF:

1. The struct implements all the public fields of the trait.
2. Functions or inherent functions (those defined in the struct itself) cover all the public functions of the trait. Note that functions whose first argument is a trait implemented by the struct do not count.

If this is the case, you can directly claim:

```vox
impl Drawable for Point;
```

If there are missing functions, you need only implement them in the visible field of the `impl` statement.

```vox
fun ...     // fulfill the missing functions
impl Drawable for Point;
```

Since such missing functions often occur, we all functions whose first argument is this struct to be defined in a trailing block of the `impl` statement:

```vox
impl Drawable for Point {
    fun draw(): PixelSeq = ...;
}
```

These two forms have no difference in the generated code.
