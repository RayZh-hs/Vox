use vox_core::{
    diagnostics::{Diagnostic, DiagnosticBag, TextSpan},
    host::ParameterSpec,
    source::{ModuleKind, ModulePath, SourceText, SurfaceHeader},
    types::VoxType,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceParameter {
    pub name: String,
    pub ty: VoxType,
    pub has_default: bool,
}

impl SurfaceParameter {
    pub fn into_spec(self) -> ParameterSpec {
        ParameterSpec {
            name: self.name,
            ty: self.ty,
            has_default: self.has_default,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontEndUnit {
    pub header: SurfaceHeader,
    pub parameters: Vec<SurfaceParameter>,
}

pub fn analyze_source(source: &SourceText) -> Result<FrontEndUnit, DiagnosticBag> {
    let header = parse_header(source)?;
    let parameters = parse_parameters(source, header.kind)?;

    Ok(FrontEndUnit { header, parameters })
}

fn parse_header(source: &SourceText) -> Result<SurfaceHeader, DiagnosticBag> {
    let mut diagnostics = DiagnosticBag::default();

    for (offset, statement) in split_statements(&source.text) {
        let Some(header) = parse_header_statement(statement, offset, &mut diagnostics) else {
            continue;
        };

        if diagnostics.has_errors() {
            return Err(diagnostics);
        }

        return Ok(header);
    }

    diagnostics.push(Diagnostic::error(
        "source must start with `package`, `script`, or `evil script`",
    ));
    Err(diagnostics)
}

fn parse_header_statement(
    statement: &str,
    offset: usize,
    diagnostics: &mut DiagnosticBag,
) -> Option<SurfaceHeader> {
    let statement = statement.trim();
    if statement.is_empty() {
        return None;
    }

    let (kind, raw_path, keyword_len) = if let Some(rest) = statement.strip_prefix("package ") {
        (ModuleKind::Package, rest, "package ".len())
    } else if let Some(rest) = statement.strip_prefix("evil script ") {
        (
            ModuleKind::Script { evil: true },
            rest,
            "evil script ".len(),
        )
    } else if let Some(rest) = statement.strip_prefix("script ") {
        (ModuleKind::Script { evil: false }, rest, "script ".len())
    } else {
        diagnostics.push(
            Diagnostic::error("first statement must declare a package or script")
                .with_span(TextSpan::new(offset, offset + statement.len())),
        );
        return None;
    };

    match ModulePath::parse(raw_path.trim()) {
        Ok(module) => Some(SurfaceHeader {
            kind,
            module,
            span: TextSpan::new(offset, offset + keyword_len + raw_path.trim().len()),
        }),
        Err(diagnostic) => {
            diagnostics.push(diagnostic.with_span(TextSpan::new(offset, offset + statement.len())));
            None
        }
    }
}

fn parse_parameters(
    source: &SourceText,
    module_kind: ModuleKind,
) -> Result<Vec<SurfaceParameter>, DiagnosticBag> {
    let mut diagnostics = DiagnosticBag::default();
    let mut parameters = Vec::new();

    for (offset, statement) in split_statements(&source.text) {
        let statement = statement.trim();
        if !statement.starts_with("param ") {
            continue;
        }

        if !matches!(module_kind, ModuleKind::Script { .. }) {
            diagnostics.push(
                Diagnostic::error("`param` is only valid in scripts")
                    .with_span(TextSpan::new(offset, offset + statement.len())),
            );
            continue;
        }

        match parse_parameter_statement(statement, offset) {
            Ok(parameter) => parameters.push(parameter),
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
    }

    if diagnostics.has_errors() {
        Err(diagnostics)
    } else {
        Ok(parameters)
    }
}

fn parse_parameter_statement(
    statement: &str,
    offset: usize,
) -> Result<SurfaceParameter, Diagnostic> {
    let raw = statement
        .strip_prefix("param ")
        .expect("parameter parsing only called for param statements");

    let (name, rest) = raw
        .split_once(':')
        .ok_or_else(|| Diagnostic::error("parameter is missing a `:` type annotation"))?;

    let name = name.trim();
    if name.is_empty() {
        return Err(Diagnostic::error("parameter name cannot be empty")
            .with_span(TextSpan::new(offset, offset + statement.len())));
    }

    let (raw_type, has_default) = match rest.split_once('=') {
        Some((left, _)) => (left.trim(), true),
        None => (rest.trim(), false),
    };

    if raw_type.is_empty() {
        return Err(Diagnostic::error("parameter type cannot be empty")
            .with_span(TextSpan::new(offset, offset + statement.len())));
    }

    Ok(SurfaceParameter {
        name: name.to_owned(),
        ty: VoxType::opaque_surface(raw_type),
        has_default,
    })
}

fn split_statements(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split_inclusive(';').scan(0usize, |offset, chunk| {
        let start = *offset;
        *offset += chunk.len();

        let statement = chunk
            .split("//")
            .next()
            .unwrap_or_default()
            .trim_end_matches(';')
            .trim();

        Some((start, statement))
    })
}

#[cfg(test)]
mod tests {
    use super::analyze_source;
    use vox_core::source::{ModuleKind, SourceText};

    #[test]
    fn parses_script_header_and_parameters() {
        let source = SourceText::new(
            "demo.vox",
            1,
            r#"
            script voxini.demo;
            param input: image.Image;
            param strength: Float = 0.5;
            "#,
        );

        let unit = analyze_source(&source).expect("script should parse");
        assert_eq!(unit.header.kind, ModuleKind::Script { evil: false });
        assert_eq!(unit.header.module.as_str(), "voxini.demo");
        assert_eq!(unit.parameters.len(), 2);
        assert!(unit.parameters[1].has_default);
    }

    #[test]
    fn rejects_params_in_packages() {
        let source = SourceText::new(
            "pkg.vox",
            1,
            r#"
            package voxini.pkg;
            param input: image.Image;
            "#,
        );

        let diagnostics = analyze_source(&source).expect_err("package params should fail");
        assert!(diagnostics.has_errors());
    }
}
