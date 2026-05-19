# Vox Documentation

This book collects the language and system documentation for Vox.

It is organized into three main areas:

- the language overview, for the broad shape and goals of Vox;
- the language specification, for the precise surface syntax and rules;
- the system design notes, for the compiler, runtime, and REPL.

The language specification is the normative source for Vox surface syntax.
Higher-level overview documents are intended to help readers navigate the
design before they need the full spec.

To build or serve the documentation locally, use `mdBook` from the `docs/`
directory:

```sh
mdbook build docs
mdbook serve docs
```
