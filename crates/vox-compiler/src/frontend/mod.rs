pub mod ast;
pub mod lexer;
mod parser;

pub use ast::{FrontendUnit, SurfaceParameter};

use vox_core::{diagnostics::DiagnosticBag, source::SourceText};

use self::{lexer::Lexer, parser::Parser};

pub fn analyze_source(source: &SourceText) -> Result<FrontendUnit, DiagnosticBag> {
    let tokens = Lexer::new(&source.text, 0).lex()?;
    Parser::new(tokens).parse_unit()
}
