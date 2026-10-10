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
use capl_sema::{usual_conversion, SemaResult};
use std::collections::HashMap;

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

fn default_ty() -> Ty {
    Ty::scalar(TyKind::Long)
}

fn expr_ty(expr: &Expr) -> Ty {
    expr.ty.clone().unwrap_or_else(|| match &expr.kind {
        ExprKind::FloatLit { .. } => Ty::scalar(TyKind::Double),
        ExprKind::StrLit { ref value } => Ty::array(Ty::scalar(TyKind::Char), value.len() as u32 + 1),
        _ => default_ty(),
    })
}

/* ── the backend ────────────────────────────────────────── */

pub struct WasmBackend<'s> {
    sema: &'s SemaResult,
    heap: u32,
    data: Vec<(u32, Vec<u8>)>,
    codes: Vec<Code>,
    layout: Vec<LayoutEntry>,
    msgbuf: u32,
    global_addr: HashMap<SymId, u32>,
    slots: HashMap<SymId, u32>,
    static_addr: HashMap<SymId, u32>,
    func_idx: HashMap<SymId, u32>,
    consts: HashMap<SymId, i64>,
    label_n: u32,
    breaks: Vec<String>,
    continues: Vec<String>,
    current_ret: Ty,
}

impl<'s> WasmBackend<'s> {
    pub fn new(sema: &'s SemaResult) -> Self {
        Self { sema, heap: DATA_BASE, data: Vec::new(), codes: Vec::new(), layout: Vec::new(), msgbuf: 0, global_addr: HashMap::new(), slots: HashMap::new(), static_addr: HashMap::new(), func_idx: HashMap::new(), consts: HashMap::new(), label_n: 0, breaks: Vec::new(), continues: Vec::new(), current_ret: default_ty() }
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
        self.collect_consts(unit);
        for &id in &self.sema.globals {
            let Some(v) = self.sema.var(id) else { continue };
            if matches!(v.ty.kind, TyKind::Timer | TyKind::MsTimer) {
                continue; // timers are host-side handles
            }
            let size = v.ty.size_of();
            let addr = self.alloc(size, size.clamp(1, 8));
            self.global_addr.insert(id, addr);
            if let Some(Expr { kind: ExprKind::StrLit { value }, .. }) = unit.decls.iter().find_map(|d| match d {
                Decl::Var(vd) if vd.sym == Some(id) => vd.init.as_ref(),
                Decl::Variables { decls, .. } => decls.iter().find(|vd| vd.sym == Some(id)).and_then(|vd| vd.init.as_ref()),
                _ => None,
            }) {
                let mut bytes = value.as_bytes().to_vec(); bytes.push(0); bytes.resize(size as usize, 0);
                self.data.push((addr, bytes));
            }
            if v.ty.kind == TyKind::Message {
                let mut bytes = vec![0u8; 32];
                let id = v.ty.msg_id.unwrap_or(0).to_le_bytes(); bytes[0..4].copy_from_slice(&id);
                bytes[4..8].copy_from_slice(&8i32.to_le_bytes()); bytes[16..20].copy_from_slice(&1i32.to_le_bytes());
                self.data.push((addr, bytes));
            }
            if let Some(init) = unit.decls.iter().find_map(|d| match d {
                Decl::Var(vd) if vd.sym == Some(id) => vd.init.as_ref(),
                Decl::Variables { decls, .. } => decls.iter().find(|vd| vd.sym == Some(id)).and_then(|vd| vd.init.as_ref()),
                _ => None,
            }) {
                if let Some(value) = match init.kind { ExprKind::IntLit { value } | ExprKind::CharLit { value } => Some(value), _ => None } {
                    let mut bytes = vec![0u8; v.ty.size_of() as usize]; let n = 4.min(bytes.len()); bytes[..n].copy_from_slice(&(value as i32).to_le_bytes()[..n]); self.data.push((addr, bytes));
                }
                if let ExprKind::InitList { items } = &init.kind {
                    let elem_size = v.ty.elem.as_ref().map(|e| e.size_of()).unwrap_or(4);
                    let mut bytes = vec![0u8; v.ty.size_of() as usize];
                    for (i, item) in items.iter().enumerate().take(v.ty.len.unwrap_or(0) as usize) {
                        if let ExprKind::IntLit { value } | ExprKind::CharLit { value } = item.kind {
                            let off = i * elem_size as usize; let n = (elem_size as usize).min(4); bytes[off..off+n].copy_from_slice(&(value as i32).to_le_bytes()[..n]);
                        }
                    }
                    self.data.push((addr, bytes));
                }
            }
            self.layout.push(LayoutEntry { name: v.name.clone(), addr: addr as i64, size, ty: format!("{:?}", v.ty.kind) });
        }
        self.msgbuf = self.alloc(capl_sema::MSG_SIZE, 8);

        for decl in &unit.decls {
            match decl {
                Decl::Func(f) => {
                    if let Some(id) = self.sema.funcs.iter().copied().find(|id| self.sema.func(*id).map(|s| s.name == f.name).unwrap_or(false)) { self.func_idx.insert(id, IMPORTS.len() as u32 + self.codes.len() as u32); }
                    let mut code = self.begin_code(&f.name, &f.params, &f.name, &f.ret_ty);
                    self.lower_stmt(&mut code, &f.body);
                    self.finish_code(&mut code, &f.ret_ty);
                    self.codes.push(code);
                }
                Decl::Handler(h) => {
                    let mut code = self.begin_code(&h.export_name, &[], &h.export_name, &Ty::scalar(TyKind::Void));
                    self.lower_stmt(&mut code, &h.body);
                    self.finish_code(&mut code, &Ty::scalar(TyKind::Void));
                    self.codes.push(code);
                }
                _ => {}
            }
        }
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

    fn collect_consts(&mut self, unit: &Unit) {
        for (id, sym) in self.sema.syms.iter().enumerate() {
            if let capl_sema::Sym::EnumConst(value) = sym {
                self.consts.insert(id as SymId, *value);
            }
        }
        for decl in &unit.decls {
            let vars = match decl { Decl::Var(v) => std::slice::from_ref(v), Decl::Variables { decls, .. } => decls.as_slice(), _ => &[] };
            for v in vars {
                if !v.is_const { continue; }
                let Some(id) = v.sym else { continue };
                if let Some(init) = &v.init { if let ExprKind::IntLit { value } | ExprKind::CharLit { value } = init.kind { self.consts.insert(id, value); } }
            }
        }
    }

    fn begin_code(&mut self, name: &str, params: &[Param], owner: &str, ret: &Ty) -> Code {
        self.slots.clear();
        self.static_addr.clear();
        self.current_ret = ret.clone();
        let mut code = Code::new(name, params.iter().map(|p| vt(&p.decl_ty)).collect(), (ret.kind != TyKind::Void).then(|| vt(ret)));
        for (i, p) in params.iter().enumerate() {
            if let Some(id) = p.sym { self.slots.insert(id, i as u32); }
        }
        if let Some(locals) = self.sema.locals.get(owner) {
            for id in locals {
                let Some(v) = self.sema.var(*id) else { continue };
                if matches!(v.ty.kind, TyKind::Timer | TyKind::MsTimer) { continue; }
                if v.ty.kind == TyKind::Array || v.ty.kind == TyKind::Message {
                    let addr = self.alloc(v.ty.size_of(), v.ty.size_of().clamp(1, 8));
                    self.static_addr.insert(*id, addr);
                    self.layout.push(LayoutEntry { name: format!("{owner}::{}", v.name), addr: addr as i64, size: v.ty.size_of(), ty: format!("{:?}", v.ty.kind) });
                } else {
                    let slot = code.local(vt(&v.ty));
                    self.slots.insert(*id, slot);
                }
            }
        }
        code
    }

    fn finish_code(&mut self, code: &mut Code, ret: &Ty) {
        if ret.kind != TyKind::Void && code.ret.is_some() { self.zero_value(code, ret); }
    }

    fn lower_stmt(&mut self, code: &mut Code, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Block { stmts } => for s in stmts { self.lower_stmt(code, s); },
            StmtKind::Empty => {}
            StmtKind::Expr { expr } => { self.lower_value(code, expr); code.op(0x1a, "drop"); }
            StmtKind::Decl { decls } => for d in decls { self.lower_decl(code, d); },
            StmtKind::If { cond, then, otherwise } => {
                self.lower_condition(code, cond); code.block_start("if", &format!("if{}", self.label_n), None); self.label_n += 1;
                self.lower_stmt(code, then); if let Some(otherwise) = otherwise { code.else_op(); self.lower_stmt(code, otherwise); } code.end();
            }
            StmtKind::While { cond, body } => self.lower_loop(code, Some(cond), body, false),
            StmtKind::DoWhile { body, cond } => self.lower_loop(code, Some(cond), body, true),
            StmtKind::For { init, cond, step, body } => {
                if let Some(init) = init { self.lower_stmt(code, init); }
                self.lower_for(code, cond.as_ref(), step.as_ref(), body);
            }
            StmtKind::Break => if let Some(label) = self.breaks.last().cloned() { code.br(&label); },
            StmtKind::Continue => if let Some(label) = self.continues.last().cloned() { code.br(&label); },
            StmtKind::Return { expr } => {
                if let Some(expr) = expr { self.lower_value_as(code, expr, &self.current_ret.clone()); }
                code.op(0x0f, "return");
            }
            StmtKind::Switch { disc, clauses } => self.lower_switch(code, disc, clauses),
        }
    }

