use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use vox_compiler::frontend;
use vox_core::builtins::BuiltinReceiver;
use vox_core::diagnostics::DiagnosticBag;
use vox_core::host::PackageManifest;
use vox_core::source::SourceText;

pub struct VoxLanguageServer {
    client: Client,
    documents: Mutex<HashMap<Url, String>>,
    libraries: Mutex<LspLibraries>,
}

#[derive(Debug, Clone, Default)]
struct LspLibraries {
    manifests: Vec<PackageManifest>,
    load_errors: Vec<String>,
}

impl VoxLanguageServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Mutex::new(HashMap::new()),
            libraries: Mutex::new(LspLibraries::default()),
        }
    }
}

fn collect_diagnostics(
    path: &str,
    source: &str,
    libraries: &LspLibraries,
) -> Option<Vec<Diagnostic>> {
    let source_text = SourceText::new(path, 0, source);
    let analysis = frontend::analyze_source_lossy(&source_text);

    let mut diagnostics = convert_diagnostics(&source_text.text, analysis.diagnostics);
    if let Some(unit) = analysis.unit.as_ref() {
        diagnostics.extend(collect_semantic_diagnostics(
            source,
            unit,
            &libraries.manifests,
        ));
    }

    diagnostics.extend(collect_library_load_diagnostics(source, libraries));
    let doc_warnings = collect_docstring_warnings(source);
    diagnostics.extend(doc_warnings);

    Some(diagnostics)
}

fn convert_diagnostics(source: &str, bag: DiagnosticBag) -> Vec<Diagnostic> {
    bag.into_vec()
        .into_iter()
        .map(|diag| convert_diagnostic(source, diag))
        .collect()
}

fn collect_semantic_diagnostics(
    source: &str,
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    manifests: &[PackageManifest],
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    match vox_runtime::infer_environment(&unit.syntax, manifests) {
        Ok(env) => {
            for warning in &env.warnings {
                let span = unit.syntax.span.clone();
                diagnostics.push(Diagnostic {
                    range: byte_span_to_range(source, span.start, span.end),
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("vox".to_string()),
                    message: warning.clone(),
                    ..Default::default()
                });
            }
        }
        Err(message) => {
            let span = semantic_error_span(source, unit, &message)
                .unwrap_or_else(|| unit.syntax.span.clone());
            diagnostics.push(Diagnostic {
                range: byte_span_to_range(source, span.start, span.end),
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("vox".to_string()),
                message,
                ..Default::default()
            });
        }
    }
    diagnostics
}

fn collect_library_load_diagnostics(source: &str, libraries: &LspLibraries) -> Vec<Diagnostic> {
    libraries
        .load_errors
        .iter()
        .map(|message| Diagnostic {
            range: byte_span_to_range(source, 0, source.len().min(1)),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("vox-lsp".to_string()),
            message: message.clone(),
            ..Default::default()
        })
        .collect()
}

fn load_libraries(paths: &[PathBuf]) -> LspLibraries {
    let mut runtime = vox_runtime::Runtime::default();
    let mut load_errors = Vec::new();

    for path in paths {
        if !path.exists() {
            load_errors.push(format!("library path `{}` does not exist", path.display()));
            continue;
        }

        let result = if path.is_dir() {
            mount_lsp_library_dir(&mut runtime, path)
        } else {
            mount_lsp_library_file(&mut runtime, path).map(|_| ())
        };

        if let Err(error) = result {
            load_errors.push(format!(
                "failed to load library `{}`: {error}",
                path.display()
            ));
        }
    }

    LspLibraries {
        manifests: runtime.package_manifests(),
        load_errors,
    }
}

fn mount_lsp_library_dir(
    runtime: &mut vox_runtime::Runtime,
    dir: &Path,
) -> std::result::Result<(), String> {
    let mut files = Vec::new();
    collect_library_files(dir, &mut files)?;
    files.sort();

    for file in files {
        mount_lsp_library_file(runtime, &file)?;
    }

    Ok(())
}

fn collect_library_files(dir: &Path, files: &mut Vec<PathBuf>) -> std::result::Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("failed to read directory {}: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("directory read error: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_library_files(&path, files)?;
        } else if is_lsp_library_file(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_lsp_library_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("vox") | Some("voxlib")
    )
}

fn mount_lsp_library_file(
    runtime: &mut vox_runtime::Runtime,
    path: &Path,
) -> std::result::Result<vox_core::ids::LibraryId, String> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("vox") => runtime.mount_vox_file(path),
        Some("voxlib") => runtime.mount_voxlib_file(path),
        other => Err(format!(
            "unsupported library file extension for `{}`: {:?}",
            path.display(),
            other
        )),
    }
}

fn library_paths_from_initialize(params: &InitializeParams) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(options) = &params.initialization_options {
        collect_library_paths_from_value(options, &mut paths);
    }

    if let Ok(env_paths) = std::env::var("VOX_LSP_LIBRARIES") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        for item in env_paths
            .split(separator)
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            paths.push(PathBuf::from(item));
        }
    }

    paths
}

fn collect_library_paths_from_value(value: &serde_json::Value, paths: &mut Vec<PathBuf>) {
    match value {
        serde_json::Value::String(path) => paths.push(PathBuf::from(path)),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_library_paths_from_value(item, paths);
            }
        }
        serde_json::Value::Object(map) => {
            for key in [
                "libraries",
                "libraryPaths",
                "library_paths",
                "voxLibraries",
                "vox_libraries",
            ] {
                if let Some(value) = map.get(key) {
                    collect_library_paths_from_value(value, paths);
                }
            }
            for key in ["library", "libraryPath", "library_path", "path"] {
                if let Some(serde_json::Value::String(path)) = map.get(key) {
                    paths.push(PathBuf::from(path));
                }
            }
        }
        _ => {}
    }
}

fn semantic_error_span(
    source: &str,
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    message: &str,
) -> Option<vox_core::diagnostics::TextSpan> {
    if let Some(name) = message
        .strip_prefix("Unknown type ")
        .map(|rest| rest.trim_end_matches('.'))
    {
        return type_name_span(unit, name).or_else(|| identifier_substring_span(source, name));
    }

    if let Some(package) = message
        .strip_prefix("imported package `")
        .and_then(|rest| rest.split_once('`').map(|(name, _)| name))
        .or_else(|| {
            message
                .strip_prefix("package `")
                .and_then(|rest| rest.split_once('`').map(|(name, _)| name))
        })
    {
        if let Some(span) = import_module_span(unit, package) {
            return Some(span);
        }
        return source_substring_span(source, package);
    }

    if let Some(name) = message
        .strip_prefix("unknown qualified name `")
        .and_then(|rest| rest.split_once('`').map(|(name, _)| name))
    {
        return qualified_name_span(source, unit, name)
            .or_else(|| source_substring_span(source, name));
    }

    if let Some(name) = message
        .strip_prefix("unknown name `")
        .and_then(|rest| rest.split_once('`').map(|(name, _)| name))
        .or_else(|| {
            message
                .strip_prefix("unknown local `")
                .and_then(|rest| rest.split_once('`').map(|(name, _)| name))
        })
        .or_else(|| {
            message
                .strip_prefix("ambiguous name `")
                .and_then(|rest| rest.split_once('`').map(|(name, _)| name))
        })
    {
        let spans = collect_references(unit, source, name);
        if let Some(span) = spans.into_iter().next() {
            return Some(span);
        }
        return identifier_substring_span(source, name);
    }

    if let Some(name) = assignability_subject_name(message) {
        return declaration_name_span(source, unit, name)
            .or_else(|| identifier_substring_span(source, name));
    }

    None
}

fn assignability_subject_name(message: &str) -> Option<&str> {
    message
        .strip_prefix("value `")
        .and_then(|rest| rest.split_once('`').map(|(name, _)| name))
        .or_else(|| {
            message
                .strip_prefix("local `")
                .and_then(|rest| rest.split_once('`').map(|(name, _)| name))
        })
        .or_else(|| {
            message
                .strip_prefix("parameter `")
                .and_then(|rest| rest.split_once('`').map(|(name, _)| name))
        })
        .or_else(|| {
            message
                .strip_prefix("function `")
                .and_then(|rest| rest.split_once('`').map(|(name, _)| name))
        })
        .filter(|_| message.contains("not assignable"))
}

fn type_name_span(
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    name: &str,
) -> Option<vox_core::diagnostics::TextSpan> {
    use vox_compiler::frontend::ast::*;

    fn visit_type(ty: &TypeSyntax, name: &str) -> Option<vox_core::diagnostics::TextSpan> {
        match &ty.kind {
            TypeKind::Function { parameters, result } => parameters
                .iter()
                .find_map(|parameter| visit_type(parameter, name))
                .or_else(|| visit_type(result, name)),
            TypeKind::Nullable(inner) | TypeKind::Grouped(inner) => visit_type(inner, name),
            TypeKind::Named {
                name: type_name,
                arguments,
            } => {
                if type_name.to_source_string() == name {
                    return Some(type_name.span.clone());
                }
                arguments
                    .iter()
                    .find_map(|argument| visit_type(argument, name))
            }
            TypeKind::Dyn(type_name) => {
                if type_name.to_source_string() == name {
                    Some(type_name.span.clone())
                } else {
                    None
                }
            }
            TypeKind::Tuple(items) => items.iter().find_map(|item| visit_type(item, name)),
            TypeKind::Record(fields) => fields.iter().find_map(|field| visit_type(&field.ty, name)),
        }
    }

    fn visit_expr(expr: &Expr, name: &str) -> Option<vox_core::diagnostics::TextSpan> {
        match &expr.kind {
            ExprKind::String(literal) => literal.parts.iter().find_map(|part| match part {
                StringPart::Text(_) => None,
                StringPart::Interpolation(expr) => visit_expr(expr, name),
            }),
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                items.iter().find_map(|item| visit_expr(item, name))
            }
            ExprKind::Record(fields) => fields.iter().find_map(|field| {
                field
                    .ty
                    .as_ref()
                    .and_then(|ty| visit_type(ty, name))
                    .or_else(|| visit_expr(&field.value, name))
            }),
            ExprKind::Call { callee, arguments } => visit_expr(callee, name).or_else(|| {
                arguments.iter().find_map(|argument| match argument {
                    Argument::Positional(expr) => visit_expr(expr, name),
                    Argument::Named { value, .. } => visit_expr(value, name),
                })
            }),
            ExprKind::Intrinsic(IntrinsicExpr::Updated(updated)) => {
                visit_expr(&updated.target, name).or_else(|| {
                    updated
                        .updates
                        .iter()
                        .find_map(|update| visit_expr(&update.value, name))
                })
            }
            ExprKind::Intrinsic(IntrinsicExpr::Econ(econ)) => {
                visit_type(&econ.ty, name).or_else(|| visit_block(&econ.body, name))
            }
            ExprKind::Index { target, index } => {
                visit_expr(target, name).or_else(|| visit_expr(index, name))
            }
            ExprKind::Field { target, .. }
            | ExprKind::SafeField { target, .. }
            | ExprKind::NonNull { target } => visit_expr(target, name),
            ExprKind::ReceiverCall {
                receiver,
                arguments,
                ..
            } => visit_expr(receiver, name).or_else(|| {
                arguments.iter().find_map(|argument| match argument {
                    Argument::Positional(expr) => visit_expr(expr, name),
                    Argument::Named { value, .. } => visit_expr(value, name),
                })
            }),
            ExprKind::Unary { expr, .. } => visit_expr(expr, name),
            ExprKind::Binary { left, right, .. } => {
                visit_expr(left, name).or_else(|| visit_expr(right, name))
            }
            ExprKind::Range(range) => range
                .start
                .as_ref()
                .and_then(|expr| visit_expr(expr, name))
                .or_else(|| range.end.as_ref().and_then(|expr| visit_expr(expr, name))),
            ExprKind::If(if_expr) => if_expr
                .branches
                .iter()
                .find_map(|branch| {
                    visit_expr(&branch.condition, name).or_else(|| visit_block(&branch.body, name))
                })
                .or_else(|| {
                    if_expr
                        .else_branch
                        .as_ref()
                        .and_then(|branch| visit_block(branch, name))
                }),
            ExprKind::When(when_expr) => visit_expr(&when_expr.subject, name)
                .or_else(|| {
                    when_expr.arms.iter().find_map(|arm| {
                        visit_type(&arm.ty, name).or_else(|| visit_expr(&arm.body, name))
                    })
                })
                .or_else(|| {
                    when_expr
                        .else_arm
                        .as_ref()
                        .and_then(|expr| visit_expr(expr, name))
                }),
            ExprKind::For(for_expr) => for_expr
                .init
                .as_ref()
                .and_then(|item| visit_block_item(item, name))
                .or_else(|| match &for_expr.header {
                    ForHeader::In { iterable, .. } => visit_expr(iterable, name),
                    ForHeader::Condition(condition) => visit_expr(condition, name),
                })
                .or_else(|| visit_block(&for_expr.body, name)),
            ExprKind::Lambda(lambda) => lambda
                .parameters
                .iter()
                .find_map(|parameter| parameter.ty.as_ref().and_then(|ty| visit_type(ty, name)))
                .or_else(|| visit_expr(&lambda.body, name)),
            ExprKind::Block(block) => visit_block(block, name),
            ExprKind::Integer(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::Name(_) => None,
        }
    }

    fn visit_block(block: &BlockExpr, name: &str) -> Option<vox_core::diagnostics::TextSpan> {
        block
            .items
            .iter()
            .find_map(|item| visit_block_item(item, name))
            .or_else(|| {
                block
                    .trailing
                    .as_ref()
                    .and_then(|expr| visit_expr(expr, name))
            })
    }

    fn visit_block_item(item: &BlockItem, name: &str) -> Option<vox_core::diagnostics::TextSpan> {
        match item {
            BlockItem::LocalValue(value) => value
                .ty
                .as_ref()
                .and_then(|ty| visit_type(ty, name))
                .or_else(|| visit_expr(&value.initializer, name)),
            BlockItem::Assignment(value) => visit_expr(&value.value, name),
            BlockItem::CompoundAssignment(value) => visit_expr(&value.value, name),
            BlockItem::Return(value) => {
                value.value.as_ref().and_then(|expr| visit_expr(expr, name))
            }
            BlockItem::Panic(value) => value.message.parts.iter().find_map(|part| match part {
                StringPart::Text(_) => None,
                StringPart::Interpolation(expr) => visit_expr(expr, name),
            }),
            BlockItem::BlockStatement(expr) | BlockItem::Expr(expr) => visit_expr(expr, name),
            BlockItem::Break(_) | BlockItem::Continue(_) => None,
        }
    }

    unit.syntax.items.iter().find_map(|item| match item {
        TopLevelItem::Param(parameter) => visit_type(&parameter.ty, name),
        TopLevelItem::Value(value) => value
            .ty
            .as_ref()
            .and_then(|ty| visit_type(ty, name))
            .or_else(|| visit_expr(&value.initializer, name)),
        TopLevelItem::Function(function) => function
            .parameters
            .iter()
            .find_map(|parameter| visit_type(&parameter.ty, name))
            .or_else(|| {
                function
                    .return_type
                    .as_ref()
                    .and_then(|ty| visit_type(ty, name))
            })
            .or_else(|| visit_expr(&function.body, name)),
        TopLevelItem::Statement(item) => visit_block_item(item, name),
        TopLevelItem::Import(_) => None,
    })
}

