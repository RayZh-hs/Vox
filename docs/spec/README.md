# Vox Language Specification

This directory contains the standalone language specification for Vox.

The specification is organized as follows:

1. [Source Model](./source-model.md)
2. [Lexical Structure](./lexical-structure.md)
3. [Types and Declarations](./types-and-declarations.md)
4. [Expressions](./expressions.md)
5. [Statements and Control Flow](./statements-and-control-flow.md)
6. [Effects and Execution](./effects-and-execution.md)

## Status

This specification defines the current Vox surface language.

It covers:

- source files and modules;
- comments, identifiers, keywords, and literals;
- type syntax;
- declarations;
- expressions, statements, and control flow;
- purity, `evil`, and `econ`.

## Notation

Grammar productions use a lightweight EBNF style:

- quoted text denotes literal tokens;
- `A?` means an optional `A`;
- `A*` means zero or more repetitions of `A`;
- `A+` means one or more repetitions of `A`;
- parentheses group sub-productions.

Unless a chapter states otherwise, whitespace and comments may appear between
tokens.
