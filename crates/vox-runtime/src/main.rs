use std::{
    env, error::Error,
    path::PathBuf,
};

use vox_runtime::{Runtime, RuntimeServer};

fn main() -> Result<(), Box<dyn Error>> {
    let (addr, mount_paths) = parse_args()?;

    let mut runtime = Runtime::default();
    for path in &mount_paths {
        mount_at_path(&mut runtime, path)?;
    }

    eprintln!("vox-runtime listening on {addr}");
    RuntimeServer::new(runtime).serve_tcp(addr)?;
    Ok(())
}

fn parse_args() -> Result<(String, Vec<PathBuf>), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let mut listen = "127.0.0.1:4545".to_owned();
    let mut mount_paths = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                let Some(addr) = args.next() else {
                    return Err("`--listen` requires an address".into());
                };
                listen = addr;
            }
            "--mount" => {
                let Some(path) = args.next() else {
                    return Err("`--mount` requires a path".into());
                };
                mount_paths.push(PathBuf::from(path));
            }
            "--help" | "-h" => {
                println!(
                    "Usage: vox-runtime [--listen host:port] [--mount <path>]..."
                );
                std::process::exit(0);
            }
            other => {
                return Err(format!("unrecognized argument `{other}`").into());
            }
        }
    }

    Ok((listen, mount_paths))
}

fn mount_at_path(runtime: &mut Runtime, path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    if path.is_dir() {
        runtime.mount_dir(path)?;
    } else {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("vox") => {
                runtime.mount_vox_file(path)?;
            }
            Some("voxlib") => {
                runtime.mount_voxlib_file(path)?;
            }
            other => {
                return Err(format!(
                    "unsupported file extension for mounting: {:?}",
                    other
                )
                .into());
            }
        }
    }
    Ok(())
}
