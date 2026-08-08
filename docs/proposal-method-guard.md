# Proposal: Method Guard

Currently there ZERO guards for the this case:

```vox
// support that Dot is a Drawable, and implements the `draw` method
fun draw(p: Dot) = p.draw();

val x = Dot.new(1, 2);
x.draw(); // ambiguous call
```

This leads to confusion. This proposition suggests that AT MOST ONE of the following could exist for type `T`:

1. A method `foo` defined in the struct `T`.
2. A function `foo` whose first argument is of type `T`.
3. A function `foo` whose first argument is of a trait that `T` implements.

This removes the ambiguity in the call, and removes the border between methods and functions cleanly.