    fn lower_decl(&mut self, code: &mut Code, d: &VarDecl) {
        let Some(id) = d.sym else { return };
        let Some(v) = self.sema.var(id) else { return };
        if let Some(init) = &d.init { self.store_sym(code, id, &v.ty, init); }
    }

    fn lower_loop(&mut self, code: &mut Code, cond: Option<&Expr>, body: &Stmt, do_first: bool) {
        let n = self.label_n; self.label_n += 1;
        let brk = format!("brk{n}"); let loop_label = format!("loop{n}"); let cont = format!("cont{n}");
        code.block_start("block", &brk, None); code.block_start("loop", &loop_label, None);
        if do_first { code.block_start("block", &cont, None); self.breaks.push(brk.clone()); self.continues.push(cont.clone()); self.lower_stmt(code, body); self.continues.pop(); self.breaks.pop(); code.end(); if let Some(cond) = cond { self.lower_condition(code, cond); code.br_if(&loop_label); } }
        else { if let Some(cond) = cond { self.lower_condition(code, cond); code.op(0x45, "i32.eqz"); code.br_if(&brk); } code.block_start("block", &cont, None); self.breaks.push(brk.clone()); self.continues.push(cont.clone()); self.lower_stmt(code, body); self.continues.pop(); self.breaks.pop(); code.end(); code.br(&loop_label); }
        code.end(); code.end();
    }

