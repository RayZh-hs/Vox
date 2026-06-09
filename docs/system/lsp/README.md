# LSP

`vox-lsp` is a Language Server Protocol server that provides IDE features for
Vox source files.

## Starting the Server

Build and run the server:

```sh
cargo build -p vox-lsp
./target/debug/vox-lsp
```

The server communicates over stdio using JSON-RPC. Editors with LSP support can
launch it as a language server for `.vox` files.

## What the LSP Owns

The LSP server owns editor integration:

- document synchronization (open, change, close);
- parsing and diagnostic publishing;
- text position mapping (byte offsets to line/column).

The compiler (`vox-compiler`) owns analysis:

- lexing and parsing;
- diagnostic collection.

## Current Capabilities

When the server receives a document:

1. Runs the Vox lexer and parser on the source text.
2. Converts any parse errors into editor diagnostics with source positions.

### Planned

- semantic tokens from the lexer;
- go-to-definition using the AST;
- document symbols for functions, values, and records;
- hover with inferred types via `vox-runtime` analysis;
- completions adapted from `vox-repl` completion logic;
- signature help at call sites.

## Editor Integration

### VS Code

Create a `.vscode/extensions/vox/` directory with a `package.json` that declares
the Vox language and launches `vox-lsp` as the language server.

Minimal `package.json`:

```json
{
  "name": "vox-lang",
  "contributes": {
    "languages": [
      {
        "id": "vox",
        "extensions": [".vox"],
        "aliases": ["Vox"]
      }
    ],
    "grammars": [
      {
        "language": "vox",
        "scopeName": "source.vox",
        "path": "./syntaxes/vox.tmLanguage.json"
      }
    ]
  },
  "main": "./out/extension.js",
  "activationEvents": ["onLanguage:vox"]
}
```

The extension (`extension.js`) spawns the `vox-lsp` binary and connects its
stdio as the LSP transport:

```js
const { LanguageClient } = require("vscode-languageclient/node");
const client = new LanguageClient("vox-lsp", "Vox", {
  command: "vox-lsp",
  args: [],
});
client.start();
```

Other editors (Neovim, Helix, Emacs) can be configured to launch `vox-lsp` using
their native LSP client support.

## Architecture

```
Editor (VS Code / Neovim / ...)
    │  LSP (JSON-RPC over stdio)
    ▼
vox-lsp ── vox-compiler (parse, diagnostics)
        ── vox-core (source model, text spans)
```

The LSP server is stateless across documents. Each document is parsed
independently on open or change. The server does not require a running
`vox-runtime`.
