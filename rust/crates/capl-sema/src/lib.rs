//! `capl-sema` — the semantic passes.
//!
//! DMD runs `semantic1` (declarations), `semantic2` (initialisers) and
//! `semantic3` (bodies). We do the same with two passes, because CAPL has no
//! templates: pass 1 publishes every global declaration so order does not
//! matter, pass 2 walks the bodies and decorates the tree.
//!
//! Symbols live in one flat arena; AST nodes only store a [`SymId`], which
//! keeps the tree `Clone + Serialize` and free of lifetimes.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use capl_ast::*;
use capl_diag::{DiagBag, Loc, Stage};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    Global,
    Local,
    Param,
}

#[derive(Clone, Debug)]
pub struct VarSym {
    pub name: String,
    pub ty: Ty,
    pub storage: Storage,
    pub timer_id: Option<u32>,
    pub loc: Loc,
    /// assigned by the backend
    pub addr: Option<u32>,
    pub slot: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct FuncSym {
    pub name: String,
    pub ret: Ty,
    pub params: Vec<Ty>,
    pub index: usize,
}

#[derive(Clone, Debug)]
pub enum Sym {
    Var(VarSym),
    Func(FuncSym),
}

#[derive(Debug, Default)]
pub struct SemaResult {
    pub syms: Vec<Sym>,
    pub globals: Vec<SymId>,
    pub timers: Vec<SymId>,
    pub funcs: Vec<SymId>,
    /// locals per function/handler export name, in declaration order
    pub locals: HashMap<String, Vec<SymId>>,
}

impl SemaResult {
    pub fn var(&self, id: SymId) -> Option<&VarSym> {
        match self.syms.get(id as usize) {
            Some(Sym::Var(v)) => Some(v),
            _ => None,
        }
    }
    pub fn func(&self, id: SymId) -> Option<&FuncSym> {
        match self.syms.get(id as usize) {
            Some(Sym::Func(f)) => Some(f),
            _ => None,
        }
    }
}

/// `name -> (ret, arity, variadic)`
pub fn builtin(name: &str) -> Option<(TyKind, usize, bool)> {
    Some(match name {
        "write" => (TyKind::Void, 1, true),
        "setTimer" | "setTimerCyclic" => (TyKind::Void, 2, false),
        "cancelTimer" => (TyKind::Void, 1, false),
        "output" => (TyKind::Void, 1, false),
        "timeNow" => (TyKind::Long, 0, false),
        "random" => (TyKind::Long, 1, false),
        "elCount" => (TyKind::Long, 1, false),
        "strlen" => (TyKind::Long, 1, false),
        "abs" | "min" | "max" => (TyKind::Double, 1, true),
        "sqrt" | "sin" | "cos" | "floor" | "ceil" => (TyKind::Double, 1, false),
        "pow" => (TyKind::Double, 2, false),
        "canOnline" | "canOffline" | "stopMeasurement" => (TyKind::Void, 0, false),
        _ => return None,
    })
}

/// Byte offsets of the fields of a CAPL message object.
pub const MSG_FIELDS: &[(&str, u32, TyKind)] = &[
    ("id", 0, TyKind::Dword),
    ("dlc", 4, TyKind::Long),
    ("can", 16, TyKind::Long),
    ("dir", 20, TyKind::Long),
    ("rtr", 24, TyKind::Long),
    ("time", 28, TyKind::Long),
];
pub const MSG_DATA_OFFSET: u32 = 8;
pub const MSG_SIZE: u32 = 32;

/// `byte(n)` / `word(n)` / `dword(n)` accessors.
pub fn msg_accessor(name: &str) -> Option<(u32, TyKind)> {
    Some(match name {
        "byte" => (1, TyKind::Byte),
        "word" => (2, TyKind::Word),
        "dword" | "long" => (4, TyKind::Dword),
        "qword" => (8, TyKind::Qword),
        _ => return None,
    })
}

#[derive(Default)]
struct Scope {
    names: HashMap<String, SymId>,
}

pub struct Sema<'d> {
    diags: &'d mut DiagBag,
    out: SemaResult,
    scopes: Vec<Scope>,
    cur_ret: Ty,
    cur_this: Option<Ty>,
    cur_fn: String,
    loop_depth: u32,
    switch_depth: u32,
    timer_counter: u32,
}

