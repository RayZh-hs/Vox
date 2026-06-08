# Expressions

This chapter defines Vox expressions and operator precedence.

## 1. Overview

Vox is expression-oriented. Most constructs produce values.

The following constructs are expressions:

- literals;
- name references;
- calls;
- built-in intrinsic forms;
- indexing;
- field access;
- receiver-call sugar;
- unary and binary operator expressions;
- `if` expressions;
- `when` expressions;
- lambda expressions;
- block expressions.

## 2. Expression Grammar

```ebnf
Expr
  ::= LambdaExpr
   |  CoalesceExpr

LambdaExpr
  ::= LambdaParameters "->" LambdaBody

LambdaParameters
  ::= Identifier
   |  "(" LambdaParameterList? ")"

LambdaParameterList
  ::= LambdaParameter ("," LambdaParameter)* ","?

LambdaParameter
  ::= Identifier TypeAnnotation?

LambdaBody
  ::= Expr
   |  BlockExpr

CoalesceExpr
  ::= RangeExpr ("?:" CoalesceExpr)?

RangeExpr
  ::= OrExpr RangeSuffix?
   |  PrefixRangeExpr

RangeSuffix
  ::= ".." OrExpr?
   |  "..=" OrExpr

PrefixRangeExpr
  ::= ".." OrExpr?
   |  "..=" OrExpr

OrExpr
  ::= AndExpr ("||" AndExpr)*

AndExpr
  ::= EqualityExpr ("&&" EqualityExpr)*

EqualityExpr
  ::= ComparisonExpr (EqualityOp ComparisonExpr)*

EqualityOp
  ::= "==" | "!="

ComparisonExpr
  ::= AdditiveExpr (ComparisonOp AdditiveExpr)*

ComparisonOp
  ::= "<" | "<=" | ">" | ">="

AdditiveExpr
  ::= MultiplicativeExpr (AdditiveOp MultiplicativeExpr)*

AdditiveOp
  ::= "+" | "-"

MultiplicativeExpr
  ::= UnaryExpr (MultiplicativeOp UnaryExpr)*

MultiplicativeOp
  ::= "*" | "/" | "%"

UnaryExpr
  ::= UnaryOp UnaryExpr
   |  PostfixExpr

UnaryOp
  ::= "-" | "!"

PostfixExpr
  ::= PrimaryExpr PostfixOp*

PostfixOp
  ::= CallSuffix
   |  UpdatedSuffix
   |  IndexSuffix
   |  FieldSuffix
   |  SafeFieldSuffix
   |  NonNullSuffix
   |  ReceiverCallSuffix
```

## 3. Primary Expressions

```ebnf
PrimaryExpr
  ::= Literal
   |  QualifiedIdentifier
   |  ParenExpr
   |  IfExpr
   |  WhenExpr
   |  BlockExpr
   |  EconExpr
```

`ParenExpr` is defined in Chapter 2.

## 4. Postfix Forms

```ebnf
CallSuffix
  ::= "(" ArgumentList? ")"

ArgumentList
  ::= Argument ("," Argument)* ","?

Argument
  ::= Expr
   |  Identifier "=" Expr

IndexSuffix
  ::= "[" Expr "]"

FieldSuffix
  ::= "." Identifier

SafeFieldSuffix
  ::= "?." Identifier

NonNullSuffix
  ::= "!!"

ReceiverCallSuffix
  ::= ".(" QualifiedIdentifier ")" "(" ArgumentList? ")"

UpdatedCallExpr
  ::= "updated" "(" Expr "," UpdatedAssignmentList ")"

UpdatedSuffix
  ::= "." "updated" "(" UpdatedAssignmentList ")"

UpdatedAssignmentList
  ::= UpdatedAssignment ("," UpdatedAssignment)* ","?

UpdatedAssignment
  ::= UpdatedPath "=" Expr

UpdatedPath
  ::= UpdatedPathSegment ("." UpdatedPathSegment)*

UpdatedPathSegment
  ::= Identifier
   |  "#" IntegerLiteral
```

Rules:

