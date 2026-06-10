use std::collections::{BTreeMap, BTreeSet};

use crate::{
    host::{
        FieldSpec, FunctionExportKind, FunctionSpec, PackageManifest, ParameterSpec, Purity,
        TraitMethodSpec, TraitSpec, TypeSpec,
    },
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
    pub fields: &'static [ExportedSurfaceField],
    pub order: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ExportedSurfaceField {
    pub name: &'static str,
    pub rust_type: &'static str,
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

#[derive(Debug, Clone, Copy)]
pub struct ExportedTraitMethodRegistration {
    pub trait_vox_name: &'static str,
    pub rust_name: &'static str,
    pub vox_name: &'static str,
    pub lowered_by: &'static str,
    pub purity: Purity,
    pub parameters: &'static [ExportedFunctionParameter],
    pub return_rust_type: &'static str,
    pub order: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ExportedTraitImplRegistration {
    pub struct_vox_name: &'static str,
    pub trait_vox_name: &'static str,
    pub order: u32,
}

inventory::collect!(ExportedSurfaceRegistration);
inventory::collect!(ExportedFunctionRegistration);
inventory::collect!(ExportedTraitMethodRegistration);
inventory::collect!(ExportedTraitImplRegistration);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedPackageExports {
    pub types: Vec<TypeSpec>,
    pub traits: Vec<TraitSpec>,
    pub functions: Vec<FunctionSpec>,
    pub trait_impls: BTreeMap<QualifiedTypeName, BTreeSet<QualifiedTypeName>>,
}

pub fn extend_manifest_with_registered_exports(
    manifest: PackageManifest,
) -> Result<PackageManifest, String> {
    let collected = collect_registered_package_exports(&manifest)?;
    Ok(PackageManifest {
        package: manifest.package.clone(),
        types: collected.types,
        traits: collected.traits,
        functions: collected.functions,
        trait_impls: collected.trait_impls,
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

    let mut registered_methods = inventory::iter::<ExportedTraitMethodRegistration>
        .into_iter()
        .copied()
        .collect::<Vec<_>>();
    registered_methods.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then(left.trait_vox_name.cmp(right.trait_vox_name))
            .then(left.vox_name.cmp(right.vox_name))
            .then(left.rust_name.cmp(right.rust_name))
    });

    let mut methods_by_trait = BTreeMap::<&str, Vec<TraitMethodSpec>>::new();
    let mut seen_methods = BTreeSet::new();
    for method in &registered_methods {
        if !seen_methods.insert((method.trait_vox_name, method.vox_name)) {
            return Err(format!(
                "duplicate exported trait method `{}.{}` from `{}`",
                method.trait_vox_name, method.vox_name, method.rust_name
            ));
        }

        let trait_name = qualified(package, method.trait_vox_name);
        let Some(function) = functions
            .iter_mut()
            .find(|function| function.name == method.lowered_by)
        else {
            return Err(format!(
                "lowered function `{}` for trait method `{}.{}` is not exported",
                method.lowered_by, method.trait_vox_name, method.vox_name
            ));
        };
        function.export = FunctionExportKind::LoweredTraitMethod {
            trait_name,
            method_name: method.vox_name.to_owned(),
        };

        methods_by_trait
            .entry(method.trait_vox_name)
            .or_default()
            .push(build_trait_method_spec(package, method)?);
    }

    let mut referenced_surface_names = manifest
        .types
        .iter()
        .map(|ty| ty.name.name.clone())
        .collect::<BTreeSet<_>>();
    referenced_surface_names.extend(manifest.traits.iter().map(|ty| ty.name.name.clone()));
    referenced_surface_names.extend(methods_by_trait.keys().map(|name| (*name).to_owned()));
    for function in &functions {
        collect_surface_names(&function.return_type, &mut referenced_surface_names);
        for parameter in &function.parameters {
            collect_surface_names(&parameter.ty, &mut referenced_surface_names);
        }
    }
    for methods in methods_by_trait.values() {
        for method in methods {
            collect_surface_names(&method.return_type, &mut referenced_surface_names);
            for parameter in &method.parameters {
                collect_surface_names(&parameter.ty, &mut referenced_surface_names);
            }
        }
    }

    let mut types = Vec::with_capacity(manifest.types.len());
    let mut seen_type_names = BTreeSet::new();
    for ty in &manifest.types {
        if seen_type_names.insert(ty.name.name.clone()) {
            types.push(ty.clone());
        }
    }

    let mut traits = Vec::with_capacity(manifest.traits.len());
    let mut seen_trait_names = BTreeSet::new();
    for trait_spec in &manifest.traits {
        if seen_trait_names.insert(trait_spec.name.name.clone()) {
            let mut trait_spec = trait_spec.clone();
            if let Some(mut methods) = methods_by_trait.remove(trait_spec.name.name.as_str()) {
                trait_spec.methods.append(&mut methods);
            }
            traits.push(trait_spec);
        }
    }

    for name in &referenced_surface_names {
        if seen_type_names.contains(name) || seen_trait_names.contains(name) {
            continue;
        }
        let Some(surface) = surface_registry.get(name.as_str()) else {
            return Err(format!(
                "exported function surface `{name}` is not registered as a Vox struct or trait"
            ));
        };
        match surface.kind {
            ExportedSurfaceKind::Struct => {
                types.push(TypeSpec {
                    name: qualified(package, surface.vox_name),
                    fields: build_field_specs(package, surface)?,
                });
                seen_type_names.insert(surface.vox_name.to_owned());
            }
            ExportedSurfaceKind::Trait => {
                traits.push(TraitSpec {
                    name: qualified(package, surface.vox_name),
                    methods: methods_by_trait
                        .remove(surface.vox_name)
                        .unwrap_or_default(),
                });
                seen_trait_names.insert(surface.vox_name.to_owned());
            }
        }
    }

    Ok(CollectedPackageExports {
        types,
        traits,
        functions,
        trait_impls: collect_trait_impls(package, &surface_registry),
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
        export: FunctionExportKind::Function,
    })
}

fn build_trait_method_spec(
    package: &ModulePath,
    method: &ExportedTraitMethodRegistration,
) -> Result<TraitMethodSpec, String> {
    Ok(TraitMethodSpec {
        name: method.vox_name.to_owned(),
        lowered_by: method.lowered_by.to_owned(),
        parameters: method
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
        return_type: parse_rust_type(package, method.return_rust_type)?,
        purity: method.purity,
    })
}

fn build_field_specs(
    package: &ModulePath,
    surface: &ExportedSurfaceRegistration,
) -> Result<Vec<FieldSpec>, String> {
    surface
        .fields
        .iter()
        .map(|field| {
            Ok(FieldSpec {
                name: field.name.to_owned(),
                ty: parse_rust_type(package, field.rust_type)?,
            })
        })
        .collect()
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

fn collect_trait_impls(
    package: &ModulePath,
    _surface_registry: &BTreeMap<&str, ExportedSurfaceRegistration>,
) -> BTreeMap<QualifiedTypeName, BTreeSet<QualifiedTypeName>> {
    let mut trait_impls: BTreeMap<QualifiedTypeName, BTreeSet<QualifiedTypeName>> = BTreeMap::new();
    let registered = inventory::iter::<ExportedTraitImplRegistration>
        .into_iter()
        .copied()
        .collect::<Vec<_>>();

    for reg in registered {
        let trait_name = parse_qualified_name(package, reg.trait_vox_name);
        let struct_name = parse_qualified_name(package, reg.struct_vox_name);
        trait_impls
            .entry(trait_name)
            .or_default()
            .insert(struct_name);
    }

    trait_impls
}

fn parse_qualified_name(package: &ModulePath, raw: &str) -> QualifiedTypeName {
    if let Some((module_str, name)) = raw.rsplit_once('.') {
        if let Ok(module) = ModulePath::parse(module_str) {
            return QualifiedTypeName {
                module,
                name: name.to_owned(),
            };
        }
    }
    QualifiedTypeName {
        module: package.clone(),
        name: raw.to_owned(),
    }
}
