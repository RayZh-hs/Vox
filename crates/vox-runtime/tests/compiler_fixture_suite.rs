use std::{
    fs,
    path::{Path, PathBuf},
};

use vox_compiler::{CompileRequest, Compiler, FrontendUnit};
use vox_core::{
    host::{
        FieldSpec, FunctionExportKind, FunctionSpec, HostRegistry, PackageManifest, ParameterSpec,
        Purity, TraitMethodSpec, TraitSpec, TypeSpec,
    },
    opt::OptimizationLevel,
    source::{ModulePath, SourceText},
    types::{QualifiedTypeName, VoxType},
};
use vox_runtime::infer_environment;

#[test]
fn compile_ok_fixtures_compile_at_all_optimization_levels() {
    for path in fixture_paths("compile_ok") {
        expect_compile_success(&path);
    }
}

#[test]
fn semantic_ok_fixtures_compile_and_infer() {
    let manifests = semantic_manifests();
    for path in fixture_paths("semantic_ok") {
        let frontend = expect_compile_success(&path);
        infer_environment(&frontend.syntax, &manifests).unwrap_or_else(|error| {
            panic!(
                "expected `{}` to pass semantic inference, found: {error}",
                path.display()
            )
        });
    }
}

#[test]
fn syntax_fail_fixtures_are_rejected_by_compiler() {
    for path in fixture_paths("syntax_fail") {
        for level in optimization_levels() {
            let result = compile_fixture(&path, level);
            assert!(
                result.diagnostics.has_errors(),
                "expected `{}` to fail compiler syntax/frontend validation at {:?}",
                path.display(),
                level
            );
        }
    }
}

#[test]
fn semantic_fail_fixtures_compile_then_fail_inference() {
    let manifests = semantic_manifests();
    for path in fixture_paths("semantic_fail") {
        let frontend = expect_compile_success(&path);
        assert!(
            infer_environment(&frontend.syntax, &manifests).is_err(),
            "expected `{}` to fail semantic inference",
            path.display()
        );
    }
}

fn expect_compile_success(path: &Path) -> FrontendUnit {
    let mut frontend = None;
    for level in optimization_levels() {
        let result = compile_fixture(path, level);
        assert!(
            !result.diagnostics.has_errors(),
            "expected `{}` to compile at {:?}, found diagnostics:\n{}",
            path.display(),
            level,
            result.diagnostics
        );
        assert!(
            result.artifact.is_some(),
            "expected `{}` to produce a compiled artifact at {:?}",
            path.display(),
            level
        );
        let artifact = result
            .artifact
            .as_ref()
            .expect("artifact presence should have been checked");
        assert!(
            artifact
                .mir
                .as_ref()
                .is_some_and(|mir| !mir.bodies.is_empty()),
            "expected `{}` to emit MIR at {:?}",
            path.display(),
            level
        );
        assert!(
            artifact
                .plan
                .mir_text
                .as_ref()
                .is_some_and(|text| text.contains("body @")),
            "expected `{}` to expose MIR text at {:?}",
            path.display(),
            level
        );
        if level == OptimizationLevel::SOpt {
            frontend = result.frontend;
        }
    }

    frontend.unwrap_or_else(|| panic!("expected `{}` to produce a frontend unit", path.display()))
}

fn compile_fixture(path: &Path, optimization: OptimizationLevel) -> vox_compiler::CompileResult {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read `{}`: {error}", path.display()));
    Compiler::default().compile(CompileRequest {
        source: SourceText::new(path.display().to_string(), 1, source),
        optimization,
        optimization_overrides: Default::default(),
        host: host_registry(),
    })
}

fn fixture_paths(group: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("compiler")
        .join(group);
    let mut paths = fs::read_dir(&dir)
        .unwrap_or_else(|error| {
            panic!(
                "failed to read fixture directory `{}`: {error}",
                dir.display()
            )
        })
        .map(|entry| {
            entry
                .unwrap_or_else(|error| {
                    panic!(
                        "failed to read fixture entry in `{}`: {error}",
                        dir.display()
                    )
                })
                .path()
        })
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("vox"))
        .collect::<Vec<_>>();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "expected at least one `.vox` fixture in `{}`",
        dir.display()
    );
    paths
}

fn optimization_levels() -> [OptimizationLevel; 3] {
    [
        OptimizationLevel::NOpt,
        OptimizationLevel::IOpt,
        OptimizationLevel::SOpt,
    ]
}

fn host_registry() -> HostRegistry {
    let mut registry = HostRegistry::default();
    registry.register_package(host_manifest());
    registry.register_package(image_manifest());
    registry.register_package(tools_manifest());
    registry
}

fn semantic_manifests() -> Vec<PackageManifest> {
    vec![host_manifest(), image_manifest(), tools_manifest()]
}

