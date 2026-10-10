//! `capl` — command line driver.
//!
//! ```text
//! capl node.can                       # type check only
//! capl node.can --emit=wasm -o node.wasm
//! capl node.can --emit=c    -o node.c
//! capl node.can --emit=ast  -o node.ast.json
//! capl node.can --emit=wat            # textual listing on stdout
//! ```

use std::process::ExitCode;

use capl_driver::{compile, BackendId, Options};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!("usage: capl <file.can> [--emit=wasm|wat|c|ast] [-o <out>]");
        return ExitCode::from(2);
    }

    let mut path = None;
    let mut emit = "check".to_string();
    let mut out_path: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--emit=") {
            emit = v.to_string();
        } else if a == "-o" {
            out_path = it.next().cloned();
        } else if !a.starts_with('-') {
            path = Some(a.clone());
        }
    }

    let Some(path) = path else {
        eprintln!("capl: no input file");
        return ExitCode::from(2);
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("capl: cannot read {path}: {e}");
            return ExitCode::from(2);
        }
    };

    let backends = match emit.as_str() {
        "c" => vec![BackendId::C],
        "ast" => vec![BackendId::Ast],
        "wasm" | "wat" => vec![BackendId::Wasm],
        _ => vec![],
    };
    let include_paths = std::env::var("CAPL_INCLUDE_PATH")
        .ok()
        .into_iter()
        .collect();
    let out = compile(
        &src,
        Options {
            file_name: path.clone(),
            backends,
            include_paths,
        },
    );

    for d in &out.diagnostics {
        eprintln!("{d}");
    }
    if !out.ok {
        eprintln!("capl: {} error(s)", out.diagnostics.len());
        return ExitCode::FAILURE;
    }
    eprintln!(
        "capl: parse {} us, sema {} us, codegen {} us",
        out.timings.parse_us, out.timings.sema_us, out.timings.codegen_us
    );

    let write = |bytes: &[u8], default: &str| match &out_path {
        Some(p) => std::fs::write(p, bytes).map_err(|e| e.to_string()),
        None => {
            if default == "-" {
                print!("{}", String::from_utf8_lossy(bytes));
                Ok(())
            } else {
                std::fs::write(default, bytes).map_err(|e| e.to_string())
            }
        }
    };

    let res = match emit.as_str() {
        "ast" => write(out.ast_json.as_bytes(), "-"),
        "c" => write(out.c.unwrap_or_default().as_bytes(), "-"),
        "wat" => write(
            out.wasm
                .as_ref()
                .map(|w| w.wat.clone())
                .unwrap_or_default()
                .as_bytes(),
            "-",
        ),
        "wasm" => write(
            &out.wasm
                .as_ref()
                .map(|w| w.bytes.clone())
                .unwrap_or_default(),
            "a.wasm",
        ),
        _ => Ok(()),
    };
    if let Err(e) = res {
        eprintln!("capl: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