fn declaration_name_span(
    source: &str,
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    name: &str,
) -> Option<vox_core::diagnostics::TextSpan> {
    use vox_compiler::frontend::ast::*;

    fn visit_expr(
        source: &str,
        expr: &Expr,
        name: &str,
    ) -> Option<vox_core::diagnostics::TextSpan> {
        match &expr.kind {
            ExprKind::String(literal) => literal.parts.iter().find_map(|part| match part {
                StringPart::Text(_) => None,
                StringPart::Interpolation(expr) => visit_expr(source, expr, name),
            }),
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                items.iter().find_map(|item| visit_expr(source, item, name))
            }
            ExprKind::Record(fields) => fields
                .iter()
                .find_map(|field| visit_expr(source, &field.value, name)),
            ExprKind::Call { callee, arguments } => {
                visit_expr(source, callee, name).or_else(|| {
                    arguments.iter().find_map(|argument| match argument {
                        Argument::Positional(expr) => visit_expr(source, expr, name),
                        Argument::Named { value, .. } => visit_expr(source, value, name),
                    })
                })
            }
            ExprKind::Intrinsic(IntrinsicExpr::Updated(updated)) => {
                visit_expr(source, &updated.target, name).or_else(|| {
                    updated
                        .updates
                        .iter()
                        .find_map(|update| visit_expr(source, &update.value, name))
                })
            }
            ExprKind::Intrinsic(IntrinsicExpr::Econ(econ)) => visit_block(source, &econ.body, name),
            ExprKind::Index { target, index } => {
                visit_expr(source, target, name).or_else(|| visit_expr(source, index, name))
            }
            ExprKind::Field { target, .. }
            | ExprKind::SafeField { target, .. }
            | ExprKind::NonNull { target } => visit_expr(source, target, name),
            ExprKind::ReceiverCall {
                receiver,
                arguments,
                ..
            } => visit_expr(source, receiver, name).or_else(|| {
                arguments.iter().find_map(|argument| match argument {
                    Argument::Positional(expr) => visit_expr(source, expr, name),
                    Argument::Named { value, .. } => visit_expr(source, value, name),
                })
            }),
            ExprKind::Unary { expr, .. } => visit_expr(source, expr, name),
            ExprKind::Binary { left, right, .. } => {
                visit_expr(source, left, name).or_else(|| visit_expr(source, right, name))
            }
            ExprKind::Range(range) => range
                .start
                .as_ref()
                .and_then(|expr| visit_expr(source, expr, name))
                .or_else(|| {
                    range
                        .end
                        .as_ref()
                        .and_then(|expr| visit_expr(source, expr, name))
                }),
            ExprKind::If(if_expr) => if_expr
                .branches
                .iter()
                .find_map(|branch| {
                    visit_expr(source, &branch.condition, name)
                        .or_else(|| visit_block(source, &branch.body, name))
                })
                .or_else(|| {
                    if_expr
                        .else_branch
                        .as_ref()
                        .and_then(|branch| visit_block(source, branch, name))
                }),
            ExprKind::When(when_expr) => visit_expr(source, &when_expr.subject, name)
                .or_else(|| {
                    when_expr
                        .arms
                        .iter()
                        .find_map(|arm| visit_expr(source, &arm.body, name))
                })
                .or_else(|| {
                    when_expr
                        .else_arm
                        .as_ref()
                        .and_then(|expr| visit_expr(source, expr, name))
                }),
            ExprKind::For(for_expr) => for_expr
                .init
                .as_ref()
                .and_then(|item| visit_block_item(source, item, name))
                .or_else(|| match &for_expr.header {
                    ForHeader::In { iterable, .. } => visit_expr(source, iterable, name),
                    ForHeader::Condition(condition) => visit_expr(source, condition, name),
                })
                .or_else(|| visit_block(source, &for_expr.body, name)),
            ExprKind::Lambda(lambda) => visit_expr(source, &lambda.body, name),
            ExprKind::Block(block) => visit_block(source, block, name),
            ExprKind::Integer(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::Name(_) => None,
        }
    }

    fn visit_block(
        source: &str,
        block: &BlockExpr,
        name: &str,
    ) -> Option<vox_core::diagnostics::TextSpan> {
        block
            .items
            .iter()
            .find_map(|item| visit_block_item(source, item, name))
            .or_else(|| {
                block
                    .trailing
                    .as_ref()
                    .and_then(|expr| visit_expr(source, expr, name))
            })
    }

    fn visit_block_item(
        source: &str,
        item: &BlockItem,
        name: &str,
    ) -> Option<vox_core::diagnostics::TextSpan> {
        match item {
            BlockItem::LocalValue(value) if value.name == name => {
                find_identifier_span_in(source, &value.span, name)
            }
            BlockItem::LocalValue(value) => visit_expr(source, &value.initializer, name),
            BlockItem::Assignment(value) => visit_expr(source, &value.value, name),
            BlockItem::CompoundAssignment(value) => visit_expr(source, &value.value, name),
            BlockItem::Return(value) => value
                .value
                .as_ref()
                .and_then(|expr| visit_expr(source, expr, name)),
            BlockItem::Panic(value) => value.message.parts.iter().find_map(|part| match part {
                StringPart::Text(_) => None,
                StringPart::Interpolation(expr) => visit_expr(source, expr, name),
            }),
            BlockItem::BlockStatement(expr) | BlockItem::Expr(expr) => {
                visit_expr(source, expr, name)
            }
            BlockItem::Break(_) | BlockItem::Continue(_) => None,
        }
    }

    unit.syntax.items.iter().find_map(|item| match item {
        TopLevelItem::Param(parameter) if parameter.name == name => {
            find_identifier_span_in(source, &parameter.span, name)
        }
        TopLevelItem::Value(value) if value.name == name => {
            find_identifier_span_in(source, &value.span, name)
        }
        TopLevelItem::Function(function) if function.name == name => {
            find_identifier_span_in(source, &function.span, name)
        }
        TopLevelItem::Function(function) => visit_expr(source, &function.body, name),
        TopLevelItem::Statement(item) => visit_block_item(source, item, name),
        TopLevelItem::Import(_) | TopLevelItem::Param(_) | TopLevelItem::Value(_) => None,
    })
}

fn import_module_span(
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    package: &str,
) -> Option<vox_core::diagnostics::TextSpan> {
    use vox_compiler::frontend::ast::TopLevelItem;

    unit.syntax.items.iter().find_map(|item| match item {
        TopLevelItem::Import(import) if import.module.to_source_string() == package => {
            Some(import.module.span.clone())
        }
        _ => None,
    })
}

fn qualified_name_span(
    source: &str,
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    name: &str,
) -> Option<vox_core::diagnostics::TextSpan> {
    source_substring_span(&source[unit.syntax.span.start..unit.syntax.span.end], name).map(|span| {
        vox_core::diagnostics::TextSpan {
            start: span.start + unit.syntax.span.start,
            end: span.end + unit.syntax.span.start,
        }
    })
}

fn source_substring_span(source: &str, needle: &str) -> Option<vox_core::diagnostics::TextSpan> {
    source
        .find(needle)
        .map(|start| vox_core::diagnostics::TextSpan {
            start,
            end: start + needle.len(),
        })
}

fn identifier_substring_span(
    source: &str,
    needle: &str,
) -> Option<vox_core::diagnostics::TextSpan> {
    let mut search_start = 0usize;
    while let Some(relative) = source[search_start..].find(needle) {
        let start = search_start + relative;
        let end = start + needle.len();
        let before = start
            .checked_sub(1)
            .and_then(|idx| source.as_bytes().get(idx))
            .copied();
        let after = source.as_bytes().get(end).copied();
        let before_ok = before.is_none_or(|b| !is_ident_byte(b));
        let after_ok = after.is_none_or(|b| !is_ident_byte(b));
        if before_ok && after_ok {
            return Some(vox_core::diagnostics::TextSpan { start, end });
        }
        search_start = end;
    }
    None
}