pub fn analyze(unit: &mut Unit, diags: &mut DiagBag) -> SemaResult {
    Sema {
        diags,
        out: SemaResult::default(),
        scopes: vec![Scope::default()],
        cur_ret: Ty::scalar(TyKind::Void),
        cur_this: None,
        cur_fn: String::new(),
        loop_depth: 0,
        switch_depth: 0,
        timer_counter: 0,
    }
    .run(unit)
}

impl<'d> Sema<'d> {
    fn declare(&mut self, name: &str, sym: Sym, loc: Loc) -> SymId {
        let id = self.out.syms.len() as SymId;
        self.out.syms.push(sym);
        let scope = self.scopes.last_mut().expect("a scope is always open");
        if scope.names.insert(name.to_string(), id).is_some() {
            self.diags.error(Stage::Sema, loc, format!("'{name}' is already declared in this scope"));
        }
        id
    }

    fn lookup(&self, name: &str) -> Option<SymId> {
        self.scopes.iter().rev().find_map(|s| s.names.get(name).copied())
    }

    fn run(mut self, unit: &mut Unit) -> SemaResult {
        // ── pass 1: every global declaration becomes visible ──
        let mut funcs: Vec<usize> = Vec::new();
        for (i, decl) in unit.decls.iter_mut().enumerate() {
            match decl {
                Decl::Variables { decls, .. } => {
                    for v in decls.iter_mut() {
                        Self::declare_var(&mut self, v, Storage::Global);
                    }
                }
                Decl::Var(v) => Self::declare_var(&mut self, v, Storage::Global),
                Decl::Func(f) => {
                    let index = self.out.funcs.len();
                    let sym = Sym::Func(FuncSym {
                        name: f.name.clone(),
                        ret: f.ret_ty.clone(),
                        params: f.params.iter().map(|p| p.decl_ty.clone()).collect(),
                        index,
                    });
                    if builtin(&f.name).is_some() {
                        self.diags.error(Stage::Sema, f.loc, format!("'{}' shadows a CAPL built-in", f.name));
                    }
                    let id = self.declare(&f.name.clone(), sym, f.loc);
                    self.out.funcs.push(id);
                    funcs.push(i);
                }
                Decl::Handler(_) | Decl::Includes { .. } => {}
            }
        }

        // ── pass 2: bodies ──
        for decl in unit.decls.iter_mut() {
            match decl {
                Decl::Func(f) => {
                    self.cur_ret = f.ret_ty.clone();
                    self.cur_this = None;
                    self.cur_fn = f.name.clone();
                    self.scopes.push(Scope::default());
                    for p in f.params.iter_mut() {
                        let sym = Sym::Var(VarSym {
                            name: p.name.clone(),
                            ty: p.decl_ty.clone(),
                            storage: Storage::Param,
                            timer_id: None,
                            loc: p.loc,
                            addr: None,
                            slot: None,
                        });
                        p.sym = Some(self.declare(&p.name.clone(), sym, p.loc));
                    }
                    let mut body = std::mem::replace(&mut f.body, Stmt { kind: StmtKind::Empty, loc: f.loc });
                    self.stmt(&mut body);
                    f.body = body;
                    self.scopes.pop();
                }
                Decl::Handler(h) => {
                    self.cur_ret = Ty::scalar(TyKind::Void);
                    self.cur_this = match &h.event {
                        EventSpec::Message { .. } => Some(Ty::scalar(TyKind::Message)),
                        EventSpec::Key { .. } => Some(Ty::scalar(TyKind::Dword)),
                        EventSpec::SysVar { .. } | EventSpec::EnvVar { .. } => Some(Ty::scalar(TyKind::Long)),
                        _ => None,
                    };
                    self.cur_fn = h.export_name.clone();
                    self.scopes.push(Scope::default());
                    let mut body = std::mem::replace(&mut h.body, Stmt { kind: StmtKind::Empty, loc: h.loc });
                    self.stmt(&mut body);
                    h.body = body;
                    self.scopes.pop();
                }
                _ => {}
            }
        }
        self.out
    }

