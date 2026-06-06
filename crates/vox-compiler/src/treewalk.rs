use vox_core::{diagnostics::DiagnosticBag, source::ModuleKind};

use crate::front_end::{
    FrontEndUnit,
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
    pub fn lower(front_end: &FrontEndUnit) -> Result<Self, DiagnosticBag> {
        if !matches!(front_end.header.kind, ModuleKind::Script { .. }) {
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

        for item in &front_end.syntax.items {
            match item {
                TopLevelItem::Import(import) => imports.push(import.clone()),
                TopLevelItem::Param(param) => parameters.push(param.clone()),
                TopLevelItem::Value(value) => values.push(value.clone()),
                TopLevelItem::Function(function) => functions.push(function.clone()),
                TopLevelItem::Statement(_) => {}
            }
        }

        Ok(Self {
            syntax: front_end.syntax.clone(),
            imports,
            parameters,
            values,
            functions,
        })
    }
}
