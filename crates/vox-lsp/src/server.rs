use std::collections::HashMap;
use std::sync::Mutex;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use vox_compiler::frontend;
use vox_core::diagnostics::DiagnosticBag;
use vox_core::source::SourceText;

pub struct VoxLanguageServer {
    client: Client,
    documents: Mutex<HashMap<Url, String>>,
}

impl VoxLanguageServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Mutex::new(HashMap::new()),
        }
    }
}

fn collect_diagnostics(path: &str, source: &str) -> Option<Vec<Diagnostic>> {
    let source_text = SourceText::new(path, 0, source);
    let result = frontend::analyze_source(&source_text);

    let mut diagnostics = match result {
        Ok(_) => Vec::new(),
        Err(bag) => convert_diagnostics(&source_text.text, bag),
    };

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

fn collect_docstring_warnings(source: &str) -> Vec<Diagnostic> {
    use vox_compiler::frontend::lexer::{Lexer, TokenKind};

    let tokens = match Lexer::new(source, 0).lex() {
        Ok(tokens) => tokens,
        Err(_) => return Vec::new(),
    };

    let source_text = SourceText::new("", 0, source);
    let unit = frontend::analyze_source(&source_text).ok();

    let mut warnings = Vec::new();
    let mut depth = 0u32;
    let mut i = 0;

    while i < tokens.len() {
        let token = &tokens[i];
        match &token.kind {
            TokenKind::DocComment(_) => {
                if is_valid_doc_comment(&tokens, i, depth, &unit) {
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
        if pos == 0 {
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

fn is_doc_inside_span(doc_span: &vox_core::diagnostics::TextSpan, decl_span: &vox_core::diagnostics::TextSpan) -> bool {
    doc_span.start >= decl_span.start && doc_span.end <= decl_span.end
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

fn segment_at_offset(_source: &str, qname: &vox_compiler::frontend::ast::QualifiedName, offset: usize) -> Option<String> {
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

    let Ok(tokens) = Lexer::new(source, 0).lex() else {
        return Vec::new();
    };

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
    let Ok(unit) = frontend::analyze_source(&source_text) else {
        return Vec::new();
    };

    let mut symbols = Vec::new();

    for item in &unit.syntax.items {
        let (name, kind, span) = match item {
            TopLevelItem::Function(f) => (f.name.clone(), SymbolKind::FUNCTION, f.span.clone()),
            TopLevelItem::Value(v) => (v.name.clone(), SymbolKind::VARIABLE, v.span.clone()),
            TopLevelItem::Param(p) => (p.name.clone(), SymbolKind::VARIABLE, p.span.clone()),
            TopLevelItem::Import(i) => (i.module.to_source_string(), SymbolKind::MODULE, i.span.clone()),
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

fn build_symbol_table(unit: &vox_compiler::frontend::ast::FrontendUnit) -> HashMap<String, vox_core::diagnostics::TextSpan> {
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
            ExprKind::ReceiverCall { receiver, arguments, .. } => {
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

    fn collect_type_syntax(ty: &TypeSyntax, target: &str, spans: &mut Vec<vox_core::diagnostics::TextSpan>) {
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

    fn collect_in_expr(expr: &Expr, target: &str, spans: &mut Vec<vox_core::diagnostics::TextSpan>) {
        match &expr.kind {
            ExprKind::Name(qname) => {
                if qname.segments.last().map(|s| s.as_str()) == Some(target) {
                    spans.push(qname.span.clone());
                }
            }
            ExprKind::ReceiverCall { receiver, callee, arguments } => {
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
                            BlockItem::BlockStatement(e) | BlockItem::Expr(e) => walk_expr(e, offset, best),
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
                                BlockItem::BlockStatement(e) | BlockItem::Expr(e) => walk_expr(e, offset, best),
                            }
                        }
                        if let Some(ref trailing) = branch.body.trailing {
                            walk_expr(trailing, offset, best);
                        }
                    }
                    if let Some(ref else_branch) = if_expr.else_branch {
                        for item in &else_branch.items {
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
                                BlockItem::BlockStatement(e) | BlockItem::Expr(e) => walk_expr(e, offset, best),
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
                            BlockItem::BlockStatement(e) | BlockItem::Expr(e) => walk_expr(e, offset, best),
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
                ExprKind::ReceiverCall { receiver, callee: _, arguments } => {
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
                                BlockItem::BlockStatement(e) | BlockItem::Expr(e) => walk_expr(e, offset, best),
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
                        if let ExprKind::Name(ref qname) = callee.kind {
                            if !qname.segments.is_empty() {
                                *best = Some((qname.to_source_string(), i));
                            }
                        }
                    }
                }
                walk_children(expr, offset, best);
            }
            ExprKind::ReceiverCall { callee, arguments, .. } => {
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

fn compute_signature_help(source: &str, position: Position) -> Option<SignatureHelp> {
    use vox_compiler::frontend::ast::*;
    let source_text = SourceText::new("", 0, source);
    let unit = frontend::analyze_source(&source_text).ok()?;
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
    let unit = frontend::analyze_source(&source_text).ok()?;
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
            Some(ReplType::Record(fields)) => {
                fields
                    .iter()
                    .find(|f| f.name == *segment)
                    .map(|f| f.ty.clone())
            }
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

    // Generate completions from the final type's fields
    match current_type {
        Some(ReplType::Record(fields)) => Some(
            fields
                .iter()
                .map(|f| CompletionItem {
                    label: f.name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(f.ty.render()),
                    ..Default::default()
                })
                .collect(),
        ),
        Some(ReplType::Nullable(inner)) => {
            if let ReplType::Record(fields) = *inner {
                Some(
                    fields
                        .iter()
                        .map(|f| CompletionItem {
                            label: f.name.clone(),
                            kind: Some(CompletionItemKind::FIELD),
                            detail: Some(f.ty.render()),
                            ..Default::default()
                        })
                        .collect(),
                )
            } else {
                None
            }
        }
        _ => None,
    }
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
    if segments.is_empty() { None } else { Some(segments) }
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
    let unit = frontend::analyze_source(&source_text).ok()?;

    let mut names: BTreeMap<String, CompletionItemKind> = BTreeMap::new();

    for kw in vox_runtime::language_keywords() {
        names.entry(kw).or_insert(CompletionItemKind::KEYWORD);
    }

    fn collect_block(
        items: &[BlockItem],
        names: &mut BTreeMap<String, CompletionItemKind>,
    ) {
        for item in items {
            match item {
                BlockItem::LocalValue(lv) => {
                    names.entry(lv.name.clone()).or_insert(CompletionItemKind::VARIABLE);
                }
                _ => {}
            }
        }
    }

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(f) => {
                names.entry(f.name.clone()).or_insert(CompletionItemKind::FUNCTION);
                for p in &f.parameters {
                    names.entry(p.name.clone()).or_insert(CompletionItemKind::VARIABLE);
                }
                if let ExprKind::Block(ref block) = f.body.kind {
                    collect_block(&block.items, &mut names);
                }
            }
            TopLevelItem::Value(v) => {
                names.entry(v.name.clone()).or_insert(CompletionItemKind::VARIABLE);
            }
            TopLevelItem::Param(p) => {
                names.entry(p.name.clone()).or_insert(CompletionItemKind::VARIABLE);
            }
            TopLevelItem::Statement(s) => {
                if let BlockItem::LocalValue(lv) = s {
                    names.entry(lv.name.clone()).or_insert(CompletionItemKind::VARIABLE);
                }
            }
            TopLevelItem::Import(i) => {
                if let Some(last) = i.module.segments.last() {
                    names.entry(last.clone()).or_insert(CompletionItemKind::MODULE);
                }
                names.entry(i.module.to_source_string()).or_insert(CompletionItemKind::MODULE);
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

fn compute_goto_definition(source: &str, uri: Url, position: Position) -> Option<GotoDefinitionResponse> {
    let source_text = SourceText::new("", 0, source);
    let unit = frontend::analyze_source(&source_text).ok()?;
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

    let (unit, parse_diags) = match frontend::analyze_source(&source_text) {
        Ok(unit) => (Some(unit), DiagnosticBag::default()),
        Err(bag) => (None, bag),
    };

    let env = unit
        .as_ref()
        .and_then(|u| vox_runtime::infer_environment(&u.syntax, &[]).ok());

    let mut parts: Vec<String> = Vec::new();

    if let Some(ref unit) = unit {
        if let Some(hover_line) = build_decl_hover_line(source, unit, offset) {
            parts.push(format!("```vox\n{}\n```", hover_line));
        }
    }

    let head_docs = unit
        .as_ref()
        .and_then(|u| find_head_docs_at_offset(u, offset));
    let body_docs = unit.as_ref().and_then(|u| find_body_docs(source, u, offset));

    if let Some(ref docs) = head_docs {
        for line in docs {
            parts.push(line.clone());
        }
    }
    if let Some(ref docs) = body_docs {
        for line in docs {
            parts.push(line.clone());
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
            parts.push(format!("---\n*{}:* {}", severity_label(diag.severity), diag.message));
        }
    }

    if parts.is_empty() {
        return None;
    }

    let contents = HoverContents::Markup(MarkupContent {
        kind: MarkupKind::Markdown,
        value: parts.join("\n"),
    });

    Some(Hover { contents, range: None })
}

fn severity_label(severity: vox_core::diagnostics::Severity) -> &'static str {
    match severity {
        vox_core::diagnostics::Severity::Note => "Note",
        vox_core::diagnostics::Severity::Warning => "Warning",
        vox_core::diagnostics::Severity::Error => "Error",
    }
}

fn build_decl_hover_line(
    source: &str,
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    offset: usize,
) -> Option<String> {
    use vox_compiler::frontend::ast::*;

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(f) => {
                if offset >= f.span.start && offset < f.span.end {
                    return Some(extract_source_line(source, f.span.start, f.span.end));
                }
            }
            TopLevelItem::Value(v) => {
                if offset >= v.span.start && offset < v.span.end {
                    return Some(extract_source_line(source, v.span.start, v.span.end));
                }
            }
            TopLevelItem::Param(p) => {
                if offset >= p.span.start && offset < p.span.end {
                    return Some(extract_source_line(source, p.span.start, p.span.end));
                }
            }
            TopLevelItem::Import(i) => {
                if offset >= i.span.start && offset < i.span.end {
                    return Some(extract_source_line(source, i.span.start, i.span.end));
                }
            }
            TopLevelItem::Statement(s) => {
                let span = block_item_span(s);
                if offset >= span.start && offset < span.end {
                    return Some(extract_source_line(source, span.start, span.end));
                }
            }
        }
    }

    if offset >= unit.header.span.start && offset < unit.header.span.end {
        return Some(extract_source_line(
            source,
            unit.header.span.start,
            unit.header.span.end,
        ));
    }

    if let Some(ref result) = unit.syntax.result {
        if offset >= result.span.start && offset < result.span.end {
            return Some(extract_source_line(source, result.span.start, result.span.end));
        }
    }

    None
}

fn block_item_span(item: &vox_compiler::frontend::ast::BlockItem) -> vox_core::diagnostics::TextSpan {
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

fn extract_source_line(source: &str, start: usize, end: usize) -> String {
    let start = start.min(source.len());
    let end = end.min(source.len());

    let line_start = source[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = source[end..].find('\n').map(|i| end + i).unwrap_or(source.len());

    source[line_start..line_end].trim_end().to_string()
}

fn find_head_docs_at_offset(
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    offset: usize,
) -> Option<Vec<String>> {
    use vox_compiler::frontend::ast::*;

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(f) => {
                if offset >= f.span.start && offset < f.span.end && !f.docs.is_empty() {
                    return Some(
                        f.docs
                            .iter()
                            .map(|d| d.trim_end().to_string())
                            .collect(),
                    );
                }
            }
            TopLevelItem::Value(v) => {
                if offset >= v.span.start && offset < v.span.end && !v.docs.is_empty() {
                    return Some(
                        v.docs
                            .iter()
                            .map(|d| d.trim_end().to_string())
                            .collect(),
                    );
                }
            }
            TopLevelItem::Param(p) => {
                if offset >= p.span.start && offset < p.span.end && !p.docs.is_empty() {
                    return Some(
                        p.docs
                            .iter()
                            .map(|d| d.trim_end().to_string())
                            .collect(),
                    );
                }
            }
            TopLevelItem::Import(i) => {
                if offset >= i.span.start && offset < i.span.end && !i.docs.is_empty() {
                    return Some(
                        i.docs
                            .iter()
                            .map(|d| d.trim_end().to_string())
                            .collect(),
                    );
                }
            }
            _ => {}
        }
    }

    if offset >= unit.header.span.start && offset < unit.header.span.end && !unit.docs.is_empty() {
        return Some(unit.docs.iter().map(|d| d.trim_end().to_string()).collect());
    }

    None
}

fn find_body_docs(
    source: &str,
    unit: &vox_compiler::frontend::ast::FrontendUnit,
    offset: usize,
) -> Option<Vec<String>> {
    use vox_compiler::frontend::ast::*;

    for item in &unit.syntax.items {
        match item {
            TopLevelItem::Function(f) => {
                if offset >= f.span.start && offset < f.span.end {
                    let body_docs = collect_function_body_docs(source, f);
                    if !body_docs.is_empty() {
                        return Some(body_docs);
                    }
                }
            }
            TopLevelItem::Value(v) => {
                if offset >= v.span.start && offset < v.span.end {
                    let trailing = collect_value_trailing_docs(source, &v.span);
                    if !trailing.is_empty() {
                        return Some(trailing);
                    }
                }
            }
            _ => {}
        }
    }

    None
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

fn collect_value_trailing_docs(
    source: &str,
    span: &vox_core::diagnostics::TextSpan,
) -> Vec<String> {
    let decl_str = &source[span.start..span.end];
    let after_last_semi = decl_str.rfind(';').map(|i| i + 1).unwrap_or(0);
    let rest = &decl_str[after_last_semi..];
    let trimmed = rest.trim();
    if trimmed.starts_with("///") {
        vec![trimmed.to_string()]
    } else {
        Vec::new()
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for VoxLanguageServer {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
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
                    }),
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
        self.documents
            .lock()
            .unwrap()
            .insert(params.text_document.uri.clone(), params.text_document.text.clone());
        let diagnostics =
            collect_diagnostics(params.text_document.uri.as_str(), &params.text_document.text)
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
            let diagnostics =
                collect_diagnostics(params.text_document.uri.as_str(), &change.text)
                    .unwrap_or_default();
            self.client
                .publish_diagnostics(params.text_document.uri, diagnostics, None)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.lock().unwrap().remove(&params.text_document.uri);
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

    async fn references(
        &self,
        params: ReferenceParams,
    ) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&uri)
            .cloned();

        let text = match source {
            Some(t) => t,
            None => return Ok(None),
        };

        let source_text = SourceText::new("", 0, &text);
        let unit = match frontend::analyze_source(&source_text) {
            Ok(u) => u,
            Err(_) => return Ok(None),
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

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> Result<Option<SignatureHelp>> {
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

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> Result<Option<CompletionResponse>> {
        let source = self
            .documents
            .lock()
            .unwrap()
            .get(&params.text_document_position.text_document.uri)
            .cloned();

        match source {
            Some(text) => {
                let items = compute_completion(
                    &text,
                    params.text_document_position.position,
                );
                Ok(items.map(CompletionResponse::Array))
            }
            None => Ok(None),
        }
    }
}