- arguments may be positional or named;
- named arguments use `Identifier "=" Expr`;
- `updated(value, ...)` and `value.updated(...)` are compiler-known intrinsic
  forms, not ordinary function calls;
- `updated` paths use `#index` for tuple and list positions;
- `a?.b` performs nullable-safe field access;
- `a!!` asserts that `a` is non-null;
- `value.(pkg.fun)(x, y)` is sugar for `pkg.fun(value, x, y)`.

## 5. `if` Expressions

```ebnf
IfExpr
  ::= "if" "(" Expr ")" BlockExpr
      ("else" "if" "(" Expr ")" BlockExpr)*
      ("else" BlockExpr)?
```

Rules:

- `if` is an expression, not a statement;
- each branch produces a value;
- the overall type is the common type of the branch results;
- when `if` appears at the head of a statement position inside a block, it
  acts as an expression-statement without a trailing `;`. To use `if` as a
  trailing expression in that position, wrap it in parentheses:
  `(if (cond) { a } else { b })`.

## 6. `when` Expressions

`when` is used for type-based dispatch.

```ebnf
WhenExpr
  ::= "when" "(" Expr ")" "{" TypeWhenArm+ ElseArm? "}"

TypeWhenArm
  ::= "is" Type Binding? "->" (InlineExpr ";" | BlockExpr)

Binding
  ::= "as" Identifier

ElseArm
  ::= "else" "->" Expr ";"

InlineExpr
  ::= Expr
```

Rules:

- each `is` arm tests the subject against a type;
- `as Identifier` binds the refined subject value inside that arm;
- `when` does not support range matching or general pattern matching;
- an inline arm ends with `;`;
- a block arm does not use `;` after its closing `}`;
- `else` is optional;
- when `when` appears at the head of a statement position inside a block, it
  acts as an expression-statement without a trailing `;`. To use `when` as a
  trailing expression, wrap it in parentheses.

## 7. Block Expressions

```ebnf
BlockExpr
  ::= "{" BlockItem* TrailingExpr? "}"

TrailingExpr
  ::= Expr
```

A block evaluates to:

- the value of its trailing expression, if present; or
- the unit value `()`, otherwise.

`{}` is also a valid unit literal. It is equivalent to `()`.

## 8. Range Expressions

Range expressions use standard half-open and closed forms.

The range forms are:

```ebnf
RangeExpr
  ::= OrExpr ".." OrExpr
   |  OrExpr ".."
   |  ".." OrExpr
   |  ".."
   |  OrExpr "..=" OrExpr
   |  "..=" OrExpr
```

Range meanings:

- `start..end`: inclusive lower bound, exclusive upper bound;
- `start..`: inclusive lower bound with no upper bound;
- `..end`: exclusive upper bound with no lower bound;
- `..`: unbounded range;
- `start..=end`: inclusive lower bound, inclusive upper bound;
- `..=end`: inclusive upper bound with no lower bound.

## 9. Nullability Operators

`?.`, `?:`, and `!!` have the following semantics:

- `a?.b` evaluates to `null` when `a` is `null`, otherwise it evaluates to
  `a.b`;
- `a ?: b` evaluates to `a` when `a` is non-null, otherwise to `b`;
- `a!!` evaluates to `a` when `a` is non-null and fails at runtime when `a` is
  `null`.

## 10. Precedence and Associativity

From highest precedence to lowest, Vox expressions are parsed in this order:

1. postfix forms: calls, indexing, field access, safe field access, `!!`, and
   receiver-call sugar;
2. unary `-` and `!`;
3. multiplicative `*`, `/`, `%`;
4. additive `+`, `-`;
5. comparison `<`, `<=`, `>`, `>=`;
6. equality `==`, `!=`;
7. logical `&&`;
8. logical `||`;
9. ranges `..`, `..=`;
10. null coalescing `?:`;
11. lambda `->`.

Associativity rules:

- postfix operators associate left to right;
- multiplicative and additive operators associate left to right;
- comparison and equality operators associate left to right;
- `&&` and `||` associate left to right;
- `?:` associates right to left;
- function types and lambdas associate right to left.
