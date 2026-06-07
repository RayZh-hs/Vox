use std::collections::BTreeSet;

use vox_compiler::frontend::{
    analyze_source,
    ast::{BlockItem, CompilationUnit, TopLevelItem},
};
use vox_core::source::SourceText;

const HIDDEN_LAST_NAME: &str = "__repl_last";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditableKey {
    Import(String),
    Symbol(String),
    Statement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditableItem {
    key: EditableKey,
    source: String,
}

pub fn build_edit_buffer(snapshot: &str, symbols: &[String]) -> Result<String, String> {
    if symbols.is_empty() {
        return Ok(String::new());
    }

    let items = parse_snapshot_items(snapshot)?;
    let wanted = symbols
        .iter()
        .map(|symbol| symbol.trim())
        .filter(|symbol| !symbol.is_empty())
        .collect::<Vec<_>>();
    let unique = wanted.iter().copied().collect::<BTreeSet<_>>();
    let mut found = BTreeSet::new();
    let mut chunks = Vec::new();

    for item in items {
        let matches = match &item.key {
            EditableKey::Import(module) => unique.contains(module.as_str()),
            EditableKey::Symbol(name) => unique.contains(name.as_str()),
            EditableKey::Statement => false,
        };
        if matches {
            match &item.key {
                EditableKey::Import(module) => {
                    found.insert(module.clone());
                }
                EditableKey::Symbol(name) => {
                    found.insert(name.clone());
                }
                EditableKey::Statement => {}
            }
            chunks.push(item.source.trim().to_owned());
        }
    }

    let missing = unique
        .into_iter()
        .filter(|name| !found.contains(*name))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "no stored definition matched {}",
            render_name_list(&missing)
        ));
    }

    Ok(chunks.join("\n\n"))
}

pub fn validate_edited_symbols(raw: &str, expected: &[String]) -> Result<(), String> {
    if expected.is_empty() {
        return Ok(());
    }

    let items = parse_fragment_items(raw)?;
    let present = items
        .into_iter()
        .filter_map(|item| match item.key {
            EditableKey::Import(module) => Some(module),
            EditableKey::Symbol(name) => Some(name),
            EditableKey::Statement => None,
        })
        .collect::<BTreeSet<_>>();
    let missing = expected
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .filter(|name| !present.contains(*name))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "edited chunk must still define {}",
            render_name_list(&missing)
        ))
    }
}

fn parse_snapshot_items(snapshot: &str) -> Result<Vec<EditableItem>, String> {
    let unit = parse_unit("<repl-snapshot>", snapshot)?;
    Ok(rebuild_items(snapshot, &unit)
        .into_iter()
        .filter(|item| !matches!(&item.key, EditableKey::Symbol(name) if name == HIDDEN_LAST_NAME))
        .collect())
}

fn parse_fragment_items(raw: &str) -> Result<Vec<EditableItem>, String> {
    let wrapped = format!("script repl.session;\n{raw}\n");
    let unit = parse_unit("<repl-edit>", &wrapped)?;
    Ok(rebuild_items(&wrapped, &unit))
}

fn parse_unit(label: &str, source: &str) -> Result<CompilationUnit, String> {
    analyze_source(&SourceText::new(label, 1, source))
        .map(|frontend| frontend.syntax)
        .map_err(|diagnostics| diagnostics.to_string())
}

fn rebuild_items(source: &str, unit: &CompilationUnit) -> Vec<EditableItem> {
    unit.items
        .iter()
        .map(|item| EditableItem {
            key: item_key(item),
            source: slice_item_source(source, item),
        })
        .collect()
}

fn item_key(item: &TopLevelItem) -> EditableKey {
    match item {
        TopLevelItem::Import(import) => EditableKey::Import(import.module.to_source_string()),
        TopLevelItem::Param(param) => EditableKey::Symbol(param.name.clone()),
        TopLevelItem::Value(value) => EditableKey::Symbol(value.name.clone()),
        TopLevelItem::Function(function) => EditableKey::Symbol(function.name.clone()),
        TopLevelItem::Statement(_) => EditableKey::Statement,
    }
}

fn slice_item_source(source: &str, item: &TopLevelItem) -> String {
    let span = match item {
        TopLevelItem::Import(import) => &import.span,
        TopLevelItem::Param(param) => &param.span,
        TopLevelItem::Value(value) => &value.span,
        TopLevelItem::Function(function) => &function.span,
        TopLevelItem::Statement(statement) => match statement {
            BlockItem::LocalValue(value) => &value.span,
            BlockItem::Assignment(assignment) => &assignment.span,
            BlockItem::CompoundAssignment(assignment) => &assignment.span,
            BlockItem::For(statement) => &statement.span,
            BlockItem::Return(statement) => &statement.span,
            BlockItem::Panic(statement) => &statement.span,
            BlockItem::Expr(expr) => &expr.span,
        },
    };
    source[span.start..span.end].to_owned()
}

fn render_name_list(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}
