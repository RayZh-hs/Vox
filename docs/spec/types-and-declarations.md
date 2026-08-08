# Types and Declarations

This chapter defines Vox type syntax and top-level declarations.

## 1. Types

```ebnf
Type
  ::= FunctionType

FunctionType
  ::= NullableType
   |  "(" TypeList? ")" "->" Type

TypeList
  ::= Type ("," Type)* ","?

NullableType
  ::= PrimaryType ("?")?

PrimaryType
  ::= NamedType
   |  DynType
   |  GroupedType
   |  TupleType
   |  RecordType

GroupedType
  ::= "(" Type ")"

NamedType
  ::= QualifiedIdentifier TypeArgumentClause?

QualifiedIdentifier
  ::= Identifier ("." Identifier)*

TypeArgumentClause
  ::= "[" Type ("," Type)* ","? "]"

DynType
  ::= "dyn" QualifiedIdentifier

TupleType
  ::= "(" ")"
   |  "(" Type "," ")"
   |  "(" Type "," Type ("," Type)* ","? ")"

RecordType
  ::= "{" (RecordTypeField ("," RecordTypeField)* ","?)? "}"

RecordTypeField
  ::= Identifier ":" Type
```

Rules:

- `T?` denotes the nullable form of `T`;
- `()` is the unit type;
- `Unit` is equivalent to the zero-element tuple type `()`;
- `{}` is also equivalent to `Unit` in type positions;
- `(A, B) -> C` denotes a function that takes `A` and `B` and returns `C`;
- function types are right-associative;
- record types are structural and anonymous;
- a record literal may appear either where its type is inferred or where an
  explicit record type annotation is present.

## 2. Predefined Types

The following predefined scalar types are available:

- `Int`
- `Float`
- `Bool`
- `String`
- `Unit`

The following type forms are built into the language:

- nullable types;
- tuple types;
- record types;
- function types;
- dynamic trait types introduced by `dyn`.

`List[T]` is a predefined generic type constructor.

`Econ[T]` is a predefined generic type constructor. `Econ[T]` values expose
`update(self: Econ[T]) -> T` as a built-in effectful method.

## 3. Generic Parameter Clauses

```ebnf
GenericParameterClause
  ::= "[" GenericParameter ("," GenericParameter)* ","? "]"

GenericParameter
  ::= TypeParameter ":" TraitBound

TypeParameter
  ::= Identifier

TraitBound
  ::= Identifier
```

Rules:

- each generic parameter has exactly one trait bound;
- bounds are named trait constraints;
- user-authored trait declarations are not available in Vox.

Examples:

```vox
fun mix[T: Numeric](a: T, b: T, t: Float): T = a;
fun pair[A: Show, B: Show](a: A, b: B): (A, B) = (a, b);
```

## 4. Native Structs and Traits

Native structs and traits are available at their declared tier. Struct fields
and methods are public by default and accept `private` for debug-only access.

```ebnf
StructDecl ::= VisibilityModifier? "struct" Identifier "{" StructMember* "}"
StructMember ::= VisibilityModifier? ("val" | "var") Identifier ":" Type ";"
               | VisibilityModifier? "struct"? EvilModifier? "fun" Identifier
                 "(" ParameterList? ")" ReturnTypeAnnotation? FunctionBody
TraitDecl ::= VisibilityModifier? "trait" Identifier "{" TraitMember* "}"
ImplDecl ::= "impl" QualifiedIdentifier "for" QualifiedIdentifier
             (";" | "{" FunctionDecl* "}")
```

An instance method receives an implicit `self` parameter. `struct fun` declares
an associated function and does not receive `self`. A trait implementation must
provide every public trait field and method, either in the struct, in the
implementation block, or through a visible function with the same receiver.

## 5. Imports

```ebnf
ImportDecl
  ::= VisibilityModifier? "import" ModulePath ";"
```

Rules:

- an import makes the package available for qualified access under the final
  module-path segment (for example, `import foo.bar; bar.baz()`); `import foo;`
  remains addressable as `foo.bar()`;
- `public import` re-exports the imported package from a package file;
- `private import` is permitted but equivalent to an omitted visibility
  modifier.

Selective imports, nested selective imports, and explicit module/item aliasing
are supported by the frontend and runtime. An explicit module alias replaces
the implicit final segment binding.

## 6. Script Parameters

```ebnf
ParamDecl
  ::= "param" Identifier ":" Type DefaultValue? ";"

DefaultValue
  ::= "=" Expr
```

Rules:

- `param` is valid only in scripts;
- script parameters define the script entrypoint inputs;
- a parameter with a default value may be omitted by the caller.

## 7. Value Declarations

```ebnf
ValueDecl
  ::= VisibilityModifier? ImmutableValueDecl
   |  VisibilityModifier? MutableValueDecl

ImmutableValueDecl
  ::= "val" Identifier TypeAnnotation? "=" Expr ";"

MutableValueDecl
  ::= "var" Identifier TypeAnnotation? "=" Expr ";"

TypeAnnotation
  ::= ":" Type
```

Rules:

- `val` declares an immutable binding;
- `var` declares a reassignable binding;
- an omitted type annotation is inferred from the initializer;
- package top-level value declarations must use `val`;
- script top-level and local value declarations may use either `val` or `var`.

## 8. Function Declarations

```ebnf
FunctionDecl
  ::= VisibilityModifier? EvilModifier? "fun" Identifier GenericParameterClause?
      "(" ParameterList? ")" ReturnTypeAnnotation? FunctionBody

EvilModifier
  ::= "evil"

ParameterList
  ::= Parameter ("," Parameter)* ","?

Parameter
  ::= Identifier ":" Type DefaultValue?

ReturnTypeAnnotation
  ::= ":" Type

FunctionBody
  ::= "=" Expr ";"
   |  BlockExpr
```

Rules:

- a function is pure unless it is marked `evil`;
- parameters are ordered from left to right;
- default parameter values are part of the function signature;
- an expression body and a block body are semantically equivalent;
- package functions are order-independent and may not collide with another
  package function whose callable signature can match the same call;
- script function headers are visible throughout the whole script, including
  before the function body appears in source order.

## 9. Visibility Modifiers

```ebnf
VisibilityModifier
  ::= "public"
   |  "private"
```

If a declaration omits visibility, it is private.