    fn lower_for(&mut self, code: &mut Code, cond: Option<&Expr>, step: Option<&Expr>, body: &Stmt) {
        let n = self.label_n; self.label_n += 1; let brk = format!("brk{n}"); let loop_label = format!("loop{n}"); let cont = format!("cont{n}");
        code.block_start("block", &brk, None); code.block_start("loop", &loop_label, None);
        if let Some(cond) = cond { self.lower_condition(code, cond); code.op(0x45, "i32.eqz"); code.br_if(&brk); }
        code.block_start("block", &cont, None); self.breaks.push(brk.clone()); self.continues.push(cont.clone()); self.lower_stmt(code, body); self.continues.pop(); self.breaks.pop(); code.end();
        if let Some(step) = step { self.lower_value(code, step); code.op(0x1a, "drop"); }
        code.br(&loop_label); code.end(); code.end();
    }

    fn lower_switch(&mut self, code: &mut Code, disc: &Expr, clauses: &[SwitchClause]) {
        let ty = expr_ty(disc);
        let tmp = code.local(vt(&ty)); self.lower_value_as(code, disc, &ty); code.set_local(tmp);
        let n = self.label_n; self.label_n += 1; let brk = format!("swbrk{n}");
        let case_labels: Vec<String> = (0..clauses.len()).map(|i| format!("case{n}_{i}")).collect();
        let def_label = format!("swdef{n}");
        code.block_start("block", &brk, None);
        code.block_start("block", &def_label, None);
        for label in case_labels.iter().rev() { code.block_start("block", label, None); }
        let default_index = clauses.iter().position(|c| c.test.is_none());
        for (i, c) in clauses.iter().enumerate() {
            if let Some(test) = &c.test {
                code.get_local(tmp);
                self.lower_value_as(code, test, &ty);
                self.emit_binary_op(code, "==", &ty);
                code.br_if(&case_labels[i]);
            }
        }
        if let Some(i) = default_index { code.br(&case_labels[i]); } else { code.br(&def_label); }
        self.breaks.push(brk);
        for c in clauses {
            code.end();
            for s in &c.body { self.lower_stmt(code, s); }
        }
        self.breaks.pop();
        code.end();
        code.end();
    }

    fn lower_value(&mut self, code: &mut Code, expr: &Expr) {
        let ty = expr_ty(expr);
        self.lower_value_as(code, expr, &ty);
    }