    fn declare_var(&mut self, v: &mut VarDecl, storage: Storage) {
        let mut ty = v.decl_ty.clone();
        if v.is_array {
            let len = match (v.dims.first(), &v.init) {
                (Some(e), _) => self.const_fold(e).unwrap_or(0).max(0) as u32,
                (None, Some(Expr { kind: ExprKind::InitList { items }, .. })) => items.len() as u32,
                (None, Some(Expr { kind: ExprKind::StrLit { value }, .. })) => value.len() as u32 + 1,
                _ => 0,
            };
            if len == 0 {
                self.diags.error(Stage::Sema, v.loc, format!("cannot deduce the length of array '{}'", v.name));
            }
            ty = Ty::array(v.decl_ty.clone(), len.max(1));
        }
        let timer_id = if matches!(ty.kind, TyKind::Timer | TyKind::MsTimer) {
            let id = self.timer_counter;
            self.timer_counter += 1;
            Some(id)
        } else {
            None
        };
        let sym = Sym::Var(VarSym {
            name: v.name.clone(),
            ty,
            storage,
            timer_id,
            loc: v.loc,
            addr: None,
            slot: None,
        });
        let id = self.declare(&v.name.clone(), sym, v.loc);
        v.sym = Some(id);
        match storage {
            Storage::Global => self.out.globals.push(id),
            _ => self.out.locals.entry(self.cur_fn.clone()).or_default().push(id),
        }
        if timer_id.is_some() {
            self.out.timers.push(id);
        }
        if let Some(init) = &mut v.init.clone() {
            self.expr(init);
        }
    }