fn collect_docstring_warnings(source: &str) -> Vec<Diagnostic> {
    use vox_compiler::frontend::lexer::{Lexer, TokenKind};

    let tokens = match Lexer::new(source, 0).lex() {
        Ok(tokens) => tokens,
        Err(_) => return Vec::new(),
    };

    let source_text = SourceText::new("", 0, source);
    let unit = frontend::analyze_source_lossy(&source_text).unit;

    let mut warnings = Vec::new();
    let mut depth = 0u32;
    let mut i = 0;

    while i < tokens.len() {
        let token = &tokens[i];
        match &token.kind {
            TokenKind::DocComment(_) => {
                if is_valid_doc_comment(source, &tokens, i, depth, &unit) {
                    i += 1;
                    continue;
                }
                let range = byte_span_to_range(source, token.span.start, token.span.end);
                warnings.push(Diagnostic {
                    range,
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("vox".to_string()),
                    message: "doc comment is not attached to a declaration".to_string(),
                    ..Default::default()
                });
            }
            TokenKind::LBrace => depth += 1,
            TokenKind::RBrace => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }

    warnings
}

fn is_valid_doc_comment(
    source: &str,
    tokens: &[vox_compiler::frontend::lexer::Token],
    pos: usize,
    depth: u32,
    unit: &Option<vox_compiler::frontend::ast::FrontendUnit>,
) -> bool {
    use vox_compiler::frontend::lexer::TokenKind;

    if depth > 0 {
        return true;
    }

    if let Some(unit) = unit {
        let span = &tokens[pos].span;
        for item in &unit.syntax.items {
            if is_doc_inside_span(span, &item_span(item)) {
                return true;
            }
        }
        if is_doc_inside_span(span, &unit.header.span) {
            return true;
        }
        for item in &unit.syntax.items {
            if matches!(
                item,
                vox_compiler::frontend::ast::TopLevelItem::Value(_)
                    | vox_compiler::frontend::ast::TopLevelItem::Param(_)
            ) {
                let item_span = item_span(item);
                if span.start >= item_span.end
                    && is_same_source_line(source, item_span.end, span.start)
                {
                    return true;
                }
            }
        }
        if pos == 0 && !unit.header.anonymous {
            return true;
        }
    }

    let mut j = pos + 1;
    while j < tokens.len() {
        match &tokens[j].kind {
            TokenKind::DocComment(_) | TokenKind::LBrace | TokenKind::RBrace => {
                j += 1;
                continue;
            }
            TokenKind::Package
            | TokenKind::Script
            | TokenKind::Evil
            | TokenKind::Import
            | TokenKind::Val
            | TokenKind::Var
            | TokenKind::Fun
            | TokenKind::Param
            | TokenKind::Public
            | TokenKind::Private => return true,
            _ => return false,
        }
    }

    false
}

fn item_span(item: &vox_compiler::frontend::ast::TopLevelItem) -> vox_core::diagnostics::TextSpan {
    use vox_compiler::frontend::ast::*;
    match item {
        TopLevelItem::Import(i) => i.span.clone(),
        TopLevelItem::Param(p) => p.span.clone(),
        TopLevelItem::Value(v) => v.span.clone(),
        TopLevelItem::Function(f) => f.span.clone(),
        TopLevelItem::Statement(s) => block_item_span(s),
    }
}

fn is_doc_inside_span(
    doc_span: &vox_core::diagnostics::TextSpan,
    decl_span: &vox_core::diagnostics::TextSpan,
) -> bool {
    doc_span.start >= decl_span.start && doc_span.end <= decl_span.end
}

fn is_same_source_line(source: &str, left: usize, right: usize) -> bool {
    let start = left.min(right).min(source.len());
    let end = left.max(right).min(source.len());
    !source[start..end].contains('\n')
}

fn convert_diagnostic(source: &str, diag: vox_core::diagnostics::Diagnostic) -> Diagnostic {
    let severity = match diag.severity {
        vox_core::diagnostics::Severity::Note => DiagnosticSeverity::INFORMATION,
        vox_core::diagnostics::Severity::Warning => DiagnosticSeverity::WARNING,
        vox_core::diagnostics::Severity::Error => DiagnosticSeverity::ERROR,
    };

    let range = diag
        .span
        .map(|span| byte_span_to_range(source, span.start, span.end))
        .unwrap_or_default();

    Diagnostic {
        range,
        severity: Some(severity),
        source: Some("vox".to_string()),
        message: diag.message,
        ..Default::default()
    }
}

fn byte_span_to_range(source: &str, start: usize, end: usize) -> Range {
    let start = byte_offset_to_position(source, start);
    let end = byte_offset_to_position(source, end);
    Range { start, end }
}

fn byte_offset_to_position(source: &str, offset: usize) -> Position {
    let offset = offset.min(source.len());
    let prefix = &source[..offset];
    let line = prefix.chars().filter(|&c| c == '\n').count() as u32;
    let last_newline = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = prefix[last_newline..].chars().count() as u32;
    Position { line, character }
}

fn position_to_byte_offset(source: &str, position: Position) -> usize {
    let mut current_line = 0u32;
    let mut current_char = 0u32;
    for (idx, c) in source.char_indices() {
        if current_line == position.line && current_char == position.character {
            return idx;
        }
        if c == '\n' {
            current_line += 1;
            current_char = 0;
        } else {
            current_char += 1;
        }
    }
    source.len()
}

fn segment_at_offset(
    _source: &str,
    qname: &vox_compiler::frontend::ast::QualifiedName,
    offset: usize,
) -> Option<String> {
    if offset < qname.span.start || offset >= qname.span.end {
        return None;
    }
    let rel = offset - qname.span.start;
    let mut pos = 0usize;
    for seg in &qname.segments {
        let seg_end = pos + seg.len();
        if rel >= pos && rel < seg_end {
            return Some(seg.clone());
        }
        pos = seg_end + 1;
    }
    None
}

const TOKEN_KEYWORD: u32 = 0;
const TOKEN_VARIABLE: u32 = 1;
const TOKEN_STRING: u32 = 2;
const TOKEN_NUMBER: u32 = 3;
const TOKEN_COMMENT: u32 = 4;
const TOKEN_OPERATOR: u32 = 5;

fn compute_semantic_tokens(source: &str) -> Vec<SemanticToken> {
    use vox_compiler::frontend::lexer::{Lexer, TokenKind};

    let (tokens, _) = Lexer::new(source, 0).lex_lossy();

    let mut result = Vec::new();
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;

    for token in &tokens {
        let token_type = match &token.kind {
            TokenKind::DocComment(_) => TOKEN_COMMENT,
            TokenKind::Identifier(_) => TOKEN_VARIABLE,
            TokenKind::Integer(_) | TokenKind::Float(_) => TOKEN_NUMBER,
            TokenKind::StringLiteral(_) => TOKEN_STRING,
            TokenKind::As
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::Dyn
            | TokenKind::Econ
            | TokenKind::Else
            | TokenKind::Evil
            | TokenKind::False
            | TokenKind::For
            | TokenKind::Fun
            | TokenKind::If
            | TokenKind::Import
            | TokenKind::In
            | TokenKind::Is
            | TokenKind::Null
            | TokenKind::Package
            | TokenKind::Panic
            | TokenKind::Param
            | TokenKind::Private
            | TokenKind::Public
            | TokenKind::Return
            | TokenKind::Script
            | TokenKind::True
            | TokenKind::Val
            | TokenKind::Var
            | TokenKind::When => TOKEN_KEYWORD,
            TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::Bang
            | TokenKind::Assign
            | TokenKind::PlusEq
            | TokenKind::MinusEq
            | TokenKind::StarEq
            | TokenKind::SlashEq
            | TokenKind::PercentEq
            | TokenKind::EqEq
            | TokenKind::BangEq
            | TokenKind::Less
            | TokenKind::LessEq
            | TokenKind::Greater
            | TokenKind::GreaterEq
            | TokenKind::AmpAmp
            | TokenKind::PipePipe
            | TokenKind::QuestionDot
            | TokenKind::QuestionColon
            | TokenKind::BangBang
            | TokenKind::DotDot
            | TokenKind::DotDotEq
            | TokenKind::Arrow
            | TokenKind::FatArrow
            | TokenKind::Question
            | TokenKind::Dot => TOKEN_OPERATOR,
            TokenKind::LParen
            | TokenKind::RParen
            | TokenKind::LBracket
            | TokenKind::RBracket
            | TokenKind::LBrace
            | TokenKind::RBrace
            | TokenKind::Comma
            | TokenKind::Hash
            | TokenKind::Colon
            | TokenKind::Semicolon
            | TokenKind::Eof => continue,
        };

        let start_pos = byte_offset_to_position(source, token.span.start);
        let end_pos = byte_offset_to_position(source, token.span.end);

        let delta_line = start_pos.line - prev_line;
        let delta_start = if delta_line == 0 {
            start_pos.character - prev_start
        } else {
            start_pos.character
        };
        let length = end_pos.character - start_pos.character;

        result.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        });

        prev_line = start_pos.line;
        prev_start = start_pos.character;
    }

    result
}

#[allow(deprecated)]
fn compute_document_symbols(source: &str) -> Vec<DocumentSymbol> {
    use vox_compiler::frontend::ast::TopLevelItem;

    let source_text = SourceText::new("", 0, source);
    let Some(unit) = frontend::analyze_source_lossy(&source_text).unit else {
        return Vec::new();
    };

    let mut symbols = Vec::new();

    for item in &unit.syntax.items {
        let (name, kind, span) = match item {
            TopLevelItem::Function(f) => (f.name.clone(), SymbolKind::FUNCTION, f.span.clone()),
            TopLevelItem::Value(v) => (v.name.clone(), SymbolKind::VARIABLE, v.span.clone()),
            TopLevelItem::Param(p) => (p.name.clone(), SymbolKind::VARIABLE, p.span.clone()),
            TopLevelItem::Import(i) => (
                i.module.to_source_string(),
                SymbolKind::MODULE,
                i.span.clone(),
            ),
            TopLevelItem::Statement(_) => continue,
        };

        let range = byte_span_to_range(source, span.start, span.end);

        symbols.push(DocumentSymbol {
            name,
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range,
            selection_range: range,
            children: None,
        });
    }

    symbols
}

fn build_symbol_table(
    unit: &vox_compiler::frontend::ast::FrontendUnit,
) -> HashMap<String, vox_core::diagnostics::TextSpan> {
    use vox_compiler::frontend::ast::*;
    let mut symbols = HashMap::new();

    fn walk_expr(expr: &Expr, symbols: &mut HashMap<String, vox_core::diagnostics::TextSpan>) {
        match &expr.kind {
            ExprKind::Block(block) => {
                walk_block_items(&block.items, symbols);
                if let Some(ref trailing) = block.trailing {
                    walk_expr(trailing, symbols);
                }
            }
            ExprKind::If(if_expr) => {
                for branch in &if_expr.branches {
                    walk_expr(&branch.condition, symbols);
                    walk_block_items(&branch.body.items, symbols);
                    if let Some(ref trailing) = branch.body.trailing {
                        walk_expr(trailing, symbols);
                    }
                }
                if let Some(ref else_branch) = if_expr.else_branch {
                    walk_block_items(&else_branch.items, symbols);
                    if let Some(ref trailing) = else_branch.trailing {
                        walk_expr(trailing, symbols);
                    }
                }
            }
            ExprKind::When(when_expr) => {
                walk_expr(&when_expr.subject, symbols);
                for arm in &when_expr.arms {
                    if let Some(ref binding) = arm.binding {
                        symbols.insert(binding.clone(), arm.span.clone());
                    }
                    walk_expr(&arm.body, symbols);
                }
                if let Some(ref else_arm) = when_expr.else_arm {
                    walk_expr(else_arm, symbols);
                }
            }
            ExprKind::For(for_expr) => {
                use vox_compiler::frontend::ast::ForHeader;
                if let Some(ref init) = for_expr.init {
                    match init.as_ref() {
                        BlockItem::LocalValue(lv) => {
                            symbols.insert(lv.name.clone(), lv.span.clone());
                            walk_expr(&lv.initializer, symbols);
                        }
                        BlockItem::Assignment(a) => walk_expr(&a.value, symbols),
                        BlockItem::CompoundAssignment(ca) => walk_expr(&ca.value, symbols),
                        BlockItem::Return(r) => {
                            if let Some(ref val) = r.value {
                                walk_expr(val, symbols);
                            }
                        }
                        BlockItem::Expr(e) => walk_expr(e, symbols),
                        BlockItem::BlockStatement(e) => walk_expr(e, symbols),
                        _ => {}
                    }
                }
                let body = for_expr.body();
                match &for_expr.header {
                    ForHeader::In { pattern, iterable } => {
                        symbols.insert(pattern.clone(), body.span.clone());
                        walk_expr(iterable, symbols);
                    }
                    ForHeader::Condition(condition) => {
                        walk_expr(condition, symbols);
                    }
                }
                walk_block_items(&body.items, symbols);
                if let Some(ref trailing) = body.trailing {
                    walk_expr(trailing, symbols);
                }
            }
            ExprKind::Lambda(lambda) => {
                for param in &lambda.parameters {
                    symbols.insert(param.name.clone(), param.span.clone());
                }
                walk_expr(&lambda.body, symbols);
            }
            ExprKind::Call { callee, arguments } => {
                walk_expr(callee, symbols);
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) => walk_expr(e, symbols),
                        Argument::Named { value, .. } => walk_expr(value, symbols),
                    }
                }
            }
            ExprKind::ReceiverCall {
                receiver,
                arguments,
                ..
            } => {
                walk_expr(receiver, symbols);
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) => walk_expr(e, symbols),
                        Argument::Named { value, .. } => walk_expr(value, symbols),
                    }
                }
            }
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for item in items {
                    walk_expr(item, symbols);
                }
            }
            ExprKind::Record(fields) => {
                for field in fields {
                    walk_expr(&field.value, symbols);
                }
            }
            ExprKind::Index { target, index } => {
                walk_expr(target, symbols);
                walk_expr(index, symbols);
            }
            ExprKind::Field { target, .. }
            | ExprKind::SafeField { target, .. }
            | ExprKind::NonNull { target } => {
                walk_expr(target, symbols);
            }
            ExprKind::Unary { expr: inner, .. } => walk_expr(inner, symbols),
            ExprKind::Binary { left, right, .. } => {
                walk_expr(left, symbols);
                walk_expr(right, symbols);
            }
            ExprKind::Range(range) => {
                if let Some(ref start) = range.start {
                    walk_expr(start, symbols);
                }
                if let Some(ref end) = range.end {
                    walk_expr(end, symbols);
                }
            }
            ExprKind::Intrinsic(intr) => match intr {
                IntrinsicExpr::Updated(u) => {
                    walk_expr(&u.target, symbols);
                    for upd in &u.updates {
                        walk_expr(&upd.value, symbols);
                    }
                }
                IntrinsicExpr::Econ(e) => {
                    walk_block_items(&e.body.items, symbols);
                    if let Some(ref trailing) = e.body.trailing {
                        walk_expr(trailing, symbols);
                    }
                }
            },
            ExprKind::Name(_)
            | ExprKind::Integer(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::String(_) => {}
        }
    }

    fn walk_block_items(
        items: &[BlockItem],
        symbols: &mut HashMap<String, vox_core::diagnostics::TextSpan>,
    ) {
        for item in items {
            match item {
                BlockItem::LocalValue(lv) => {
                    symbols.insert(lv.name.clone(), lv.span.clone());
                    walk_expr(&lv.initializer, symbols);
                }
                BlockItem::Assignment(a) => {
                    walk_expr(&a.value, symbols);
                }
                BlockItem::CompoundAssignment(ca) => {
                    walk_expr(&ca.value, symbols);
                }
                BlockItem::Return(r) => {
                    if let Some(ref val) = r.value {
                        walk_expr(val, symbols);
                    }
                }
                BlockItem::Panic(_) => {}
                BlockItem::Break(_) => {}
                BlockItem::Continue(_) => {}
                BlockItem::BlockStatement(e) | BlockItem::Expr(e) => walk_expr(e, symbols),
            }
        }
    }

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(f) => {
                symbols.insert(f.name.clone(), f.span.clone());
                for param in &f.parameters {
                    symbols.insert(param.name.clone(), param.span.clone());
                }
                walk_expr(&f.body, &mut symbols);
            }
            TopLevelItem::Value(v) => {
                symbols.insert(v.name.clone(), v.span.clone());
                walk_expr(&v.initializer, &mut symbols);
            }
            TopLevelItem::Param(p) => {
                symbols.insert(p.name.clone(), p.span.clone());
            }
            TopLevelItem::Import(i) => {
                if let Some(last) = i.module.segments.last() {
                    symbols.insert(last.clone(), i.span.clone());
                }
            }
            TopLevelItem::Statement(s) => match s {
                BlockItem::LocalValue(lv) => {
                    symbols.insert(lv.name.clone(), lv.span.clone());
                    walk_expr(&lv.initializer, &mut symbols);
                }
                _ => {}
            },
        }
    }

    symbols
}

