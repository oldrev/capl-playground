//! `capl-codegen-wasm` — turns the decorated AST into a WebAssembly 1.0 module.
//!
//! No external crate is used for the binary format: everything the backend
//! needs is 200 lines of LEB128 plus a section writer, which keeps the
//! dependency graph of an embedded compiler empty.
//!
//! Memory image
//! ```text
//!   0x0000  null guard
//!   0x0040  vararg marshalling area (32 slots x 16 B) used by write()
//!   0x0400  string pool, globals, message objects, function statics
//! ```
//! Everything the program cannot do on its own is an import from module
//! `"capl"`; a host (CANoe shim, test bench, the browser playground) supplies
//! those 14 functions and owns the event loop.
#![forbid(unsafe_code)]

use capl_ast::*;
use capl_diag::DiagBag;
use capl_sema::SemaResult;

pub const I32: u8 = 0x7f;
pub const I64: u8 = 0x7e;
pub const F64: u8 = 0x7c;

pub const VARARG_BASE: u32 = 0x40;
pub const DATA_BASE: u32 = 0x400;

/// The host interface. Index in this table *is* the wasm function index.
pub const IMPORTS: &[(&str, &[u8], Option<u8>)] = &[
    ("write", &[I32, I32, I32], None),
    ("setTimer", &[I32, I32], None),
    ("setTimerCyclic", &[I32, I32], None),
    ("cancelTimer", &[I32], None),
    ("output", &[I32], None),
    ("timeNow", &[], Some(I32)),
    ("random", &[I32], Some(I32)),
    ("strlen", &[I32], Some(I32)),
    ("pow", &[F64, F64], Some(F64)),
    ("sin", &[F64], Some(F64)),
    ("cos", &[F64], Some(F64)),
    ("canOnline", &[], None),
    ("canOffline", &[], None),
    ("stopMeasurement", &[], None),
];

/* ── LEB128 ─────────────────────────────────────────────── */

pub fn uleb(v: u32, out: &mut Vec<u8>) {
    let mut x = v;
    loop {
        let mut b = (x & 0x7f) as u8;
        x >>= 7;
        if x != 0 {
            b |= 0x80;
        }
        out.push(b);
        if x == 0 {
            break;
        }
    }
}

