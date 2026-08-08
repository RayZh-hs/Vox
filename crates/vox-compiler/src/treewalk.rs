use vox_core::{diagnostics::DiagnosticBag, source::ModuleKind};

use crate::frontend::{
    FrontendUnit,
    ast::{
        CompilationUnit, FunctionDecl, ImplDecl, ImportDecl, ParamDecl, StructDecl, TopLevelItem,
        TraitDecl, ValueDecl,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreewalkScript {
    pub syntax: CompilationUnit,
    pub imports: Vec<ImportDecl>,
    pub parameters: Vec<ParamDecl>,
    pub values: Vec<ValueDecl>,
    pub functions: Vec<FunctionDecl>,
    pub structs: Vec<StructDecl>,
    pub traits: Vec<TraitDecl>,
    pub impls: Vec<ImplDecl>,
}

impl TreewalkScript {
    pub fn lower(frontend: &FrontendUnit) -> Result<Self, DiagnosticBag> {
        if !matches!(
            frontend.header.kind,
            ModuleKind::Script { .. } | ModuleKind::Package
        ) {
            return Err(DiagnosticBag::from(vec![
                vox_core::diagnostics::Diagnostic::error(
                    "tree-walk execution is only available for scripts and packages",
                ),
            ]));
        }

        let mut imports = Vec::new();
        let mut parameters = Vec::new();
        let mut values = Vec::new();
        let mut functions = Vec::new();
        let mut structs = Vec::new();
        let mut traits = Vec::new();
        let mut impls = Vec::new();

        for item in &frontend.syntax.items {
            match item {
                TopLevelItem::Import(import) => imports.extend(import.expanded()),
                TopLevelItem::Param(param) => parameters.push(param.clone()),
                TopLevelItem::Value(value) => values.push(value.clone()),
                TopLevelItem::Function(function) => functions.push(function.clone()),
                TopLevelItem::Struct(structure) => structs.push(structure.clone()),
                TopLevelItem::Trait(trait_decl) => traits.push(trait_decl.clone()),
                TopLevelItem::Impl(implementation) => impls.push(implementation.clone()),
                TopLevelItem::Statement(_) => {}
            }
        }

        Ok(Self {
            syntax: frontend.syntax.clone(),
            imports,
            parameters,
            values,
            functions,
            structs,
            traits,
            impls,
        })
    }

    pub fn has_native_declarations(&self) -> bool {
        !self.structs.is_empty() || !self.traits.is_empty() || !self.impls.is_empty()
    }
}
