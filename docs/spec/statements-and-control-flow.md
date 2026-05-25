# Statements and Control Flow

This chapter defines statement forms that may appear inside block expressions.

## 1. Block Items

```ebnf
BlockItem
  ::= LocalValueDecl
   |  AssignmentStatement
   |  CompoundAssignmentStatement
   |  ForStatement
   |  ReturnStatement
   |  PanicStatement
   |  ExprStatement

ExprStatement
  ::= Expr ";"
```

The final expression in a block is a trailing expression, not an
`ExprStatement`, and it has no semicolon.

## 2. Local Value Declarations

```ebnf
LocalValueDecl
  ::= "val" Identifier TypeAnnotation? "=" Expr ";"
   |  "var" Identifier TypeAnnotation? "=" Expr ";"
```

`val` introduces an immutable local binding.

`var` introduces a local binding that may be reassigned.

## 3. Assignments

```ebnf
AssignmentStatement
  ::= Identifier "=" Expr ";"
```

Rules:

- assignment is valid only for a previously declared `var`;
- assignment targets are identifiers only;
- field assignment and indexed assignment are not part of Vox.

## 4. Compound Assignments

```ebnf
CompoundAssignmentStatement
  ::= Identifier CompoundAssignmentOp Expr ";"

CompoundAssignmentOp
  ::= "+="
   |  "-="
   |  "*="
   |  "/="
   |  "%="
```

Rules:

- compound assignment is valid only for a previously declared `var`;
- compound assignment is not valid for `val`.

## 5. `for` Statements

```ebnf
ForStatement
  ::= "for" "(" Pattern "in" Expr ")" BlockExpr

Pattern
  ::= Identifier
```

Rules:

- parentheses around the loop header are required;
- the current language defines only identifier loop patterns;
- the loop body is always a block expression.

`for` is a statement form, not an expression.

## 6. `return` Statements

```ebnf
ReturnStatement
  ::= "return" Expr? ";"
```

Rules:

- `return` exits the innermost enclosing function;
- `return;` returns the unit value `()`;
- `return expr;` returns the value of `expr`.

## 7. `panic` Statements

```ebnf
PanicStatement
  ::= "panic" StringLiteral ";"
```

`panic` raises an unrecoverable error with the given message.

The panic message is passed to the host.

## 8. Local Mutation

`var`, assignment, compound assignment, and `for` are local control-flow
features. They do not introduce shared mutable references.
