//! `capl-sema` — the semantic passes.
//!
//! The semantic pipeline uses separate declaration and body passes. We use two
//! passes because CAPL has no
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
pub struct StructLayout {
    pub name: String,
    pub members: Vec<StructMember>,
    pub size: u32,
}

#[derive(Clone, Debug)]
pub struct EnumVariant {
    pub name: String,
    pub value: i64,
}

#[derive(Clone, Debug)]
pub struct EnumLayout {
    pub name: String,
    pub variants: Vec<EnumVariant>,
    pub size: u32,
}

#[derive(Clone, Debug)]
pub enum Sym {
    Var(VarSym),
    Func(FuncSym),
    Enum(EnumLayout),
    EnumConst { value: i64, ty: Ty },
    Struct(StructLayout),
}

#[derive(Debug, Default)]
pub struct SemaResult {
    pub syms: Vec<Sym>,
    pub globals: Vec<SymId>,
    pub timers: Vec<SymId>,
    pub funcs: Vec<SymId>,
    /// locals per function/handler export name, in declaration order
    pub locals: HashMap<String, Vec<SymId>>,
    pub structs: HashMap<String, StructLayout>,
    pub enums: HashMap<String, EnumLayout>,
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

    pub fn enum_const(&self, id: SymId) -> Option<i64> {
        match self.syms.get(id as usize) {
            Some(Sym::EnumConst { value, .. }) => Some(*value),
            _ => None,
        }
    }

