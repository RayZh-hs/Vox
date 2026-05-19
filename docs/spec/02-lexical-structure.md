# 02 Lexical Structure

This chapter defines comments, identifiers, keywords, operators, and literals.

## 1. Whitespace

Whitespace separates tokens where needed.

Whitespace includes:

- spaces;
- horizontal tabs;
- line feeds;
- carriage returns.

Whitespace is otherwise insignificant.

## 2. Comments

Vox supports three comment forms:

```ebnf
LineComment
  ::= "//" <all characters up to line end>

DocComment
  ::= "///" <all characters up to line end>

BlockComment
  ::= "/*" <comment text> "*/"
```

Rules:

- `//` introduces an ordinary line comment;
- `///` introduces a documentation comment;
- `/* ... */` introduces a block comment;
- a documentation comment documents the declaration that immediately follows it;
- comments may appear wherever whitespace may appear.

## 3. Identifiers

Vox identifiers are ASCII-only.

```ebnf
Identifier
  ::= IdentifierStart IdentifierContinue*

IdentifierStart
  ::= "_" | [a-zA-Z]

IdentifierContinue
  ::= "_" | [a-zA-Z0-9]
```

Examples of valid identifiers:

- `x`
- `_tmp`
- `Point2D`

Examples of invalid identifiers:

- `2d`
- `blur-radius`
- `with space`

## 4. Keywords

The following words are reserved keywords:

- `as`
- `dyn`
- `econ`
- `else`
- `evil`
- `false`
- `for`
- `fun`
- `if`
- `import`
- `in`
- `is`
- `null`
- `package`
- `panic`
- `param`
- `private`
- `public`
- `return`
- `script`
- `true`
- `val`
- `var`
- `when`

## 5. Operators and Punctuation

The language uses the following operators and punctuation:

```text
( ) [ ] { }
, . : ; ? -> => 
+ - * / % ! 
 = += -= *= /= %=
== != < <= > >=
&& || 
?. ?: !!
.. ..=
```

The `=>` token is reserved and has no meaning in this specification.

## 6. Literals

```ebnf
Literal
  ::= IntegerLiteral
   |  FloatLiteral
   |  StringLiteral
   |  InterpolatedStringLiteral
   |  BooleanLiteral
   |  NullLiteral
   |  ListLiteral
   |  TupleLiteral
   |  RecordLiteral
```

### 6.1 Numeric Literals

```ebnf
Digit
  ::= [0-9]

HexDigit
  ::= [0-9a-fA-F]

DigitSeq
  ::= Digit ("_"? Digit)*

IntegerLiteral
  ::= DigitSeq

FloatLiteral
  ::= DigitSeq "." DigitSeq ExponentPart?
   |  DigitSeq ExponentPart

ExponentPart
  ::= ["eE"] ["+-"]? DigitSeq
```

Rules:

- numeric separators are permitted between digits;
- exponent notation is permitted only for floating-point literals.

### 6.2 String Literals

```ebnf
StringLiteral
  ::= "\"" StringPart* "\""

InterpolatedStringLiteral
  ::= "\"" InterpolatedStringPart* "\""

StringPart
  ::= EscapeSequence
   |  StringChar

InterpolatedStringPart
  ::= EscapeSequence
   |  InterpolationSequence
   |  StringChar

StringChar
  ::= any Unicode scalar value except `"`, `\`, `$`, LF, CR

InterpolationSequence
  ::= "$" Identifier
   |  "${" Expr "}"

EscapeSequence
  ::= "\\" (
          "\""
        | "\\"
        | "$"
        | "n"
        | "r"
        | "t"
        | UnicodeEscape
      )

UnicodeEscape
  ::= "u" "{" HexDigit HexDigit? HexDigit? HexDigit? HexDigit? HexDigit? "}"
```

Rules:

- raw string literals are not part of Vox;
- a string that contains interpolation uses the interpolated form;
- both plain and interpolated string literals produce values of type `String`.

### 6.3 Boolean and Null Literals

```ebnf
BooleanLiteral
  ::= "true" | "false"

NullLiteral
  ::= "null"
```

### 6.4 Collection and Aggregate Literals

```ebnf
ListLiteral
  ::= "[" (Expr ("," Expr)* ","?)? "]"

TupleLiteral
  ::= "(" ")"
   |  "(" Expr "," ")"
   |  "(" Expr "," Expr ("," Expr)* ","? ")"

ParenExpr
  ::= "(" Expr ")"

RecordLiteral
  ::= "{" "}"
   |  "{" RecordFieldInit "," "}"
   |  "{" RecordFieldInit ("," RecordFieldInit)+ ","? "}"

RecordFieldInit
  ::= Identifier "=" Expr
   |  Identifier ":" Type "=" Expr
```

Rules:

- `()` is the unit literal;
- `{}` is also a unit literal;
- a single parenthesized expression without a comma is not a tuple literal;
- a single braced field without a comma is not a record literal;
- record literal keys are constant identifier names;
- `name = expr` initializes a field from a value expression;
- `name: Type = expr` initializes a field from a value expression with an
  explicit field type annotation.

## 7. Statement Terminators

Declarations and simple statements terminate with `;`.

A block expression does not require a trailing semicolon after its closing `}`.

The final trailing expression in a block or script does not use `;`.
