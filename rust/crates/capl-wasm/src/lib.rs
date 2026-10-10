//! `capl-wasm` — the compiler itself, compiled to WebAssembly.
//!
//! Build with:
//! ```text
//! wasm-pack build crates/capl-wasm --release --target web --out-dir ../../../public/capl
//! ```
//!
//! The playground loads `public/capl/capl_wasm.js`, calls [`register`] and
//! from then on `globalThis.__CAPL_CORE__` is this Rust core instead of the
//! TypeScript mirror — the UI never notices the difference because both
//! expose the same `compile(src) -> CompileResult` shape.
#![forbid(unsafe_code)]

use capl_driver::{compile as compile_impl, BackendId, Options};
use serde::Serialize;
use wasm_bindgen::prelude::*;

#[derive(Serialize)]
struct JsDiag {
    severity: &'static str,
    stage: &'static str,
    message: String,
    loc: JsLoc,
}

#[derive(Serialize)]
struct JsLoc {
    line: u32,
    col: u32,
    pos: u32,
    end: u32,
}

#[derive(Serialize)]
struct JsResult {
    ok: bool,
    diags: Vec<JsDiag>,
    #[serde(rename = "astJson")]
    ast_json: String,
    c: Option<String>,
    wat: Option<String>,
    bytes: Option<Vec<u8>>,
    exports: Vec<String>,
    #[serde(rename = "msgbufAddr")]
    msgbuf_addr: u32,
    timings: JsTimings,
}

#[derive(Serialize)]
struct JsTimings {
    parse: f64,
    sema: f64,
    codegen: f64,
}

/// Compile one translation unit; returns a plain JS object.
#[wasm_bindgen]
pub fn compile(src: &str, backends: &str) -> Result<JsValue, JsValue> {
    #[cfg(feature = "panic-hook")]
    console_error_panic_hook::set_once();

    let backends: Vec<BackendId> = backends
        .split(',')
        .filter_map(|b| match b.trim() {
            "wasm" => Some(BackendId::Wasm),
            "c" => Some(BackendId::C),
            "ast" => Some(BackendId::Ast),
            _ => None,
        })
        .collect();

    let out = compile_impl(src, Options { file_name: "source.can".into(), backends });

    let result = JsResult {
        ok: out.ok,
        diags: out
            .diagnostics
            .iter()
            .map(|d| JsDiag {
                severity: match d.severity {
                    capl_diag::Severity::Error => "error",
                    capl_diag::Severity::Warning => "warning",
                    capl_diag::Severity::Note => "note",
                },
                stage: match d.stage {
                    capl_diag::Stage::Lex => "lex",
                    capl_diag::Stage::Parse => "parse",
                    capl_diag::Stage::Sema => "sema",
                    capl_diag::Stage::Codegen => "codegen",
                    capl_diag::Stage::Link => "link",
                },
                message: d.message.clone(),
                loc: JsLoc { line: d.loc.line, col: d.loc.col, pos: d.loc.pos, end: d.loc.end },
            })
            .collect(),
        ast_json: out.ast_json.clone(),
        c: out.c.clone(),
        wat: out.wasm.as_ref().map(|w| w.wat.clone()),
        bytes: out.wasm.as_ref().map(|w| w.bytes.clone()),
        exports: out.wasm.as_ref().map(|w| w.exports.clone()).unwrap_or_default(),
        msgbuf_addr: out.wasm.as_ref().map(|w| w.msgbuf_addr).unwrap_or(0),
        timings: JsTimings {
            parse: out.timings.parse_us as f64 / 1000.0,
            sema: out.timings.sema_us as f64 / 1000.0,
            codegen: out.timings.codegen_us as f64 / 1000.0,
        },
    };
    serde_wasm_bindgen::to_value(&result).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Installs this core as `globalThis.__CAPL_CORE__`.
#[wasm_bindgen(start)]
pub fn register() {
    let global = js_sys::global();
    let core = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&core, &"id".into(), &"rust-wasm".into());
    let _ = js_sys::Reflect::set(&core, &"version".into(), &version().into());
    let _ = js_sys::Reflect::set(&global, &"__CAPL_CORE__".into(), &core);
}
