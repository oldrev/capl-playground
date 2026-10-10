//! `capl-parse` — recursive descent, precedence climbing, error recovery.
//!
//! Mirrors DMD's `parse.d`: this crate is a pure syntax machine. It never
//! looks up a symbol, never folds a constant, never reports a type error.
//! Anything it cannot make sense of becomes a diagnostic plus a resync to the
//! next `;`, `}` or top-level keyword, so one typo does not bury the file.
#![forbid(unsafe_code)]

use capl_ast::*;
use capl_diag::{DiagBag, Loc, Stage};
use capl_lexer::{Lexer, TokKind, Token};

const ASSIGN_OPS: &[&str] = &["=", "+=", "-=", "*=", "/=", "%=", "<<=", ">>=", "&=", "^=", "|="];

fn bin_prec(op: &str) -> Option<u8> {
    Some(match op {
        "||" => 1,
        "&&" => 2,
        "|" => 3,
        "^" => 4,
        "&" => 5,
        "==" | "!=" => 6,
        "<" | ">" | "<=" | ">=" => 7,
        "<<" | ">>" => 8,
        "+" | "-" => 9,
        "*" | "/" | "%" => 10,
        _ => return None,
    })
}

pub struct Parser<'d> {
    toks: Vec<Token>,
    p: usize,
    diags: &'d mut DiagBag,
}

pub fn parse(src: &str, name: &str, diags: &mut DiagBag) -> Unit {
    let toks = Lexer::new(src, diags).collect_all();
    Parser { toks, p: 0, diags }.unit(name)
}

