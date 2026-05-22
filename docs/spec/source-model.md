# Source Model

This chapter defines Vox source files, modules, and top-level structure.

## 1. Compilation Units

A Vox source file is a compilation unit.

Each compilation unit is exactly one of:

- a package file; or
- a script file.

The first declaration in the file is the file header.

## 2. Module Paths

A module path is a dot-separated sequence of identifiers.

```ebnf
ModulePath
  ::= Identifier ("." Identifier)*
```

Examples:

- `voxini.filters`
- `std.file`
- `demo.preview`

## 3. Package Files

A package file declares reusable code that may be imported by other Vox files.

```ebnf
PackageUnit
  ::= PackageHeader ";" TopLevelItem*

PackageHeader
  ::= "package" ModulePath
```

Rules:

- a package file must not contain a top-level trailing expression;
- a package file may contain top-level `val` initializers;
- a package exports its `public` declarations and `public import`s;
- a package may contain `evil fun` declarations, but the package itself does
  not evaluate as a top-level effectful computation.

## 4. Script Files

A script file declares an executable entrypoint.

```ebnf
ScriptUnit
  ::= ScriptHeader ";" TopLevelItem* ScriptResult?

ScriptHeader
  ::= "script" ModulePath
   |  "evil" "script" ModulePath

ScriptResult
  ::= Expr
```

Rules:

- a script may declare `param` inputs;
- a script may end with one top-level trailing expression;
- the trailing expression, when present, is the script result;
- declarations inside a script are script-local and are not importable.

## 5. Top-Level Items

After the header, a compilation unit may contain the following top-level items:

```ebnf
TopLevelItem
  ::= ImportDecl
   |  ParamDecl
   |  ValueDecl
   |  FunctionDecl
```

Additional top-level forms are not part of this specification.

## 6. Visibility

Vox has two visibility modifiers:

- `public`
- `private`

They are mutually exclusive.

If a declaration has no visibility modifier, it is private by default.

`public` and `private` may prefix any top-level declaration form defined by
this specification that accepts visibility.

In a package file:

- `public` exports the declaration from the package;
- `private` keeps it internal to the file.

In a script file:

- declarations remain script-local regardless of visibility spelling;
- `public` has no import/export effect.
