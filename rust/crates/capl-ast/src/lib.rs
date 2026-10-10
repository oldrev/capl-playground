//! `capl-ast` — the tree the parser builds and the semantic pass decorates.
//!
//! Design notes:
//! * nodes are *data*, not behaviour — no `self.semantic()` methods here;
//! * every node carries a [`Loc`] so later passes can report precisely;
//! * expressions have an `Option<Ty>` slot that `capl-sema` fills in;
//! * the whole tree is `Serialize`, which is what powers "export AST as JSON".
#![forbid(unsafe_code)]

use capl_diag::Loc;

#[cfg(feature = "json")]
use serde::Serialize;

macro_rules! ast_node {
    ($(#[$m:meta])* pub struct $name:ident { $($(#[$fm:meta])* pub $f:ident : $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Debug)]
        #[cfg_attr(feature = "json", derive(Serialize))]
        pub struct $name { $($(#[$fm])* pub $f : $t),* }
    };
}

/* ── types ─────────────────────────────────────────────── */

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "json", derive(Serialize))]
#[cfg_attr(feature = "json", serde(rename_all = "lowercase"))]
pub enum TyKind {
    Void,
    Char,
    Byte,
    Int,
    Word,
    Long,
    Dword,
    Int64,
    Qword,
    Float,
    Double,
    Timer,
    MsTimer,
    Message,
    /// A named CAPL enumeration. Enum values use signed 32-bit storage.
    Enum,
    Array,
    Struct,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "json", derive(Serialize))]
pub struct Ty {
    pub kind: TyKind,
    pub name: Option<String>,
    pub size: Option<u32>,
    pub elem: Option<Box<Ty>>,
    pub len: Option<u32>,
    pub msg_id: Option<u32>,
}

impl Ty {
    pub const fn scalar(kind: TyKind) -> Self {
        Self { kind, name: None, size: None, elem: None, len: None, msg_id: None }
    }
    pub fn array(elem: Ty, len: u32) -> Self {
        Self { kind: TyKind::Array, name: None, size: None, elem: Some(Box::new(elem)), len: Some(len), msg_id: None }
    }
    pub fn named_struct(name: impl Into<String>) -> Self {
        Self { kind: TyKind::Struct, name: Some(name.into()), size: None, elem: None, len: None, msg_id: None }
    }
    pub fn named_enum(name: impl Into<String>) -> Self {
        Self { kind: TyKind::Enum, name: Some(name.into()), size: Some(4), elem: None, len: None, msg_id: None }
    }
    /// Storage width in bytes, following CAPL (int = 2, long = 4, float = 8).
    pub fn size_of(&self) -> u32 {
        match self.kind {
            TyKind::Char | TyKind::Byte => 1,
            TyKind::Int | TyKind::Word => 2,
            TyKind::Long | TyKind::Dword | TyKind::Timer | TyKind::MsTimer | TyKind::Enum => 4,
            TyKind::Int64 | TyKind::Qword | TyKind::Float | TyKind::Double => 8,
            TyKind::Message => 32,
            TyKind::Struct => self.size.unwrap_or(0),
            TyKind::Array => self.len.unwrap_or(0) * self.elem.as_ref().map_or(1, |e| e.size_of()),
            _ => 4,
        }
    }
    pub fn is_float(&self) -> bool {
        matches!(self.kind, TyKind::Float | TyKind::Double)
    }
    pub fn is_64(&self) -> bool {
        matches!(self.kind, TyKind::Int64 | TyKind::Qword)
    }
    pub fn is_unsigned(&self) -> bool {
        matches!(self.kind, TyKind::Byte | TyKind::Word | TyKind::Dword | TyKind::Qword)
    }
    pub fn is_scalar(&self) -> bool {
        !matches!(self.kind, TyKind::Void | TyKind::Array | TyKind::Message | TyKind::Struct | TyKind::Error)
    }
}

/* ── expressions ───────────────────────────────────────── */

#[derive(Clone, Debug)]
#[cfg_attr(feature = "json", derive(Serialize))]
#[cfg_attr(feature = "json", serde(tag = "kind"))]
pub enum ExprKind {
    IntLit { value: i64 },
    FloatLit { value: f64 },
    CharLit { value: i64 },
    StrLit { value: String },
    Ident { name: String, sym: Option<SymId> },
    This,
    Unary { op: String, operand: Box<Expr>, postfix: bool },
    Binary { op: String, left: Box<Expr>, right: Box<Expr> },
    Assign { op: String, target: Box<Expr>, value: Box<Expr> },
    Cond { cond: Box<Expr>, then: Box<Expr>, otherwise: Box<Expr> },
    Call { callee: Box<Expr>, args: Vec<Expr>, sym: Option<SymId> },
    Index { base: Box<Expr>, index: Box<Expr> },
    Member { base: Box<Expr>, name: String },
    InitList { items: Vec<Expr> },
}

ast_node! {
    pub struct Expr {
        pub kind: ExprKind,
        pub loc: Loc,
        /// filled in by `capl-sema`
        pub ty: Option<Ty>,
    }
}

impl Expr {
    pub fn new(kind: ExprKind, loc: Loc) -> Self {
        Self { kind, loc, ty: None }
    }
}

/* ── statements ────────────────────────────────────────── */

#[derive(Clone, Debug)]
#[cfg_attr(feature = "json", derive(Serialize))]
#[cfg_attr(feature = "json", serde(tag = "kind"))]
pub enum StmtKind {
    Block { stmts: Vec<Stmt> },
    Expr { expr: Expr },
    Decl { decls: Vec<VarDecl> },
    If { cond: Expr, then: Box<Stmt>, otherwise: Option<Box<Stmt>> },
    While { cond: Expr, body: Box<Stmt> },
    DoWhile { body: Box<Stmt>, cond: Expr },
    For { init: Option<Box<Stmt>>, cond: Option<Expr>, step: Option<Expr>, body: Box<Stmt> },
    Switch { disc: Expr, clauses: Vec<SwitchClause> },
    Break,
    Continue,
    Return { expr: Option<Expr> },
    Empty,
}

ast_node! {
    pub struct Stmt {
        pub kind: StmtKind,
        pub loc: Loc,
    }
}

ast_node! {
    pub struct SwitchClause {
        /// `None` is `default:`
        pub test: Option<Expr>,
        pub body: Vec<Stmt>,
        pub loc: Loc,
    }
}

/* ── declarations ──────────────────────────────────────── */

pub type SymId = u32;

ast_node! {
    pub struct VarDecl {
        pub name: String,
        pub decl_ty: Ty,
        /// every `[...]` group, outermost first; empty for scalars
        pub dims: Vec<Expr>,
        pub is_array: bool,
        pub is_const: bool,
        pub init: Option<Expr>,
        pub loc: Loc,
        pub sym: Option<SymId>,
    }
}

/* ── enums ──────────────────────────────────────────────── */

ast_node! {
    pub struct EnumItem {
        pub name: String,
        /// explicit `= expr`, folded into `resolved` by sema
        pub init: Option<Expr>,
        pub resolved: i64,
        /// The enum type assigned by `capl-sema`.
        pub ty: Option<Ty>,
        pub loc: Loc,
    }
}

ast_node! {
    pub struct EnumDecl {
        /// empty for an anonymous definition
        pub name: String,
        pub items: Vec<EnumItem>,
        pub loc: Loc,
    }
}

/* ── structs ────────────────────────────────────────────── */

ast_node! {
    pub struct StructField {
        pub name: String,
        pub decl_ty: Ty,
        pub dims: Vec<Expr>,
        pub loc: Loc,
    }
}

/// One laid-out field; `offset` is filled in by `capl-sema`.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "json", derive(Serialize))]
pub struct StructMember {
    pub name: String,
    pub ty: Ty,
    pub offset: u32,
}

ast_node! {
    pub struct StructDecl {
        /// synthetic `__anonN` when the definition is anonymous
        pub name: String,
        pub fields: Vec<StructField>,
        /// the laid-out type, filled in by sema
        pub ty: Option<Ty>,
        pub loc: Loc,
    }
}

ast_node! {
    pub struct Param {
        pub name: String,
        pub decl_ty: Ty,
        pub loc: Loc,
        pub sym: Option<SymId>,
    }
}

ast_node! {
    pub struct FuncDecl {
        pub name: String,
        pub ret_ty: Ty,
        pub params: Vec<Param>,
        pub body: Stmt,
        pub loc: Loc,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "json", derive(Serialize))]
#[cfg_attr(feature = "json", serde(tag = "event", rename_all = "camelCase"))]
pub enum EventSpec {
    Start,
    PreStart,
    StopMeasurement,
    PreStop,
    ErrorFrame,
    BusOff,
    Timer { name: String },
    Message { id: Option<u32>, name: Option<String>, channel: Option<u8>, wildcard: bool },
    Key { key: Option<char>, wildcard: bool },
    SysVar { name: String },
    EnvVar { name: String },
}

impl EventSpec {
    /// Export symbol used by every backend, e.g. `on_message_0x100`.
    pub fn mangle(&self) -> String {
        match self {
            EventSpec::Start => "on_start".into(),
            EventSpec::PreStart => "on_preStart".into(),
            EventSpec::StopMeasurement => "on_stopMeasurement".into(),
            EventSpec::PreStop => "on_preStop".into(),
            EventSpec::ErrorFrame => "on_errorFrame".into(),
            EventSpec::BusOff => "on_busOff".into(),
            EventSpec::Timer { name } => format!("on_timer_{name}"),
            EventSpec::Message { id, name, wildcard, .. } => {
                if *wildcard {
                    "on_message_any".into()
                } else if let Some(id) = id {
                    format!("on_message_0x{id:x}")
                } else {
                    format!("on_message_{}", name.clone().unwrap_or_else(|| "anon".into()))
                }
            }
            EventSpec::Key { key, wildcard } => {
                if *wildcard {
                    "on_key_any".into()
                } else {
                    format!("on_key_{}", key.map_or(0, |c| c as u32))
                }
            }
            EventSpec::SysVar { name } => format!("on_sysvar_{}", name.replace("::", "_")),
            EventSpec::EnvVar { name } => format!("on_envvar_{name}"),
        }
    }
}

ast_node! {
    pub struct HandlerDecl {
        pub event: EventSpec,
        pub body: Stmt,
        pub loc: Loc,
        pub export_name: String,
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "json", derive(Serialize))]
#[cfg_attr(feature = "json", serde(tag = "kind"))]
pub enum Decl {
    Enum(EnumDecl),
    Struct(StructDecl),
    Variables { decls: Vec<VarDecl>, loc: Loc },
    Includes { text: String, loc: Loc },
    Func(FuncDecl),
    Handler(HandlerDecl),
    Var(VarDecl),
}

ast_node! {
    pub struct Unit {
        pub name: String,
        pub decls: Vec<Decl>,
    }
}

#[cfg(feature = "json")]
impl Unit {
    /// `capl --emit=ast` and the playground's AST tab use this.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
    pub fn to_json_string(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/* ── visitor ───────────────────────────────────────────── */

/// Depth-first visitor; override only what you need.
pub trait Visitor {
    fn visit_unit(&mut self, u: &Unit) {
        for d in &u.decls {
            self.visit_decl(d);
        }
    }
    fn visit_decl(&mut self, d: &Decl) {
        match d {
            Decl::Enum(_) => {}
            Decl::Struct(_) => {}
            Decl::Func(f) => self.visit_stmt(&f.body),
            Decl::Handler(h) => self.visit_stmt(&h.body),
            Decl::Variables { decls, .. } => {
                for v in decls {
                    if let Some(i) = &v.init {
                        self.visit_expr(i);
                    }
                }
            }
            Decl::Var(v) => {
                if let Some(i) = &v.init {
                    self.visit_expr(i);
                }
            }
            Decl::Includes { .. } => {}
        }
    }
    fn visit_stmt(&mut self, s: &Stmt) {
        walk_stmt(self, s);
    }
    fn visit_expr(&mut self, e: &Expr) {
        walk_expr(self, e);
    }
}

pub fn walk_stmt<V: Visitor + ?Sized>(v: &mut V, s: &Stmt) {
    match &s.kind {
        StmtKind::Block { stmts } => stmts.iter().for_each(|s| v.visit_stmt(s)),
        StmtKind::Expr { expr } => v.visit_expr(expr),
        StmtKind::Decl { decls } => decls.iter().filter_map(|d| d.init.as_ref()).for_each(|e| v.visit_expr(e)),
        StmtKind::If { cond, then, otherwise } => {
            v.visit_expr(cond);
            v.visit_stmt(then);
            if let Some(o) = otherwise {
                v.visit_stmt(o);
            }
        }
        StmtKind::While { cond, body } => {
            v.visit_expr(cond);
            v.visit_stmt(body);
        }
        StmtKind::DoWhile { body, cond } => {
            v.visit_stmt(body);
            v.visit_expr(cond);
        }
        StmtKind::For { init, cond, step, body } => {
            if let Some(i) = init {
                v.visit_stmt(i);
            }
            if let Some(c) = cond {
                v.visit_expr(c);
            }
            if let Some(st) = step {
                v.visit_expr(st);
            }
            v.visit_stmt(body);
        }
        StmtKind::Switch { disc, clauses } => {
            v.visit_expr(disc);
            for c in clauses {
                if let Some(t) = &c.test {
                    v.visit_expr(t);
                }
                c.body.iter().for_each(|s| v.visit_stmt(s));
            }
        }
        StmtKind::Return { expr: Some(e) } => v.visit_expr(e),
        _ => {}
    }
}

pub fn walk_expr<V: Visitor + ?Sized>(v: &mut V, e: &Expr) {
    match &e.kind {
        ExprKind::Unary { operand, .. } => v.visit_expr(operand),
        ExprKind::Binary { left, right, .. } => {
            v.visit_expr(left);
            v.visit_expr(right);
        }
        ExprKind::Assign { target, value, .. } => {
            v.visit_expr(target);
            v.visit_expr(value);
        }
        ExprKind::Cond { cond, then, otherwise } => {
            v.visit_expr(cond);
            v.visit_expr(then);
            v.visit_expr(otherwise);
        }
        ExprKind::Call { callee, args, .. } => {
            v.visit_expr(callee);
            args.iter().for_each(|a| v.visit_expr(a));
        }
        ExprKind::Index { base, index } => {
            v.visit_expr(base);
            v.visit_expr(index);
        }
        ExprKind::Member { base, .. } => v.visit_expr(base),
        ExprKind::InitList { items } => items.iter().for_each(|i| v.visit_expr(i)),
        _ => {}
    }
}
