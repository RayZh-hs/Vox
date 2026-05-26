use std::collections::{BTreeMap, BTreeSet};

use crate::{
    host::{FunctionSpec, PackageManifest, ParameterSpec, Purity, TypeSpec},
    source::ModulePath,
    types::{QualifiedTypeName, RecordField, VoxType},
};

pub use inventory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportedSurfaceKind {
    Struct,
    Trait,
}

#[derive(Debug, Clone, Copy)]
pub struct ExportedSurfaceRegistration {
    pub rust_name: &'static str,
    pub vox_name: &'static str,
    pub kind: ExportedSurfaceKind,
    pub order: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ExportedFunctionParameter {
    pub name: &'static str,
    pub rust_type: &'static str,
    pub has_default: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ExportedFunctionRegistration {
    pub rust_name: &'static str,
    pub vox_name: &'static str,
    pub purity: Purity,
    pub parameters: &'static [ExportedFunctionParameter],
    pub return_rust_type: &'static str,
    pub return_type_override: Option<&'static str>,
    pub order: u32,
}

inventory::collect!(ExportedSurfaceRegistration);
inventory::collect!(ExportedFunctionRegistration);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedSurface {
    pub name: String,
    pub kind: Option<ExportedSurfaceKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedPackageExports {
    pub surfaces: Vec<CollectedSurface>,
    pub functions: Vec<FunctionSpec>,
}

pub fn extend_manifest_with_registered_exports(
    manifest: PackageManifest,
) -> Result<PackageManifest, String> {
    let collected = collect_registered_package_exports(&manifest)?;
    let package = manifest.package.clone();
    Ok(PackageManifest {
        package: package.clone(),
        types: collected
            .surfaces
            .into_iter()
            .map(|surface| TypeSpec {
                name: QualifiedTypeName {
                    module: package.clone(),
                    name: surface.name,
                },
            })
            .collect(),
        functions: collected.functions,
    })
}

pub fn collect_registered_package_exports(
    manifest: &PackageManifest,
) -> Result<CollectedPackageExports, String> {
    let package = &manifest.package;

    let mut registered_surfaces = inventory::iter::<ExportedSurfaceRegistration>
        .into_iter()
        .copied()
        .collect::<Vec<_>>();
    registered_surfaces.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then(left.vox_name.cmp(right.vox_name))
            .then(left.rust_name.cmp(right.rust_name))
    });

    let mut surface_registry = BTreeMap::new();
    for surface in &registered_surfaces {
        if let Some(previous) = surface_registry.insert(surface.vox_name, *surface) {
            return Err(format!(
                "duplicate exported surface `{}` from `{}` and `{}`",
                surface.vox_name, previous.rust_name, surface.rust_name
            ));
        }
    }

    let mut functions = manifest.functions.clone();
    let mut seen_function_names = functions
        .iter()
        .map(|function| function.name.clone())
        .collect::<BTreeSet<_>>();

    let mut registered_functions = inventory::iter::<ExportedFunctionRegistration>
        .into_iter()
        .copied()
        .collect::<Vec<_>>();
    registered_functions.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then(left.vox_name.cmp(right.vox_name))
            .then(left.rust_name.cmp(right.rust_name))
    });

    for function in &registered_functions {
        if !seen_function_names.insert(function.vox_name.to_owned()) {
            return Err(format!(
                "duplicate exported function `{}` from `{}`",
                function.vox_name, function.rust_name
            ));
        }
        functions.push(build_function_spec(package, function)?);
    }

    let mut referenced_surface_names = manifest
        .types
        .iter()
        .map(|ty| ty.name.name.clone())
        .collect::<BTreeSet<_>>();
    for function in &functions {
        collect_surface_names(&function.return_type, &mut referenced_surface_names);
        for parameter in &function.parameters {
            collect_surface_names(&parameter.ty, &mut referenced_surface_names);
        }
    }

    let mut manual_surfaces = Vec::with_capacity(manifest.types.len());
    let mut seen_surface_names = BTreeSet::new();
    for ty in &manifest.types {
        if seen_surface_names.insert(ty.name.name.clone()) {
            manual_surfaces.push(CollectedSurface {
                name: ty.name.name.clone(),
                kind: None,
            });
        }
    }

    let mut surfaces = manual_surfaces;
    for name in &referenced_surface_names {
        if seen_surface_names.contains(name) {
            continue;
        }
        let Some(surface) = surface_registry.get(name.as_str()) else {
            return Err(format!(
                "exported function surface `{name}` is not registered as a Vox struct or trait"
            ));
        };
        surfaces.push(CollectedSurface {
            name: surface.vox_name.to_owned(),
            kind: Some(surface.kind),
        });
        seen_surface_names.insert(surface.vox_name.to_owned());
    }

    Ok(CollectedPackageExports {
        surfaces,
        functions,
    })
}

