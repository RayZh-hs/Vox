use vox_core::{diagnostics::DiagnosticBag, source::ModuleKind};

use crate::frontend::{
    FrontendUnit,
    ast::{CompilationUnit, FunctionDecl, ImportDecl, ParamDecl, TopLevelItem, ValueDecl},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreewalkScript {
    pub syntax: CompilationUnit,
    pub imports: Vec<ImportDecl>,
    pub parameters: Vec<ParamDecl>,
    pub values: Vec<ValueDecl>,
    pub functions: Vec<FunctionDecl>,
}

impl TreewalkScript {
    pub fn lower(frontend: &FrontendUnit) -> Result<Self, DiagnosticBag> {
        if !matches!(frontend.header.kind, ModuleKind::Script { .. }) {
            return Err(DiagnosticBag::from(vec![
                vox_core::diagnostics::Diagnostic::error(
                    "tree-walk execution is only available for scripts",
                ),
            ]));
        }

        let mut imports = Vec::new();
        let mut parameters = Vec::new();
        let mut values = Vec::new();
        let mut functions = Vec::new();

        for item in &frontend.syntax.items {
            match item {
                TopLevelItem::Import(import) => imports.push(import.clone()),
                TopLevelItem::Param(param) => parameters.push(param.clone()),
                TopLevelItem::Value(value) => values.push(value.clone()),
                TopLevelItem::Function(function) => functions.push(function.clone()),
                TopLevelItem::Statement(_) => {}
            }
        }

        Ok(Self {
            syntax: frontend.syntax.clone(),
            imports,
            parameters,
            values,
            functions,
        })
    }
}
