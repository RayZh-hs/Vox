# Builtins

## Builtin Types

Builtin types come with intrinsic support in the compiler and runtime.

| Type | Description |
|------|-------------|
| `Int` | 32-bit signed integer |
| `UInt` | 32-bit unsigned integer |
| `Float` | 32-bit IEEE 754 floating point |
| `Bool` | Boolean value (`true` or `false`) |
| `String` | UTF-8 encoded string |
| `List[T]` | Ordered collection of elements of type `T` |
| `Tuple[...]` | Collection of compile-time-known fields accessed by index |
| `Record[...]` | Collection of compile-time-known fields accessed by name |

The builtin types in Vox are minimal by design. This is to ensure that the core
language remains small and easy to embed. The standard library provides additional
container types in `std.containers`, implemented in C++.

Provided by `std.containers`:

| Type | Description |
|------|-------------|
| `Map[K: Hashable, V]` | Hashmap of key-value pairs with unique keys |
| `Set[T: Hashable]` | Collection of unique elements of type `T` |

## Builtin Methods

Design for builtin methods on primitive and collection types.
Inspired by Kotlin's standard library but slightly less fluent.

Signatures are of the same form as regular functions, first parameter being the receiver.

### Design approach: mixed prelude + intrinsics

Vox resolves `value.method(args)` as syntactic sugar for `method(value, args)`.
Resolution order:

1. **Fields** — record field access (existing)
2. **Local functions** — functions in scope whose first parameter type matches the receiver (existing)
3. **Imports** — imported functions with matching first-param type (existing)
4. **Prelude** — `std` module functions, always implicitly available (**new**)
5. **Trait methods** — trait impls from external `.voxlib` packages (existing)

The prelude is a synthetic module `std`. Its function *signatures* are always
in scope for type-checking, so `42.toString()` resolves to
`std::toString(self: Int): String` without an explicit import.

Implementation splits into two categories:

- **Builtin**: Cannot be expressed in user-level Vox. Needs intrinsic support
  in the tree-walk interpreter, MIR executor, and WASM backend (e.g., type
  conversions, string parsing, collection size).
- **Prelude**: Expressible as a regular Vox function in the `std` module,
  built on top of other builtins or language constructs (e.g., `isEmpty` is
  just `length() == 0`).

All methods are **pure** unless noted.

### Int

| Method | Signature | Impl | Description |
|--------|-----------|------|-------------|
| `toString` | `(Int) -> String` | Builtin | Decimal string representation |
| `toFloat` | `(Int) -> Float` | Builtin | Widening conversion |
| `toUInt` | `(Int) -> UInt?` | Builtin | Convert non-negative values to unsigned; `null` if negative |

### UInt

| Method | Signature | Impl | Description |
|--------|-----------|------|-------------|
| `toString` | `(UInt) -> String` | Builtin | Decimal string representation |
| `toFloat` | `(UInt) -> Float` | Builtin | Widening conversion |
| `toInt` | `(UInt) -> Int?` | Builtin | Convert to signed; `null` if > `Int.max` |

#### Float

| Method | Signature | Impl | Description |
|--------|-----------|------|-------------|
| `toString` | `(Float) -> String` | Builtin | Decimal string representation |
| `toInt` | `(Float) -> Int?` | Builtin | Truncate toward zero; `null` if out of `Int` range |
| `round` | `(Float) -> Int` | Builtin | Round to nearest integer, half up |
| `floor` | `(Float) -> Int` | Builtin | Largest integer ≤ value |
| `ceil` | `(Float) -> Int` | Builtin | Smallest integer ≥ value |

### Bool

| Method | Signature | Impl | Description |
|--------|-----------|------|-------------|
| `toString` | `(Bool) -> String` | Builtin | `"true"` / `"false"` |

### String