    fn lower_value_as(&mut self, code: &mut Code, expr: &Expr, target: &Ty) {
        let source = expr_ty(expr);
        self.lower_value_native(code, expr);
        self.convert(code, &source, target);
    }

    fn lower_value_native(&mut self, code: &mut Code, expr: &Expr) {
        match &expr.kind {
            ExprKind::IntLit { value } | ExprKind::CharLit { value } => code.i32c(*value as i32),
            ExprKind::FloatLit { value } => code.f64c(*value),
            ExprKind::StrLit { value } => code.i32c(self.intern(value) as i32),
            ExprKind::Ident { sym: Some(id), .. } => self.load_sym(code, *id),
            ExprKind::Ident { sym: None, .. } => code.i32c(0),
            ExprKind::This => code.get_global(0),
            ExprKind::Unary { op, operand, postfix } => self.lower_unary(code, op, operand, *postfix),
            ExprKind::Binary { op, left, right } => self.lower_binary(code, op, left, right),
            ExprKind::Assign { target, value, op } => self.lower_assign(code, target, value, op),
            ExprKind::Cond { cond, then, otherwise } => {
                let ty = expr_ty(expr);
                self.lower_condition(code, cond);
                code.block_start("if", &format!("cond{}", self.label_n), Some(vt(&ty)));
                self.label_n += 1;
                self.lower_value_as(code, then, &ty);
                code.else_op();
                self.lower_value_as(code, otherwise, &ty);
                code.end();
            }
            ExprKind::Call { callee, args, sym } => self.lower_call(code, callee, args, *sym),
            ExprKind::Index { base, index } => {
                let elem = base.ty.as_ref().and_then(|t| t.elem.as_ref()).map(|t| (**t).clone()).unwrap_or_else(default_ty);
                self.lower_value(code, base);
                self.lower_value_as(code, index, &default_ty());
                code.i32c(elem.size_of() as i32);
                code.op(0x6c, "i32.mul");
                code.op(0x6a, "i32.add");
                self.load_mem(code, &elem);
            }
            ExprKind::Member { base, name } => {
                let ty = self.member_ty(name);
                if let Some(off) = self.member_offset(name) {
                    if matches!(base.kind, ExprKind::This) {
                        code.get_global(0); code.i32c(off as i32); code.op(0x6a, "i32.add"); self.load_mem(code, &ty);
                    } else if let Some(addr) = self.member_addr(code, base, name) {
                        self.load_mem(code, &addr.1);
                    } else { code.i32c(0); }
                } else { code.i32c(0); }
            }
            ExprKind::InitList { .. } => code.i32c(0),
        }
    }

    fn lower_unary(&mut self, code: &mut Code, op: &str, operand: &Expr, postfix: bool) {
        let ty = expr_ty(operand);
        if matches!(op, "++" | "--") {
            let ExprKind::Ident { sym: Some(id), .. } = operand.kind else { return };
            self.load_sym(code, id);
            if postfix { let tmp = code.local(vt(&ty)); code.tee_local(tmp); }
            self.emit_const(code, &ty, 1);
            self.emit_binary_op(code, if op == "++" { "+" } else { "-" }, &ty);
            self.store_sym_value(code, id);
            self.load_sym(code, id);
            return;
        }
        if op == "!" {
            self.lower_condition(code, operand);
            code.op(0x45, "i32.eqz");
        } else {
            self.lower_value_as(code, operand, &ty);
            match op {
                "-" if ty.is_float() => code.op(0x9a, "f64.neg"),
                "-" => { self.emit_const(code, &ty, -1); self.emit_binary_op(code, "*", &ty); }
                "~" => { self.emit_const(code, &ty, -1); self.emit_binary_op(code, "^", &ty); }
                _ => {}
            }
        }
    }

    fn lower_binary(&mut self, code: &mut Code, op: &str, left: &Expr, right: &Expr) {
        if op == "&&" || op == "||" {
            self.lower_condition(code, left); self.lower_condition(code, right);
            code.op(if op == "&&" { 0x71 } else { 0x72 }, if op == "&&" { "i32.and" } else { "i32.or" });
            return;
        }
        let ty = usual_conversion(&expr_ty(left), &expr_ty(right));
        self.lower_value_as(code, left, &ty);
        self.lower_value_as(code, right, &ty);
        self.emit_binary_op(code, op, &ty);
    }

