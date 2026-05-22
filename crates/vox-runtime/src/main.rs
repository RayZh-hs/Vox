use std::{env, error::Error};

use vox_runtime::RuntimeServer;

fn main() -> Result<(), Box<dyn Error>> {
    let addr = parse_listen_addr()?;
    eprintln!("vox-runtime listening on {addr}");
    RuntimeServer::default().serve_tcp(addr)?;
    Ok(())
}

fn parse_listen_addr() -> Result<String, Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let mut listen = "127.0.0.1:4545".to_owned();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                let Some(addr) = args.next() else {
                    return Err("`--listen` requires an address".into());
                };
                listen = addr;
            }
            "--help" | "-h" => {
                println!("Usage: vox-runtime [--listen host:port]");
                std::process::exit(0);
            }
            other => {
                return Err(format!("unrecognized argument `{other}`").into());
            }
        }
    }

    Ok(listen)
}