fn host_manifest() -> PackageManifest {
    let package = ModulePath::parse("fixtures.host").expect("valid host package path");
    PackageManifest {
        package: package.clone(),
        types: vec![TypeSpec {
            name: QualifiedTypeName {
                module: package.clone(),
                name: "Service".to_owned(),
            },
            fields: Vec::new(),
        }],
        traits: Vec::new(),
        functions: vec![FunctionSpec {
            name: "add".to_owned(),
            parameters: vec![
                ParameterSpec {
                    name: "lhs".to_owned(),
                    ty: VoxType::Int,
                    has_default: false,
                },
                ParameterSpec {
                    name: "rhs".to_owned(),
                    ty: VoxType::Int,
                    has_default: false,
                },
            ],
            return_type: VoxType::Int,
            purity: Purity::Pure,
            export: FunctionExportKind::Function,
        }],
    }
}

fn tools_manifest() -> PackageManifest {
    PackageManifest {
        package: ModulePath::parse("fixtures.tools").expect("valid tools package path"),
        types: Vec::new(),
        traits: Vec::new(),
        functions: Vec::new(),
    }
}

fn image_manifest() -> PackageManifest {
    let package = ModulePath::parse("image").expect("valid image package path");
    let image_type = VoxType::Named(qualified_type(&package, "Image"));
    let filter_type = VoxType::DynTrait(qualified_type(&package, "Filter"));

    PackageManifest {
        package: package.clone(),
        types: vec![TypeSpec {
            name: qualified_type(&package, "Image"),
            fields: vec![
                FieldSpec {
                    name: "width".to_owned(),
                    ty: VoxType::Int,
                },
                FieldSpec {
                    name: "height".to_owned(),
                    ty: VoxType::Int,
                },
            ],
        }],
        traits: vec![TraitSpec {
            name: qualified_type(&package, "Filter"),
            methods: vec![TraitMethodSpec {
                name: "apply".to_owned(),
                lowered_by: "filter_apply".to_owned(),
                parameters: vec![ParameterSpec {
                    name: "input".to_owned(),
                    ty: image_type.clone(),
                    has_default: false,
                }],
                return_type: image_type.clone(),
                purity: Purity::Pure,
            }],
        }],
        functions: vec![
            FunctionSpec {
                name: "load".to_owned(),
                parameters: vec![ParameterSpec {
                    name: "path".to_owned(),
                    ty: VoxType::String,
                    has_default: false,
                }],
                return_type: image_type.clone(),
                purity: Purity::Pure,
                export: FunctionExportKind::Function,
            },
            FunctionSpec {
                name: "blur".to_owned(),
                parameters: vec![
                    ParameterSpec {
                        name: "input".to_owned(),
                        ty: image_type.clone(),
                        has_default: false,
                    },
                    ParameterSpec {
                        name: "radius".to_owned(),
                        ty: VoxType::Float,
                        has_default: true,
                    },
                ],
                return_type: image_type.clone(),
                purity: Purity::Pure,
                export: FunctionExportKind::Function,
            },
            FunctionSpec {
                name: "filter_apply".to_owned(),
                parameters: vec![
                    ParameterSpec {
                        name: "filter".to_owned(),
                        ty: filter_type,
                        has_default: false,
                    },
                    ParameterSpec {
                        name: "input".to_owned(),
                        ty: image_type.clone(),
                        has_default: false,
                    },
                ],
                return_type: image_type.clone(),
                purity: Purity::Pure,
                export: FunctionExportKind::LoweredTraitMethod {
                    trait_name: qualified_type(&package, "Filter"),
                    method_name: "apply".to_owned(),
                },
            },
            FunctionSpec {
                name: "histogram".to_owned(),
                parameters: vec![ParameterSpec {
                    name: "input".to_owned(),
                    ty: image_type.clone(),
                    has_default: false,
                }],
                return_type: VoxType::Record(vec![
                    vox_core::types::RecordField {
                        name: "shadows".to_owned(),
                        ty: VoxType::Int,
                    },
                    vox_core::types::RecordField {
                        name: "mids".to_owned(),
                        ty: VoxType::Int,
                    },
                    vox_core::types::RecordField {
                        name: "highlights".to_owned(),
                        ty: VoxType::Int,
                    },
                ]),
                purity: Purity::Pure,
                export: FunctionExportKind::Function,
            },
            FunctionSpec {
                name: "dimensions".to_owned(),
                parameters: vec![ParameterSpec {
                    name: "input".to_owned(),
                    ty: image_type.clone(),
                    has_default: false,
                }],
                return_type: VoxType::Tuple(vec![VoxType::Int, VoxType::Int]),
                purity: Purity::Pure,
                export: FunctionExportKind::Function,
            },
            FunctionSpec {
                name: "tags".to_owned(),
                parameters: vec![ParameterSpec {
                    name: "input".to_owned(),
                    ty: image_type.clone(),
                    has_default: false,
                }],
                return_type: VoxType::List(Box::new(VoxType::String)),
                purity: Purity::Pure,
                export: FunctionExportKind::Function,
            },
            FunctionSpec {
                name: "optional".to_owned(),
                parameters: vec![ParameterSpec {
                    name: "path".to_owned(),
                    ty: VoxType::String,
                    has_default: false,
                }],
                return_type: VoxType::Nullable(Box::new(image_type)),
                purity: Purity::Pure,
                export: FunctionExportKind::Function,
            },
        ],
    }
}

fn qualified_type(module: &ModulePath, name: &str) -> QualifiedTypeName {
    QualifiedTypeName {
        module: module.clone(),
        name: name.to_owned(),
    }
}