    fn stmt(&mut self, s: &mut Stmt) {
        match &mut s.kind {
            StmtKind::Block { stmts } => {
                self.scopes.push(Scope::default());
                for st in stmts.iter_mut() {
                    self.stmt(st);
                }
                self.scopes.pop();
            }
            StmtKind::Expr { expr } => {
                self.expr(expr);
            }
            StmtKind::Decl { decls } => {
                for d in decls.iter_mut() {
                    self.declare_var(d, Storage::Local);
                }
            }
            StmtKind::If { cond, then, otherwise } => {
                self.expr(cond);
                self.stmt(then);
                if let Some(o) = otherwise {
                    self.stmt(o);
                }
            }
            StmtKind::While { cond, body } => {
                self.expr(cond);
                self.loop_depth += 1;
                self.stmt(body);
                self.loop_depth -= 1;
            }
            StmtKind::DoWhile { body, cond } => {
                self.loop_depth += 1;
                self.stmt(body);
                self.loop_depth -= 1;
                self.expr(cond);
            }
            StmtKind::For { init, cond, step, body } => {
                self.scopes.push(Scope::default());
                if let Some(i) = init {
                    self.stmt(i);
                }
                if let Some(c) = cond {
                    self.expr(c);
                }
                if let Some(st) = step {
                    self.expr(st);
                }
                self.loop_depth += 1;
                self.stmt(body);
                self.loop_depth -= 1;
                self.scopes.pop();
            }
            StmtKind::Switch { disc, clauses } => {
                self.expr(disc);
                self.switch_depth += 1;
                let mut seen: Vec<i64> = Vec::new();
                let mut defaults = 0;
                for c in clauses.iter_mut() {
                    match &c.test {
                        Some(t) => match self.const_fold(t) {
                            Some(v) if seen.contains(&v) => {
                                self.diags.error(Stage::Sema, c.loc, format!("duplicate case label {v}"))
                            }
                            Some(v) => seen.push(v),
                            None => self.diags.error(Stage::Sema, c.loc, "case label must be constant"),
                        },
                        None => defaults += 1,
                    }
                    for st in c.body.iter_mut() {
                        self.stmt(st);
                    }
                }
                if defaults > 1 {
                    self.diags.error(Stage::Sema, s.loc, "more than one 'default' label");
                }
                self.switch_depth -= 1;
            }
            StmtKind::Break => {
                if self.loop_depth == 0 && self.switch_depth == 0 {
                    self.diags.error(Stage::Sema, s.loc, "'break' outside of a loop or switch");
                }
            }
            StmtKind::Continue => {
                if self.loop_depth == 0 {
                    self.diags.error(Stage::Sema, s.loc, "'continue' outside of a loop");
                }
            }
            StmtKind::Return { expr } => {
                let void = self.cur_ret.kind == TyKind::Void;
                match expr {
                    Some(e) => {
                        self.expr(e);
                        if void {
                            self.diags.error(Stage::Sema, s.loc, "cannot return a value from a void function");
                        }
                    }
                    None if !void => {
                        self.diags.error(Stage::Sema, s.loc, "function must return a value");
                    }
                    None => {}
                }
            }
            StmtKind::Empty => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) -> Ty {
        let ty = match &mut e.kind {
            ExprKind::IntLit { .. } | ExprKind::CharLit { .. } => Ty::scalar(TyKind::Long),
            ExprKind::FloatLit { .. } => Ty::scalar(TyKind::Double),
            ExprKind::StrLit { value } => Ty::array(Ty::scalar(TyKind::Char), value.len() as u32 + 1),
            ExprKind::This => self.cur_this.clone().unwrap_or_else(|| {
                self.diags.error(Stage::Sema, e.loc, "'this' is only valid inside an event handler");
                Ty::scalar(TyKind::Error)
            }),
            ExprKind::Ident { name, sym } => match self.lookup(name) {
                Some(id) => {
                    *sym = Some(id);
                    match &self.out.syms[id as usize] {
                        Sym::Var(v) => v.ty.clone(),
                        Sym::Func(_) => Ty::scalar(TyKind::Error),
                    }
                }
                None => {
                    if builtin(name).is_none() {
                        self.diags.error(Stage::Sema, e.loc, format!("undefined identifier '{name}'"));
                    }
                    Ty::scalar(TyKind::Error)
                }
            },
            ExprKind::Unary { operand, op, .. } => {
                let t = self.expr(operand);
                if op == "!" { Ty::scalar(TyKind::Long) } else { t }
            }
            ExprKind::Binary { left, right, op } => {
                let (l, r) = (self.expr(left), self.expr(right));
                if matches!(op.as_str(), "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||") {
                    Ty::scalar(TyKind::Long)
                } else {
                    usual_conversion(&l, &r)
                }
            }
            ExprKind::Assign { target, value, .. } => {
                let t = self.expr(target);
                self.expr(value);
                t
            }
            ExprKind::Cond { cond, then, otherwise } => {
                self.expr(cond);
                let (a, b) = (self.expr(then), self.expr(otherwise));
                usual_conversion(&a, &b)
            }
            ExprKind::Index { base, index } => {
                let b = self.expr(base);
                self.expr(index);
                b.elem.map(|e| *e).unwrap_or(Ty::scalar(TyKind::Error))
            }
            ExprKind::Member { base, name } => {
                let b = self.expr(base);
                if b.kind != TyKind::Message {
                    self.diags.error(Stage::Sema, e.loc, format!("type has no member '{name}'"));
                    Ty::scalar(TyKind::Error)
                } else {
                    MSG_FIELDS
                        .iter()
                        .find(|(f, _, _)| f == name)
                        .map(|(_, _, k)| Ty::scalar(k.clone()))
                        .unwrap_or(Ty::scalar(TyKind::Error))
                }
            }
            ExprKind::Call { callee, args, sym } => self.call(callee, args, sym, e.loc),
            ExprKind::InitList { items } => {
                for i in items.iter_mut() {
                    self.expr(i);
                }
                Ty::scalar(TyKind::Error)
            }
        };
        e.ty = Some(ty.clone());
        ty
    }

    fn call(&mut self, callee: &mut Expr, args: &mut [Expr], sym: &mut Option<SymId>, loc: Loc) -> Ty {
        if let ExprKind::Member { base, name } = &mut callee.kind {
            let b = self.expr(base);
            for a in args.iter_mut() {
                self.expr(a);
            }
            return match (b.kind, msg_accessor(name)) {
                (TyKind::Message, Some((_, k))) => Ty::scalar(k),
                _ => Ty::scalar(TyKind::Error),
            };
        }
        let ExprKind::Ident { name, .. } = &callee.kind else {
            self.diags.error(Stage::Sema, loc, "call target is not a function");
            return Ty::scalar(TyKind::Error);
        };
        let name = name.clone();
        let arg_tys: Vec<Ty> = args.iter_mut().map(|a| self.expr(a)).collect();

        if let Some(id) = self.lookup(&name) {
            if let Sym::Func(f) = self.out.syms[id as usize].clone() {
                *sym = Some(id);
                if arg_tys.len() != f.params.len() {
                    self.diags.error(
                        Stage::Sema,
                        loc,
                        format!("'{name}' expects {} argument(s), got {}", f.params.len(), arg_tys.len()),
                    );
                }
                return f.ret;
            }
        }
        match builtin(&name) {
            Some((ret, arity, variadic)) => {
                if (!variadic && arg_tys.len() != arity) || (variadic && arg_tys.len() < arity) {
                    self.diags.error(Stage::Sema, loc, format!("'{name}' expects {arity} argument(s)"));
                }
                Ty::scalar(ret)
            }
            None => {
                self.diags.error(Stage::Sema, loc, format!("call to undeclared function '{name}'"));
                Ty::scalar(TyKind::Error)
            }
        }
    }

    fn const_fold(&self, e: &Expr) -> Option<i64> {
        match &e.kind {
            ExprKind::IntLit { value } | ExprKind::CharLit { value } => Some(*value),
            ExprKind::Unary { op, operand, .. } => {
                let v = self.const_fold(operand)?;
                Some(match op.as_str() {
                    "-" => -v,
                    "+" => v,
                    "!" => (v == 0) as i64,
                    "~" => !v,
                    _ => return None,
                })
            }
            ExprKind::Binary { op, left, right } => {
                let (l, r) = (self.const_fold(left)?, self.const_fold(right)?);
                Some(match op.as_str() {
                    "+" => l + r,
                    "-" => l - r,
                    "*" => l * r,
                    "/" if r != 0 => l / r,
                    "%" if r != 0 => l % r,
                    "<<" => l << r,
                    ">>" => l >> r,
                    "&" => l & r,
                    "|" => l | r,
                    "^" => l ^ r,
                    _ => return None,
                })
            }
            _ => None,
        }
    }
}

pub fn usual_conversion(a: &Ty, b: &Ty) -> Ty {
    if a.is_float() || b.is_float() {
        Ty::scalar(TyKind::Double)
    } else if a.is_64() || b.is_64() {
        Ty::scalar(if a.kind == TyKind::Qword || b.kind == TyKind::Qword { TyKind::Qword } else { TyKind::Int64 })
    } else if a.kind == TyKind::Dword || b.kind == TyKind::Dword {
        Ty::scalar(TyKind::Dword)
    } else {
        Ty::scalar(TyKind::Long)
    }
}