| Method | Signature | Impl | Description |
|--------|-----------|------|-------------|
| `length` | `(String) -> Int` | Builtin | Number of characters (UTF-8 aware) |
| `isEmpty` | `(String) -> Bool` | Prelude | `length() == 0` |
| `toInt` | `(String) -> Int?` | Builtin | Parse decimal integer; `null` on failure |
| `toFloat` | `(String) -> Float?` | Builtin | Parse decimal float; `null` on failure |
| `startsWith` | `(String, prefix: String) -> Bool` | Builtin | Whether string starts with prefix |
| `endsWith` | `(String, suffix: String) -> Bool` | Builtin | Whether string ends with suffix |
| `contains` | `(String, sub: String) -> Bool` | Builtin | Whether string contains substring |
| `indexOf` | `(String, sub: String) -> Int?` | Builtin | First index of substring; `null` if not found |
| `substring` | `(String, start: Int, end: Int) -> String` | Builtin | Extract characters `[start, end)` |
| `replace` | `(String, old: String, new: String) -> String` | Builtin | Replace all occurrences of `old` with `new` |
| `split` | `(String, delim: String) -> List[String]` | Builtin | Split by delimiter |
| `toLower` | `(String) -> String` | Builtin | Convert to lowercase |
| `toUpper` | `(String) -> String` | Builtin | Convert to uppercase |
| `trim` | `(String) -> String` | Builtin | Remove leading and trailing whitespace |
| `repeat` | `(String, n: Int) -> String` | Builtin | Repeat the string `n` times |

### List[T]

| Method | Signature | Impl | Description |
|--------|-----------|------|-------------|
| `length` | `(List[T]) -> Int` | Builtin | Number of elements |
| `size` | `(List[T]) -> Int` | Prelude | Alias for `length()` |
| `isEmpty` | `(List[T]) -> Bool` | Prelude | `length() == 0` |
| `first` | `(List[T]) -> T?` | Prelude | First element; `null` if empty |
| `last` | `(List[T]) -> T?` | Prelude | Last element; `null` if empty |
| `get` | `(List[T], index: Int) -> T?` | Prelude | Safe indexed access; `null` if out of bounds |
| `contains` | `(List[T], element: T) -> Bool` | Prelude | Whether element is in the list |
| `indexOf` | `(List[T], element: T) -> Int?` | Prelude | First index of element; `null` if not found |
| `slice` | `(List[T], from: Int, to: Int) -> List[T]` | Prelude | Sub-list `[from, to)` |
| `reversed` | `(List[T]) -> List[T]` | Prelude | New list in reverse order |

### Econ[T]

| Method | Signature | Impl | Purity | Description |
|--------|-----------|------|--------|-------------|
| `update` | `(Econ[T]) -> T` | Builtin | Evil | Re-run the captured `econ` block, replace the snapshot, and return the refreshed value |

### `std.containers`

`std.containers` is an ordinary standard-library module, not part of the
implicit prelude. Its functions become available through normal imports. Because
method syntax is receiver-call sugar, importing `std.containers.toSet` makes
`items.toSet()` resolve to `std.containers.toSet(items)`.

Pure containers in `std.containers` use value semantics. Transforming them
returns a new container and does not mutate the receiver.

`Varargs[T]` is treated as a special syntax that takes zero or more arguments of type `T` in the parenthesis and converts them into a fat pointer to be passed to the function. It is only valid in external libraries and is not a first-class type in Vox since it requires handling of raw pointers. Only the final parameter of a function can be a varargs.

#### Container Creation Functions

These functions are ordinary functions in `std.containers`; they do not use
receiver syntax because they do not have a receiver.

| Function | Signature | Purity | Description |
|----------|-----------|--------|-------------|
| `emptyMap` | `[K: Hashable, V]() -> Map[K, V]` | Pure | Create an empty immutable map |
| `mapOf` | `(Varargs[(K: Hashable, V)]) -> Map[K, V]` | Pure | Build an immutable map from key-value pairs; later pairs replace earlier pairs with the same key |
| `emptySet` | `[T: Hashable]() -> Set[T]` | Pure | Create an empty immutable set |
| `setOf` | `(Varargs[T: Hashable]) -> Set[T]` | Pure | Build an immutable set from values |

#### List Transfer Functions

These functions are provided by `std.containers`, but their receiver is the
builtin `List[T]`.

| Method | Signature | Purity | Description |
|--------|-----------|--------|-------------|
| `toSet` | `(List[T: Hashable]) -> Set[T]` | Pure | Build a set from list items |
| `toMap` | `(List[(K: Hashable, V)]) -> Map[K, V]` | Pure | Build a map from key-value pairs; later pairs replace earlier pairs with the same key |

#### Map[K: Hashable, V]

