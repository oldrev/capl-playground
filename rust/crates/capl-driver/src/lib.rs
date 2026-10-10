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

#[cfg(feature = "wasm-backend")]
pub use capl_codegen_wasm::{WasmArtifact, WasmBackend};

/// What a backend hands back to the driver.
pub enum Artifact {
    Binary { bytes: Vec<u8>, listing: String },
    Source { text: String, extension: &'static str },
    Json(String),
}

#[derive(Debug)]
pub struct BackendError(pub String);

/// Implement this to add a target. The decorated AST is read-only here.
pub trait Backend {
    /// stable id used by `--backend=<id>`
    fn id(&self) -> &'static str;
    fn emit(&mut self, unit: &Unit, sema: &SemaResult, diags: &mut DiagBag) -> Result<Artifact, BackendError>;
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
}

impl Default for Options {
    fn default() -> Self {
        Self { file_name: "source.can".into(), backends: vec![BackendId::Wasm, BackendId::C, BackendId::Ast] }
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
    START.get_or_init(std::time::Instant::now).elapsed().as_micros()
}

#[cfg(target_arch = "wasm32")]
fn clock_us() -> u128 { 0 }

/// Run lex → parse → sema → backends.
pub fn compile(src: &str, opts: Options) -> Output {
    let mut diags = DiagBag::new();
    let mut out = Output::default();

    let t0 = clock_us();
    let mut unit = capl_parse::parse(src, &opts.file_name, &mut diags);
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