fn find_name_at_offset(
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    source: &str,
    offset: usize,
) -> Option<String> {
    use vox_compiler::frontend::ast::*;

    fn find_in_expr(expr: &Expr, source: &str, offset: usize) -> Option<String> {
        match &expr.kind {
            ExprKind::Name(qname) => {
                return segment_at_offset(source, qname, offset);
            }
            ExprKind::ReceiverCall {
                receiver,
                callee,
                arguments,
            } => {
                if let Some(name) = segment_at_offset(source, callee, offset) {
                    return Some(name);
                }
                if let Some(name) = find_in_expr(receiver, source, offset) {
                    return Some(name);
                }
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) | Argument::Named { value: e, .. } => {
                            if let Some(name) = find_in_expr(e, source, offset) {
                                return Some(name);
                            }
                        }
                    }
                }
                return None;
            }
            _ => {}
        }

        match &expr.kind {
            ExprKind::Block(block) => {
                for item in &block.items {
                    if let Some(name) = find_in_block_item(item, source, offset) {
                        return Some(name);
                    }
                }
                if let Some(ref trailing) = block.trailing {
                    if let Some(name) = find_in_expr(trailing, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::If(if_expr) => {
                for branch in &if_expr.branches {
                    if let Some(name) = find_in_expr(&branch.condition, source, offset) {
                        return Some(name);
                    }
                    for item in &branch.body.items {
                        if let Some(name) = find_in_block_item(item, source, offset) {
                            return Some(name);
                        }
                    }
                    if let Some(ref trailing) = branch.body.trailing {
                        if let Some(name) = find_in_expr(trailing, source, offset) {
                            return Some(name);
                        }
                    }
                }
                if let Some(ref else_branch) = if_expr.else_branch {
                    for item in &else_branch.items {
                        if let Some(name) = find_in_block_item(item, source, offset) {
                            return Some(name);
                        }
                    }
                    if let Some(ref trailing) = else_branch.trailing {
                        if let Some(name) = find_in_expr(trailing, source, offset) {
                            return Some(name);
                        }
                    }
                }
            }
            ExprKind::When(when_expr) => {
                if let Some(name) = find_in_expr(&when_expr.subject, source, offset) {
                    return Some(name);
                }
                for arm in &when_expr.arms {
                    if let Some(name) = find_in_expr(&arm.body, source, offset) {
                        return Some(name);
                    }
                }
                if let Some(ref else_arm) = when_expr.else_arm {
                    if let Some(name) = find_in_expr(else_arm, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::For(for_expr) => {
                use vox_compiler::frontend::ast::ForHeader;
                if let Some(ref init) = for_expr.init {
                    if let Some(name) = find_in_block_item(init.as_ref(), source, offset) {
                        return Some(name);
                    }
                }
                let body = match &for_expr.header {
                    ForHeader::In { iterable, .. } => {
                        if let Some(name) = find_in_expr(iterable, source, offset) {
                            return Some(name);
                        }
                        &for_expr.body
                    }
                    ForHeader::Condition(condition) => {
                        if let Some(name) = find_in_expr(condition, source, offset) {
                            return Some(name);
                        }
                        &for_expr.body
                    }
                };
                for item in &body.items {
                    if let Some(name) = find_in_block_item(item, source, offset) {
                        return Some(name);
                    }
                }
                if let Some(ref trailing) = body.trailing {
                    if let Some(name) = find_in_expr(trailing, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::Lambda(lambda) => {
                if let Some(name) = find_in_expr(&lambda.body, source, offset) {
                    return Some(name);
                }
            }
            ExprKind::Call { callee, arguments } => {
                if let Some(name) = find_in_expr(callee, source, offset) {
                    return Some(name);
                }
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) | Argument::Named { value: e, .. } => {
                            if let Some(name) = find_in_expr(e, source, offset) {
                                return Some(name);
                            }
                        }
                    }
                }
            }
            ExprKind::ReceiverCall {
                receiver,
                callee: _,
                arguments,
            } => {
                if let Some(name) = find_in_expr(receiver, source, offset) {
                    return Some(name);
                }
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) | Argument::Named { value: e, .. } => {
                            if let Some(name) = find_in_expr(e, source, offset) {
                                return Some(name);
                            }
                        }
                    }
                }
            }
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for item in items {
                    if let Some(name) = find_in_expr(item, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::Record(fields) => {
                for field in fields {
                    if let Some(name) = find_in_expr(&field.value, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::Index { target, index } => {
                if let Some(name) = find_in_expr(target, source, offset) {
                    return Some(name);
                }
                if let Some(name) = find_in_expr(index, source, offset) {
                    return Some(name);
                }
            }
            ExprKind::Field { target, .. }
            | ExprKind::SafeField { target, .. }
            | ExprKind::NonNull { target } => {
                if let Some(name) = find_in_expr(target, source, offset) {
                    return Some(name);
                }
            }
            ExprKind::Unary { expr: inner, .. } => {
                if let Some(name) = find_in_expr(inner, source, offset) {
                    return Some(name);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                if let Some(name) = find_in_expr(left, source, offset) {
                    return Some(name);
                }
                if let Some(name) = find_in_expr(right, source, offset) {
                    return Some(name);
                }
            }
            ExprKind::Range(range) => {
                if let Some(ref start) = range.start {
                    if let Some(name) = find_in_expr(start, source, offset) {
                        return Some(name);
                    }
                }
                if let Some(ref end) = range.end {
                    if let Some(name) = find_in_expr(end, source, offset) {
                        return Some(name);
                    }
                }
            }
            ExprKind::Intrinsic(intr) => match intr {
                IntrinsicExpr::Updated(u) => {
                    if let Some(name) = find_in_expr(&u.target, source, offset) {
                        return Some(name);
                    }
                    for upd in &u.updates {
                        if let Some(name) = find_in_expr(&upd.value, source, offset) {
                            return Some(name);
                        }
                    }
                }
                IntrinsicExpr::Econ(e) => {
                    for item in &e.body.items {
                        if let Some(name) = find_in_block_item(item, source, offset) {
                            return Some(name);
                        }
                    }
                    if let Some(ref trailing) = e.body.trailing {
                        if let Some(name) = find_in_expr(trailing, source, offset) {
                            return Some(name);
                        }
                    }
                }
            },
            ExprKind::Name(_)
            | ExprKind::Integer(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::String(_) => {}
        }

        None
    }

    fn find_in_block_item(item: &BlockItem, source: &str, offset: usize) -> Option<String> {
        match item {
            BlockItem::LocalValue(lv) => {
                if offset >= lv.span.start && offset < lv.span.end {
                    if let Some(ident) = identifier_at_offset(source, offset) {
                        if ident == lv.name {
                            return Some(lv.name.clone());
                        }
                    }
                }
                find_in_expr(&lv.initializer, source, offset)
            }
            BlockItem::Assignment(a) => find_in_expr(&a.value, source, offset),
            BlockItem::CompoundAssignment(ca) => find_in_expr(&ca.value, source, offset),
            BlockItem::Return(r) => match &r.value {
                Some(val) => find_in_expr(val, source, offset),
                None => None,
            },
            BlockItem::Panic(_) => None,
            BlockItem::Break(_) => None,
            BlockItem::Continue(_) => None,
            BlockItem::BlockStatement(e) | BlockItem::Expr(e) => find_in_expr(e, source, offset),
        }
    }

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(f) => {
                if let Some(name) = find_in_expr(&f.body, source, offset) {
                    return Some(name);
                }
            }
            TopLevelItem::Value(v) => {
                if let Some(name) = find_in_expr(&v.initializer, source, offset) {
                    return Some(name);
                }
            }
            TopLevelItem::Statement(s) => {
                if let Some(name) = find_in_block_item(s, source, offset) {
                    return Some(name);
                }
            }
            TopLevelItem::Param(p) => {
                if let Some(ref default) = p.default {
                    if let Some(name) = find_in_expr(default, source, offset) {
                        return Some(name);
                    }
                }
            }
            TopLevelItem::Import(_) => {}
        }
    }

    if let Some(ref result) = unit.syntax.result {
        if let Some(name) = find_in_expr(result, source, offset) {
            return Some(name);
        }
    }

    None
}

fn builtin_signature_help(call_info: &CallInfo) -> Option<SignatureHelp> {
    let candidates = [
        BuiltinReceiver::Int,
        BuiltinReceiver::UInt,
        BuiltinReceiver::Float,
        BuiltinReceiver::Bool,
        BuiltinReceiver::String,
        BuiltinReceiver::List,
        BuiltinReceiver::Econ,
    ];
    let summary = candidates
        .iter()
        .flat_map(|receiver| {
            let ty = repl_type_for_builtin_receiver(*receiver);
            vox_runtime::builtin_method_summaries_for_type(&ty)
        })
        .find(|summary| summary.name == call_info.callee_name)?;
    let params = summary
        .parameters
        .iter()
        .skip(1)
        .map(|param| ParameterInformation {
            label: ParameterLabel::Simple(format!("{}: {}", param.name, param.ty.render())),
            documentation: None,
        })
        .collect::<Vec<_>>();
    let active = call_info
        .active_parameter
        .min(params.len().saturating_sub(1));
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label: format!(
                "{}{}",
                summary.name,
                format_function_summary_detail(&summary)
            ),
            documentation: None,
            parameters: Some(params),
            active_parameter: Some(active as u32),
        }],
        active_signature: Some(0),
        active_parameter: Some(active as u32),
    })
}

fn repl_type_for_builtin_receiver(receiver: BuiltinReceiver) -> vox_runtime::ReplType {
    match receiver {
        BuiltinReceiver::Int => vox_runtime::ReplType::Int,
        BuiltinReceiver::UInt => vox_runtime::ReplType::UInt,
        BuiltinReceiver::Float => vox_runtime::ReplType::Float,
        BuiltinReceiver::Bool => vox_runtime::ReplType::Bool,
        BuiltinReceiver::String => vox_runtime::ReplType::String,
        BuiltinReceiver::List => {
            vox_runtime::ReplType::List(Box::new(vox_runtime::ReplType::Unknown("T".to_owned())))
        }
        BuiltinReceiver::Econ => vox_runtime::ReplType::Named {
            name: "Econ".to_owned(),
            arguments: vec![vox_runtime::ReplType::Unknown("T".to_owned())],
        },
    }
}

fn identifier_at_offset(source: &str, offset: usize) -> Option<&str> {
    let bytes = source.as_bytes();
    if offset >= bytes.len() {
        return None;
    }
    if !is_ident_byte(bytes[offset]) {
        return None;
    }
    let mut start = offset;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = offset;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    Some(&source[start..end])
}

fn collect_references(
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    _source: &str,
    target: &str,
) -> Vec<vox_core::diagnostics::TextSpan> {
    use vox_compiler::frontend::ast::*;
    let mut spans = Vec::new();

    fn collect_type_syntax(
        ty: &TypeSyntax,
        target: &str,
        spans: &mut Vec<vox_core::diagnostics::TextSpan>,
    ) {
        match &ty.kind {
            TypeKind::Function { parameters, result } => {
                for param in parameters {
                    collect_type_syntax(param, target, spans);
                }
                collect_type_syntax(result, target, spans);
            }
            TypeKind::Nullable(inner) => collect_type_syntax(inner, target, spans),
            TypeKind::Named { name, arguments } => {
                if name.segments.last().map(|s| s.as_str()) == Some(target) {
                    spans.push(name.span.clone());
                }
                for arg in arguments {
                    collect_type_syntax(arg, target, spans);
                }
            }
            TypeKind::Dyn(name) => {
                if name.segments.last().map(|s| s.as_str()) == Some(target) {
                    spans.push(name.span.clone());
                }
            }
            TypeKind::Grouped(inner) => collect_type_syntax(inner, target, spans),
            TypeKind::Tuple(items) => {
                for item in items {
                    collect_type_syntax(item, target, spans);
                }
            }
            TypeKind::Record(fields) => {
                for field in fields {
                    collect_type_syntax(&field.ty, target, spans);
                }
            }
        }
    }

    fn collect_in_expr(
        expr: &Expr,
        target: &str,
        spans: &mut Vec<vox_core::diagnostics::TextSpan>,
    ) {
        match &expr.kind {
            ExprKind::Name(qname) => {
                if qname.segments.last().map(|s| s.as_str()) == Some(target) {
                    spans.push(qname.span.clone());
                }
            }
            ExprKind::ReceiverCall {
                receiver,
                callee,
                arguments,
            } => {
                if callee.segments.last().map(|s| s.as_str()) == Some(target) {
                    spans.push(callee.span.clone());
                }
                collect_in_expr(receiver, target, spans);
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) | Argument::Named { value: e, .. } => {
                            collect_in_expr(e, target, spans);
                        }
                    }
                }
            }
            _ => {}
        }

        match &expr.kind {
            ExprKind::Block(block) => {
                for item in &block.items {
                    collect_in_block_item(item, target, spans);
                }
                if let Some(ref trailing) = block.trailing {
                    collect_in_expr(trailing, target, spans);
                }
            }
            ExprKind::If(if_expr) => {
                for branch in &if_expr.branches {
                    collect_in_expr(&branch.condition, target, spans);
                    for item in &branch.body.items {
                        collect_in_block_item(item, target, spans);
                    }
                    if let Some(ref trailing) = branch.body.trailing {
                        collect_in_expr(trailing, target, spans);
                    }
                }
                if let Some(ref else_branch) = if_expr.else_branch {
                    for item in &else_branch.items {
                        collect_in_block_item(item, target, spans);
                    }
                    if let Some(ref trailing) = else_branch.trailing {
                        collect_in_expr(trailing, target, spans);
                    }
                }
            }
            ExprKind::When(when_expr) => {
                collect_in_expr(&when_expr.subject, target, spans);
                for arm in &when_expr.arms {
                    collect_type_syntax(&arm.ty, target, spans);
                    collect_in_expr(&arm.body, target, spans);
                }
                if let Some(ref else_arm) = when_expr.else_arm {
                    collect_in_expr(else_arm, target, spans);
                }
            }
            ExprKind::For(for_expr) => {
                use vox_compiler::frontend::ast::ForHeader;
                if let Some(ref init) = for_expr.init {
                    collect_in_block_item(init.as_ref(), target, spans);
                }
                let body = match &for_expr.header {
                    ForHeader::In { iterable, .. } => {
                        collect_in_expr(iterable, target, spans);
                        &for_expr.body
                    }
                    ForHeader::Condition(condition) => {
                        collect_in_expr(condition, target, spans);
                        &for_expr.body
                    }
                };
                for item in &body.items {
                    collect_in_block_item(item, target, spans);
                }
                if let Some(ref trailing) = body.trailing {
                    collect_in_expr(trailing, target, spans);
                }
            }
            ExprKind::Lambda(lambda) => {
                for param in &lambda.parameters {
                    if let Some(ref ty) = param.ty {
                        collect_type_syntax(ty, target, spans);
                    }
                }
                collect_in_expr(&lambda.body, target, spans);
            }
            ExprKind::Call { callee, arguments } => {
                collect_in_expr(callee, target, spans);
                for arg in arguments {
                    match arg {
                        Argument::Positional(e) | Argument::Named { value: e, .. } => {
                            collect_in_expr(e, target, spans);
                        }
                    }
                }
            }
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for item in items {
                    collect_in_expr(item, target, spans);
                }
            }
            ExprKind::Record(fields) => {
                for field in fields {
                    if let Some(ref ty) = field.ty {
                        collect_type_syntax(ty, target, spans);
                    }
                    collect_in_expr(&field.value, target, spans);
                }
            }
            ExprKind::Index { target: tgt, index } => {
                collect_in_expr(tgt, target, spans);
                collect_in_expr(index, target, spans);
            }
            ExprKind::Field { target: tgt, .. }
            | ExprKind::SafeField { target: tgt, .. }
            | ExprKind::NonNull { target: tgt } => {
                collect_in_expr(tgt, target, spans);
            }
            ExprKind::Unary { expr: inner, .. } => collect_in_expr(inner, target, spans),
            ExprKind::Binary { left, right, .. } => {
                collect_in_expr(left, target, spans);
                collect_in_expr(right, target, spans);
            }
            ExprKind::Range(range) => {
                if let Some(ref start) = range.start {
                    collect_in_expr(start, target, spans);
                }
                if let Some(ref end) = range.end {
                    collect_in_expr(end, target, spans);
                }
            }
            ExprKind::Intrinsic(intr) => match intr {
                IntrinsicExpr::Updated(u) => {
                    collect_in_expr(&u.target, target, spans);
                    for upd in &u.updates {
                        collect_in_expr(&upd.value, target, spans);
                    }
                }
                IntrinsicExpr::Econ(e) => {
                    collect_type_syntax(&e.ty, target, spans);
                    for item in &e.body.items {
                        collect_in_block_item(item, target, spans);
                    }
                    if let Some(ref trailing) = e.body.trailing {
                        collect_in_expr(trailing, target, spans);
                    }
                }
            },
            ExprKind::Name(_)
            | ExprKind::ReceiverCall { .. }
            | ExprKind::Integer(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::String(_) => {}
        }
    }

    fn collect_in_block_item(
        item: &BlockItem,
        target: &str,
        spans: &mut Vec<vox_core::diagnostics::TextSpan>,
    ) {
        match item {
            BlockItem::LocalValue(lv) => {
                if let Some(ref ty) = lv.ty {
                    collect_type_syntax(ty, target, spans);
                }
                collect_in_expr(&lv.initializer, target, spans);
            }
            BlockItem::Assignment(a) => collect_in_expr(&a.value, target, spans),
            BlockItem::CompoundAssignment(ca) => collect_in_expr(&ca.value, target, spans),
            BlockItem::Return(r) => {
                if let Some(ref val) = r.value {
                    collect_in_expr(val, target, spans);
                }
            }
            BlockItem::Panic(_) => {}
            BlockItem::Break(_) => {}
            BlockItem::Continue(_) => {}
            BlockItem::BlockStatement(e) | BlockItem::Expr(e) => collect_in_expr(e, target, spans),
        }
    }

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(f) => {
                for param in &f.parameters {
                    collect_type_syntax(&param.ty, target, &mut spans);
                    if let Some(ref default) = param.default {
                        collect_in_expr(default, target, &mut spans);
                    }
                }
                if let Some(ref ret) = f.return_type {
                    collect_type_syntax(ret, target, &mut spans);
                }
                collect_in_expr(&f.body, target, &mut spans);
            }
            TopLevelItem::Value(v) => {
                if let Some(ref ty) = v.ty {
                    collect_type_syntax(ty, target, &mut spans);
                }
                collect_in_expr(&v.initializer, target, &mut spans);
            }
            TopLevelItem::Param(p) => {
                collect_type_syntax(&p.ty, target, &mut spans);
                if let Some(ref default) = p.default {
                    collect_in_expr(default, target, &mut spans);
                }
            }
            TopLevelItem::Import(i) => {
                if i.module.segments.last().map(|s| s.as_str()) == Some(target) {
                    spans.push(i.module.span.clone());
                }
            }
            TopLevelItem::Statement(s) => collect_in_block_item(s, target, &mut spans),
        }
    }

    if let Some(ref result) = unit.syntax.result {
        collect_in_expr(result, target, &mut spans);
    }

    spans
}

