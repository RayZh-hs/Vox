# Source Model

This chapter defines the file-level structure of Vox source code.

## 1. Files

A Vox source file is exactly one of:

- a package file;
- a named script file;
- an anonymous script file.

Package files and named script files start with a file header. Anonymous script
files omit the header and begin directly with script top-level items or a
script result expression.

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

A package file declares reusable code.

```ebnf
PackageUnit
  ::= PackageHeader ";" TopLevelItem*

PackageHeader
  ::= "package" ModulePath
```

Package rules:

- a package file must not contain a top-level trailing expression;
- a package file may contain top-level `val` and `fun` declarations;
- a package file must not contain top-level `var` declarations or assignment
  statements;
- package declarations are order-independent;
- package top-level declarations are not redefinable;
- package values collide when they have the same name;
- package functions collide when their callable signatures overlap. The current
  implementation has no overload set representation, so two package functions
  with the same name are a collision;
- a package exports its `public` declarations and `public import`s;
- a package may contain `evil fun` declarations.

## 4. Script Files

A script file declares an executable entrypoint. A named script carries an
explicit module path. An anonymous script has no source-level module path.

```ebnf
ScriptUnit
  ::= NamedScriptUnit
   |  AnonymousScriptUnit

NamedScriptUnit
  ::= ScriptHeader ";" ScriptTopLevelItem* ScriptResult?

AnonymousScriptUnit
  ::= ScriptTopLevelItem* ScriptResult?

ScriptHeader
  ::= "script" ModulePath
   |  "evil" "script" ModulePath

ScriptResult
  ::= Expr
```

Script rules:

- a script may declare `param` inputs;
- a script may end with one top-level trailing expression;
- the trailing expression, when present, is the script result;
- declarations inside a script are script-local and are not importable;
- anonymous scripts cannot be imported or compiled as libraries; they can only
  be executed directly;
- anonymous scripts are pure scripts. Use `evil script ModulePath;` when the
  script entrypoint itself must be marked effectful;
- script values and statements are processed in source order;
- script value initializers and statements may reference only values already
  introduced earlier in the script;
- script function headers are visible throughout the whole script, so functions
  may be mutually recursive without forward declarations;
- script top-level values may be redefined. Later value definitions shadow
  earlier value definitions from that point onward;
- script functions may be redefined. Because script function headers are
  visible throughout the script, a later colliding function declaration replaces
  the earlier active function for that script.

## 5. Top-Level Items

After the header, a package compilation unit may contain the following
top-level items:

```ebnf
TopLevelItem
  ::= ImportDecl
   |  ValueDecl
   |  FunctionDecl
```

After the header, a script compilation unit may contain the following top-level
items:

```ebnf
ScriptTopLevelItem
  ::= ImportDecl
   |  ParamDecl
   |  ValueDecl
   |  FunctionDecl
   |  ScriptStatement

ScriptStatement
  ::= AssignmentStatement
   |  CompoundAssignmentStatement
   |  ForStatement
   |  PanicStatement
   |  ExprStatement
```

## 6. Visibility

Vox has two visibility modifiers:

- `public`
- `private`

They are mutually exclusive.

If a declaration has no visibility modifier, it is private by default.

`public` and `private` may prefix any top-level declaration form that accepts
visibility.

In packages:

- `public` exports the declaration from the package;
- `private` keeps it internal to the file.

In scripts:

- declarations remain script-local regardless of visibility spelling;
- `public` has no import/export effect.

## 7. Imports

An import declaration makes symbols from another package available in the
current module. There are three import forms:

```ebnf
ImportDecl
  ::= "import" ModulePath ";"
   |  "import" ModulePath "as" Identifier ";"
   |  "import" ModulePath "." "(" ImportItem ("," ImportItem)* ","? ")" ";"

ImportItem
  ::= Identifier
   |  Identifier "as" Identifier
```

### 7.1. Wildcard Import

A bare import makes all public symbols from the target package available. If a
symbol name is provided by exactly one imported package, it may be used
unqualified. If multiple imports provide the same name, that symbol must be used
with a qualified path.

```
import foo.bar;     // foo.bar provides baz and qux
baz();              // ok: only foo.bar provides baz
foo.bar.qux();      // always works
```

### 7.2. Module Alias

The `as` keyword creates a local alias for the module path.

```
import foo.bar as other;
other.baz();        // equivalent to foo.bar.baz()
```

Module aliases may be combined with selective imports.

```
import foo.bar as other.(baz);   // alias + selective
other.baz();                     // ok
```

### 7.3. Selective Import

A parenthesised list after a `.` imports only the named symbols, with optional
per-item aliasing.

```
import foo.bar.(baz, goo as go);
baz();              // shorthand: equivalent to foo.bar.baz
go();               // aliased: equivalent to foo.bar.goo
foo.bar.goo();      // original name still works
```

### 7.4. Public Import Aliasing

When two import paths refer to the same underlying function implementation
(e.g. a package re-exports another's symbol under the same name), they are not
considered a naming conflict for unqualified name resolution.