    fn emit_binary_op(&self, code: &mut Code, op: &str, ty: &Ty) {
        let (opcode, text) = match (vt(ty), op) {
            (I64, "+") => (0x7c, "i64.add"), (I64, "-") => (0x7d, "i64.sub"), (I64, "*") => (0x7e, "i64.mul"),
            (I64, "/") => (0x7f, "i64.div_s"), (I64, "%") => (0x81, "i64.rem_s"), (I64, "&") => (0x83, "i64.and"),
            (I64, "|") => (0x84, "i64.or"), (I64, "^") => (0x85, "i64.xor"), (I64, "<<") => (0x86, "i64.shl"), (I64, ">>") => (0x87, "i64.shr_s"),
            (I64, "==") => (0x51, "i64.eq"), (I64, "!=") => (0x52, "i64.ne"), (I64, "<") => (0x53, "i64.lt_s"), (I64, ">") => (0x55, "i64.gt_s"), (I64, "<=") => (0x57, "i64.le_s"), (I64, ">=") => (0x59, "i64.ge_s"),
            (F64, "+") => (0xa0, "f64.add"), (F64, "-") => (0xa1, "f64.sub"), (F64, "*") => (0xa2, "f64.mul"), (F64, "/") => (0xa3, "f64.div"),
            (F64, "==") => (0x61, "f64.eq"), (F64, "!=") => (0x62, "f64.ne"), (F64, "<") => (0x63, "f64.lt"), (F64, ">") => (0x64, "f64.gt"), (F64, "<=") => (0x65, "f64.le"), (F64, ">=") => (0x66, "f64.ge"),
            (_, "+") => (0x6a, "i32.add"), (_, "-") => (0x6b, "i32.sub"), (_, "*") => (0x6c, "i32.mul"), (_, "/") => (0x6d, "i32.div_s"), (_, "%") => (0x6f, "i32.rem_s"),
            (_, "&") => (0x71, "i32.and"), (_, "|") => (0x72, "i32.or"), (_, "^") => (0x73, "i32.xor"), (_, "<<") => (0x74, "i32.shl"), (_, ">>") => (0x75, "i32.shr_s"),
            (_, "==") => (0x46, "i32.eq"), (_, "!=") => (0x47, "i32.ne"), (_, "<") => (0x48, "i32.lt_s"), (_, ">") => (0x4a, "i32.gt_s"), (_, "<=") => (0x4c, "i32.le_s"), (_, ">=") => (0x4e, "i32.ge_s"),
            _ => (0x6a, "i32.add"),
        };
        code.op(opcode, text);
    }

    fn lower_assign(&mut self, code: &mut Code, target: &Expr, value: &Expr, op: &str) {
        if let ExprKind::Member { base, name } = &target.kind {
            if op == "=" {
                let ty = self.member_ty(name);
                if let Some(off) = self.member_offset(name) {
                    self.lower_value_as(code, value, &ty); let tmp = code.local(vt(&ty)); code.set_local(tmp);
                    if matches!(base.kind, ExprKind::This) { code.get_global(0); code.i32c(off as i32); code.op(0x6a, "i32.add"); }
                    else if let Some((addr, _)) = self.member_addr(code, base, name) { code.i32c(addr as i32); } else { return; }
                    code.get_local(tmp); self.store_mem(code, &ty); code.get_local(tmp); return;
                }
            }
            self.lower_value(code, value); return;
        }
        if let ExprKind::Call { callee, args, .. } = &target.kind {
            if let ExprKind::Member { base, name } = &callee.kind {
                let width = match name.as_str() { "byte" => 1, "word" => 2, "dword" | "long" => 4, "qword" => 8, _ => 0 };
                if width > 0 {
                    let ty = Ty::scalar(match width { 1 => TyKind::Byte, 2 => TyKind::Word, 8 => TyKind::Qword, _ => TyKind::Dword });
                    self.lower_value_as(code, value, &ty); let tmp = code.local(vt(&ty)); code.set_local(tmp);
                    let index = args.first().and_then(|a| if let ExprKind::IntLit { value } = a.kind { Some(value as u32) } else { None }).unwrap_or(0);
                    if matches!(base.kind, ExprKind::This) { code.get_global(0); code.i32c((8 + index * width) as i32); code.op(0x6a, "i32.add"); }
                    else if let Some((addr, _)) = self.member_addr(code, base, "id") { code.i32c((addr + 8 + index * width) as i32); } else { return; }
                    code.get_local(tmp); self.store_mem(code, &ty); code.get_local(tmp); return;
                }
            }
        }
        if let ExprKind::Index { base, index } = &target.kind {
            let elem = base.ty.as_ref().and_then(|t| t.elem.as_ref()).map(|t| (**t).clone()).unwrap_or_else(|| Ty::scalar(TyKind::Long));
            self.lower_value_as(code, value, &elem); let tmp = code.local(vt(&elem)); code.set_local(tmp);
            self.lower_value(code, base); self.lower_value_as(code, index, &default_ty()); code.i32c(elem.size_of() as i32); code.op(0x6c, "i32.mul"); code.op(0x6a, "i32.add"); code.get_local(tmp); self.store_mem(code, &elem); code.get_local(tmp); return;
        }
        let ExprKind::Ident { sym: Some(id), .. } = target.kind else { self.lower_value(code, value); return };
        let ty = self.sema.var(id).map(|v| v.ty.clone()).unwrap_or_else(default_ty);
        if op == "=" { self.lower_value_as(code, value, &ty); self.store_sym_value(code, id); self.load_sym(code, id); }
        else { self.load_sym(code, id); self.lower_value_as(code, value, &ty); self.emit_binary_op(code, &op[..op.len()-1], &ty); self.store_sym_value(code, id); self.load_sym(code, id); }
    }