struct CallInfo {
    callee_name: String,
    active_parameter: usize,
}

fn find_call_at_offset(
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    offset: usize,
) -> Option<CallInfo> {
    use vox_compiler::frontend::ast::*;
    let mut best: Option<(String, usize)> = None;

    fn walk_expr(expr: &Expr, offset: usize, best: &mut Option<(String, usize)>) {
        if offset < expr.span.start || offset >= expr.span.end {
            return;
        }

        fn walk_children(expr: &Expr, offset: usize, best: &mut Option<(String, usize)>) {
            match &expr.kind {
                ExprKind::Block(block) => {
                    for item in &block.items {
                        match item {
                            BlockItem::LocalValue(lv) => walk_expr(&lv.initializer, offset, best),
                            BlockItem::Assignment(a) => walk_expr(&a.value, offset, best),
                            BlockItem::CompoundAssignment(ca) => walk_expr(&ca.value, offset, best),
                            BlockItem::Return(r) => {
                                if let Some(ref val) = r.value {
                                    walk_expr(val, offset, best);
                                }
                            }
                            BlockItem::Panic(_) => {}
                            BlockItem::Break(_) => {}
                            BlockItem::Continue(_) => {}
                            BlockItem::BlockStatement(e) | BlockItem::Expr(e) => {
                                walk_expr(e, offset, best)
                            }
                        }
                    }
                    if let Some(ref trailing) = block.trailing {
                        walk_expr(trailing, offset, best);
                    }
                }
                ExprKind::If(if_expr) => {
                    for branch in &if_expr.branches {
                        walk_expr(&branch.condition, offset, best);
                        for item in &branch.body.items {
                            match item {
                                BlockItem::LocalValue(lv) => {
                                    walk_expr(&lv.initializer, offset, best)
                                }
                                BlockItem::Assignment(a) => walk_expr(&a.value, offset, best),
                                BlockItem::CompoundAssignment(ca) => {
                                    walk_expr(&ca.value, offset, best)
                                }
                                BlockItem::Return(r) => {
                                    if let Some(ref val) = r.value {
                                        walk_expr(val, offset, best);
                                    }
                                }
                                BlockItem::Panic(_) => {}
                                BlockItem::Break(_) => {}
                                BlockItem::Continue(_) => {}
                                BlockItem::BlockStatement(e) | BlockItem::Expr(e) => {
                                    walk_expr(e, offset, best)
                                }
                            }
                        }
                        if let Some(ref trailing) = branch.body.trailing {
                            walk_expr(trailing, offset, best);
                        }
                    }
                    if let Some(ref else_branch) = if_expr.else_branch {
                        for item in &else_branch.items {
                            match item {
                                BlockItem::LocalValue(lv) => {
                                    walk_expr(&lv.initializer, offset, best)
                                }
                                BlockItem::Assignment(a) => walk_expr(&a.value, offset, best),
                                BlockItem::CompoundAssignment(ca) => {
                                    walk_expr(&ca.value, offset, best)
                                }
                                BlockItem::Return(r) => {
                                    if let Some(ref val) = r.value {
                                        walk_expr(val, offset, best);
                                    }
                                }
                                BlockItem::Panic(_) => {}
                                BlockItem::Break(_) => {}
                                BlockItem::Continue(_) => {}
                                BlockItem::BlockStatement(e) | BlockItem::Expr(e) => {
                                    walk_expr(e, offset, best)
                                }
                            }
                        }
                        if let Some(ref trailing) = else_branch.trailing {
                            walk_expr(trailing, offset, best);
                        }
                    }
                }
                ExprKind::When(when_expr) => {
                    walk_expr(&when_expr.subject, offset, best);
                    for arm in &when_expr.arms {
                        walk_expr(&arm.body, offset, best);
                    }
                    if let Some(ref else_arm) = when_expr.else_arm {
                        walk_expr(else_arm, offset, best);
                    }
                }
                ExprKind::For(for_expr) => {
                    use vox_compiler::frontend::ast::ForHeader;
                    if let Some(ref init) = for_expr.init {
                        match init.as_ref() {
                            BlockItem::LocalValue(lv) => walk_expr(&lv.initializer, offset, best),
                            BlockItem::Assignment(a) => walk_expr(&a.value, offset, best),
                            BlockItem::CompoundAssignment(ca) => walk_expr(&ca.value, offset, best),
                            BlockItem::Expr(e) => walk_expr(e, offset, best),
                            BlockItem::BlockStatement(e) => walk_expr(e, offset, best),
                            _ => {}
                        }
                    }
                    let body = match &for_expr.header {
                        ForHeader::In { iterable, .. } => {
                            walk_expr(iterable, offset, best);
                            &for_expr.body
                        }
                        ForHeader::Condition(condition) => {
                            walk_expr(condition, offset, best);
                            &for_expr.body
                        }
                    };
                    for item in &body.items {
                        match item {
                            BlockItem::LocalValue(lv) => walk_expr(&lv.initializer, offset, best),
                            BlockItem::Assignment(a) => walk_expr(&a.value, offset, best),
                            BlockItem::CompoundAssignment(ca) => walk_expr(&ca.value, offset, best),
                            BlockItem::Return(r) => {
                                if let Some(ref val) = r.value {
                                    walk_expr(val, offset, best);
                                }
                            }
                            BlockItem::Panic(_) => {}
                            BlockItem::Break(_) => {}
                            BlockItem::Continue(_) => {}
                            BlockItem::BlockStatement(e) | BlockItem::Expr(e) => {
                                walk_expr(e, offset, best)
                            }
                        }
                    }
                    if let Some(ref trailing) = body.trailing {
                        walk_expr(trailing, offset, best);
                    }
                }
                ExprKind::Lambda(lambda) => walk_expr(&lambda.body, offset, best),
                ExprKind::Call { callee, arguments } => {
                    walk_expr(callee, offset, best);
                    for arg in arguments {
                        match arg {
                            Argument::Positional(e) => walk_expr(e, offset, best),
                            Argument::Named { value, .. } => walk_expr(value, offset, best),
                        }
                    }
                }
                ExprKind::ReceiverCall {
                    receiver,
                    callee: _,
                    arguments,
                } => {
                    walk_expr(receiver, offset, best);
                    for arg in arguments {
                        match arg {
                            Argument::Positional(e) => walk_expr(e, offset, best),
                            Argument::Named { value, .. } => walk_expr(value, offset, best),
                        }
                    }
                }
                ExprKind::List(items) | ExprKind::Tuple(items) => {
                    for item in items {
                        walk_expr(item, offset, best);
                    }
                }
                ExprKind::Record(fields) => {
                    for field in fields {
                        walk_expr(&field.value, offset, best);
                    }
                }
                ExprKind::Index { target, index } => {
                    walk_expr(target, offset, best);
                    walk_expr(index, offset, best);
                }
                ExprKind::Field { target, .. }
                | ExprKind::SafeField { target, .. }
                | ExprKind::NonNull { target } => walk_expr(target, offset, best),
                ExprKind::Unary { expr: inner, .. } => walk_expr(inner, offset, best),
                ExprKind::Binary { left, right, .. } => {
                    walk_expr(left, offset, best);
                    walk_expr(right, offset, best);
                }
                ExprKind::Range(range) => {
                    if let Some(ref start) = range.start {
                        walk_expr(start, offset, best);
                    }
                    if let Some(ref end) = range.end {
                        walk_expr(end, offset, best);
                    }
                }
                ExprKind::Intrinsic(intr) => match intr {
                    IntrinsicExpr::Updated(u) => {
                        walk_expr(&u.target, offset, best);
                        for upd in &u.updates {
                            walk_expr(&upd.value, offset, best);
                        }
                    }
                    IntrinsicExpr::Econ(e) => {
                        for item in &e.body.items {
                            match item {
                                BlockItem::LocalValue(lv) => {
                                    walk_expr(&lv.initializer, offset, best)
                                }
                                BlockItem::Assignment(a) => walk_expr(&a.value, offset, best),
                                BlockItem::CompoundAssignment(ca) => {
                                    walk_expr(&ca.value, offset, best)
                                }
                                BlockItem::Return(r) => {
                                    if let Some(ref val) = r.value {
                                        walk_expr(val, offset, best);
                                    }
                                }
                                BlockItem::Panic(_) => {}
                                BlockItem::Break(_) => {}
                                BlockItem::Continue(_) => {}
                                BlockItem::BlockStatement(e) | BlockItem::Expr(e) => {
                                    walk_expr(e, offset, best)
                                }
                            }
                        }
                        if let Some(ref trailing) = e.body.trailing {
                            walk_expr(trailing, offset, best);
                        }
                    }
                },
                ExprKind::Name(_)
                | ExprKind::Integer(_)
                | ExprKind::Float(_)
                | ExprKind::Bool(_)
                | ExprKind::Null
                | ExprKind::String(_) => {}
            }
        }

        match &expr.kind {
            ExprKind::Call { callee, arguments } => {
                for (i, arg) in arguments.iter().enumerate() {
                    let arg_span = match arg {
                        Argument::Positional(e) => e.span.clone(),
                        Argument::Named { value: e, .. } => e.span.clone(),
                    };
                    if offset >= arg_span.start && offset <= arg_span.end {
                        match &callee.kind {
                            ExprKind::Name(qname) if !qname.segments.is_empty() => {
                                *best = Some((qname.to_source_string(), i));
                            }
                            ExprKind::Field { name, .. } => {
                                *best = Some((name.clone(), i));
                            }
                            _ => {}
                        }
                    }
                }
                walk_children(expr, offset, best);
            }
            ExprKind::ReceiverCall {
                callee, arguments, ..
            } => {
                for (i, arg) in arguments.iter().enumerate() {
                    let arg_span = match arg {
                        Argument::Positional(e) => e.span.clone(),
                        Argument::Named { value: e, .. } => e.span.clone(),
                    };
                    if offset >= arg_span.start && offset <= arg_span.end {
                        *best = Some((callee.to_source_string(), i));
                    }
                }
                walk_children(expr, offset, best);
            }
            _ => walk_children(expr, offset, best),
        }
    }

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(f) => walk_expr(&f.body, offset, &mut best),
            TopLevelItem::Value(v) => walk_expr(&v.initializer, offset, &mut best),
            TopLevelItem::Param(p) => {
                if let Some(ref default) = p.default {
                    walk_expr(default, offset, &mut best);
                }
            }
            TopLevelItem::Statement(s) => match s {
                BlockItem::LocalValue(lv) => walk_expr(&lv.initializer, offset, &mut best),
                _ => {}
            },
            TopLevelItem::Import(_) => {}
        }
    }

    if let Some(ref result) = unit.syntax.result {
        walk_expr(result, offset, &mut best);
    }

    best.map(|(name, idx)| CallInfo {
        callee_name: name,
        active_parameter: idx,
    })
}

