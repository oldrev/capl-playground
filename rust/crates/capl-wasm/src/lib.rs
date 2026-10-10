//! `capl-wasm` — the compiler itself, compiled to WebAssembly.
//!
//! Build with:
//! ```text
//! wasm-pack build crates/capl-wasm --release --target web --out-dir ../../../public/capl
//! ```
//!
//! The playground loads `public/capl/capl_wasm.js`, calls [`register`] and
//! from then on `globalThis.__CAPL_CORE__` is the Rust compiler used by the UI.
#![forbid(unsafe_code)]

use capl_ast::{Decl, EventSpec};
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
    #[serde(rename = "tokenCount")]
    token_count: usize,
    #[serde(rename = "nodeCount")]
    node_count: usize,
    c: Option<String>,
    wasm: Option<JsWasm>,
    sema: JsSema,
    timings: JsTimings,
}

#[derive(Serialize)]
struct JsWasm {
    bytes: Vec<u8>,
    wat: String,
    exports: Vec<String>,
    imports: Vec<String>,
    #[serde(rename = "memoryPages")]
    memory_pages: u32,
    #[serde(rename = "msgbufAddr")]
    msgbuf_addr: u32,
    layout: Vec<JsLayout>,
}

#[derive(Serialize, Default)]
struct JsLayout {
    name: String,
    addr: i64,
    size: u32,
    ty: String,
    kind: String,
}

#[derive(Serialize, Default)]
struct JsSema {
    handlers: Vec<JsHandler>,
    timers: Vec<JsTimer>,
}

#[derive(Serialize)]
struct JsHandler {
    #[serde(rename = "exportName")]
    export_name: String,
    event: JsEvent,
}

#[derive(Serialize)]
struct JsTimer {
    id: u32,
    name: String,
}

#[derive(Serialize)]
struct JsEvent {
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wildcard: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
}

#[derive(Serialize)]
struct JsTimings {
    lex: f64,
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

    let lex_start = js_sys::Date::now();
    let mut lex_diags = capl_diag::DiagBag::new();
    let token_count = capl_parse::Lexer::new(src, &mut lex_diags).collect_all().len();
    let lex_ms = js_sys::Date::now() - lex_start;
    let out = compile_impl(src, Options { file_name: "source.can".into(), backends, include_paths: Vec::new() });

    let node_count = serde_json::from_str::<serde_json::Value>(&out.ast_json)
        .map(|v| count_nodes(&v))
        .unwrap_or(0);
    let sema = out.sema.as_ref();
    let handlers = out.unit.as_ref().map(|u| {
        u.decls.iter().filter_map(|d| match d {
            Decl::Handler(h) => Some(JsHandler { export_name: h.export_name.clone(), event: event_json(&h.event) }),
            _ => None,
        }).collect()
    }).unwrap_or_default();
    let timers = sema.map(|s| s.timers.iter().filter_map(|id| {
        s.var(*id).map(|v| JsTimer { id: v.timer_id.unwrap_or(0), name: v.name.clone() })
    }).collect()).unwrap_or_default();
    let wasm = out.wasm.as_ref().map(|w| JsWasm {
        bytes: w.bytes.clone(), wat: w.wat.clone(), exports: w.exports.clone(),
        imports: vec!["capl.write".into(), "capl.setTimer".into(), "capl.setTimerCyclic".into(), "capl.cancelTimer".into(), "capl.output".into(), "capl.timeNow".into(), "capl.random".into(), "capl.strlen".into(), "capl.pow".into(), "capl.sin".into(), "capl.cos".into(), "capl.canOnline".into(), "capl.canOffline".into(), "capl.stopMeasurement".into()],
        memory_pages: w.memory_pages, msgbuf_addr: w.msgbuf_addr,
        layout: w.layout.iter().map(|l| JsLayout { name: l.name.clone(), addr: l.addr, size: l.size, ty: l.ty.clone(), kind: "data".into() }).collect(),
    });

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
        ast_json: out.ast_json.clone(), token_count, node_count,
        c: out.c.clone(),
        wasm,
        sema: JsSema { handlers, timers },
        timings: JsTimings {
            lex: lex_ms,
            parse: out.timings.parse_us as f64 / 1000.0,
            sema: out.timings.sema_us as f64 / 1000.0,
            codegen: out.timings.codegen_us as f64 / 1000.0,
        },
    };
    serde_wasm_bindgen::to_value(&result).map_err(|e| JsValue::from_str(&e.to_string()))
}

fn count_nodes(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::Array(a) => a.iter().map(count_nodes).sum(),
        serde_json::Value::Object(o) => usize::from(o.contains_key("kind")) + o.values().map(count_nodes).sum::<usize>(),
        _ => 0,
    }
}

fn event_json(e: &EventSpec) -> JsEvent {
    match e {
        EventSpec::Start => JsEvent { kind: "start".into(), name: None, id: None, wildcard: None, channel: None, key: None },
        EventSpec::PreStart => JsEvent { kind: "preStart".into(), name: None, id: None, wildcard: None, channel: None, key: None },
        EventSpec::StopMeasurement => JsEvent { kind: "stopMeasurement".into(), name: None, id: None, wildcard: None, channel: None, key: None },
        EventSpec::PreStop => JsEvent { kind: "preStop".into(), name: None, id: None, wildcard: None, channel: None, key: None },
        EventSpec::ErrorFrame => JsEvent { kind: "errorFrame".into(), name: None, id: None, wildcard: None, channel: None, key: None },
        EventSpec::BusOff => JsEvent { kind: "busOff".into(), name: None, id: None, wildcard: None, channel: None, key: None },
        EventSpec::Timer { name } => JsEvent { kind: "timer".into(), name: Some(name.clone()), id: None, wildcard: None, channel: None, key: None },
        EventSpec::Message { id, name, channel, wildcard } => JsEvent { kind: "message".into(), name: name.clone(), id: *id, wildcard: Some(*wildcard), channel: *channel, key: None },
        EventSpec::Key { key, wildcard } => JsEvent { kind: "key".into(), name: None, id: None, wildcard: Some(*wildcard), channel: None, key: key.map(|c| c.to_string()) },
        EventSpec::SysVar { name } => JsEvent { kind: "sysvar".into(), name: Some(name.clone()), id: None, wildcard: None, channel: None, key: None },
        EventSpec::EnvVar { name } => JsEvent { kind: "envVar".into(), name: Some(name.clone()), id: None, wildcard: None, channel: None, key: None },
    }
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
    let compile_fn = Closure::wrap(Box::new(|src: String| -> Result<JsValue, JsValue> {
        compile(&src, "wasm,c,ast")
    }) as Box<dyn FnMut(String) -> Result<JsValue, JsValue>>);
    let _ = js_sys::Reflect::set(&core, &"compile".into(), compile_fn.as_ref());
    compile_fn.forget();
    let _ = js_sys::Reflect::set(&global, &"__CAPL_CORE__".into(), &core);
}