    fn lower_condition(&mut self, code: &mut Code, expr: &Expr) {
        let ty = expr_ty(expr);
        self.lower_value(code, expr);
        match vt(&ty) {
            I64 => { code.i64c(0); code.op(0x52, "i64.ne"); }
            F64 => { code.f64c(0.0); code.op(0x62, "f64.ne"); }
            _ => {}
        }
    }

    fn load_sym(&self, code: &mut Code, id: SymId) {
        let ty = self.sema.var(id).map(|v| v.ty.clone()).unwrap_or_else(default_ty);
        if let Some(value) = self.consts.get(&id) { self.emit_const(code, &ty, *value); return; }
        if let Some(v) = self.sema.var(id) {
            if v.ty.kind == TyKind::Array || v.ty.kind == TyKind::Message {
                if let Some(addr) = self.global_addr.get(&id).or_else(|| self.static_addr.get(&id)) { code.i32c(*addr as i32); return; }
            }
        }
        if let Some(slot) = self.slots.get(&id) { code.get_local(*slot); return; }
        if let Some(addr) = self.global_addr.get(&id) { code.i32c(*addr as i32); self.load_mem(code, &ty); return; }
        if let Some(addr) = self.static_addr.get(&id) { code.i32c(*addr as i32); self.load_mem(code, &ty); return; }
        code.i32c(0);
    }

    fn load_mem(&self, code: &mut Code, ty: &Ty) {
        match ty.kind {
            TyKind::Char | TyKind::Byte => code.mem(0x2d, 0, 0, "i32.load8_u"),
            TyKind::Int | TyKind::Word => code.mem(0x2f, 1, 0, "i32.load16_s"),
            TyKind::Int64 | TyKind::Qword => code.mem(0x29, 3, 0, "i64.load"),
            TyKind::Float | TyKind::Double => code.mem(0x2b, 3, 0, "f64.load"),
            _ => code.mem(0x28, 2, 0, "i32.load"),
        }
    }

    fn store_sym_value(&self, code: &mut Code, id: SymId) {
        let ty = self.sema.var(id).map(|v| v.ty.clone()).unwrap_or_else(default_ty);
        let tmp = code.local(vt(&ty)); code.set_local(tmp);
        if let Some(slot) = self.slots.get(&id) { code.get_local(tmp); code.set_local(*slot); return; }
        if let Some(addr) = self.global_addr.get(&id).or_else(|| self.static_addr.get(&id)) {
            code.i32c(*addr as i32); code.get_local(tmp);
            self.store_mem(code, &ty);
        }
    }

    fn store_sym(&mut self, code: &mut Code, id: SymId, ty: &Ty, value: &Expr) { self.lower_value_as(code, value, ty); self.store_sym_value(code, id); }

    fn zero_value(&self, code: &mut Code, ty: &Ty) { match vt(ty) { I32 => code.i32c(0), I64 => code.i64c(0), F64 => code.f64c(0.0), _ => code.i32c(0) } }

    fn emit_const(&self, code: &mut Code, ty: &Ty, value: i64) {
        match vt(ty) {
            I64 => code.i64c(value),
            F64 => code.f64c(value as f64),
            _ => code.i32c(value as i32),
        }
    }