fn format_function_summary_detail(function: &vox_runtime::FunctionSummary) -> String {
    let params = function
        .parameters
        .iter()
        .map(|param| format!("{}: {}", param.name, param.ty.render()))
        .collect::<Vec<_>>()
        .join(", ");
    format!("({params}) -> {}", function.return_type.render())
}

fn compute_signature_help(source: &str, position: Position) -> Option<SignatureHelp> {
    use vox_compiler::frontend::ast::*;
    let source_text = SourceText::new("", 0, source);
    let unit = frontend::analyze_source_lossy(&source_text).unit?;
    let offset = position_to_byte_offset(source, position);
    let call_info = find_call_at_offset(&unit, offset)?;

    for item in &unit.syntax.items {
        if let TopLevelItem::Function(f) = item {
            if f.name == call_info.callee_name {
                let params: Vec<ParameterInformation> = f
                    .parameters
                    .iter()
                    .map(|p| {
                        let label = format!("{}: {}", p.name, p.ty.to_source_string());
                        ParameterInformation {
                            label: ParameterLabel::Simple(label),
                            documentation: None,
                        }
                    })
                    .collect();

                let param_count = f.parameters.len().max(1);
                let sig = SignatureInformation {
                    label: format!(
                        "{}({}){}",
                        f.name,
                        f.parameters
                            .iter()
                            .map(|p| format!("{}: {}", p.name, p.ty.to_source_string()))
                            .collect::<Vec<_>>()
                            .join(", "),
                        f.return_type
                            .as_ref()
                            .map(|t| format!(" -> {}", t.to_source_string()))
                            .unwrap_or_default()
                    ),
                    documentation: None,
                    parameters: Some(params),
                    active_parameter: Some(call_info.active_parameter.min(param_count - 1) as u32),
                };

                return Some(SignatureHelp {
                    signatures: vec![sig],
                    active_signature: Some(0),
                    active_parameter: Some(call_info.active_parameter.min(param_count - 1) as u32),
                });
            }
        }
    }

    if let Some(sig) = builtin_signature_help(&call_info) {
        return Some(sig);
    }

    None
}

fn try_dot_completion(source: &str, position: Position) -> Option<Vec<CompletionItem>> {
    use vox_runtime::ReplType;

    let offset = position_to_byte_offset(source, position);
    let bytes = source.as_bytes();

    // Detect if cursor is right after `.` or `?.`
    if offset == 0 || offset > source.len() {
        return None;
    }

    let mut dot_end = offset;
    let is_safe = dot_end >= 2 && bytes[dot_end - 2] == b'.' && bytes[dot_end - 1] == b'?';
    if !is_safe && (dot_end == 0 || bytes[dot_end - 1] != b'.') {
        return None;
    }
    if is_safe {
        dot_end -= 2;
    } else {
        dot_end -= 1;
    }

    // Extract receiver chain: scan backwards from dot to find identifier segments separated by dots
    let prefix_before_dot = &source[..dot_end];
    let chain = extract_receiver_chain(prefix_before_dot)?;

    // Try to parse the prefix (source up to the dot, excluding the dot and trailing content)
    // We use the prefix before the dot to avoid the incomplete dot access parsing error
    let source_text = SourceText::new("", 0, prefix_before_dot);
    let unit = frontend::analyze_source_lossy(&source_text).unit?;
    let env = vox_runtime::infer_environment(&unit.syntax, &[]).ok()?;

    // Resolve the first segment in bindings
    let first = &chain[0];
    let mut current_type: Option<ReplType> = None;

    for binding in &env.bindings {
        if binding.name == *first {
            current_type = Some(binding.ty.clone());
            break;
        }
    }

    // Walk remaining segments as field accesses on record types
    for segment in chain.iter().skip(1) {
        current_type = match current_type {
            Some(ReplType::Record(fields)) => fields
                .iter()
                .find(|f| f.name == *segment)
                .map(|f| f.ty.clone()),
            Some(ReplType::Nullable(inner)) => match *inner {
                ReplType::Record(fields) => fields
                    .iter()
                    .find(|f| f.name == *segment)
                    .map(|f| f.ty.clone()),
                _ => None,
            },
            _ => None,
        };
    }

    let current_type = current_type?;
    let mut items = Vec::new();
    let field_type = match current_type.clone() {
        ReplType::Record(fields) => Some(fields),
        ReplType::Nullable(inner) => match *inner {
            ReplType::Record(fields) => Some(fields),
            _ => None,
        },
        _ => None,
    };
    if let Some(fields) = field_type {
        items.extend(fields.into_iter().map(|f| CompletionItem {
            label: f.name,
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(f.ty.render()),
            ..Default::default()
        }));
    }
    items.extend(
        vox_runtime::builtin_method_summaries_for_type(&current_type)
            .into_iter()
            .map(|method| CompletionItem {
                label: method.name.clone(),
                kind: Some(CompletionItemKind::METHOD),
                detail: Some(format_function_summary_detail(&method)),
                ..Default::default()
            }),
    );
    if items.is_empty() { None } else { Some(items) }
}

fn extract_receiver_chain(prefix: &str) -> Option<Vec<String>> {
    let trimmed = prefix.trim_end();
    if trimmed.is_empty() {
        return None;
    }

    let bytes = trimmed.as_bytes();
    let mut end = trimmed.len();
    let mut segments: Vec<String> = Vec::new();

    loop {
        // Skip trailing whitespace
        while end > 0 && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end == 0 {
            break;
        }

        // Find identifier: scan backwards for identifier characters
        let mut ident_start = end;
        while ident_start > 0 && is_ident_byte(bytes[ident_start - 1]) {
            ident_start -= 1;
        }

        if ident_start == end {
            break;
        }

        segments.push(trimmed[ident_start..end].to_string());
        end = ident_start;

        // Skip whitespace before separator
        while end > 0 && bytes[end - 1] == b' ' {
            end -= 1;
        }

        // Check for dot separator
        if end > 0 && bytes[end - 1] == b'.' {
            end -= 1;
        } else {
            break;
        }
    }

    segments.reverse();
    if segments.is_empty() {
        None
    } else {
        Some(segments)
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn compute_completion(source: &str, position: Position) -> Option<Vec<CompletionItem>> {
    if let Some(items) = try_dot_completion(source, position) {
        return Some(items);
    }

    use std::collections::BTreeMap;
    use vox_compiler::frontend::ast::*;

    let source_text = SourceText::new("", 0, source);
    let unit = frontend::analyze_source_lossy(&source_text).unit?;

    let mut names: BTreeMap<String, CompletionItemKind> = BTreeMap::new();

    for kw in vox_runtime::language_keywords() {
        names.entry(kw).or_insert(CompletionItemKind::KEYWORD);
    }

    fn collect_block(items: &[BlockItem], names: &mut BTreeMap<String, CompletionItemKind>) {
        for item in items {
            match item {
                BlockItem::LocalValue(lv) => {
                    names
                        .entry(lv.name.clone())
                        .or_insert(CompletionItemKind::VARIABLE);
                }
                _ => {}
            }
        }
    }

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(f) => {
                names
                    .entry(f.name.clone())
                    .or_insert(CompletionItemKind::FUNCTION);
                for p in &f.parameters {
                    names
                        .entry(p.name.clone())
                        .or_insert(CompletionItemKind::VARIABLE);
                }
                if let ExprKind::Block(ref block) = f.body.kind {
                    collect_block(&block.items, &mut names);
                }
            }
            TopLevelItem::Value(v) => {
                names
                    .entry(v.name.clone())
                    .or_insert(CompletionItemKind::VARIABLE);
            }
            TopLevelItem::Param(p) => {
                names
                    .entry(p.name.clone())
                    .or_insert(CompletionItemKind::VARIABLE);
            }
            TopLevelItem::Statement(s) => {
                if let BlockItem::LocalValue(lv) = s {
                    names
                        .entry(lv.name.clone())
                        .or_insert(CompletionItemKind::VARIABLE);
                }
            }
            TopLevelItem::Import(i) => {
                if let Some(last) = i.module.segments.last() {
                    names
                        .entry(last.clone())
                        .or_insert(CompletionItemKind::MODULE);
                }
                names
                    .entry(i.module.to_source_string())
                    .or_insert(CompletionItemKind::MODULE);
            }
        }
    }

    if let Some(ref result) = unit.syntax.result {
        if let ExprKind::Block(ref block) = result.kind {
            collect_block(&block.items, &mut names);
        }
    }

    let completions: Vec<CompletionItem> = names
        .into_iter()
        .map(|(name, kind)| CompletionItem {
            label: name,
            kind: Some(kind),
            ..Default::default()
        })
        .collect();

    Some(completions)
}

fn compute_goto_definition(
    source: &str,
    uri: Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let source_text = SourceText::new("", 0, source);
    let unit = frontend::analyze_source_lossy(&source_text).unit?;
    let offset = position_to_byte_offset(source, position);
    let name = find_name_at_offset(&unit, source, offset)?;
    let symbols = build_symbol_table(&unit);
    let target_span = symbols.get(&name)?;
    let range = byte_span_to_range(source, target_span.start, target_span.end);
    Some(GotoDefinitionResponse::Scalar(Location { uri, range }))
}