impl<'d> Parser<'d> {
    fn cur(&self) -> &Token {
        &self.toks[self.p.min(self.toks.len() - 1)]
    }
    fn nth(&self, n: usize) -> &Token {
        &self.toks[(self.p + n).min(self.toks.len() - 1)]
    }
    fn at_eof(&self) -> bool {
        self.cur().kind == TokKind::Eof
    }
    fn bump(&mut self) -> Token {
        let t = self.cur().clone();
        if !self.at_eof() {
            self.p += 1;
        }
        t
    }
    fn eat_punct(&mut self, p: &str) -> bool {
        if self.cur().is_punct(p) {
            self.p += 1;
            true
        } else {
            false
        }
    }
    fn eat_kw(&mut self, k: &str) -> bool {
        if self.cur().is_kw(k) {
            self.p += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, p: &str) {
        if !self.eat_punct(p) {
            let (loc, found) = (self.cur().loc, self.describe());
            self.diags.error(Stage::Parse, loc, format!("expected '{p}', found '{found}'"));
        }
    }
    fn expect_ident(&mut self) -> String {
        if self.cur().kind == TokKind::Ident {
            return self.bump().text;
        }
        let (loc, found) = (self.cur().loc, self.describe());
        self.diags.error(Stage::Parse, loc, format!("expected identifier, found '{found}'"));
        "<error>".into()
    }
    fn describe(&self) -> String {
        if self.at_eof() { "end of file".into() } else { self.cur().text.clone() }
    }
    fn span(&self, start: Loc) -> Loc {
        start.to(self.toks[self.p.saturating_sub(1)].loc)
    }

    /* ── types ─────────────────────────────────────── */
    fn at_type(&self) -> bool {
        self.cur().kind == TokKind::Keyword && capl_lexer::TYPE_KEYWORDS.contains(&self.cur().text.as_str())
    }
    fn ty(&mut self) -> Ty {
        let t = self.bump();
        Ty::scalar(match t.text.as_str() {
            "void" => TyKind::Void,
            "char" => TyKind::Char,
            "byte" => TyKind::Byte,
            "int" => TyKind::Int,
            "word" => TyKind::Word,
            "long" => TyKind::Long,
            "dword" => TyKind::Dword,
            "int64" => TyKind::Int64,
            "qword" => TyKind::Qword,
            "float" => TyKind::Float,
            "double" => TyKind::Double,
            "timer" => TyKind::Timer,
            "msTimer" => TyKind::MsTimer,
            "message" => TyKind::Message,
            _ => {
                self.diags.error(Stage::Parse, t.loc, format!("unknown type '{}'", t.text));
                TyKind::Error
            }
        })
    }

    /* ── translation unit ──────────────────────────── */
    fn unit(mut self, name: &str) -> Unit {
        let mut decls = Vec::new();
        while !self.at_eof() {
            let before = self.p;
            if let Some(mut d) = self.top_level() {
                decls.append(&mut d);
            }
            if self.p == before {
                self.p += 1;
            }
        }
        Unit { name: name.to_string(), decls }
    }

    fn top_level(&mut self) -> Option<Vec<Decl>> {
        let start = self.cur().loc;
        if self.eat_kw("includes") {
            let text = self.skip_block();
            return Some(vec![Decl::Includes { text, loc: self.span(start) }]);
        }
        if self.eat_kw("variables") {
            let mut decls = Vec::new();
            self.expect("{");
            while !self.at_eof() && !self.cur().is_punct("}") {
                let before = self.p;
                decls.append(&mut self.var_group());
                if self.p == before {
                    self.p += 1;
                }
            }
            self.expect("}");
            return Some(vec![Decl::Variables { decls, loc: self.span(start) }]);
        }
        if self.cur().is_kw("on") {
            return Some(vec![Decl::Handler(self.handler())]);
        }
        while self.eat_kw("export") || self.eat_kw("static") {}
        if self.at_type() || self.cur().is_kw("const") {
            let save = self.p;
            let _ = self.eat_kw("const");
            let ty = self.ty();
            if self.cur().kind == TokKind::Ident && self.nth(1).is_punct("(") {
                return Some(vec![Decl::Func(self.function(ty, start))]);
            }
            self.p = save;
            return Some(self.var_group().into_iter().map(Decl::Var).collect());
        }
        let (loc, found) = (self.cur().loc, self.describe());
        self.diags.error(Stage::Parse, loc, format!("unexpected '{found}' at top level"));
        self.resync();
        None
    }

    fn resync(&mut self) {
        while !self.at_eof() {
            if self.cur().is_kw("on") || self.cur().is_kw("variables") {
                return;
            }
            if self.eat_punct(";") {
                return;
            }
            if self.cur().is_punct("{") {
                self.skip_block();
                return;
            }
            self.p += 1;
        }
    }

    fn skip_block(&mut self) -> String {
        let mut out = String::new();
        if !self.eat_punct("{") {
            self.expect("{");
            return out;
        }
        let mut depth = 1usize;
        while !self.at_eof() && depth > 0 {
            if self.cur().is_punct("{") {
                depth += 1;
            } else if self.cur().is_punct("}") {
                depth -= 1;
                if depth == 0 {
                    self.p += 1;
                    break;
                }
            }
            out.push_str(&self.bump().text);
            out.push(' ');
        }
        out
    }

    fn function(&mut self, ret_ty: Ty, start: Loc) -> FuncDecl {
        let name = self.expect_ident();
        self.expect("(");
        let mut params = Vec::new();
        if !self.cur().is_punct(")") {
            loop {
                if self.cur().is_kw("void") && self.nth(1).is_punct(")") {
                    self.p += 1;
                    break;
                }
                let p_start = self.cur().loc;
                let base = if self.at_type() { self.ty() } else { Ty::scalar(TyKind::Long) };
                let pname = self.expect_ident();
                let ty = if self.eat_punct("[") {
                    self.expect("]");
                    Ty::array(base, 0)
                } else {
                    base
                };
                params.push(Param { name: pname, decl_ty: ty, loc: self.span(p_start), sym: None });
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        self.expect(")");
        let body = self.block();
        FuncDecl { name, ret_ty, params, body, loc: self.span(start) }
    }

    fn var_group(&mut self) -> Vec<VarDecl> {
        let is_const = self.eat_kw("const");
        if !self.at_type() {
            let (loc, found) = (self.cur().loc, self.describe());
            self.diags.error(Stage::Parse, loc, format!("expected a declaration, found '{found}'"));
            while !self.at_eof() && !self.cur().is_punct(";") && !self.cur().is_punct("}") {
                self.p += 1;
            }
            self.eat_punct(";");
            return Vec::new();
        }
        let mut base = self.ty();
        if base.kind == TyKind::Message {
            if self.cur().kind == TokKind::Int {
                base.msg_id = Some(self.bump().int as u32);
            } else if self.cur().kind == TokKind::Ident && self.nth(1).kind == TokKind::Ident {
                let _symbolic = self.bump();
            }
        }
        let mut out = Vec::new();
        loop {
            let d_start = self.cur().loc;
            let name = self.expect_ident();
            let mut is_array = false;
            let mut dims = Vec::new();
            if self.eat_punct("[") {
                is_array = true;
                if !self.cur().is_punct("]") {
                    dims.push(self.assign_expr());
                }
                self.expect("]");
            }
            let init = if self.eat_punct("=") {
                Some(if self.cur().is_punct("{") { self.init_list() } else { self.assign_expr() })
            } else {
                None
            };
            out.push(VarDecl {
                name,
                decl_ty: base.clone(),
                dims,
                is_array,
                is_const,
                init,
                loc: self.span(d_start),
                sym: None,
            });
            if !self.eat_punct(",") {
                break;
            }
        }
        self.expect(";");
        out
    }

    fn init_list(&mut self) -> Expr {
        let start = self.cur().loc;
        self.expect("{");
        let mut items = Vec::new();
        if !self.cur().is_punct("}") {
            loop {
                if self.cur().is_punct("}") {
                    break;
                }
                items.push(if self.cur().is_punct("{") { self.init_list() } else { self.assign_expr() });
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        self.expect("}");
        Expr::new(ExprKind::InitList { items }, self.span(start))
    }

    /* ── events ────────────────────────────────────── */
    fn handler(&mut self) -> HandlerDecl {
        let start = self.cur().loc;
        self.p += 1; // 'on'
        let kw = self.bump();
        let event = match kw.text.as_str() {
            "start" => EventSpec::Start,
            "preStart" => EventSpec::PreStart,
            "stopMeasurement" => EventSpec::StopMeasurement,
            "preStop" => EventSpec::PreStop,
            "errorFrame" => EventSpec::ErrorFrame,
            "busOff" => EventSpec::BusOff,
            "timer" => EventSpec::Timer { name: self.expect_ident() },
            "envVar" => EventSpec::EnvVar { name: self.expect_ident() },
            "sysvar" | "sysvar_update" => {
                let mut parts = vec![self.expect_ident()];
                while self.eat_punct("::") || self.eat_punct(".") {
                    parts.push(self.expect_ident());
                }
                EventSpec::SysVar { name: parts.join("::") }
            }
            "key" => {
                if self.eat_punct("*") {
                    EventSpec::Key { key: None, wildcard: true }
                } else if self.cur().kind == TokKind::Char || self.cur().kind == TokKind::Int {
                    EventSpec::Key { key: char::from_u32(self.bump().int as u32), wildcard: false }
                } else {
                    let s = self.bump().string;
                    EventSpec::Key { key: s.chars().next(), wildcard: false }
                }
            }
            "message" => {
                if self.eat_punct("*") {
                    EventSpec::Message { id: None, name: None, channel: None, wildcard: true }
                } else if self.cur().kind == TokKind::Int {
                    EventSpec::Message { id: Some(self.bump().int as u32), name: None, channel: None, wildcard: false }
                } else {
                    let first = self.expect_ident();
                    if self.eat_punct(".") {
                        let ch = first.trim_start_matches("CAN").parse::<u8>().ok();
                        if self.cur().kind == TokKind::Int {
                            EventSpec::Message { id: Some(self.bump().int as u32), name: None, channel: ch, wildcard: false }
                        } else {
                            EventSpec::Message { id: None, name: Some(self.expect_ident()), channel: ch, wildcard: false }
                        }
                    } else {
                        EventSpec::Message { id: None, name: Some(first), channel: None, wildcard: false }
                    }
                }
            }
            other => {
                self.diags.error(Stage::Parse, kw.loc, format!("unknown event type '{other}'"));
                EventSpec::Start
            }
        };
        let body = self.block();
        let export_name = event.mangle();
        HandlerDecl { event, body, loc: self.span(start), export_name }
    }

    /* ── statements ────────────────────────────────── */
    fn block(&mut self) -> Stmt {
        let start = self.cur().loc;
        let mut stmts = Vec::new();
        if !self.eat_punct("{") {
            self.expect("{");
            return Stmt { kind: StmtKind::Block { stmts }, loc: self.span(start) };
        }
        while !self.at_eof() && !self.cur().is_punct("}") {
            let before = self.p;
            if let Some(s) = self.stmt() {
                stmts.push(s);
            }
            if self.p == before {
                self.p += 1;
            }
        }
        self.expect("}");
        Stmt { kind: StmtKind::Block { stmts }, loc: self.span(start) }
    }

    fn stmt(&mut self) -> Option<Stmt> {
        let start = self.cur().loc;
        let kind = if self.cur().is_punct("{") {
            return Some(self.block());
        } else if self.eat_punct(";") {
            StmtKind::Empty
        } else if self.at_type() || self.cur().is_kw("const") {
            StmtKind::Decl { decls: self.var_group() }
        } else if self.eat_kw("if") {
            self.expect("(");
            let cond = self.expr();
            self.expect(")");
            let then = Box::new(self.stmt()?);
            let otherwise = if self.eat_kw("else") { self.stmt().map(Box::new) } else { None };
            StmtKind::If { cond, then, otherwise }
        } else if self.eat_kw("while") {
            self.expect("(");
            let cond = self.expr();
            self.expect(")");
            StmtKind::While { cond, body: Box::new(self.stmt()?) }
        } else if self.eat_kw("do") {
            let body = Box::new(self.stmt()?);
            self.eat_kw("while");
            self.expect("(");
            let cond = self.expr();
            self.expect(")");
            self.expect(";");
            StmtKind::DoWhile { body, cond }
        } else if self.eat_kw("for") {
            self.expect("(");
            let init = if self.cur().is_punct(";") {
                self.p += 1;
                None
            } else if self.at_type() {
                Some(Box::new(Stmt { kind: StmtKind::Decl { decls: self.var_group() }, loc: start }))
            } else {
                let e = self.expr();
                self.expect(";");
                Some(Box::new(Stmt { kind: StmtKind::Expr { expr: e }, loc: start }))
            };
            let cond = if self.cur().is_punct(";") { None } else { Some(self.expr()) };
            self.expect(";");
            let step = if self.cur().is_punct(")") { None } else { Some(self.expr()) };
            self.expect(")");
            StmtKind::For { init, cond, step, body: Box::new(self.stmt()?) }
        } else if self.eat_kw("switch") {
            self.expect("(");
            let disc = self.expr();
            self.expect(")");
            let mut clauses: Vec<SwitchClause> = Vec::new();
            self.expect("{");
            while !self.at_eof() && !self.cur().is_punct("}") {
                let before = self.p;
                let c_start = self.cur().loc;
                if self.eat_kw("case") {
                    let test = Some(self.assign_expr());
                    self.expect(":");
                    clauses.push(SwitchClause { test, body: Vec::new(), loc: self.span(c_start) });
                } else if self.eat_kw("default") {
                    self.expect(":");
                    clauses.push(SwitchClause { test: None, body: Vec::new(), loc: self.span(c_start) });
                } else if let Some(s) = self.stmt() {
                    if clauses.is_empty() {
                        clauses.push(SwitchClause { test: None, body: Vec::new(), loc: c_start });
                    }
                    clauses.last_mut().unwrap().body.push(s);
                }
                if self.p == before {
                    self.p += 1;
                }
            }
            self.expect("}");
            StmtKind::Switch { disc, clauses }
        } else if self.eat_kw("break") {
            self.expect(";");
            StmtKind::Break
        } else if self.eat_kw("continue") {
            self.expect(";");
            StmtKind::Continue
        } else if self.eat_kw("return") {
            let expr = if self.cur().is_punct(";") { None } else { Some(self.expr()) };
            self.expect(";");
            StmtKind::Return { expr }
        } else {
            let e = self.expr();
            self.expect(";");
            StmtKind::Expr { expr: e }
        };
        Some(Stmt { kind, loc: self.span(start) })
    }

    /* ── expressions ───────────────────────────────── */
    pub fn expr(&mut self) -> Expr {
        let start = self.cur().loc;
        let mut e = self.assign_expr();
        while self.eat_punct(",") {
            let right = self.assign_expr();
            e = Expr::new(
                ExprKind::Binary { op: ",".into(), left: Box::new(e), right: Box::new(right) },
                self.span(start),
            );
        }
        e
    }

    fn assign_expr(&mut self) -> Expr {
        let start = self.cur().loc;
        let lhs = self.conditional();
        if self.cur().kind == TokKind::Punct && ASSIGN_OPS.contains(&self.cur().text.as_str()) {
            let op = self.bump().text;
            let value = self.assign_expr();
            return Expr::new(
                ExprKind::Assign { op, target: Box::new(lhs), value: Box::new(value) },
                self.span(start),
            );
        }
        lhs
    }

    fn conditional(&mut self) -> Expr {
        let start = self.cur().loc;
        let cond = self.binary(1);
        if self.eat_punct("?") {
            let then = self.assign_expr();
            self.expect(":");
            let otherwise = self.conditional();
            return Expr::new(
                ExprKind::Cond { cond: Box::new(cond), then: Box::new(then), otherwise: Box::new(otherwise) },
                self.span(start),
            );
        }
        cond
    }

    fn binary(&mut self, min_prec: u8) -> Expr {
        let start = self.cur().loc;
        let mut left = self.unary();
        loop {
            if self.cur().kind != TokKind::Punct {
                break;
            }
            let Some(prec) = bin_prec(&self.cur().text) else { break };
            if prec < min_prec {
                break;
            }
            let op = self.bump().text;
            let right = self.binary(prec + 1);
            left = Expr::new(
                ExprKind::Binary { op, left: Box::new(left), right: Box::new(right) },
                self.span(start),
            );
        }
        left
    }

    fn unary(&mut self) -> Expr {
        let start = self.cur().loc;
        if self.cur().kind == TokKind::Punct {
            let op = self.cur().text.clone();
            if matches!(op.as_str(), "!" | "~" | "-" | "+" | "++" | "--") {
                self.p += 1;
                let operand = Box::new(self.unary());
                return Expr::new(ExprKind::Unary { op, operand, postfix: false }, self.span(start));
            }
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Expr {
        let start = self.cur().loc;
        let mut e = self.primary();
        loop {
            if self.eat_punct("(") {
                let mut args = Vec::new();
                if !self.cur().is_punct(")") {
                    loop {
                        args.push(self.assign_expr());
                        if !self.eat_punct(",") {
                            break;
                        }
                    }
                }
                self.expect(")");
                e = Expr::new(ExprKind::Call { callee: Box::new(e), args, sym: None }, self.span(start));
            } else if self.eat_punct("[") {
                let index = self.expr();
                self.expect("]");
                e = Expr::new(ExprKind::Index { base: Box::new(e), index: Box::new(index) }, self.span(start));
            } else if self.eat_punct(".") {
                let name = self.bump().text;
                e = Expr::new(ExprKind::Member { base: Box::new(e), name }, self.span(start));
            } else if self.cur().is_punct("++") || self.cur().is_punct("--") {
                let op = self.bump().text;
                e = Expr::new(ExprKind::Unary { op, operand: Box::new(e), postfix: true }, self.span(start));
            } else {
                break;
            }
        }
        e
    }

    fn primary(&mut self) -> Expr {
        let start = self.cur().loc;
        let t = self.cur().clone();
        match t.kind {
            TokKind::Int => {
                self.p += 1;
                Expr::new(ExprKind::IntLit { value: t.int }, self.span(start))
            }
            TokKind::Float => {
                self.p += 1;
                Expr::new(ExprKind::FloatLit { value: t.float }, self.span(start))
            }
            TokKind::Char => {
                self.p += 1;
                Expr::new(ExprKind::CharLit { value: t.int }, self.span(start))
            }
            TokKind::Str => {
                self.p += 1;
                let mut value = t.string;
                while self.cur().kind == TokKind::Str {
                    value.push_str(&self.bump().string);
                }
                Expr::new(ExprKind::StrLit { value }, self.span(start))
            }
            TokKind::Ident => {
                self.p += 1;
                Expr::new(ExprKind::Ident { name: t.text, sym: None }, self.span(start))
            }
            TokKind::Keyword if t.text == "this" => {
                self.p += 1;
                Expr::new(ExprKind::This, self.span(start))
            }
            TokKind::Punct if t.text == "(" => {
                self.p += 1;
                let e = self.expr();
                self.expect(")");
                e
            }
            _ => {
                let found = self.describe();
                self.diags.error(Stage::Parse, t.loc, format!("expected an expression, found '{found}'"));
                if !self.at_eof() && !self.cur().is_punct(";") && !self.cur().is_punct(")") {
                    self.p += 1;
                }
                Expr::new(ExprKind::IntLit { value: 0 }, self.span(start))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_small_program() {
        let mut bag = DiagBag::new();
        let unit = parse("variables { int x = 1; }\non start { x += 2; }", "t.can", &mut bag);
        assert!(!bag.has_errors(), "{:?}", bag.items());
        assert_eq!(unit.decls.len(), 2);
    }

    #[test]
    fn recovers_from_a_missing_semicolon() {
        let mut bag = DiagBag::new();
        let unit = parse("on start { int x = 1 write(\"hi\"); }", "t.can", &mut bag);
        assert!(bag.has_errors());
        assert_eq!(unit.decls.len(), 1); // the handler still made it into the tree
    }
}
