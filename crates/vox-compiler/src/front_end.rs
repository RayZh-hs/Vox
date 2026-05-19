mod ast;
mod lexer;
mod parser;

pub use ast::{FrontEndUnit, SurfaceParameter};

use vox_core::{diagnostics::DiagnosticBag, source::SourceText};

use self::{lexer::Lexer, parser::Parser};

pub fn analyze_source(source: &SourceText) -> Result<FrontEndUnit, DiagnosticBag> {
    let tokens = Lexer::new(&source.text, 0).lex()?;
    Parser::new(tokens).parse_unit()
}
