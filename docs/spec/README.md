# Vox Language Specification

Use this section when you need exact Vox syntax or precise semantic rules.

Chapters:

1. [Source Model](./source-model.md)
2. [Lexical Structure](./lexical-structure.md)
3. [Types and Declarations](./types-and-declarations.md)
4. [Expressions](./expressions.md)
5. [Statements and Control Flow](./statements-and-control-flow.md)
6. [Effects and Execution](./effects-and-execution.md)

This specification describes the current Vox surface language. One runtime
feature is still incomplete: refreshing `econ` snapshots will be implemented.

## Notation

Grammar examples use a lightweight EBNF style:

- quoted text means a literal token;
- `A?` means optional;
- `A*` means zero or more;
- `A+` means one or more;
- parentheses group sub-productions.

Unless a chapter says otherwise, whitespace and comments may appear between
tokens.
