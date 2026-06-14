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

#[derive(Debug, Clone)]
pub struct FrontendAnalysis {
    pub unit: Option<FrontendUnit>,
    pub diagnostics: DiagnosticBag,
}

pub fn analyze_source_lossy(source: &SourceText) -> FrontendAnalysis {
    let (tokens, mut diagnostics) = Lexer::new(&source.text, 0).lex_lossy();
    let (unit, parse_diagnostics) = Parser::new(tokens).parse_unit_lossy();
    diagnostics.extend(parse_diagnostics.into_vec());
    FrontendAnalysis { unit, diagnostics }
}
