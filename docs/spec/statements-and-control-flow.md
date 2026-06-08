# Statements and Control Flow

This chapter defines statement forms that may appear inside block expressions.
Script files may also use assignment, compound assignment, `panic`, and
expression statements at top level. Package files may not use statements at top
level.

## 1. Block Items

```ebnf
BlockItem
  ::= LocalValueDecl
   |  AssignmentStatement
   |  CompoundAssignmentStatement
   |  ReturnStatement
   |  PanicStatement
   |  BlockStatement
   |  ExprStatement

BlockStatement
  ::= IfExpr
   |  WhenExpr
   |  ForExpr

ExprStatement
  ::= Expr ";"
```

A `BlockStatement` is a block-like expression (`if`, `when`, or `for`) that
appears at the head of a statement position inside a block. It is consumed as a
statement without a trailing `;`. The parser consumes it and the block loop
continues to the next item.

All other expressions at statement position require a `;`. The final
expression in a block is a trailing expression with no semicolon.

To use a block-like expression as a trailing expression when it would
otherwise appear at the head of a statement position, wrap it in parentheses:

```vox
fun describe(x: Int?): String {
    if (x == null) { return "none"; }

    (if (x > 0) { "positive" } else { "non-positive" })
}
```

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
- field assignment and indexed assignment are not part of Vox;
- in scripts, top-level assignment may target a previously declared script
  top-level `var`.

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

## 5. `for` Expressions

```ebnf
ForExpr
  ::= "for" "(" Pattern "in" Expr ")" BlockExpr

Pattern
  ::= Identifier
```

Rules:

- parentheses around the loop header are required;
- the current language defines only identifier loop patterns;
- the loop body is always a block expression.

`for` is an expression that evaluates to the unit value `()`.

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
