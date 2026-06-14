use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use vox_compiler::{CompileRequest, Compiler, FrontendUnit};
use vox_core::{
    VoxExport,
    external_library::ExternalLibrary,
    host::{
        FieldSpec, FunctionExportKind, FunctionSpec, HostRegistry, PackageManifest, ParameterSpec,
        Purity, TraitMethodSpec, TraitSpec, TypeSpec,
    },
    opt::OptimizationLevel,
    source::{ModulePath, SourceText},
    types::{QualifiedTypeName, VoxType},
    vox_fn,
};
use vox_runtime::infer_environment;

use std::env;

// =============================================================================
// Test external library items (used by both manifest-generation and combined tests).
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, VoxExport)]
struct TestPoint {
    x: i64,
    y: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, VoxExport)]
#[allow(dead_code)]
struct TestEmpty {}

#[vox_fn(purity = "pure")]
fn make_empty() -> TestEmpty {
    TestEmpty {}
}

#[vox_fn(purity = "pure")]
fn make_point(x: i64, y: i64) -> TestPoint {
    TestPoint { x, y }
}

#[vox_fn(purity = "pure")]
fn point_distance(a: TestPoint, b: TestPoint) -> f64 {
    (((b.x - a.x).pow(2) + (b.y - a.y).pow(2)) as f64).sqrt()
}

#[vox_fn(purity = "pure")]
fn maybe_point(has: bool, x: i64, y: i64) -> Option<TestPoint> {
    if has { Some(TestPoint { x, y }) } else { None }
}

#[vox_fn(purity = "pure")]
fn points(count: i64) -> Vec<TestPoint> {
    (0..count).map(|i| TestPoint { x: i, y: i * 2 }).collect()
}

#[vox_fn(purity = "pure")]
fn tags() -> Vec<String> {
    vec!["fast".into(), "gpu".into()]
}

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
    compile_with_registry(path, optimization, host_registry())
}

fn compile_with_registry(
    path: &Path,
    optimization: OptimizationLevel,
    registry: HostRegistry,
) -> vox_compiler::CompileResult {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read `{}`: {error}", path.display()));
    Compiler::default().compile(CompileRequest {
        source: SourceText::new(path.display().to_string(), 1, source),
        optimization,
        optimization_overrides: Default::default(),
        host: registry,
    })
}

fn fixture_paths(group: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("compiler")
        .join(group);
    collect_vox_paths(&dir)
}

fn collect_vox_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(dir)
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

// =========================================================================
// External-library manifest generation test
// =========================================================================

#[test]
fn test_external_library_manifest_generation() {
    let package_name = "test.fxt";
    let (manifest, _metadata) = ExternalLibrary::new(package_name)
        .expect("valid package name")
        .build()
        .expect("external library build should collect registered exports");

    let package = ModulePath::parse(package_name).expect("valid package path");
    assert_eq!(manifest.package, package, "package name mismatch");

    let types: BTreeMap<&str, &TypeSpec> = manifest
        .types
        .iter()
        .map(|t| (t.name.name.as_str(), t))
        .collect();

    let point = types
        .get("TestPoint")
        .expect("TestPoint should be registered");
    assert_eq!(point.name.module, package);
    assert_eq!(point.fields.len(), 2, "TestPoint should have 2 fields");
    let fields: BTreeMap<&str, &FieldSpec> =
        point.fields.iter().map(|f| (f.name.as_str(), f)).collect();
    assert_eq!(fields.get("x").map(|f| &f.ty), Some(&VoxType::Int));
    assert_eq!(fields.get("y").map(|f| &f.ty), Some(&VoxType::Int));

    let empty = types
        .get("TestEmpty")
        .expect("TestEmpty should be registered");
    assert!(empty.fields.is_empty(), "TestEmpty should have no fields");

    let functions: BTreeMap<&str, &FunctionSpec> = manifest
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();

    let mk = functions
        .get("make_point")
        .expect("make_point should be registered");
    assert_eq!(mk.parameters.len(), 2, "make_point expects 2 parameters");
    assert_eq!(mk.parameters[0].name, "x");
    assert_eq!(mk.parameters[0].ty, VoxType::Int);
    assert_eq!(mk.parameters[1].name, "y");
    assert_eq!(mk.parameters[1].ty, VoxType::Int);
    assert_eq!(
        mk.return_type,
        VoxType::Named(qualified_type(&package, "TestPoint"))
    );
    assert_eq!(mk.purity, Purity::Pure);
    assert_eq!(mk.export, FunctionExportKind::Function);

    let me = functions
        .get("make_empty")
        .expect("make_empty should be registered");
    assert!(me.parameters.is_empty(), "make_empty expects no parameters");
    assert_eq!(
        me.return_type,
        VoxType::Named(qualified_type(&package, "TestEmpty"))
    );
    assert_eq!(me.purity, Purity::Pure);
    assert_eq!(me.export, FunctionExportKind::Function);

    let dist = functions
        .get("point_distance")
        .expect("point_distance should be registered");
    assert_eq!(dist.return_type, VoxType::Float);
    assert_eq!(dist.export, FunctionExportKind::Function);

    let maybe = functions
        .get("maybe_point")
        .expect("maybe_point should be registered");
    assert_eq!(maybe.parameters.len(), 3);
    assert_eq!(maybe.parameters[0].name, "has");
    assert_eq!(maybe.parameters[0].ty, VoxType::Bool);
    assert_eq!(
        maybe.return_type,
        VoxType::Nullable(Box::new(VoxType::Named(qualified_type(
            &package,
            "TestPoint"
        ))))
    );

    let pts = functions
        .get("points")
        .expect("points should be registered");
    assert_eq!(
        pts.return_type,
        VoxType::List(Box::new(VoxType::Named(qualified_type(
            &package,
            "TestPoint"
        ))))
    );

    let t = functions.get("tags").expect("tags should be registered");
    assert_eq!(t.return_type, VoxType::List(Box::new(VoxType::String)));
}

