# Vox Language Specification

This directory contains the standalone language specification for Vox.

The specification is organized as follows:

1. [01 Source Model](./01-source-model.md)
2. [02 Lexical Structure](./02-lexical-structure.md)
3. [03 Types and Declarations](./03-types-and-declarations.md)
4. [04 Expressions](./04-expressions.md)
5. [05 Statements and Control Flow](./05-statements-and-control-flow.md)
6. [06 Effects and Execution](./06-effects-and-execution.md)
7. [07 Sealed Lowering](./07-sealed-lowering.md)

## Status

This specification defines the current Vox surface language.

It covers:

- source files and modules;
- comments, identifiers, keywords, and literals;
- type syntax;
- declarations;
- expressions, statements, and control flow;
- purity, `evil`, and `econ`;
- sealed-function lowering for `SOpt`.

## Notation

Grammar productions use a lightweight EBNF style:

- quoted text denotes literal tokens;
- `A?` means an optional `A`;
- `A*` means zero or more repetitions of `A`;
- `A+` means one or more repetitions of `A`;
- parentheses group sub-productions.

Unless a chapter states otherwise, whitespace and comments may appear between
tokens.