fn compute_hover(source: &str, position: Position) -> Option<Hover> {
    let offset = position_to_byte_offset(source, position);
    let source_text = SourceText::new("", 0, source);

    let analysis = frontend::analyze_source_lossy(&source_text);
    let unit = analysis.unit;
    let parse_diags = analysis.diagnostics;

    let env = unit
        .as_ref()
        .and_then(|u| vox_runtime::infer_environment(&u.syntax, &[]).ok());

    let mut parts: Vec<String> = Vec::new();

    if let Some(ref unit) = unit {
        if let Some(decl) = find_hover_decl(source, unit, env.as_ref(), offset) {
            parts.push(format!("```vox\n{}\n```", decl.signature));
            for line in decl.docs {
                parts.push(line);
            }
        }
    }

    if let Some(ref env) = env {
        if let Some(ty) = env.find_type_at_offset(offset) {
            let name = unit
                .as_ref()
                .and_then(|u| find_name_at_offset(u, source, offset))
                .or_else(|| identifier_at_offset(source, offset).map(String::from));
            if parts.is_empty() {
                let label = match name {
                    Some(n) => format!("```vox\n{}: {}\n```", n, ty.render()),
                    None => format!("```vox\n{}\n```", ty.render()),
                };
                parts.push(label);
            }
        }
    }

    if parts.is_empty() {
        if let Some(ref unit) = unit {
            if let Some(name) = find_name_at_offset(unit, source, offset) {
                if let Some(ref env) = env {
                    for binding in &env.bindings {
                        if binding.name == name {
                            parts.push(format!(
                                "```vox\n{}: {}\n```",
                                binding.name,
                                binding.ty.render()
                            ));
                            break;
                        }
                    }
                    if parts.is_empty() {
                        for func in &env.functions {
                            if func.name == name {
                                let sig = format!(
                                    "fun {}({}) -> {}",
                                    func.name,
                                    func.parameters
                                        .iter()
                                        .map(|p| format!("{}: {}", p.name, p.ty.render()))
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                    func.return_type.render()
                                );
                                parts.push(format!("```vox\n{}\n```", sig));
                                break;
                            }
                        }
                    }
                } else {
                    let name_at = identifier_at_offset(source, offset);
                    if name_at == Some(&name) || name_at.is_none() {
                        parts.push(format!("```vox\n{}\n```", name));
                    }
                }
            }
        }
    }

    if parts.is_empty() && unit.is_none() {
        let ident = identifier_at_offset(source, offset).map(String::from);
        if let Some(ref name) = ident {
            parts.push(format!("```vox\n{}\n```", name));
        }
    }

    let diags = unit
        .as_ref()
        .map(|_| parse_diags.iter().collect::<Vec<_>>())
        .unwrap_or_else(|| parse_diags.iter().collect::<Vec<_>>());

    for diag in diags {
        let diag_offset = diag.span.as_ref().map(|s| s.start).unwrap_or(0);
        let diag_end = diag.span.as_ref().map(|s| s.end).unwrap_or(0);
        if diag_offset <= offset && offset < diag_end {
            parts.push(format!(
                "---\n*{}:* {}",
                severity_label(diag.severity),
                diag.message
            ));
        }
    }

    if parts.is_empty() {
        return None;
    }

    let contents = HoverContents::Markup(MarkupContent {
        kind: MarkupKind::Markdown,
        value: parts.join("\n"),
    });

    Some(Hover {
        contents,
        range: None,
    })
}

#[derive(Debug, Clone)]
struct HoverDecl {
    name: String,
    span: vox_core::diagnostics::TextSpan,
    name_span: vox_core::diagnostics::TextSpan,
    signature: String,
    docs: Vec<String>,
}

fn find_hover_decl(
    source: &str,
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    env: Option<&vox_runtime::TypeEnvironment>,
    offset: usize,
) -> Option<HoverDecl> {
    let declarations = build_hover_decls(source, unit, env);

    if let Some(decl) = declarations
        .iter()
        .find(|decl| offset >= decl.name_span.start && offset < decl.name_span.end)
    {
        return Some(decl.clone());
    }

    let name = find_name_at_offset(unit, source, offset)?;
    let symbols = build_symbol_table(unit);
    let target_span = symbols.get(&name)?;

    declarations
        .into_iter()
        .find(|decl| decl.name == name && spans_equal(&decl.span, target_span))
}

fn build_hover_decls(
    source: &str,
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    env: Option<&vox_runtime::TypeEnvironment>,
) -> Vec<HoverDecl> {
    use vox_compiler::frontend::ast::*;

    let mut declarations = Vec::new();

    fn walk_expr(
        source: &str,
        expr: &Expr,
        env: Option<&vox_runtime::TypeEnvironment>,
        declarations: &mut Vec<HoverDecl>,
    ) {
        match &expr.kind {
            ExprKind::Block(block) => walk_block(source, block, env, declarations),
            ExprKind::If(if_expr) => {
                for branch in &if_expr.branches {
                    walk_expr(source, &branch.condition, env, declarations);
                    walk_block(source, &branch.body, env, declarations);
                }
                if let Some(ref else_branch) = if_expr.else_branch {
                    walk_block(source, else_branch, env, declarations);
                }
            }
            ExprKind::When(when_expr) => {
                walk_expr(source, &when_expr.subject, env, declarations);
                for arm in &when_expr.arms {
                    if let Some(ref binding) = arm.binding {
                        if let Some(name_span) = find_identifier_span_in(source, &arm.span, binding)
                        {
                            let ty = env
                                .and_then(|env| env.find_type_at_offset(arm.body.span.start))
                                .map(|ty| ty.render())
                                .unwrap_or_else(|| "Unknown".to_owned());
                            declarations.push(HoverDecl {
                                name: binding.clone(),
                                span: arm.span.clone(),
                                name_span,
                                signature: format!("val {binding}: {ty}"),
                                docs: Vec::new(),
                            });
                        }
                    }
                    walk_expr(source, &arm.body, env, declarations);
                }
                if let Some(ref else_arm) = when_expr.else_arm {
                    walk_expr(source, else_arm, env, declarations);
                }
            }
            ExprKind::For(for_expr) => {
                use vox_compiler::frontend::ast::ForHeader;
                if let Some(ref init) = for_expr.init {
                    walk_block_item(source, init.as_ref(), env, declarations);
                }
                match &for_expr.header {
                    ForHeader::In { pattern, iterable } => {
                        walk_expr(source, iterable, env, declarations);
                        if let Some(name_span) =
                            find_identifier_span_in(source, &for_expr.span, pattern)
                        {
                            let ty = env
                                .and_then(|env| env.find_type_at_offset(iterable.span.start))
                                .map(|ty| ty.render())
                                .unwrap_or_else(|| "Unknown".to_owned());
                            declarations.push(HoverDecl {
                                name: pattern.clone(),
                                span: for_expr.body.span.clone(),
                                name_span,
                                signature: format!("val {pattern}: {ty}"),
                                docs: Vec::new(),
                            });
                        }
                    }
                    ForHeader::Condition(condition) => {
                        walk_expr(source, condition, env, declarations)
                    }
                }
                walk_block(source, &for_expr.body, env, declarations);
            }
            ExprKind::Lambda(lambda) => {
                for parameter in &lambda.parameters {
                    if let Some(name_span) =
                        find_identifier_span_in(source, &parameter.span, &parameter.name)
                    {
                        let ty = parameter
                            .ty
                            .as_ref()
                            .map(TypeSyntax::to_source_string)
                            .unwrap_or_else(|| "Unknown".to_owned());
                        declarations.push(HoverDecl {
                            name: parameter.name.clone(),
                            span: parameter.span.clone(),
                            name_span,
                            signature: format!("param {}: {ty}", parameter.name),
                            docs: Vec::new(),
                        });
                    }
                }
                walk_expr(source, &lambda.body, env, declarations);
            }
            ExprKind::Call { callee, arguments } => {
                walk_expr(source, callee, env, declarations);
                for argument in arguments {
                    walk_argument(source, argument, env, declarations);
                }
            }
            ExprKind::ReceiverCall {
                receiver,
                arguments,
                ..
            } => {
                walk_expr(source, receiver, env, declarations);
                for argument in arguments {
                    walk_argument(source, argument, env, declarations);
                }
            }
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for item in items {
                    walk_expr(source, item, env, declarations);
                }
            }
            ExprKind::Record(fields) => {
                for field in fields {
                    walk_expr(source, &field.value, env, declarations);
                }
            }
            ExprKind::Index { target, index } => {
                walk_expr(source, target, env, declarations);
                walk_expr(source, index, env, declarations);
            }
            ExprKind::Field { target, .. }
            | ExprKind::SafeField { target, .. }
            | ExprKind::NonNull { target } => walk_expr(source, target, env, declarations),
            ExprKind::Unary { expr: inner, .. } => walk_expr(source, inner, env, declarations),
            ExprKind::Binary { left, right, .. } => {
                walk_expr(source, left, env, declarations);
                walk_expr(source, right, env, declarations);
            }
            ExprKind::Range(range) => {
                if let Some(ref start) = range.start {
                    walk_expr(source, start, env, declarations);
                }
                if let Some(ref end) = range.end {
                    walk_expr(source, end, env, declarations);
                }
            }
            ExprKind::Intrinsic(intr) => match intr {
                IntrinsicExpr::Updated(updated) => {
                    walk_expr(source, &updated.target, env, declarations);
                    for update in &updated.updates {
                        walk_expr(source, &update.value, env, declarations);
                    }
                }
                IntrinsicExpr::Econ(econ) => walk_block(source, &econ.body, env, declarations),
            },
            ExprKind::Name(_)
            | ExprKind::Integer(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Null
            | ExprKind::String(_) => {}
        }
    }

    fn walk_argument(
        source: &str,
        argument: &Argument,
        env: Option<&vox_runtime::TypeEnvironment>,
        declarations: &mut Vec<HoverDecl>,
    ) {
        match argument {
            Argument::Positional(expr) | Argument::Named { value: expr, .. } => {
                walk_expr(source, expr, env, declarations);
            }
        }
    }

    fn walk_block(
        source: &str,
        block: &BlockExpr,
        env: Option<&vox_runtime::TypeEnvironment>,
        declarations: &mut Vec<HoverDecl>,
    ) {
        for item in &block.items {
            walk_block_item(source, item, env, declarations);
        }
        if let Some(ref trailing) = block.trailing {
            walk_expr(source, trailing, env, declarations);
        }
    }

    fn walk_block_item(
        source: &str,
        item: &BlockItem,
        env: Option<&vox_runtime::TypeEnvironment>,
        declarations: &mut Vec<HoverDecl>,
    ) {
        match item {
            BlockItem::LocalValue(value) => {
                declarations.extend(local_value_hover_decl(source, value, env, None));
                walk_expr(source, &value.initializer, env, declarations);
            }
            BlockItem::Assignment(assignment) => {
                walk_expr(source, &assignment.value, env, declarations)
            }
            BlockItem::CompoundAssignment(assignment) => {
                walk_expr(source, &assignment.value, env, declarations)
            }
            BlockItem::Return(statement) => {
                if let Some(ref value) = statement.value {
                    walk_expr(source, value, env, declarations);
                }
            }
            BlockItem::Panic(_) | BlockItem::Break(_) | BlockItem::Continue(_) => {}
            BlockItem::BlockStatement(expr) | BlockItem::Expr(expr) => {
                walk_expr(source, expr, env, declarations);
            }
        }
    }

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(function) => {
                if let Some(name_span) =
                    find_identifier_span_in(source, &function.span, &function.name)
                {
                    declarations.push(HoverDecl {
                        name: function.name.clone(),
                        span: function.span.clone(),
                        name_span,
                        signature: function_hover_signature(function, env),
                        docs: function_docs(source, function),
                    });
                }
                for parameter in &function.parameters {
                    if let Some(name_span) =
                        find_identifier_span_in(source, &parameter.span, &parameter.name)
                    {
                        let ty = parameter.ty.to_source_string();
                        declarations.push(HoverDecl {
                            name: parameter.name.clone(),
                            span: parameter.span.clone(),
                            name_span,
                            signature: format!("param {}: {ty}", parameter.name),
                            docs: Vec::new(),
                        });
                    }
                    if let Some(ref default) = parameter.default {
                        walk_expr(source, default, env, &mut declarations);
                    }
                }
                walk_expr(source, &function.body, env, &mut declarations);
            }
            TopLevelItem::Value(value) => {
                declarations.extend(value_hover_decl(source, value, env));
                walk_expr(source, &value.initializer, env, &mut declarations);
            }
            TopLevelItem::Param(parameter) => {
                if let Some(name_span) =
                    find_identifier_span_in(source, &parameter.span, &parameter.name)
                {
                    declarations.push(HoverDecl {
                        name: parameter.name.clone(),
                        span: parameter.span.clone(),
                        name_span,
                        signature: format!(
                            "param {}: {}",
                            parameter.name,
                            parameter.ty.to_source_string()
                        ),
                        docs: declaration_docs_for_span(source, &parameter.span, false),
                    });
                }
                if let Some(ref default) = parameter.default {
                    walk_expr(source, default, env, &mut declarations);
                }
            }
            TopLevelItem::Import(_) => {}
            TopLevelItem::Statement(statement) => {
                walk_block_item(source, statement, env, &mut declarations);
            }
        }
    }

    if let Some(ref result) = unit.syntax.result {
        walk_expr(source, result, env, &mut declarations);
    }

    declarations
}

fn value_hover_decl(
    source: &str,
    value: &vox_compiler::frontend::ast::ValueDecl,
    env: Option<&vox_runtime::TypeEnvironment>,
) -> Option<HoverDecl> {
    let docs = declaration_docs_for_span(source, &value.span, true);
    local_like_hover_decl(
        source,
        &value.name,
        &value.span,
        value.mutability,
        env,
        docs,
    )
}

fn local_value_hover_decl(
    source: &str,
    value: &vox_compiler::frontend::ast::LocalValueDecl,
    env: Option<&vox_runtime::TypeEnvironment>,
    docs: Option<Vec<String>>,
) -> Option<HoverDecl> {
    let docs = docs.unwrap_or_else(|| declaration_docs_for_span(source, &value.span, true));
    local_like_hover_decl(
        source,
        &value.name,
        &value.span,
        value.mutability,
        env,
        docs,
    )
}