| Method | Signature | Purity | Description |
|--------|-----------|--------|-------------|
| `length` | `(Map[K, V]) -> Int` | Pure | Number of key-value pairs |
| `size` | `(Map[K, V]) -> Int` | Pure | Alias for `length()` |
| `isEmpty` | `(Map[K, V]) -> Bool` | Pure | `length() == 0` |
| `containsKey` | `(Map[K, V], key: K) -> Bool` | Pure | Whether the key is present |
| `containsValue` | `(Map[K, V], value: V) -> Bool` | Pure | Whether any entry has the value |
| `get` | `(Map[K, V], key: K) -> V?` | Pure | Safe key lookup; `null` if absent |
| `getOrDefault` | `(Map[K, V], key: K, default: V) -> V` | Pure | Key lookup with fallback |
| `put` | `(Map[K, V], key: K, value: V) -> Map[K, V]` | Pure | New map with the key set to value |
| `remove` | `(Map[K, V], key: K) -> Map[K, V]` | Pure | New map without the key |
| `merge` | `(Map[K, V], other: Map[K, V]) -> Map[K, V]` | Pure | New map containing both maps; `other` wins on duplicate keys |
| `keys` | `(Map[K, V]) -> Set[K]` | Pure | Set of keys |
| `keysList` | `(Map[K, V]) -> List[K]` | Pure | List of keys in implementation iteration order |
| `values` | `(Map[K, V]) -> List[V]` | Pure | List of values in implementation iteration order |
| `entries` | `(Map[K, V]) -> List[(K, V)]` | Pure | List of key-value pairs in implementation iteration order |
| `toList` | `(Map[K, V]) -> List[(K, V)]` | Pure | Alias for `entries()` |
| `toSet` | `(Map[K: Hashable, V: Hashable]) -> Set[(K, V)]` | Pure | Set of key-value pairs |

#### Set[T: Hashable]

| Method | Signature | Purity | Description |
|--------|-----------|--------|-------------|
| `length` | `(Set[T]) -> Int` | Pure | Number of elements |
| `size` | `(Set[T]) -> Int` | Pure | Alias for `length()` |
| `isEmpty` | `(Set[T]) -> Bool` | Pure | `length() == 0` |
| `contains` | `(Set[T], value: T) -> Bool` | Pure | Whether the value is present |
| `add` | `(Set[T], value: T) -> Set[T]` | Pure | New set containing the value |
| `remove` | `(Set[T], value: T) -> Set[T]` | Pure | New set without the value |
| `union` | `(Set[T], other: Set[T]) -> Set[T]` | Pure | Set union |
| `intersect` | `(Set[T], other: Set[T]) -> Set[T]` | Pure | Set intersection |
| `difference` | `(Set[T], other: Set[T]) -> Set[T]` | Pure | Values present in the receiver and absent from `other` |
| `toList` | `(Set[T]) -> List[T]` | Pure | List of values in implementation iteration order |
| `toMap` | `(Set[(K: Hashable, V: Hashable)]) -> Map[K, V]` | Pure | Build a map from key-value pairs; later pairs replace earlier pairs with the same key |

## Implementation notes

### Prelude module (`std.prelude`)

The prelude is a synthetic module. It is *not* persisted as a `.vox` file;
its function declarations exist only as metadata consulted during method
resolution. This avoids circularity (the prelude needs `List[T]` and `String`,
which are defined by the language itself).

Prelude functions that can be expressed in Vox (e.g., `List.isEmpty`) are
registered as synthetic method metadata and implemented by the same runtime
builtin dispatch as intrinsic methods for now. They are not separately
importable as `std.isEmpty` — they exist only for method-style resolution on
their receiver types.

### `std.containers` module

`std.containers` is distributed as a standard library package. Its types and
functions are visible only through normal imports, but imported first-parameter
functions participate in receiver syntax through the existing method resolution
step for imports.

Pure `Map` and `Set` functions are host-backed but must be declared pure in the
package manifest and must expose value semantics.

### Builtin implementations

Builtin methods need support in all three execution paths:

1. **Tree-walk interpreter** (`crates/vox-runtime/src/interpreter.rs`):
   Handle in `eval_method_call` before the field-access fallback. Match on
   `(Value variant, method_name)` and execute directly.

2. **MIR executor** (`crates/vox-runtime/src/mir_executor.rs`):
   Emit `MirOpKind::Call` targeting a synthetic builtin callee name (e.g.,
   `__builtin__Int::toString`). The MIR executor handles these as special
   cases.

3. **WASM backend** (`crates/vox-compiler/src/backend/wasm.rs`):
   Synthetic builtin callees route through the `__vox_op` runtime builtin
   table. The WASM executor implements primitive conversions, string
   operations, and list operations from that dispatch point.

### Semantic analysis

`resolve_method_name_type` in `analysis.rs` gains a new step between imports
and trait methods that checks the builtin method registry. The registry maps
`(ReplType, method_name) → ReplType` using a static table keyed by type tag
and method name.