    fn convert(&self, code: &mut Code, from: &Ty, to: &Ty) {
        match (vt(from), vt(to)) {
            (I32, I64) => code.op(if from.is_unsigned() { 0xad } else { 0xac }, if from.is_unsigned() { "i64.extend_i32_u" } else { "i64.extend_i32_s" }),
            (I32, F64) => code.op(if from.is_unsigned() { 0xb8 } else { 0xb7 }, if from.is_unsigned() { "f64.convert_i32_u" } else { "f64.convert_i32_s" }),
            (I64, I32) => code.op(0xa7, "i32.wrap_i64"),
            (I64, F64) => code.op(if from.is_unsigned() { 0xba } else { 0xb9 }, if from.is_unsigned() { "f64.convert_i64_u" } else { "f64.convert_i64_s" }),
            (F64, I32) => code.op(0xaa, "i32.trunc_f64_s"),
            (F64, I64) => code.op(0xb0, "i64.trunc_f64_s"),
            _ => {}
        }
    }

    fn lower_call(&mut self, code: &mut Code, callee: &Expr, args: &[Expr], sym: Option<SymId>) {
        if let ExprKind::Member { base, name } = &callee.kind {
            if let Some(off) = self.member_offset(name) {
                let index = args.first().and_then(|a| if let ExprKind::IntLit { value } = a.kind { Some(value as u32) } else { None }).unwrap_or(0);
                let width = match name.as_str() { "byte" => 1, "word" => 2, "dword" | "long" => 4, "qword" => 8, _ => 0 };
                if width > 0 {
                    let ty = Ty::scalar(match width { 1 => TyKind::Byte, 2 => TyKind::Word, 8 => TyKind::Qword, _ => TyKind::Dword });
                    if matches!(base.kind, ExprKind::This) { code.get_global(0); code.i32c((off + index * width) as i32); code.op(0x6a, "i32.add"); } else if let Some((addr, _)) = self.member_addr(code, base, name) { code.i32c((addr + index * width) as i32); } else { code.i32c(0); }
                    self.load_mem(code, &ty); return;
                }
            }
        }
        let ExprKind::Ident { name, .. } = &callee.kind else { code.i32c(0); return };
        if let Some(id) = sym {
            if let Some(idx) = self.func_idx.get(&id).copied() {
                if let Some(f) = self.sema.func(id) {
                    for (i, arg) in args.iter().enumerate() { if let Some(param) = f.params.get(i) { self.lower_value_as(code, arg, param); } else { break; } }
                    code.call(idx);
                    if f.ret.kind == TyKind::Void { code.i32c(0); }
                    return;
                }
            }
        }
        match name.as_str() {
            "write" => {
                let fmt = args.first().and_then(|a| if let ExprKind::StrLit { value } = &a.kind { Some(value.as_str()) } else { None }).unwrap_or("");
                let fmt_addr = self.intern(fmt);
                for (i, arg) in args.iter().skip(1).enumerate() {
                    let base = VARARG_BASE + i as u32 * 16;
                    let ty = expr_ty(arg);
                    let tag = if ty.kind == TyKind::Array { 5 } else if ty.is_float() { 4 } else if ty.is_unsigned() { 1 } else { 0 };
                    code.i32c(base as i32); code.i32c(tag); code.mem(0x36, 2, 0, "i32.store");
                    code.i32c(base as i32 + 8);
                    if ty.kind == TyKind::Array { self.lower_value_as(code, arg, &Ty::scalar(TyKind::Int64)); code.mem(0x37, 3, 0, "i64.store"); }
                    else if ty.is_float() { self.lower_value_as(code, arg, &Ty::scalar(TyKind::Double)); code.mem(0x39, 3, 0, "f64.store"); }
                    else { self.lower_value_as(code, arg, &Ty::scalar(TyKind::Int64)); code.mem(0x37, 3, 0, "i64.store"); }
                }
                code.i32c(fmt_addr as i32); code.i32c(args.len().saturating_sub(1) as i32); code.i32c(VARARG_BASE as i32); code.call(0); code.i32c(0);
            }
            "setTimer" | "setTimerCyclic" => {
                let timer_id = args.first().and_then(|a| if let ExprKind::Ident { sym: Some(id), .. } = a.kind { self.sema.var(id).and_then(|v| v.timer_id) } else { None }).unwrap_or(0);
                code.i32c(timer_id as i32); if let Some(ms) = args.get(1) { self.lower_value_as(code, ms, &default_ty()); } else { code.i32c(0); } code.call(if name == "setTimer" { 1 } else { 2 }); code.i32c(0);
            }
            "cancelTimer" => { code.i32c(0); code.call(3); code.i32c(0); }
            "output" => { if let Some(arg) = args.first() { if let Some(addr) = self.aggregate_addr(arg) { code.i32c(addr as i32); code.call(4); } } code.i32c(0); }
            "timeNow" => { code.call(5); }
            "random" => { if let Some(a) = args.first() { self.lower_value_as(code, a, &default_ty()); } else { code.i32c(0); } code.call(6); }
            "pow" => { for a in args.iter().take(2) { self.lower_value_as(code, a, &Ty::scalar(TyKind::Double)); } code.call(8); }
            "sqrt" | "sin" | "cos" | "floor" | "ceil" => {
                if let Some(a) = args.first() {
                    self.lower_value_as(code, a, &Ty::scalar(TyKind::Double));
                } else {
                    code.f64c(0.0);
                }
                match name.as_str() {
                    "sqrt" => code.op(0x9f, "f64.sqrt"),
                    "floor" => code.op(0x9c, "f64.floor"),
                    "ceil" => code.op(0x9b, "f64.ceil"),
                    "sin" => code.call(9),
                    _ => code.call(10),
                }
            }
            "canOnline" => { code.call(10); code.i32c(0); }
            "canOffline" => { code.call(11); code.i32c(0); }
            "stopMeasurement" => { code.call(12); code.i32c(0); }
            "elCount" => { code.i32c(args.first().and_then(|a| a.ty.as_ref()).and_then(|t| t.len).unwrap_or(0) as i32); }
            _ => code.i32c(0),
        }
    }

