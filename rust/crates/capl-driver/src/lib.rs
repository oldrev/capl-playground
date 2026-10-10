//! `capl-driver` — the only crate that knows the order of the stages.
//!
//! ```no_run
//! use capl_driver::{compile, Options, BackendId};
//!
//! let out = compile("on start { write(\"hi\"); }", Options::default());
//! assert!(out.ok);
//! let wasm = out.wasm.unwrap().bytes;      // ready for WebAssembly::instantiate
//! ```
//!
//! Backends are selected at run time through [`Backend`], so an embedder can
//! register an LLVM-IR or bytecode backend without touching the front end.
#![forbid(unsafe_code)]

use capl_ast::Unit;
use capl_diag::{DiagBag, Diagnostic};
use capl_sema::SemaResult;
use std::path::{Path, PathBuf};

#[cfg(feature = "wasm-backend")]
pub use capl_codegen_wasm::{WasmArtifact, WasmBackend};

/// What a backend hands back to the driver.
pub enum Artifact {
    Binary {
        bytes: Vec<u8>,
        listing: String,
    },
    Source {
        text: String,
        extension: &'static str,
    },
    Json(String),
}

#[derive(Debug)]
pub struct BackendError(pub String);

/// Implement this to add a target. The decorated AST is read-only here.
pub trait Backend {
    /// stable id used by `--backend=<id>`
    fn id(&self) -> &'static str;
    fn emit(
        &mut self,
        unit: &Unit,
        sema: &SemaResult,
        diags: &mut DiagBag,
    ) -> Result<Artifact, BackendError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendId {
    Wasm,
    C,
    Ast,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub file_name: String,
    pub backends: Vec<BackendId>,
    pub include_paths: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            file_name: "source.can".into(),
            backends: vec![BackendId::Wasm, BackendId::C, BackendId::Ast],
            include_paths: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct Timings {
    pub parse_us: u128,
    pub sema_us: u128,
    pub codegen_us: u128,
}

#[derive(Default)]
pub struct Output {
    pub ok: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub ast_json: String,
    pub unit: Option<Unit>,
    pub sema: Option<SemaResult>,
    pub c: Option<String>,
    #[cfg(feature = "wasm-backend")]
    pub wasm: Option<WasmArtifact>,
    pub timings: Timings,
}

#[cfg(not(target_arch = "wasm32"))]
fn clock_us() -> u128 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_micros()
}

#[cfg(target_arch = "wasm32")]
fn clock_us() -> u128 {
    0
}

/// Run lex → parse → sema → backends.
pub fn compile(src: &str, opts: Options) -> Output {
    let mut diags = DiagBag::new();
    let mut out = Output::default();

    let mut stack = Vec::new();
    let source_path = Path::new(&opts.file_name);
    if source_path.is_file() {
        if let Ok(path) = std::fs::canonicalize(source_path) {
            stack.push(path);
        }
    }
    let expanded = expand_includes(
        src,
        &opts.file_name,
        &opts.include_paths,
        &mut stack,
        &mut diags,
    );

    let t0 = clock_us();
    let mut unit = capl_parse::parse(&expanded, &opts.file_name, &mut diags);
    out.timings.parse_us = clock_us().saturating_sub(t0);

    let t1 = clock_us();
    let sema = capl_sema::analyze(&mut unit, &mut diags);
    out.timings.sema_us = clock_us().saturating_sub(t1);

    out.ast_json = unit.to_json_string();

    let t2 = clock_us();
    if !diags.has_errors() {
        #[cfg(feature = "c-backend")]
        if opts.backends.contains(&BackendId::C) {
            out.c = Some(capl_codegen_c::emit_c(&unit, &sema));
        }
        #[cfg(feature = "wasm-backend")]
        if opts.backends.contains(&BackendId::Wasm) {
            out.wasm = Some(WasmBackend::new(&sema).emit(&unit, &mut diags));
        }
    }
    out.timings.codegen_us = clock_us().saturating_sub(t2);

    out.ok = !diags.has_errors();
    out.diagnostics = diags.items().to_vec();
    out.sema = Some(sema);
    out.unit = Some(unit);
    out
}

fn expand_includes(
    src: &str,
    file_name: &str,
    include_paths: &[String],
    stack: &mut Vec<PathBuf>,
    diags: &mut DiagBag,
) -> String {
    let mut lex_diags = DiagBag::new();
    let toks = capl_parse::Lexer::new(src, &mut lex_diags).collect_all();
    diags.merge(lex_diags);
    let mut out = String::new();
    let mut i = 0;
    while i < toks.len() {
        if toks[i].is_kw("includes") && toks.get(i + 1).is_some_and(|t| t.is_punct("{")) {
            let mut j = i + 2;
            while j < toks.len() && !toks[j].is_punct("}") {
                if toks[j].kind == capl_parse::TokKind::Str {
                    let requested = toks[j].string.clone();
                    let current = Path::new(file_name)
                        .parent()
                        .unwrap_or_else(|| Path::new("."));
                    let mut candidates = vec![current.join(&requested)];
                    candidates.extend(include_paths.iter().map(|p| Path::new(p).join(&requested)));
                    let Some(path) = candidates.into_iter().find(|p| p.is_file()) else {
                        diags.error(
                            capl_diag::Stage::Parse,
                            toks[j].loc,
                            format!("include file '{requested}' was not found"),
                        );
                        j += 1;
                        continue;
                    };
                    let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
                    if stack.contains(&canonical) {
                        diags.error(
                            capl_diag::Stage::Parse,
                            toks[j].loc,
                            format!("recursive include of '{}'", path.display()),
                        );
                    } else if let Ok(content) = std::fs::read_to_string(&path) {
                        stack.push(canonical);
                        out.push_str(&expand_includes(
                            &content,
                            &path.to_string_lossy(),
                            include_paths,
                            stack,
                            diags,
                        ));
                        out.push('\n');
                        stack.pop();
                    } else {
                        diags.error(
                            capl_diag::Stage::Parse,
                            toks[j].loc,
                            format!("cannot read include file '{}'", path.display()),
                        );
                    }
                }
                j += 1;
            }
            i = if toks.get(j + 1).is_some_and(|t| t.is_punct(";")) {
                j + 2
            } else {
                j.saturating_add(1)
            };
        } else {
            out.push_str(&toks[i].text);
            out.push(' ');
            i += 1;
        }
    }
    out
}