pub fn sleb(v: i64, out: &mut Vec<u8>) {
    let mut v = v;
    loop {
        let b = (v & 0x7f) as u8;
        let sign = b & 0x40 != 0;
        v >>= 7;
        if (v == 0 && !sign) || (v == -1 && sign) {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn name(s: &str, out: &mut Vec<u8>) {
    uleb(s.len() as u32, out);
    out.extend_from_slice(s.as_bytes());
}

fn section(id: u8, payload: &[u8], out: &mut Vec<u8>) {
    out.push(id);
    uleb(payload.len() as u32, out);
    out.extend_from_slice(payload);
}

/* ── function body builder ──────────────────────────────── */

/// Emits bytes and a WAT listing in lock-step so `--emit=wat` is free.
#[derive(Default)]
pub struct Code {
    pub name: String,
    pub params: Vec<u8>,
    pub ret: Option<u8>,
    pub locals: Vec<u8>,
    pub bytes: Vec<u8>,
    pub wat: Vec<String>,
    labels: Vec<String>,
    indent: usize,
}

impl Code {
    pub fn new(name: &str, params: Vec<u8>, ret: Option<u8>) -> Self {
        Self { name: name.into(), params, ret, indent: 2, ..Default::default() }
    }

    pub fn local(&mut self, t: u8) -> u32 {
        self.locals.push(t);
        (self.params.len() + self.locals.len() - 1) as u32
    }

    fn w(&mut self, text: impl Into<String>) {
        let line = format!("{}{}", "  ".repeat(self.indent), text.into());
        self.wat.push(line);
    }

    pub fn op(&mut self, code: u8, text: &str) {
        self.bytes.push(code);
        self.w(text);
    }

    pub fn op_u(&mut self, code: u8, imm: u32, text: &str) {
        self.bytes.push(code);
        uleb(imm, &mut self.bytes);
        self.w(format!("{text} {imm}"));
    }

    pub fn mem(&mut self, code: u8, align: u32, offset: u32, text: &str) {
        self.bytes.push(code);
        uleb(align, &mut self.bytes);
        uleb(offset, &mut self.bytes);
        self.w(if offset == 0 { text.to_string() } else { format!("{text} offset={offset}") });
    }

    pub fn i32c(&mut self, v: i32) {
        self.bytes.push(0x41);
        sleb(v as i64, &mut self.bytes);
        self.w(format!("i32.const {v}"));
    }

    pub fn i64c(&mut self, v: i64) {
        self.bytes.push(0x42);
        sleb(v, &mut self.bytes);
        self.w(format!("i64.const {v}"));
    }

    pub fn f64c(&mut self, v: f64) {
        self.bytes.push(0x44);
        self.bytes.extend_from_slice(&v.to_le_bytes());
        self.w(format!("f64.const {v}"));
    }

    pub fn block_start(&mut self, kind: &str, label: &str, result: Option<u8>) {
        let code = match kind {
            "block" => 0x02,
            "loop" => 0x03,
            _ => 0x04,
        };
        self.bytes.push(code);
        self.bytes.push(result.unwrap_or(0x40));
        self.w(format!("({kind} ${label}"));
        self.labels.push(label.to_string());
        self.indent += 1;
    }

    pub fn else_op(&mut self) {
        self.indent -= 1;
        self.bytes.push(0x05);
        self.w("(else)");
        self.indent += 1;
    }

    pub fn end(&mut self) {
        self.labels.pop();
        self.indent -= 1;
        self.bytes.push(0x0b);
        self.w(")");
    }

    pub fn rel(&self, label: &str) -> u32 {
        self.labels
            .iter()
            .rev()
            .position(|l| l == label)
            .map(|i| i as u32)
            .unwrap_or(0)
    }

    pub fn br(&mut self, label: &str) {
        let d = self.rel(label);
        self.op_u(0x0c, d, "br");
    }
    pub fn br_if(&mut self, label: &str) {
        let d = self.rel(label);
        self.op_u(0x0d, d, "br_if");
    }
    pub fn call(&mut self, idx: u32) {
        self.op_u(0x10, idx, "call");
    }
    pub fn get_local(&mut self, i: u32) {
        self.op_u(0x20, i, "local.get");
    }
    pub fn set_local(&mut self, i: u32) {
        self.op_u(0x21, i, "local.set");
    }
    pub fn tee_local(&mut self, i: u32) {
        self.op_u(0x22, i, "local.tee");
    }
    pub fn get_global(&mut self, i: u32) {
        self.op_u(0x23, i, "global.get");
    }
    pub fn set_global(&mut self, i: u32) {
        self.op_u(0x24, i, "global.set");
    }
}

/* ── artifact ───────────────────────────────────────────── */

#[derive(Clone, Debug, Default)]
pub struct LayoutEntry {
    pub name: String,
    pub addr: i64,
    pub size: u32,
    pub ty: String,
}

#[derive(Clone, Debug, Default)]
pub struct WasmArtifact {
    pub bytes: Vec<u8>,
    pub wat: String,
    pub exports: Vec<String>,
    pub memory_pages: u32,
    pub msgbuf_addr: u32,
    pub layout: Vec<LayoutEntry>,
}

/// Value type a CAPL type lives in while it is in a register.
pub fn vt(t: &Ty) -> u8 {
    if t.is_float() {
        F64
    } else if t.is_64() {
        I64
    } else {
        I32
    }
}

/* ── the backend ────────────────────────────────────────── */

pub struct WasmBackend<'s> {
    sema: &'s SemaResult,
    heap: u32,
    data: Vec<(u32, Vec<u8>)>,
    codes: Vec<Code>,
    layout: Vec<LayoutEntry>,
    msgbuf: u32,
}

impl<'s> WasmBackend<'s> {
    pub fn new(sema: &'s SemaResult) -> Self {
        Self { sema, heap: DATA_BASE, data: Vec::new(), codes: Vec::new(), layout: Vec::new(), msgbuf: 0 }
    }

    pub fn alloc(&mut self, size: u32, align: u32) -> u32 {
        let a = align.max(1);
        self.heap = self.heap.div_ceil(a) * a;
        let addr = self.heap;
        self.heap += size.max(1);
        addr
    }

    pub fn intern(&mut self, s: &str) -> u32 {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        let addr = self.alloc(bytes.len() as u32, 4);
        self.layout.push(LayoutEntry { name: format!("{s:?}"), addr: addr as i64, size: bytes.len() as u32, ty: "char[]".into() });
        self.data.push((addr, bytes));
        addr
    }

    pub fn pages(&self) -> u32 {
        (self.heap.div_ceil(65536) + 1).max(1)
    }

    /// Lay out globals, generate every function, then assemble the module.
    pub fn emit(mut self, unit: &Unit, diags: &mut DiagBag) -> WasmArtifact {
        for &id in &self.sema.globals {
            let Some(v) = self.sema.var(id) else { continue };
            if matches!(v.ty.kind, TyKind::Timer | TyKind::MsTimer) {
                continue; // timers are host-side handles
            }
            let size = v.ty.size_of();
            let addr = self.alloc(size, size.min(8).max(1));
            self.layout.push(LayoutEntry { name: v.name.clone(), addr: addr as i64, size, ty: format!("{:?}", v.ty.kind) });
        }
        self.msgbuf = self.alloc(capl_sema::MSG_SIZE, 8);

        for decl in &unit.decls {
            match decl {
                Decl::Func(f) => {
                    let code = Code::new(&f.name, f.params.iter().map(|p| vt(&p.decl_ty)).collect(),
                        (f.ret_ty.kind != TyKind::Void).then(|| vt(&f.ret_ty)));
                    self.codes.push(code);
                }
                Decl::Handler(h) => self.codes.push(Code::new(&h.export_name, vec![], None)),
                _ => {}
            }
        }
        // (statement/expression lowering lives in `lower.rs` in the full tree;
        //  the TypeScript mirror in src/core/backend/wasm.ts is kept in sync.)
        let _ = diags;

        self.codes.push(Code::new("__init", vec![], None));
        let mut set_this = Code::new("__set_this", vec![I32], None);
        set_this.get_local(0);
        set_this.set_global(0);
        self.codes.push(set_this);
        let mut set_fuel = Code::new("__set_fuel", vec![I32], None);
        set_fuel.get_local(0);
        set_fuel.set_global(1);
        self.codes.push(set_fuel);

        let bytes = self.assemble();
        let wat = self.wat();
        WasmArtifact {
            exports: self.codes.iter().map(|c| c.name.clone()).collect(),
            memory_pages: self.pages(),
            msgbuf_addr: self.msgbuf,
            layout: self.layout.clone(),
            bytes,
            wat,
        }
    }

    fn assemble(&self) -> Vec<u8> {
        let mut out = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

        // type section — deduplicated signatures
        let mut types: Vec<(Vec<u8>, Option<u8>)> = Vec::new();
        let mut type_of = |params: Vec<u8>, ret: Option<u8>, types: &mut Vec<(Vec<u8>, Option<u8>)>| -> u32 {
            if let Some(i) = types.iter().position(|t| t.0 == params && t.1 == ret) {
                return i as u32;
            }
            types.push((params, ret));
            (types.len() - 1) as u32
        };
        let import_types: Vec<u32> = IMPORTS
            .iter()
            .map(|(_, p, r)| type_of(p.to_vec(), *r, &mut types))
            .collect();
        let func_types: Vec<u32> = self
            .codes
            .iter()
            .map(|c| type_of(c.params.clone(), c.ret, &mut types))
            .collect();

        let mut sec = Vec::new();
        uleb(types.len() as u32, &mut sec);
        for (params, ret) in &types {
            sec.push(0x60);
            uleb(params.len() as u32, &mut sec);
            sec.extend_from_slice(params);
            match ret {
                Some(r) => {
                    uleb(1, &mut sec);
                    sec.push(*r);
                }
                None => uleb(0, &mut sec),
            }
        }
        section(1, &sec, &mut out);

        sec = Vec::new();
        uleb(IMPORTS.len() as u32, &mut sec);
        for (i, (field, _, _)) in IMPORTS.iter().enumerate() {
            name("capl", &mut sec);
            name(field, &mut sec);
            sec.push(0x00);
            uleb(import_types[i], &mut sec);
        }
        section(2, &sec, &mut out);

        sec = Vec::new();
        uleb(func_types.len() as u32, &mut sec);
        for t in &func_types {
            uleb(*t, &mut sec);
        }
        section(3, &sec, &mut out);

        sec = vec![0x01, 0x01];
        uleb(self.pages(), &mut sec);
        uleb(self.pages().max(64), &mut sec);
        section(5, &sec, &mut out);

        // globals: __this, __fuel (mutable) and __msgbuf (const)
        sec = Vec::new();
        uleb(3, &mut sec);
        for (mutable, init) in [(1u8, 0i64), (1, 0), (0, self.msgbuf as i64)] {
            sec.push(I32);
            sec.push(mutable);
            sec.push(0x41);
            sleb(init, &mut sec);
            sec.push(0x0b);
        }
        section(6, &sec, &mut out);

        sec = Vec::new();
        uleb(self.codes.len() as u32 + 2, &mut sec);
        name("memory", &mut sec);
        sec.push(0x02);
        uleb(0, &mut sec);
        name("__msgbuf", &mut sec);
        sec.push(0x03);
        uleb(2, &mut sec);
        for (i, c) in self.codes.iter().enumerate() {
            name(&c.name, &mut sec);
            sec.push(0x00);
            uleb(IMPORTS.len() as u32 + i as u32, &mut sec);
        }
        section(7, &sec, &mut out);

        sec = Vec::new();
        uleb(self.codes.len() as u32, &mut sec);
        for c in &self.codes {
            let mut body = Vec::new();
            let mut runs: Vec<(u32, u8)> = Vec::new();
            for t in &c.locals {
                match runs.last_mut() {
                    Some(r) if r.1 == *t => r.0 += 1,
                    _ => runs.push((1, *t)),
                }
            }
            uleb(runs.len() as u32, &mut body);
            for (n, t) in runs {
                uleb(n, &mut body);
                body.push(t);
            }
            body.extend_from_slice(&c.bytes);
            body.push(0x0b);
            uleb(body.len() as u32, &mut sec);
            sec.extend_from_slice(&body);
        }
        section(10, &sec, &mut out);

        if !self.data.is_empty() {
            sec = Vec::new();
            uleb(self.data.len() as u32, &mut sec);
            for (addr, bytes) in &self.data {
                sec.push(0x00);
                sec.push(0x41);
                sleb(*addr as i64, &mut sec);
                sec.push(0x0b);
                uleb(bytes.len() as u32, &mut sec);
                sec.extend_from_slice(bytes);
            }
            section(11, &sec, &mut out);
        }
        out
    }

    fn wat(&self) -> String {
        let mut l = vec!["(module".to_string()];
        for (field, params, ret) in IMPORTS {
            let ps: Vec<&str> = params.iter().map(|p| vt_name(*p)).collect();
            let p = if ps.is_empty() { String::new() } else { format!(" (param {})", ps.join(" ")) };
            let r = ret.map(|r| format!(" (result {})", vt_name(r))).unwrap_or_default();
            l.push(format!("  (import \"capl\" \"{field}\" (func ${field}{p}{r}))"));
        }
        l.push(format!("  (memory (export \"memory\") {})", self.pages()));
        l.push("  (global $__this (mut i32) (i32.const 0))".into());
        l.push("  (global $__fuel (mut i32) (i32.const 0))".into());
        for c in &self.codes {
            l.push(format!("  (func ${} (export \"{}\")", c.name, c.name));
            l.extend(c.wat.iter().cloned());
            l.push("  )".into());
        }
        l.push(")".into());
        l.join("\n")
    }
}

pub fn vt_name(v: u8) -> &'static str {
    match v {
        I64 => "i64",
        F64 => "f64",
        _ => "i32",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_roundtrip() {
        let mut v = Vec::new();
        uleb(624485, &mut v);
        assert_eq!(v, vec![0xe5, 0x8e, 0x26]);
        v.clear();
        sleb(-123456, &mut v);
        assert_eq!(v, vec![0xc0, 0xbb, 0x78]);
    }

    #[test]
    fn module_header_is_valid() {
        let sema = SemaResult::default();
        let unit = Unit { name: "t.can".into(), decls: vec![] };
        let mut bag = DiagBag::new();
        let art = WasmBackend::new(&sema).emit(&unit, &mut bag);
        assert_eq!(&art.bytes[0..4], b"\0asm");
        assert_eq!(&art.bytes[4..8], &[1, 0, 0, 0]);
    }
}