    pub fn enum_const_ty(&self, id: SymId) -> Option<Ty> {
        match self.syms.get(id as usize) {
            Some(Sym::EnumConst { ty, .. }) => Some(ty.clone()),
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

    fn resolve_type(&self, ty: &Ty) -> Ty {
        let mut resolved = ty.clone();
        match ty.kind {
            TyKind::Struct => {
                if let Some(name) = &ty.name {
                    resolved.size = self.out.structs.get(name).map(|s| s.size);
                }
            }
            TyKind::Enum => {
                if let Some(name) = &ty.name {
                    resolved.size = self.out.enums.get(name).map(|e| e.size);
                }
            }
            _ => {}
        }
        resolved
    }

    fn validate_type(&mut self, ty: &Ty, loc: Loc) {
        match ty.kind {
            TyKind::Struct => {
                if let Some(name) = &ty.name {
                    if !self.out.structs.contains_key(name) {
                        self.diags.error(Stage::Sema, loc, format!("unknown struct type '{name}'"));
                    }
                }
            }
            TyKind::Enum => {
                if let Some(name) = &ty.name {
                    if !self.out.enums.contains_key(name) {
                        self.diags.error(Stage::Sema, loc, format!("unknown enum type '{name}'"));
                    }
                }
            }
            TyKind::Array => {
                if let Some(elem) = &ty.elem { self.validate_type(elem, loc); }
            }
            _ => {}
        }
    }

    fn check_enum_compat(&mut self, expected: &Ty, actual: &Ty, loc: Loc) {
        if expected.kind == TyKind::Enum && actual.kind == TyKind::Enum {
            if let (Some(expected), Some(actual)) = (&expected.name, &actual.name) {
                if expected != actual {
                    self.diags.error(Stage::Sema, loc, format!("incompatible enum types '{expected}' and '{actual}'"));
                }
            }
        }
    }

    fn field_type(&self, base: &Ty, name: &str) -> Option<Ty> {
        let struct_name = base.name.as_deref()?;
        self.out.structs.get(struct_name).and_then(|layout| layout.members.iter().find(|m| m.name == name).map(|m| m.ty.clone()))
    }

    fn run(mut self, unit: &mut Unit) -> SemaResult {
        // Publish enum names and constants before other declarations so an enum
        // can be used by a variable or function declared earlier in the file.
        for decl in unit.decls.iter_mut() {
            let Decl::Enum(e) = decl else { continue };
            if !e.name.is_empty() && self.out.enums.contains_key(&e.name) {
                self.diags.error(Stage::Sema, e.loc, format!("enum '{}' is already declared", e.name));
                continue;
            }
            let enum_ty = Ty {
                kind: TyKind::Enum,
                name: (!e.name.is_empty()).then(|| e.name.clone()),
                size: Some(4),
                elem: None,
                len: None,
                msg_id: None,
            };
            let mut next = 0;
            let mut variants = Vec::with_capacity(e.items.len());
            for item in &mut e.items {
                let value = match &item.init {
                    Some(expr) => match self.const_fold(expr) {
                        Some(value) => value,
                        None => {
                            self.diags.error(Stage::Sema, item.loc, "enum initializer must be an integer constant");
                            next
                        }
                    },
                    None => next,
                };
                item.resolved = value;
                item.ty = Some(enum_ty.clone());
                variants.push(EnumVariant { name: item.name.clone(), value });
                let id = self.declare(&item.name, Sym::EnumConst { value, ty: enum_ty.clone() }, item.loc);
                if !e.name.is_empty() {
                    let qualified = format!("{}::{}", e.name, item.name);
                    let scope = self.scopes.last_mut().expect("a scope is always open");
                    scope.names.entry(qualified).or_insert(id);
                }
                next = value.saturating_add(1);
            }
            if !e.name.is_empty() {
                let layout = EnumLayout { name: e.name.clone(), variants, size: 4 };
                self.out.enums.insert(e.name.clone(), layout.clone());
                self.declare(&e.name, Sym::Enum(layout), e.loc);
            }
        }
        for decl in unit.decls.iter_mut() {
            let Decl::Enum(e) = decl else { continue };
            for item in &mut e.items {
                if let Some(init) = &mut item.init {
                    self.expr(init);
                }
            }
        }

        // ── pass 1: every global declaration becomes visible ──
        for decl in unit.decls.iter_mut() {
            match decl {
                Decl::Struct(s) => {
                    if self.out.structs.contains_key(&s.name) {
                        self.diags.error(Stage::Sema, s.loc, format!("struct '{}' is already declared", s.name));
                        continue;
                    }
                    let mut offset = 0;
                    let mut members = Vec::new();
                    for field in &s.fields {
                        self.validate_type(&field.decl_ty, field.loc);
                        let mut ty = self.resolve_type(&field.decl_ty);
                        for dim in field.dims.iter().rev() {
                            let len = self.const_fold(dim).unwrap_or(0).max(0) as u32;
                            if len == 0 { self.diags.error(Stage::Sema, field.loc, "struct array field requires a positive constant length"); }
                            ty = Ty::array(ty, len.max(1));
                        }
                        let size = ty.size_of();
                        if size == 0 { self.diags.error(Stage::Sema, field.loc, format!("unknown or recursive type in struct field '{}'", field.name)); }
                        members.push(StructMember { name: field.name.clone(), ty, offset });
                        offset = offset.saturating_add(size.max(1));
                    }
                    let layout = StructLayout { name: s.name.clone(), members, size: offset.max(1) };
                    s.ty = Some(Ty { kind: TyKind::Struct, name: Some(s.name.clone()), size: Some(layout.size), elem: None, len: None, msg_id: None });
                    self.out.structs.insert(s.name.clone(), layout.clone());
                    self.declare(&s.name, Sym::Struct(layout), s.loc);
                }
                Decl::Enum(_) => {}
                Decl::Variables { decls, .. } => {
                    for v in decls.iter_mut() {
                        Self::declare_var(&mut self, v, Storage::Global);
                    }
                }
                Decl::Var(v) => Self::declare_var(&mut self, v, Storage::Global),
                Decl::Func(f) => {
                    self.validate_type(&f.ret_ty, f.loc);
                    f.ret_ty = self.resolve_type(&f.ret_ty);
                    for p in &mut f.params {
                        self.validate_type(&p.decl_ty, p.loc);
                        p.decl_ty = self.resolve_type(&p.decl_ty);
                    }
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
                }
                Decl::Handler(_) | Decl::Includes { .. } => {}
            }
        }

        // Publish every global before resolving any global initializer. This
        // keeps references independent of declaration order and decorates the
        // AST that later compiler stages consume.
        for decl in unit.decls.iter_mut() {
            let vars: &mut [VarDecl] = match decl {
                Decl::Variables { decls, .. } => decls,
                Decl::Var(v) => std::slice::from_mut(v),
                _ => &mut [],
            };
            for v in vars {
                self.analyze_var_init(v);
            }
        }

        // ── pass 2: bodies ──
        for decl in unit.decls.iter_mut() {
            match decl {
                Decl::Func(f) => {
                    f.ret_ty = self.resolve_type(&f.ret_ty);
                    self.cur_ret = f.ret_ty.clone();
                    self.cur_this = None;
                    self.cur_fn = f.name.clone();
                    self.scopes.push(Scope::default());
                    for p in f.params.iter_mut() {
                        let sym = Sym::Var(VarSym {
                            name: p.name.clone(),
                            ty: self.resolve_type(&p.decl_ty),
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
        self.validate_type(&v.decl_ty, v.loc);
        let mut ty = self.resolve_type(&v.decl_ty);
        if v.is_array {
            if v.dims.is_empty() {
                let len = match &v.init {
                    Some(Expr { kind: ExprKind::InitList { items }, .. }) => items.len() as u32,
                    Some(Expr { kind: ExprKind::StrLit { value }, .. }) => value.len() as u32 + 1,
                    _ => 0,
                };
                if len == 0 {
                    self.diags.error(Stage::Sema, v.loc, format!("cannot deduce the length of array '{}'", v.name));
                }
                ty = Ty::array(ty, len.max(1));
            } else {
                for dim in v.dims.iter().rev() {
                    let len = self.const_fold(dim).unwrap_or(0).max(0) as u32;
                    if len == 0 {
                        self.diags.error(Stage::Sema, v.loc, format!("array '{}' requires positive constant dimensions", v.name));
                    }
                    ty = Ty::array(ty, len.max(1));
                }
            }
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
    }

    fn analyze_var_init(&mut self, v: &mut VarDecl) {
        let Some(init) = &mut v.init else { return };
        let expected = v
            .sym
            .and_then(|id| self.out.var(id).map(|sym| sym.ty.clone()))
            .unwrap_or_else(|| self.resolve_type(&v.decl_ty));
        let actual = self.expr(init);
        self.check_enum_compat(&expected, &actual, v.loc);
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
                    self.analyze_var_init(d);
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
                        let value_ty = self.expr(e);
                        self.check_enum_compat(&self.cur_ret.clone(), &value_ty, s.loc);
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
                        Sym::EnumConst { ty, .. } => ty.clone(),
                        Sym::Enum(_) => Ty::scalar(TyKind::Error),
                        Sym::Struct(layout) => Ty { kind: TyKind::Struct, name: Some(layout.name.clone()), size: Some(layout.size), elem: None, len: None, msg_id: None },
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
                let v = self.expr(value);
                self.check_enum_compat(&t, &v, e.loc);
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
                if b.kind == TyKind::Struct {
                    self.field_type(&b, name).unwrap_or_else(|| {
                        self.diags.error(Stage::Sema, e.loc, format!("struct has no field '{name}'"));
                        Ty::scalar(TyKind::Error)
                    })
                } else if b.kind != TyKind::Message {
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
                for (arg, param) in arg_tys.iter().zip(&f.params) {
                    self.check_enum_compat(param, arg, loc);
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
            ExprKind::Ident { name, sym } => {
                let id = sym.or_else(|| self.lookup(name))?;
                self.out.enum_const(id)
            }
            ExprKind::Cond { cond, then, otherwise } => {
                if self.const_fold(cond)? != 0 { self.const_fold(then) } else { self.const_fold(otherwise) }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_declarations_publish_nominal_types_before_variables() {
        let mut diags = DiagBag::new();
        let mut unit = capl_parse::parse(
            "variables { enum Mode current = Mode::RUNNING; } enum Mode { OFF, RUNNING = 4, FAULT } on start { current = Mode::FAULT; write(\"%d\", Mode::RUNNING); }",
            "enum.can",
            &mut diags,
        );
        let sema = analyze(&mut unit, &mut diags);
        assert!(!diags.has_errors(), "{:?}", diags.items());

        let mode = sema.enums.get("Mode").expect("enum layout");
        assert_eq!(mode.size, 4);
        assert_eq!(mode.variants.iter().map(|v| v.value).collect::<Vec<_>>(), vec![0, 4, 5]);

        let Decl::Variables { decls, .. } = &unit.decls[0] else { panic!("expected variables") };
        let current = sema.var(decls[0].sym.expect("variable symbol")).expect("variable");
        assert_eq!(current.ty.kind, TyKind::Enum);
        assert_eq!(current.ty.name.as_deref(), Some("Mode"));

        let Decl::Handler(handler) = unit.decls.last().expect("handler") else { panic!("expected handler") };
        let StmtKind::Block { stmts } = &handler.body.kind else { panic!("expected block") };
        let StmtKind::Expr { expr: Expr { kind: ExprKind::Assign { value, .. }, .. } } = &stmts[0].kind else { panic!("expected assignment") };
        assert_eq!(value.ty.as_ref().map(|ty| ty.kind.clone()), Some(TyKind::Enum));
    }

    #[test]
    fn variable_initializers_keep_symbol_bindings_for_globals_and_locals() {
        let mut diags = DiagBag::new();
        let mut unit = capl_parse::parse(
            "variables { int b = a; int a = 42; } on start { int local = a; }",
            "initializers.can",
            &mut diags,
        );
        let sema = analyze(&mut unit, &mut diags);
        assert!(!diags.has_errors(), "{:?}", diags.items());

        let Decl::Variables { decls, .. } = &unit.decls[0] else { panic!("expected variables") };
        let b_id = decls[0].sym.expect("b symbol");
        let a_id = decls[1].sym.expect("a symbol");
        let ExprKind::Ident { sym: b_init_sym, .. } = &decls[0].init.as_ref().expect("b initializer").kind else {
            panic!("expected global identifier initializer")
        };
        assert_eq!(*b_init_sym, Some(a_id));
        assert_ne!(b_id, a_id);

        let Decl::Handler(handler) = &unit.decls[1] else { panic!("expected handler") };
        let StmtKind::Block { stmts } = &handler.body.kind else { panic!("expected block") };
        let StmtKind::Decl { decls } = &stmts[0].kind else { panic!("expected local declaration") };
        let ExprKind::Ident { sym: local_init_sym, .. } = &decls[0].init.as_ref().expect("local initializer").kind else {
            panic!("expected local identifier initializer")
        };
        assert_eq!(*local_init_sym, Some(a_id));
        assert!(sema.var(a_id).is_some());
    }

    #[test]
    fn distinct_enum_values_are_not_assignable_to_each_other() {
        let mut diags = DiagBag::new();
        let mut unit = capl_parse::parse(
            "enum A { A0 } enum B { B0 } on start { enum A value = B0; value = B::B0; }",
            "enum-mismatch.can",
            &mut diags,
        );
        let _ = analyze(&mut unit, &mut diags);
        assert!(diags.items().iter().any(|d| d.message.contains("incompatible enum types")), "{:?}", diags.items());
    }
}