fn build_function_spec(
    package: &ModulePath,
    function: &ExportedFunctionRegistration,
) -> Result<FunctionSpec, String> {
    Ok(FunctionSpec {
        name: function.vox_name.to_owned(),
        parameters: function
            .parameters
            .iter()
            .map(|parameter| {
                Ok(ParameterSpec {
                    name: parameter.name.to_owned(),
                    ty: parse_rust_type(package, parameter.rust_type)?,
                    has_default: parameter.has_default,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
        return_type: match function.return_type_override {
            Some(override_type) => parse_vox_type(package, override_type)?,
            None => parse_rust_type(package, function.return_rust_type)?,
        },
        purity: function.purity,
    })
}

fn collect_surface_names(ty: &VoxType, out: &mut BTreeSet<String>) {
    match ty {
        VoxType::List(item) | VoxType::Nullable(item) => collect_surface_names(item, out),
        VoxType::Tuple(items) => {
            for item in items {
                collect_surface_names(item, out);
            }
        }
        VoxType::Record(fields) => {
            for field in fields {
                collect_surface_names(&field.ty, out);
            }
        }
        VoxType::DynTrait(name) | VoxType::Named(name) => {
            out.insert(name.name.clone());
        }
        VoxType::Int
        | VoxType::Float
        | VoxType::Bool
        | VoxType::String
        | VoxType::TypeParameter(_)
        | VoxType::OpaqueSurface(_) => {}
    }
}

fn parse_rust_type(package: &ModulePath, raw: &str) -> Result<VoxType, String> {
    let compact = compact_type(raw);
    let normalized = strip_reference_prefix(&compact);
    if let Some(inner) = normalized.strip_prefix("dyn") {
        return Ok(VoxType::DynTrait(qualified(
            package,
            simple_path_name(inner)?,
        )));
    }

    if normalized.starts_with('(') {
        return parse_tuple_like(package, &normalized, parse_rust_type);
    }

    if let Some(inner) = generic_argument(&normalized, "Option")? {
        return Ok(VoxType::Nullable(Box::new(parse_rust_type(
            package, inner,
        )?)));
    }

    if let Some(inner) = generic_argument(&normalized, "Vec")? {
        return Ok(VoxType::List(Box::new(parse_rust_type(package, inner)?)));
    }

    match simple_path_name(&normalized)? {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => Ok(VoxType::Int),
        "f32" | "f64" => Ok(VoxType::Float),
        "bool" => Ok(VoxType::Bool),
        "String" | "str" => Ok(VoxType::String),
        other => Ok(VoxType::Named(qualified(package, other))),
    }
}

fn parse_vox_type(package: &ModulePath, raw: &str) -> Result<VoxType, String> {
    let compact = compact_type(raw);
    if let Some(inner) = compact.strip_suffix('?') {
        return Ok(VoxType::Nullable(Box::new(parse_vox_type(package, inner)?)));
    }

    if let Some(inner) = compact.strip_prefix("dyn") {
        return Ok(VoxType::DynTrait(parse_vox_named_surface(package, inner)?));
    }

    if compact.starts_with('{') {
        return parse_record_type(package, &compact);
    }

    if compact.starts_with('(') {
        return parse_tuple_like(package, &compact, parse_vox_type);
    }

    if let Some(inner) = bracket_argument(&compact, "List")? {
        return Ok(VoxType::List(Box::new(parse_vox_type(package, inner)?)));
    }

    match compact.as_str() {
        "Int" => Ok(VoxType::Int),
        "Float" => Ok(VoxType::Float),
        "Bool" => Ok(VoxType::Bool),
        "String" => Ok(VoxType::String),
        _ => Ok(VoxType::Named(parse_vox_named_surface(package, &compact)?)),
    }
}

fn parse_tuple_like(
    package: &ModulePath,
    raw: &str,
    parse_item: fn(&ModulePath, &str) -> Result<VoxType, String>,
) -> Result<VoxType, String> {
    let inner = trim_delimited(raw, '(', ')')?;
    if inner.is_empty() {
        return Ok(VoxType::Tuple(Vec::new()));
    }
    Ok(VoxType::Tuple(
        split_top_level(inner, ',')?
            .into_iter()
            .map(|item| parse_item(package, &item))
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

fn parse_record_type(package: &ModulePath, raw: &str) -> Result<VoxType, String> {
    let inner = trim_delimited(raw, '{', '}')?;
    if inner.is_empty() {
        return Ok(VoxType::Record(Vec::new()));
    }

    let mut fields = Vec::new();
    for field in split_top_level(inner, ',')? {
        let Some((name, ty)) = split_once_top_level(&field, ':') else {
            return Err(format!("invalid record field `{field}`"));
        };
        fields.push(RecordField {
            name: name.to_owned(),
            ty: parse_vox_type(package, ty)?,
        });
    }
    Ok(VoxType::Record(fields))
}

fn parse_vox_named_surface(package: &ModulePath, raw: &str) -> Result<QualifiedTypeName, String> {
    if let Some((module, name)) = raw.rsplit_once('.') {
        let module = ModulePath::parse(module).map_err(|diagnostic| diagnostic.message)?;
        return Ok(QualifiedTypeName {
            module,
            name: name.to_owned(),
        });
    }

    Ok(qualified(package, raw))
}

fn qualified(package: &ModulePath, name: &str) -> QualifiedTypeName {
    QualifiedTypeName {
        module: package.clone(),
        name: name.to_owned(),
    }
}

fn simple_path_name(raw: &str) -> Result<&str, String> {
    raw.rsplit("::")
        .next()
        .filter(|segment| !segment.is_empty())
        .ok_or_else(|| format!("invalid Rust type `{raw}`"))
}

fn compact_type(raw: &str) -> String {
    raw.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn strip_reference_prefix(raw: &str) -> &str {
    let mut current = raw;
    while let Some(rest) = current.strip_prefix('&') {
        current = rest;
        if let Some(after_lifetime) = current.strip_prefix('\'') {
            let end = after_lifetime
                .find(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                .unwrap_or(after_lifetime.len());
            current = &after_lifetime[end..];
        }
        if let Some(rest) = current.strip_prefix("mut") {
            current = rest;
        }
    }
    current
}

fn generic_argument<'a>(raw: &'a str, name: &str) -> Result<Option<&'a str>, String> {
    let base = simple_path_name(raw.split('<').next().unwrap_or(raw))?;
    if base != name {
        return Ok(None);
    }
    trim_delimited_suffix(raw, '<', '>').map(Some)
}

fn bracket_argument<'a>(raw: &'a str, name: &str) -> Result<Option<&'a str>, String> {
    let Some(inner) = raw.strip_prefix(name) else {
        return Ok(None);
    };
    if !inner.starts_with('[') {
        return Ok(None);
    }
    trim_delimited(inner, '[', ']').map(Some)
}

fn trim_delimited_suffix(raw: &str, open: char, close: char) -> Result<&str, String> {
    let Some(index) = raw.find(open) else {
        return Err(format!("missing `{open}` in `{raw}`"));
    };
    trim_delimited(&raw[index..], open, close)
}

fn trim_delimited(raw: &str, open: char, close: char) -> Result<&str, String> {
    if !raw.starts_with(open) || !raw.ends_with(close) {
        return Err(format!("invalid delimited type `{raw}`"));
    }
    let inner = &raw[open.len_utf8()..raw.len() - close.len_utf8()];
    if has_unbalanced_delimiters(inner) {
        return Err(format!("unbalanced nested delimiters in `{raw}`"));
    }
    Ok(inner)
}

fn split_once_top_level(raw: &str, delimiter: char) -> Option<(&str, &str)> {
    let mut depth = DelimiterDepth::default();
    for (index, ch) in raw.char_indices() {
        depth.push(ch);
        if depth.is_top_level() && ch == delimiter {
            let left = &raw[..index];
            let right = &raw[index + ch.len_utf8()..];
            return Some((left, right));
        }
        depth.pop(ch);
    }
    None
}

fn split_top_level(raw: &str, delimiter: char) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    let mut depth = DelimiterDepth::default();
    let mut start = 0;
    for (index, ch) in raw.char_indices() {
        depth.push(ch);
        if depth.is_top_level() && ch == delimiter {
            parts.push(raw[start..index].to_owned());
            start = index + ch.len_utf8();
        }
        depth.pop(ch);
    }

    if !depth.is_balanced() {
        return Err(format!("unbalanced type expression `{raw}`"));
    }

    parts.push(raw[start..].to_owned());
    Ok(parts
        .into_iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect())
}

fn has_unbalanced_delimiters(raw: &str) -> bool {
    let mut depth = DelimiterDepth::default();
    for ch in raw.chars() {
        depth.push(ch);
        depth.pop(ch);
    }
    !depth.is_balanced()
}

#[derive(Default)]
struct DelimiterDepth {
    angles: usize,
    parens: usize,
    braces: usize,
    brackets: usize,
}

impl DelimiterDepth {
    fn push(&mut self, ch: char) {
        match ch {
            '<' => self.angles += 1,
            '(' => self.parens += 1,
            '{' => self.braces += 1,
            '[' => self.brackets += 1,
            _ => {}
        }
    }

    fn pop(&mut self, ch: char) {
        match ch {
            '>' => self.angles = self.angles.saturating_sub(1),
            ')' => self.parens = self.parens.saturating_sub(1),
            '}' => self.braces = self.braces.saturating_sub(1),
            ']' => self.brackets = self.brackets.saturating_sub(1),
            _ => {}
        }
    }

    fn is_top_level(&self) -> bool {
        self.angles == 0 && self.parens == 0 && self.braces == 0 && self.brackets == 0
    }

    fn is_balanced(&self) -> bool {
        self.angles == 0 && self.parens == 0 && self.braces == 0 && self.brackets == 0
    }
}