fn local_like_hover_decl(
    source: &str,
    name: &str,
    span: &vox_core::diagnostics::TextSpan,
    mutability: vox_compiler::frontend::ast::Mutability,
    env: Option<&vox_runtime::TypeEnvironment>,
    docs: Vec<String>,
) -> Option<HoverDecl> {
    let name_span = find_identifier_span_in(source, span, name)?;
    let ty = env
        .and_then(|env| env.find_type_at_offset(span.start))
        .map(|ty| ty.render())
        .unwrap_or_else(|| "Unknown".to_owned());
    let keyword = match mutability {
        vox_compiler::frontend::ast::Mutability::Val => "val",
        vox_compiler::frontend::ast::Mutability::Var => "var",
    };

    Some(HoverDecl {
        name: name.to_owned(),
        span: span.clone(),
        name_span,
        signature: format!("{keyword} {name}: {ty}"),
        docs,
    })
}

fn function_hover_signature(
    function: &vox_compiler::frontend::ast::FunctionDecl,
    env: Option<&vox_runtime::TypeEnvironment>,
) -> String {
    if let Some(summary) =
        env.and_then(|env| env.functions.iter().find(|f| f.name == function.name))
    {
        let generics = if summary.generic_parameters.is_empty() {
            String::new()
        } else {
            format!(
                "[{}]",
                summary
                    .generic_parameters
                    .iter()
                    .map(|parameter| format!("{}: {}", parameter.name, parameter.bound))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let parameters = summary
            .parameters
            .iter()
            .map(|parameter| {
                let default = if parameter.has_default { " = ..." } else { "" };
                format!("{}: {}{default}", parameter.name, parameter.ty.render())
            })
            .collect::<Vec<_>>()
            .join(", ");
        let effect = if summary.evil { "evil " } else { "" };
        return format!(
            "{effect}fun {}{generics}({parameters}) -> {}",
            summary.name,
            summary.return_type.render()
        );
    }

    let generics = if function.generic_parameters.is_empty() {
        String::new()
    } else {
        format!(
            "[{}]",
            function
                .generic_parameters
                .iter()
                .map(|parameter| format!("{}: {}", parameter.name, parameter.bound))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let parameters = function
        .parameters
        .iter()
        .map(|parameter| {
            let default = if parameter.default.is_some() {
                " = ..."
            } else {
                ""
            };
            format!(
                "{}: {}{default}",
                parameter.name,
                parameter.ty.to_source_string()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let effect = if function.evil { "evil " } else { "" };
    let result = function
        .return_type
        .as_ref()
        .map(vox_compiler::frontend::ast::TypeSyntax::to_source_string)
        .unwrap_or_else(|| "Unknown".to_owned());
    format!(
        "{effect}fun {}{generics}({parameters}) -> {result}",
        function.name
    )
}

fn find_identifier_span_in(
    source: &str,
    span: &vox_core::diagnostics::TextSpan,
    name: &str,
) -> Option<vox_core::diagnostics::TextSpan> {
    use vox_compiler::frontend::lexer::{Lexer, TokenKind};

    let end = span.end.min(source.len());
    let mut search_start = span.start.min(end);

    if let Ok(tokens) = Lexer::new(&source[search_start..end], search_start).lex() {
        for token in tokens {
            if let TokenKind::Identifier(identifier) = token.kind {
                if identifier == name {
                    return Some(token.span);
                }
            }
        }
    }

    while let Some(relative) = source[search_start..end].find(name) {
        let start = search_start + relative;
        let candidate_end = start + name.len();
        let before = start
            .checked_sub(1)
            .and_then(|idx| source.as_bytes().get(idx))
            .copied();
        let after = source.as_bytes().get(candidate_end).copied();
        let before_ok = before.is_none_or(|b| !is_ident_byte(b));
        let after_ok = after.is_none_or(|b| !is_ident_byte(b));
        if before_ok && after_ok {
            return Some(vox_core::diagnostics::TextSpan::new(start, candidate_end));
        }
        search_start = candidate_end;
    }

    None
}

fn spans_equal(
    left: &vox_core::diagnostics::TextSpan,
    right: &vox_core::diagnostics::TextSpan,
) -> bool {
    left.start == right.start && left.end == right.end
}

fn severity_label(severity: vox_core::diagnostics::Severity) -> &'static str {
    match severity {
        vox_core::diagnostics::Severity::Note => "Note",
        vox_core::diagnostics::Severity::Warning => "Warning",
        vox_core::diagnostics::Severity::Error => "Error",
    }
}

fn block_item_span(
    item: &vox_compiler::frontend::ast::BlockItem,
) -> vox_core::diagnostics::TextSpan {
    use vox_compiler::frontend::ast::*;
    match item {
        BlockItem::LocalValue(lv) => lv.span.clone(),
        BlockItem::Assignment(a) => a.span.clone(),
        BlockItem::CompoundAssignment(ca) => ca.span.clone(),
        BlockItem::Return(r) => r.span.clone(),
        BlockItem::Panic(p) => p.span.clone(),
        BlockItem::Break(b) => b.span.clone(),
        BlockItem::Continue(c) => c.span.clone(),
        BlockItem::BlockStatement(e) | BlockItem::Expr(e) => e.span.clone(),
    }
}

fn collect_function_body_docs(
    source: &str,
    func: &vox_compiler::frontend::ast::FunctionDecl,
) -> Vec<String> {
    let body_str = &source[func.body.span.start..func.body.span.end];
    let mut docs = Vec::new();

    for line in body_str.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("///") {
            docs.push(trimmed.to_string());
        }
    }

    docs
}

fn function_docs(
    source: &str,
    function: &vox_compiler::frontend::ast::FunctionDecl,
) -> Vec<String> {
    join_doc_blocks([
        collect_head_doc_block(
            source,
            declaration_leading_start(source, function.span.start),
        ),
        clean_doc_block(collect_function_body_docs(source, function)),
    ])
}

fn declaration_docs_for_span(
    source: &str,
    span: &vox_core::diagnostics::TextSpan,
    include_trailing: bool,
) -> Vec<String> {
    let mut blocks = vec![collect_head_doc_block(
        source,
        declaration_leading_start(source, span.start),
    )];
    if include_trailing {
        blocks.push(collect_same_line_trailing_doc_block(source, span));
    }
    join_doc_blocks(blocks)
}

fn declaration_leading_start(source: &str, keyword_start: usize) -> usize {
    use vox_compiler::frontend::lexer::{Lexer, TokenKind};

    let prefix_end = keyword_start.min(source.len());
    let Ok(tokens) = Lexer::new(&source[..prefix_end], 0).lex() else {
        return keyword_start;
    };
    let Some(previous) = tokens
        .iter()
        .rev()
        .find(|token| !matches!(token.kind, TokenKind::Eof))
    else {
        return keyword_start;
    };
    let modifier_touches_keyword = source[previous.span.end..prefix_end]
        .chars()
        .all(char::is_whitespace);

    if modifier_touches_keyword
        && matches!(previous.kind, TokenKind::Public | TokenKind::Private)
        && is_line_leading(source, previous.span.start)
    {
        previous.span.start
    } else {
        keyword_start
    }
}

fn collect_head_doc_block(source: &str, declaration_start: usize) -> Vec<String> {
    use vox_compiler::frontend::lexer::{Lexer, TokenKind};

    let prefix_end = declaration_start.min(source.len());
    let Ok(tokens) = Lexer::new(&source[..prefix_end], 0).lex() else {
        return Vec::new();
    };

    let mut lines = Vec::new();
    let mut next_start = prefix_end;
    for token in tokens
        .iter()
        .rev()
        .filter(|token| !matches!(token.kind, TokenKind::Eof))
    {
        let gap = &source[token.span.end..next_start];
        if !gap.chars().all(char::is_whitespace) || gap.chars().filter(|&c| c == '\n').count() > 1 {
            break;
        }

        match &token.kind {
            TokenKind::DocComment(text) if is_line_leading(source, token.span.start) => {
                lines.push(text.clone());
                next_start = token.span.start;
            }
            _ => break,
        }
    }

    lines.reverse();
    clean_doc_block(lines)
}

fn collect_same_line_trailing_doc_block(
    source: &str,
    span: &vox_core::diagnostics::TextSpan,
) -> Vec<String> {
    let start = span.end.min(source.len());
    let line_end = source[start..]
        .find('\n')
        .map(|relative| start + relative)
        .unwrap_or(source.len());
    let rest = &source[start..line_end];
    let trimmed = rest.trim_start();
    if let Some(text) = trimmed.strip_prefix("///") {
        clean_doc_block(vec![text.to_owned()])
    } else {
        Vec::new()
    }
}

fn clean_doc_block(lines: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut lines = lines
        .into_iter()
        .map(|line| clean_doc_line(&line))
        .collect::<Vec<_>>();

    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

fn clean_doc_line(line: &str) -> String {
    let trimmed = line.trim();
    trimmed
        .strip_prefix("///")
        .unwrap_or(trimmed)
        .trim()
        .to_owned()
}

fn join_doc_blocks(blocks: impl IntoIterator<Item = Vec<String>>) -> Vec<String> {
    let mut docs: Vec<String> = Vec::new();
    for block in blocks {
        if block.is_empty() {
            continue;
        }
        if !docs.is_empty() && docs.last().is_some_and(|line| !line.is_empty()) {
            docs.push(String::new());
        }
        docs.extend(block);
    }
    docs
}

fn is_line_leading(source: &str, offset: usize) -> bool {
    let offset = offset.min(source.len());
    let line_start = source[..offset].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    source[line_start..offset].chars().all(char::is_whitespace)
}

#[tower_lsp::async_trait]
impl LanguageServer for VoxLanguageServer {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let library_paths = library_paths_from_initialize(&params);
        let libraries = load_libraries(&library_paths);
        let load_errors = libraries.load_errors.clone();
        *self.libraries.lock().unwrap() = libraries;

        for error in load_errors {
            self.client.show_message(MessageType::ERROR, error).await;
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            work_done_progress_options: Default::default(),
                            legend: SemanticTokensLegend {
                                token_types: vec![
                                    "keyword".into(),
                                    "variable".into(),
                                    "string".into(),
                                    "number".into(),
                                    "comment".into(),
                                    "operator".into(),
                                ],
                                token_modifiers: vec![],
                            },
                            range: Some(false),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                        },
                    ),
                ),
                document_symbol_provider: Some(OneOf::Right(DocumentSymbolOptions {
                    work_done_progress_options: Default::default(),
                    label: None,
                })),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".into(), ",".into()]),
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".into()]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {}

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.documents.lock().unwrap().insert(
            params.text_document.uri.clone(),
            params.text_document.text.clone(),
        );
        let libraries = self.libraries.lock().unwrap().clone();
        let diagnostics = collect_diagnostics(
            params.text_document.uri.as_str(),
            &params.text_document.text,
            &libraries,
        )
        .unwrap_or_default();
        self.client
            .publish_diagnostics(params.text_document.uri, diagnostics, None)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.documents
                .lock()
                .unwrap()
                .insert(params.text_document.uri.clone(), change.text.clone());
            let libraries = self.libraries.lock().unwrap().clone();
            let diagnostics =
                collect_diagnostics(params.text_document.uri.as_str(), &change.text, &libraries)
                    .unwrap_or_default();
            self.client
                .publish_diagnostics(params.text_document.uri, diagnostics, None)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .lock()
            .unwrap()
            .remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&params.text_document.uri)
            .cloned();

        match source {
            Some(text) => {
                let data = compute_semantic_tokens(&text);
                Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
                    result_id: None,
                    data,
                })))
            }
            None => Ok(None),
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&params.text_document.uri)
            .cloned();

        match source {
            Some(text) => {
                let symbols = compute_document_symbols(&text);
                Ok(Some(DocumentSymbolResponse::Nested(symbols)))
            }
            None => Ok(None),
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&params.text_document_position_params.text_document.uri)
            .cloned();

        match source {
            Some(text) => Ok(compute_goto_definition(
                &text,
                params.text_document_position_params.text_document.uri,
                params.text_document_position_params.position,
            )),
            None => Ok(None),
        }
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let source = self.documents.lock().unwrap().get(&uri).cloned();

        let text = match source {
            Some(t) => t,
            None => return Ok(None),
        };

        let source_text = SourceText::new("", 0, &text);
        let Some(unit) = frontend::analyze_source_lossy(&source_text).unit else {
            return Ok(None);
        };

        let offset = position_to_byte_offset(&text, params.text_document_position.position);
        let name = match find_name_at_offset(&unit, &text, offset) {
            Some(n) => n,
            None => return Ok(None),
        };

        let mut spans = collect_references(&unit, &text, &name);

        if params.context.include_declaration {
            let symbols = build_symbol_table(&unit);
            if let Some(decl_span) = symbols.get(&name) {
                spans.push(decl_span.clone());
            }
        }

        spans.sort_by_key(|s| s.start);
        spans.dedup_by_key(|s| s.start);

        let locations: Vec<Location> = spans
            .into_iter()
            .map(|span| Location {
                uri: uri.clone(),
                range: byte_span_to_range(&text, span.start, span.end),
            })
            .collect();

        Ok(Some(locations))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&params.text_document_position_params.text_document.uri)
            .cloned();

        match source {
            Some(text) => Ok(compute_hover(
                &text,
                params.text_document_position_params.position,
            )),
            None => Ok(None),
        }
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&params.text_document_position_params.text_document.uri)
            .cloned();

        match source {
            Some(text) => Ok(compute_signature_help(
                &text,
                params.text_document_position_params.position,
            )),
            None => Ok(None),
        }
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&params.text_document_position.text_document.uri)
            .cloned();

        match source {
            Some(text) => {
                let items = compute_completion(&text, params.text_document_position.position);
                Ok(items.map(CompletionResponse::Array))
            }
            None => Ok(None),
        }
    }
}
