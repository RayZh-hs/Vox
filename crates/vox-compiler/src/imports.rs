use std::collections::{BTreeMap, BTreeSet};

use vox_core::host::HostRegistry;

use crate::frontend::ast::ImportDecl;

#[derive(Debug, Clone, Default)]
pub struct ImportResolution {
    pub unqualified: BTreeMap<String, String>,
    pub module_aliases: BTreeMap<String, String>,
}

pub fn resolve_imports(
    imports: &[ImportDecl],
    host: &HostRegistry,
) -> ImportResolution {
    let mut module_aliases = BTreeMap::new();
    let mut unqualified_sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut explicit: BTreeMap<String, String> = BTreeMap::new();

    for import in imports {
        let module_str = import.module.to_source_string();

        if let Some(alias) = &import.alias {
            module_aliases.insert(alias.clone(), module_str.clone());
        }

        let manifest = host
            .package(
                &vox_core::source::ModulePath::parse(&module_str).unwrap_or_else(|_| {
                    vox_core::source::ModulePath::parse("unknown").unwrap()
                }),
            );

        match &import.items {
            None => {
                if let Some(manifest) = manifest {
                    for function in &manifest.functions {
                        let qualified = format!("{}.{}", module_str, function.name);
                        unqualified_sources
                            .entry(function.name.clone())
                            .or_default()
                            .push(qualified);
                    }
                }
            }
            Some(items) => {
                for item in items {
                    let effective_name = item.alias.clone().unwrap_or_else(|| item.name.clone());
                    let qualified = format!("{}.{}", module_str, item.name);
                    explicit.insert(effective_name, qualified);
                }
            }
        }
    }

    let mut unqualified = BTreeMap::new();
    let mut seen_explicit = BTreeSet::new();
    for (name, qualified) in &explicit {
        unqualified.insert(name.clone(), qualified.clone());
        seen_explicit.insert(name.clone());
    }

    for (name, sources) in &unqualified_sources {
        if seen_explicit.contains(name) {
            continue;
        }
        if sources.len() == 1 {
            unqualified.insert(name.clone(), sources[0].clone());
        }
    }

    ImportResolution {
        unqualified,
        module_aliases,
    }
}
