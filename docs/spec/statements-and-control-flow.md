# Statements and Control Flow

This chapter defines the statement forms used in Vox block expressions, as well as the limited statement forms permitted at script top level.

## 1. Statement Contexts

Statements may appear inside block expressions.

Script files may also use statements at top level, while package files may not, except for public and private, non-mutable value declarations.

## 2. Block Items

A block body is a sequence of block items followed optionally by a trailing expression.

```ebnf
BlockItem
  ::= LocalValueDecl
   |  AssignmentStatement
   |  CompoundAssignmentStatement
   |  TerminationStatement
   |  BlockStatement
   |  ExprStatement
```

```ebnf
BlockStatement
  ::= IfExpr
   |  WhenExpr
   |  ForExpr
```

```ebnf
ExprStatement
  ::= Expr ";"
```

A `BlockStatement` is a block-like expression, such as `if`, `when`, or `for`, used in statement position. It is consumed as a statement without a trailing semicolon.

All other expressions in statement position require a trailing semicolon.

The final expression in a block may be written without a semicolon. This is the block’s trailing expression.

To use a block-like expression as the trailing expression of a block in a position parsed as a statement, wrap it in parentheses:

```vox
fun describe(x: Int?): String {
    if (x == null) { return "none"; }

    (if (x > 0) { "positive" } else { "non-positive" })
}
```

## 3. Local Value Declarations

```ebnf
LocalValueDecl
  ::= "val" Identifier TypeAnnotation? "=" Expr ";"
   |  "var" Identifier TypeAnnotation? "=" Expr ";"
```

`val` introduces an immutable local binding.

`var` introduces a local binding that may be reassigned.

## 4. Assignment Statements

```ebnf
AssignmentStatement
  ::= Identifier "=" Expr ";"
```

Assignment rules:

- assignment is valid only for a previously declared `var`;
- assignment targets are identifiers only;
- field assignment and indexed assignment are not part of Vox;
- in scripts, top-level assignment may target a previously declared script top-level `var`.

## 5. Compound Assignment Statements

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

Compound assignment rules:

- compound assignment is valid only for a previously declared `var`;
- compound assignment is not valid for `val`.

## 6. `for` Expressions

`for` is an expression that evaluates to the unit value `()`.

```ebnf
ForExpr
  ::= "for" "(" ForInitSemi? (Expr | Pattern "in" Expr) ")" BlockExpr

ForInitSemi
  ::= (LocalValueDecl | AssignmentStatement | CompoundAssignmentStatement | Expr) ";"

Pattern
  ::= Identifier
```

A `for` loop consists of an optional initializer (terminated by `;`), a header
expression, and a block body. The header expression is either a loop condition
(`Expr`) or an iterator (`Pattern "in" Expr`).

### 6.1 Condition-based Forms

```vox
for (condition) {
    ...
}

for (var i = 0; i < 10) {
    ...
}
```

`for (condition) block` evaluates `condition` before each iteration. If true,
executes the body; otherwise exits (a while-style loop).

`for (init; condition) block` executes `init` once, then evaluates `condition`
before each iteration. The initializer may be a local declaration, assignment,
compound assignment, or expression. Its scope is shared with the loop body.

### 6.2 Iterator-based Forms

```vox
for (item in items) {
    ...
}

for (val low = 0; x in 0..10) {
    ...
}
```

`for (pattern in iterable) block` iterates over a list or integer range, binding
each element to `pattern` per iteration.

`for (init; pattern in iterable) block` executes `init` once, then iterates. The
initializer scope is shared with the loop body.

### 6.3 Rules

- parentheses around the loop header are required;
- the pattern must be a single identifier;
- the loop body is always a block expression;
- `break` and `continue` are valid only inside a `for` loop body.

## 7. Termination Statements

```ebnf
TerminationStatement
  ::= ReturnStatement
   |  PanicStatement
   |  BreakStatement
   |  ContinueStatement
```

A termination statement stops normal execution of the current control-flow path.

After a termination statement, no later code in the same block is executed on that path. Control-flow analysis treats such code as dead code, and optimizers may remove it.

### 7.1. `return` Statements

```ebnf
ReturnStatement
  ::= "return" Expr? ";"
```

`return` exits the innermost enclosing function.

Rules:

- `return;` returns the unit value `()`;
- `return expr;` returns the value of `expr`.

### 7.2. `panic` Statements

```ebnf
PanicStatement
  ::= "panic" StringLiteral ";"
```

`panic` raises an unrecoverable error with the given message.

The panic message is passed to the host.

### 7.3. `break` Statements

```ebnf
BreakStatement
  ::= "break" ";"
```

`break` exits the innermost enclosing `for` loop immediately.

Control resumes after the loop.

`break` is only valid inside a `for` loop body.

### 7.4. `continue` Statements

```ebnf
ContinueStatement
  ::= "continue" ";"
```

`continue` skips the rest of the current iteration.

For conditional `for` loops, control jumps to the next condition check.

For iterable `for` loops, control jumps to the next element.

`continue` is only valid inside a `for` loop body.
