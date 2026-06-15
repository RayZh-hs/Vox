use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process,
};

use vox_core::{
    external_library::decode_external_library_file,
    host::HostRegistry,
    opt::OptimizationLevel,
    source::SourceText,
};
use vox_compiler::{CompileRequest, Compiler, compile_to_voxlib};

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    let mut host = HostRegistry::default();
    for mount_path in &args.mounts {
        mount_library(&mut host, mount_path)?;
    }

    let source_text = fs::read_to_string(&args.input)
        .map_err(|error| format!("cannot read `{}`: {error}", args.input.display()))?;
    let source = SourceText::new(
        &args.input.to_string_lossy(),
        1,
        &source_text,
    );

    let is_package = args.force_package || is_package_source(&source_text);

    if is_package {
        let request = CompileRequest {
            source,
            optimization: OptimizationLevel::SOpt,
            optimization_overrides: BTreeMap::new(),
            host,
        };
        let voxlib_bytes = compile_to_voxlib(request)?;
        let output = output_path(&args.input, args.output.as_deref(), "voxlib");
        fs::write(&output, &voxlib_bytes)
            .map_err(|error| format!("cannot write `{}`: {error}", output.display()))?;
        eprintln!("wrote {}", output.display());
    } else {
        let request = CompileRequest {
            source,
            optimization: OptimizationLevel::SOpt,
            optimization_overrides: BTreeMap::new(),
            host,
        };
        let result = Compiler::default().compile(request);
        let artifact = result
            .artifact
            .ok_or_else(|| result.diagnostics.to_string())?;
        let wasm_bytes = artifact
            .plan
            .wasm
            .as_ref()
            .ok_or_else(|| {
                "wasm artifact was not produced (unsupported MIR shape)".to_owned()
            })?
            .bytes
            .clone();
        let output = output_path(&args.input, args.output.as_deref(), "wasm");
        fs::write(&output, &wasm_bytes)
            .map_err(|error| format!("cannot write `{}`: {error}", output.display()))?;
        eprintln!("wrote {}", output.display());
    }

    Ok(())
}

fn is_package_source(source: &str) -> bool {
    source
        .trim_start()
        .starts_with("package")
}

struct Args {
    input: PathBuf,
    output: Option<PathBuf>,
    mounts: Vec<PathBuf>,
    force_package: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = env::args().skip(1);
    let mut input = None;
    let mut output = None;
    let mut mounts = Vec::new();
    let mut force_package = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mount" => {
                let Some(path) = args.next() else {
                    return Err("`--mount` requires a path".to_owned());
                };
                mounts.push(PathBuf::from(path));
            }
            "-o" => {
                let Some(path) = args.next() else {
                    return Err("`-o` requires a path".to_owned());
                };
                output = Some(PathBuf::from(path));
            }
            "--package" => force_package = true,
            "--help" | "-h" => {
                print_help();
                process::exit(0);
            }
            other => {
                if other.starts_with('-') {
                    return Err(format!("unrecognized argument `{other}`"));
                }
                if input.is_some() {
                    return Err("only one input file may be provided".to_owned());
                }
                let path = PathBuf::from(other);
                if !path.exists() {
                    return Err(format!("file not found: `{}`", path.display()));
                }
                input = Some(path);
            }
        }
    }

    let Some(input) = input else {
        print_help();
        process::exit(1);
    };

    Ok(Args {
        input,
        output,
        mounts,
        force_package,
    })
}

fn mount_library(host: &mut HostRegistry, path: &Path) -> Result<(), String> {
    if path.is_dir() {
        let mut count = 0usize;
        for entry in
            fs::read_dir(path).map_err(|error| format!("cannot read `{}`: {error}", path.display()))?
        {
            let entry = entry.map_err(|error| format!("cannot read entry: {error}"))?;
            let entry_path = entry.path();
            if let Some(ext) = entry_path.extension().and_then(|ext| ext.to_str()) {
                if ext == "voxlib" {
                    mount_voxlib(host, &entry_path)?;
                    count += 1;
                }
            }
        }
        eprintln!("mounted {count} librar{} from `{}`", if count == 1 { "y" } else { "ies" }, path.display());
    } else {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("voxlib") => mount_voxlib(host, path)?,
            Some("vox") => {
                return Err(format!(
                    "`.vox` files cannot be mounted as libraries by vox-compiler; \
                     compile them to `.voxlib` first or use vox-repl: `{}`",
                    path.display()
                ));
            }
            _ => {
                return Err(format!(
                    "unsupported mount file type: `{}`",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn mount_voxlib(host: &mut HostRegistry, path: &Path) -> Result<(), String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
    let header = decode_external_library_file(&bytes)
        .map_err(|error| format!("invalid .voxlib `{}`: {error}", path.display()))?;
    host.register_package(header.manifest);
    Ok(())
}

fn output_path(input: &Path, output: Option<&Path>, ext: &str) -> PathBuf {
    if let Some(output) = output {
        output.to_path_buf()
    } else {
        let mut path = input.to_path_buf();
        path.set_extension(ext);
        path
    }
}

fn print_help() {
    eprintln!("Usage: vox-compiler [OPTIONS] FILE");
    eprintln!();
    eprintln!("Compile a Vox source file to wasm or a .voxlib artifact.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --mount PATH    Mount a library directory, .vox, or .voxlib file (repeatable)");
    eprintln!("  -o OUTPUT       Output file path (default: input stem with .wasm or .voxlib)");
    eprintln!("  --package       Force .voxlib output even for script sources");
    eprintln!("  -h, --help      Show this help message");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  vox-compiler hello.vox                  # script → hello.wasm");
    eprintln!("  vox-compiler --mount ./lib/ hello.vox   # with library deps");
    eprintln!("  vox-compiler mylib.vox                  # package → mylib.voxlib");
    eprintln!("  vox-compiler -o out.wasm script.vox     # custom output path");
}