    fn aggregate_addr(&self, expr: &Expr) -> Option<u32> {
        match &expr.kind {
            ExprKind::Ident { sym: Some(id), .. } => self.global_addr.get(id).or_else(|| self.static_addr.get(id)).copied(),
            ExprKind::This => None,
            _ => None,
        }
    }

    fn member_addr(&mut self, _code: &mut Code, base: &Expr, name: &str) -> Option<(u32, Ty)> {
        let base_addr = match &base.kind {
            ExprKind::This => return None,
            ExprKind::Ident { sym: Some(id), .. } => self.global_addr.get(id).or_else(|| self.static_addr.get(id)).copied(),
            _ => None,
        }?;
        let (off, ty) = (self.member_offset(name)?, self.member_ty(name));
        Some((base_addr + off, ty))
    }

    fn member_offset(&self, name: &str) -> Option<u32> { match name { "id" => Some(0), "dlc" => Some(4), "can" => Some(16), "dir" => Some(20), "rtr" => Some(24), "time" => Some(28), "byte" | "word" | "dword" | "long" | "qword" => Some(8), _ => None } }
    fn member_ty(&self, name: &str) -> Ty { Ty::scalar(match name { "id" => TyKind::Dword, "byte" => TyKind::Byte, "word" => TyKind::Word, "qword" => TyKind::Qword, _ => TyKind::Long }) }

    fn store_mem(&self, code: &mut Code, ty: &Ty) {
        match ty.kind {
            TyKind::Char | TyKind::Byte => code.mem(0x3a, 0, 0, "i32.store8"),
            TyKind::Int | TyKind::Word => code.mem(0x3b, 1, 0, "i32.store16"),
            TyKind::Int64 | TyKind::Qword => code.mem(0x37, 3, 0, "i64.store"),
            TyKind::Float | TyKind::Double => code.mem(0x39, 3, 0, "f64.store"),
            _ => code.mem(0x36, 2, 0, "i32.store"),
        }
    }

    fn assemble(&self) -> Vec<u8> {
        let mut out = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

        // type section — deduplicated signatures
        let mut types: Vec<(Vec<u8>, Option<u8>)> = Vec::new();
        let type_of = |params: Vec<u8>, ret: Option<u8>, types: &mut Vec<(Vec<u8>, Option<u8>)>| -> u32 {
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

    #[test]
    fn lowers_event_body_and_timer_call() {
        let mut bag = DiagBag::new();
        let mut unit = capl_parse::parse(
            "variables { msTimer t; } on start { write(\"ready\"); setTimer(t, 10); }",
            "t.can",
            &mut bag,
        );
        let sema = capl_sema::analyze(&mut unit, &mut bag);
        assert!(!bag.has_errors(), "{:?}", bag.items());
        let art = WasmBackend::new(&sema).emit(&unit, &mut bag);
        assert!(art.wat.contains("on_start"));
        assert!(art.wat.contains("call 0"));
        assert!(art.wat.contains("call 1"));
    }
}