// =========================================================================
// Combined tests: external-library manifest + .vox fixture scripts
// =========================================================================

#[test]
fn extern_fixtures_compile_at_all_optimization_levels() {
    let registry = extern_registry();
    for path in extern_fixture_paths() {
        for level in optimization_levels() {
            let result = compile_with_registry(&path, level, registry.clone());
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
        }
    }
}

#[test]
fn extern_fixtures_pass_semantic_inference() {
    let manifest = extern_manifest();
    let manifests = vec![manifest];
    let registry = extern_registry();
    for path in extern_fixture_paths() {
        let result = compile_with_registry(&path, OptimizationLevel::SOpt, registry.clone());
        let frontend = result
            .frontend
            .unwrap_or_else(|| panic!("expected `{}` to produce a frontend unit", path.display()));
        infer_environment(&frontend.syntax, &manifests).unwrap_or_else(|error| {
            panic!(
                "expected `{}` to pass semantic inference, found: {error}",
                path.display()
            )
        });
    }
}

// =========================================================================
// Voxlib compile → write → load round-trip test
// =========================================================================

#[test]
fn voxlib_file_compile_write_and_mount_round_trip() {
    let source =
        "package fixtures.roundtrip; public fun add(a: Int, b: Int): Int = a + b;".to_owned();
    let module = ModulePath::parse("fixtures.roundtrip").expect("valid package path");

    let request = CompileRequest {
        source: SourceText::new("roundtrip.vox", 1, source),
        optimization: OptimizationLevel::SOpt,
        optimization_overrides: Default::default(),
        host: HostRegistry::default(),
    };
    let voxlib_bytes =
        vox_compiler::compile_to_voxlib(request).expect("voxlib compilation should succeed");

    let tmp = env::temp_dir().join("vox-test-roundtrip");
    let _ = fs::create_dir_all(&tmp);
    let lib_path = tmp.join("fixtures.roundtrip.voxlib");
    fs::write(&lib_path, &voxlib_bytes).expect("voxlib write should succeed");

    let mut runtime = vox_runtime::Runtime::default();
    let id = runtime
        .mount_voxlib_file(&lib_path)
        .expect("voxlib mount should succeed");

    let mounted = runtime
        .library(id)
        .expect("library should be accessible by id");
    assert_eq!(
        mounted.manifest.package, module,
        "mounted package name should match"
    );

    let _ = fs::remove_file(&lib_path);
    let _ = id;
}

fn extern_manifest() -> PackageManifest {
    let (manifest, _) = ExternalLibrary::new("test.fxt")
        .expect("valid package name")
        .build()
        .expect("external library build should succeed");
    manifest
}

fn extern_registry() -> HostRegistry {
    let mut registry = HostRegistry::default();
    registry.register_package(extern_manifest());
    registry
}

fn extern_fixture_paths() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("compiler")
        .join("extern_ok");
    collect_vox_paths(&dir)
}

fn host_manifest() -> PackageManifest {
    let package = ModulePath::parse("fixtures.host").expect("valid host package path");
    PackageManifest {
        package: package.clone(),
        reexports: Vec::new(),
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
        values: Vec::new(),
        trait_impls: BTreeMap::new(),
    }
}

fn tools_manifest() -> PackageManifest {
    PackageManifest {
        package: ModulePath::parse("fixtures.tools").expect("valid tools package path"),
        reexports: Vec::new(),
        types: Vec::new(),
        traits: Vec::new(),
        functions: Vec::new(),
        values: Vec::new(),
        trait_impls: BTreeMap::new(),
    }
}

fn image_manifest() -> PackageManifest {
    let package = ModulePath::parse("image").expect("valid image package path");
    let image_type = VoxType::Named(qualified_type(&package, "Image"));
    let filter_type = VoxType::DynTrait(qualified_type(&package, "Filter"));

    PackageManifest {
        package: package.clone(),
        reexports: Vec::new(),
        types: vec![
            TypeSpec {
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
            },
            TypeSpec {
                name: qualified_type(&package, "Blur"),
                fields: vec![FieldSpec {
                    name: "radius".to_owned(),
                    ty: VoxType::Float,
                }],
            },
            TypeSpec {
                name: qualified_type(&package, "Sharpen"),
                fields: vec![FieldSpec {
                    name: "amount".to_owned(),
                    ty: VoxType::Float,
                }],
            },
        ],
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
        values: Vec::new(),
        trait_impls: {
            let mut impls = BTreeMap::new();
            impls.insert(
                qualified_type(&package, "Filter"),
                BTreeSet::from([
                    qualified_type(&package, "Blur"),
                    qualified_type(&package, "Sharpen"),
                ]),
            );
            impls
        },
    }
}

fn qualified_type(module: &ModulePath, name: &str) -> QualifiedTypeName {
    QualifiedTypeName {
        module: module.clone(),
        name: name.to_owned(),
    }
}
