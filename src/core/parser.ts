/* capl-parse — recursive descent parser (mirror of crates/capl-parse).
 * Mirrors DMD's parse.d: the parser is *only* a syntax machine. It never
 * looks a symbol up, never folds a constant and never reports a type error. */

import type { DiagBag, Loc } from "./diagnostics";
import type { Token } from "./lexer";
import * as A from "./ast";

const ASSIGN_OPS = new Set(["=", "+=", "-=", "*=", "/=", "%=", "<<=", ">>=", "&=", "^=", "|="]);

const BIN_PREC: Record<string, number> = {
  "||": 1, "&&": 2, "|": 3, "^": 4, "&": 5,
  "==": 6, "!=": 6,
  "<": 7, ">": 7, "<=": 7, ">=": 7,
  "<<": 8, ">>": 8,
  "+": 9, "-": 9,
  "*": 10, "/": 10, "%": 10,
};

const SCALAR_TYPES = new Set([
  "void", "char", "byte", "int", "word", "long", "dword", "int64", "qword", "float", "double",
]);

export class Parser {
  private p = 0;
  /** synthetic tags for anonymous `struct { … } v;` definitions */
  private anonCount = 0;

  constructor(private readonly toks: Token[], private readonly diags: DiagBag) {}

  /* ── token helpers ──────────────────────────────────── */
  private get cur(): Token {
    return this.toks[Math.min(this.p, this.toks.length - 1)];
  }
  private peek(n = 1): Token {
    return this.toks[Math.min(this.p + n, this.toks.length - 1)];
  }
  /** same as `cur` but as a call, so control-flow narrowing never sticks */
  private tok(): Token {
    return this.toks[Math.min(this.p, this.toks.length - 1)];
  }
  private get atEof(): boolean {
    return this.cur.kind === "eof";
  }
  private advance(): Token {
    const t = this.cur;
    if (!this.atEof) this.p++;
    return t;
  }
  private isPunct(s: string, n = 0): boolean {
    const t = n === 0 ? this.cur : this.peek(n);
    return t.kind === "punct" && t.text === s;
  }
  private isKw(s: string, n = 0): boolean {
    const t = n === 0 ? this.cur : this.peek(n);
    return t.kind === "kw" && t.text === s;
  }
  private matchPunct(s: string): boolean {
    if (this.isPunct(s)) {
      this.p++;
      return true;
    }
    return false;
  }
  private matchKw(s: string): boolean {
    if (this.isKw(s)) {
      this.p++;
      return true;
    }
    return false;
  }
  private expectPunct(s: string): Token {
    if (this.isPunct(s)) return this.advance();
    this.err(`expected '${s}', found '${this.describe(this.cur)}'`);
    return this.cur;
  }
  private expectIdent(): string {
    if (this.cur.kind === "ident") return this.advance().text;
    this.err(`expected identifier, found '${this.describe(this.cur)}'`);
    return "<error>";
  }
  private describe(t: Token): string {
    return t.kind === "eof" ? "end of file" : t.text;
  }
  private err(msg: string, loc?: Loc): void {
    this.diags.error("parse", loc ?? this.cur.loc, msg);
  }
  private span(start: Loc): Loc {
    const prev = this.toks[Math.max(0, this.p - 1)];
    return { line: start.line, col: start.col, pos: start.pos, end: prev?.loc.end ?? start.end };
  }

  /* ── types ──────────────────────────────────────────── */
  private atTypeStart(): boolean {
    const t = this.cur;
    if (t.kind !== "kw") return false;
    return (
      SCALAR_TYPES.has(t.text) || t.text === "timer" || t.text === "msTimer" ||
      t.text === "message" || t.text === "struct" || t.text === "enum"
    );
  }

  /** `struct S { … }` / `enum E { … }` — a type *definition* rather than a use */
  private isTaggedDef(): boolean {
    if (!this.isKw("struct") && !this.isKw("enum")) return false;
    let n = 1;
    if (this.peek(n).kind === "ident") n++;
    return this.isPunct("{", n);
  }

  private parseType(): A.Ty {
    const t = this.advance();
    switch (t.text) {
      case "timer": return { kind: "timer" };
      case "msTimer": return { kind: "msTimer" };
      case "message": return { kind: "message" };
      case "struct": case "enum": {
        // `struct S v;` — the layout is resolved by sema through the tag
        const name = this.cur.kind === "ident" && !this.isPunct("{") ? this.advance().text : undefined;
        return { kind: t.text, name };
      }
      default:
        if (SCALAR_TYPES.has(t.text)) return { kind: t.text as A.TyKind };
        this.err(`unknown type '${t.text}'`, t.loc);
        return { kind: "error" };
    }
  }

  /* ── enum / struct definitions ──────────────────────── */
  /** parses `[const] (struct|enum) [Tag] { … } [declarators] ;` */
  private parseTagged(isConst: boolean): { types: A.Decl[]; vars: A.VarDecl[] } {
    const start = this.cur.loc;
    const tag = this.advance().text; // 'struct' | 'enum'
    let name = this.cur.kind === "ident" && !this.isPunct("{") ? this.advance().text : "";
    if (name === "" && tag === "struct") name = `__anon${this.anonCount++}`;
    const types: A.Decl[] = [];

    if (!this.isPunct("{")) {
      this.err(`expected '{' after '${tag} ${name}'`);
      return { types, vars: [] };
    }
    this.advance();

    if (tag === "enum") {
      const items: A.EnumItem[] = [];
      if (!this.isPunct("}")) {
        let guard = 0;
        do {
          if (this.isPunct("}") || this.atEof) break;
          const iStart = this.cur.loc;
          const iname = this.expectIdent();
          let init: A.Expr | undefined;
          if (this.matchPunct("=")) init = this.parseAssignExpr();
          items.push({ kind: "EnumItem", name: iname, init, loc: this.span(iStart) });
        } while (this.matchPunct(",") && guard++ < 1024);
      }
      this.expectPunct("}");
      types.push({ kind: "EnumDecl", name, items, loc: this.span(start) });
    } else {
      const fields: A.StructField[] = [];
      let guard = 0;
      while (!this.atEof && !this.isPunct("}") && guard++ < 512) {
        const before = this.p;
        const fStart = this.cur.loc;
        const fTy = this.parseType();
        const fname = this.cur.kind === "ident" ? this.advance().text : this.expectIdent();
        const dims: A.Expr[] = [];
        while (this.isPunct("[")) {
          this.advance();
          if (!this.isPunct("]")) dims.push(this.parseAssignExpr());
          this.expectPunct("]");
        }
        this.expectPunct(";");
        fields.push({ kind: "StructField", name: fname, declTy: fTy, dims, loc: this.span(fStart) });
        if (this.p === before) this.advance();
      }
      this.expectPunct("}");
      if (fields.length === 0) this.diags.warn("parse", this.span(start), "empty struct");
      types.push({ kind: "StructDecl", name, fields, loc: this.span(start) });
    }

    // optional declarator list: `struct Point { … } origin, points[4];`
    const def = types[0];
    const base: A.Ty =
      tag === "struct"
        ? { kind: "struct", name, fields: def && def.kind === "StructDecl" ? def.fields : [] }
        : { kind: "enum", name };
    const vars: A.VarDecl[] =
      this.cur.kind === "ident" ? this.parseVarDeclGroup(true, base, isConst, types) : [];
    this.matchPunct(";"); // tolerate a trailing ';' after a type definition
    return { types, vars };
  }

  /* ── translation unit ───────────────────────────────── */
  parseUnit(name = "source.can"): A.Unit {
    const decls: A.Decl[] = [];
    let guard = 0;
    while (!this.atEof && guard++ < 100000) {
      const before = this.p;
      const d = this.parseTopLevel();
      if (d) decls.push(...d);
      if (this.p === before) this.advance(); // guaranteed progress
    }
    return { kind: "Unit", name, decls };
  }

  private parseTopLevel(): A.Decl[] | null {
    const start = this.cur.loc;
    if (this.isKw("includes")) {
      this.advance();
      const text = this.skipBalancedBlock();
      return [{ kind: "IncludesBlock", text, loc: this.span(start) }];
    }
    if (this.isKw("variables")) {
      this.advance();
      const decls: A.VarDecl[] = [];
      const types: A.Decl[] = [];
      this.expectPunct("{");
      let guard = 0;
      while (!this.atEof && !this.isPunct("}") && guard++ < 10000) {
        const before = this.p;
        decls.push(...this.parseVarDeclGroup(true, undefined, undefined, types));
        if (this.p === before) this.advance();
      }
      this.expectPunct("}");
      return [{
        kind: "VariablesBlock",
        decls,
        types: types as (A.EnumDecl | A.StructDecl)[],
        loc: this.span(start),
      }];
    }
    if (this.isKw("on")) return [this.parseHandler()];
    if (this.isTaggedDef()) {
      const r = this.parseTagged(false);
      return [...r.types, ...r.vars];
    }

    let isExport = false;
    while (this.isKw("export") || this.isKw("static")) {
      if (this.isKw("export")) isExport = true;
      this.advance();
    }
    if (this.isKw("testcase") || this.isKw("testfunction")) {
      this.advance();
      return [this.parseFunctionRest({ kind: "void" }, start, true)];
    }
    if (this.atTypeStart() || this.isKw("const")) {
      const save = this.p;
      const isConst = this.matchKw("const");
      const ty = this.parseType();
      if (this.cur.kind === "ident" && this.isPunct("(", 1)) {
        return [this.parseFunctionRest(ty, start, isExport)];
      }
      this.p = save;
      void isConst;
      return this.parseVarDeclGroup(true);
    }
    this.err(`unexpected '${this.describe(this.cur)}' at top level`);
    this.recoverTopLevel();
    return null;
  }

  private recoverTopLevel(): void {
    let guard = 0;
    while (!this.atEof && guard++ < 10000) {
      if (this.isKw("on") || this.isKw("variables") || this.isKw("includes")) return;
      if (this.isPunct(";")) {
        this.advance();
        return;
      }
      if (this.isPunct("{")) {
        this.skipBalancedBlock();
        return;
      }
      this.advance();
    }
  }

  private skipBalancedBlock(): string {
    const parts: string[] = [];
    if (!this.isPunct("{")) {
      this.expectPunct("{");
      return "";
    }
    this.advance();
    let depth = 1;
    let guard = 0;
    while (!this.atEof && depth > 0 && guard++ < 100000) {
      if (this.isPunct("{")) depth++;
      else if (this.isPunct("}")) {
        depth--;
        if (depth === 0) {
          this.advance();
          break;
        }
      }
      parts.push(this.advance().text);
    }
    return parts.join(" ");
  }

  /* ── declarations ───────────────────────────────────── */
  private parseFunctionRest(retTy: A.Ty, start: Loc, isExport: boolean): A.FuncDecl {
    const name = this.expectIdent();
    this.expectPunct("(");
    const params: A.Param[] = [];
    if (!this.isPunct(")")) {
      let guard = 0;
      do {
        if (this.isKw("void") && this.isPunct(")", 1)) {
          this.advance();
          break;
        }
        const pStart = this.cur.loc;
        const pTy = this.atTypeStart() ? this.parseType() : ({ kind: "int" } as A.Ty);
        const pName = this.expectIdent();
        let ty = pTy;
        if (this.isPunct("[")) {
          // array parameters decay, so the extents are parsed and discarded
          while (this.isPunct("[")) {
            this.advance();
            if (!this.isPunct("]")) this.parseAssignExpr();
            this.expectPunct("]");
          }
          ty = { kind: "array", elem: pTy, len: 0 };
        }
        params.push({ kind: "Param", name: pName, declTy: ty, loc: this.span(pStart) });
      } while (this.matchPunct(",") && guard++ < 64);
    }
    this.expectPunct(")");
    const body = this.parseBlock();
    return { kind: "FuncDecl", name, retTy, params, body, isExport, loc: this.span(start) };
  }

  private parseVarDeclGroup(
    allowInit: boolean,
    baseIn?: A.Ty,
    isConstIn?: boolean,
    pendingTypes?: A.Decl[],
  ): A.VarDecl[] {
    const start = this.cur.loc;
    const isConst = isConstIn ?? this.matchKw("const");
    if (baseIn === undefined && (this.isTaggedDef() || (!this.isKw("struct") && !this.isKw("enum") && !this.atTypeStart()))) {
      // a full `struct S {…} x;` / `enum E {…} x;` definition
      if (this.isTaggedDef()) {
        const r = this.parseTagged(isConst);
        if (pendingTypes) pendingTypes.push(...r.types);
        return r.vars;
      }
      this.err(`expected a declaration, found '${this.describe(this.cur)}'`);
      this.skipToSemi();
      return [];
    }
    const base = baseIn ?? this.parseType();
    // `message 0x100 m;` / `message EngineData m;`
    if (base.kind === "message") {
      if (this.cur.kind === "int") {
        base.msgId = Number(this.advance().value);
      } else if (this.cur.kind === "ident" && (this.peek(1).kind === "ident" || this.isPunct(".", 1))) {
        const id = this.advance();
        if (this.matchPunct(".")) {
          const nx = this.tok();
          if (nx.kind === "int") {
            this.advance();
            base.msgName = `${id.text}.${nx.text}`;
          } else {
            base.msgName = `${id.text}.${this.expectIdent()}`;
          }
        } else {
          base.msgName = id.text;
        }
      } else if (this.isPunct("*")) {
        this.advance();
      }
    }

    const out: A.VarDecl[] = [];
    let guard = 0;
    do {
      const dStart = this.cur.loc;
      const name = this.expectIdent();
      const dims: A.Expr[] = [];
      while (this.isPunct("[")) {
        this.advance();
        if (!this.isPunct("]")) dims.push(this.parseAssignExpr());
        this.expectPunct("]");
      }
      let init: A.Expr | undefined;
      if (this.isPunct("=")) {
        this.advance();
        if (!allowInit) this.err("initialiser not allowed here");
        init = this.isPunct("{") ? this.parseInitList() : this.parseAssignExpr();
      }
      out.push({
        kind: "VarDecl",
        name,
        declTy: { ...base },
        dims,
        isArray: dims.length > 0,
        init,
        isConst,
        loc: this.span(dStart),
      });
    } while (this.matchPunct(",") && guard++ < 256);
    this.expectPunct(";");
    void start;
    return out;
  }

  private parseInitList(): A.InitList {
    const start = this.cur.loc;
    this.expectPunct("{");
    const items: A.Expr[] = [];
    if (!this.isPunct("}")) {
      let guard = 0;
      do {
        if (this.isPunct("}")) break;
        items.push(this.isPunct("{") ? this.parseInitList() : this.parseAssignExpr());
      } while (this.matchPunct(",") && guard++ < 4096);
    }
    this.expectPunct("}");
    return { kind: "InitList", items, loc: this.span(start) };
  }

  private skipToSemi(): void {
    let guard = 0;
    while (!this.atEof && !this.isPunct(";") && !this.isPunct("}") && guard++ < 10000) this.advance();
    this.matchPunct(";");
  }

  /* ── event handlers ─────────────────────────────────── */
  private parseHandler(): A.HandlerDecl {
    const start = this.cur.loc;
    this.advance(); // 'on'
    const ev: A.EventSpec = { kind: "start" };
    const kw = this.cur;

    const take = (k: A.EventKind) => {
      ev.kind = k;
      this.advance();
    };

    if (kw.kind === "kw" || kw.kind === "ident") {
      switch (kw.text) {
        case "start": take("start"); break;
        case "preStart": case "prestart": take("preStart"); break;
        case "stopMeasurement": take("stopMeasurement"); break;
        case "preStop": take("preStop"); break;
        case "errorFrame": take("errorFrame"); break;
        case "busOff": take("busOff"); break;
        case "timer": {
          take("timer");
          ev.name = this.expectIdent();
          break;
        }
        case "sysvar": case "sysvar_update": {
          take("sysvar");
          const parts = [this.expectIdent()];
          while (this.matchPunct("::") || this.matchPunct(".")) parts.push(this.expectIdent());
          ev.name = parts.join("::");
          break;
        }
        case "envVar": {
          take("envVar");
          ev.name = this.expectIdent();
          break;
        }
        case "key": {
          take("key");
          if (this.cur.kind === "char") {
            ev.key = String.fromCharCode(Number(this.advance().value));
          } else if (this.cur.kind === "string") {
            ev.key = String(this.advance().value);
          } else if (this.isPunct("*")) {
            this.advance();
            ev.wildcard = true;
          } else if (this.cur.kind === "int") {
            ev.key = String.fromCharCode(Number(this.advance().value));
          } else {
            ev.key = this.expectIdent();
          }
          break;
        }
        case "message": {
          take("message");
          if (this.isPunct("*")) {
            this.advance();
            ev.wildcard = true;
          } else if (this.cur.kind === "int") {
            ev.id = Number(this.advance().value);
          } else {
            const first = this.expectIdent();
            if (this.matchPunct(".")) {
              const m = /^CAN(\d+)$/i.exec(first);
              ev.channel = m ? Number(m[1]) : 1;
              if (this.tok().kind === "int") ev.id = Number(this.advance().value);
              else if (this.isPunct("*")) {
                this.advance();
                ev.wildcard = true;
              } else ev.name = this.expectIdent();
            } else {
              ev.name = first;
            }
          }
          // `on message 0x100, 0x200` — only the first selector is modelled
          while (this.matchPunct(",")) {
            this.diags.warn("parse", this.cur.loc, "multiple selectors on one handler: only the first is simulated");
            this.advance();
          }
          break;
        }
        default:
          this.err(`unknown event type '${kw.text}'`);
          this.advance();
      }
    } else {
      this.err("expected an event type after 'on'");
    }

    const body = this.parseBlock();
    return { kind: "HandlerDecl", event: ev, body, loc: this.span(start) };
  }

  /* ── statements ─────────────────────────────────────── */
  private parseBlock(): A.Block {
    const start = this.cur.loc;
    const stmts: A.Stmt[] = [];
    if (!this.isPunct("{")) {
      this.expectPunct("{");
      return { kind: "Block", stmts, loc: this.span(start) };
    }
    this.advance();
    let guard = 0;
    while (!this.atEof && !this.isPunct("}") && guard++ < 100000) {
      const before = this.p;
      const s = this.parseStmt();
      if (s) stmts.push(s);
      if (this.p === before) this.advance();
    }
    this.expectPunct("}");
    return { kind: "Block", stmts, loc: this.span(start) };
  }

  private parseStmt(): A.Stmt | null {
    const start = this.cur.loc;
    if (this.isPunct("{")) return this.parseBlock();
    if (this.isPunct(";")) {
      this.advance();
      return { kind: "Empty", loc: this.span(start) };
    }
    if (this.isTaggedDef() || this.atTypeStart() || this.isKw("const")) {
      const types: A.Decl[] = [];
      const decls = this.parseVarDeclGroup(true, undefined, undefined, types);
      return {
        kind: "DeclStmt",
        decls,
        types: types as (A.EnumDecl | A.StructDecl)[],
        loc: this.span(start),
      };
    }
    if (this.isKw("if")) {
      this.advance();
      this.expectPunct("(");
      const cond = this.parseExpr();
      this.expectPunct(")");
      const then = this.parseStmt() ?? { kind: "Empty", loc: this.span(start) };
      let otherwise: A.Stmt | undefined;
      if (this.matchKw("else")) otherwise = this.parseStmt() ?? undefined;
      return { kind: "If", cond, then, otherwise, loc: this.span(start) };
    }
    if (this.isKw("while")) {
      this.advance();
      this.expectPunct("(");
      const cond = this.parseExpr();
      this.expectPunct(")");
      const body = this.parseStmt() ?? { kind: "Empty", loc: this.span(start) };
      return { kind: "While", cond, body, loc: this.span(start) };
    }
    if (this.isKw("do")) {
      this.advance();
      const body = this.parseStmt() ?? { kind: "Empty", loc: this.span(start) };
      if (!this.matchKw("while")) this.err("expected 'while' after do-statement body");
      this.expectPunct("(");
      const cond = this.parseExpr();
      this.expectPunct(")");
      this.expectPunct(";");
      return { kind: "DoWhile", body, cond, loc: this.span(start) };
    }
    if (this.isKw("for")) {
      this.advance();
      this.expectPunct("(");
      let init: A.Stmt | undefined;
      if (!this.isPunct(";")) {
        if (this.atTypeStart() || this.isKw("const")) {
          init = { kind: "DeclStmt", decls: this.parseVarDeclGroup(true), loc: this.span(start) };
        } else {
          const e = this.parseExpr();
          this.expectPunct(";");
          init = { kind: "ExprStmt", expr: e, loc: e.loc };
        }
      } else this.advance();
      let cond: A.Expr | undefined;
      if (!this.isPunct(";")) cond = this.parseExpr();
      this.expectPunct(";");
      let step: A.Expr | undefined;
      if (!this.isPunct(")")) step = this.parseExpr();
      this.expectPunct(")");
      const body = this.parseStmt() ?? { kind: "Empty", loc: this.span(start) };
      return { kind: "For", init, cond, step, body, loc: this.span(start) };
    }
    if (this.isKw("switch")) {
      this.advance();
      this.expectPunct("(");
      const disc = this.parseExpr();
      this.expectPunct(")");
      const clauses: A.SwitchClause[] = [];
      this.expectPunct("{");
      let cl: A.SwitchClause | null = null;
      let guard = 0;
      while (!this.atEof && !this.isPunct("}") && guard++ < 100000) {
        const before = this.p;
        if (this.isKw("case")) {
          const cStart = this.cur.loc;
          this.advance();
          const test = this.parseAssignExpr();
          this.expectPunct(":");
          cl = { test, body: [], loc: this.span(cStart) };
          clauses.push(cl);
        } else if (this.isKw("default")) {
          const cStart = this.cur.loc;
          this.advance();
          this.expectPunct(":");
          cl = { test: null, body: [], loc: this.span(cStart) };
          clauses.push(cl);
        } else {
          const s = this.parseStmt();
          if (s) {
            if (!cl) {
              cl = { test: null, body: [], loc: s.loc };
              clauses.push(cl);
              this.diags.warn("parse", s.loc, "statement before the first 'case' label is unreachable");
            }
            cl.body.push(s);
          }
        }
        if (this.p === before) this.advance();
      }
      this.expectPunct("}");
      return { kind: "Switch", disc, clauses, loc: this.span(start) };
    }
    if (this.isKw("break")) {
      this.advance();
      this.expectPunct(";");
      return { kind: "Break", loc: this.span(start) };
    }
    if (this.isKw("continue")) {
      this.advance();
      this.expectPunct(";");
      return { kind: "Continue", loc: this.span(start) };
    }
    if (this.isKw("return")) {
      this.advance();
      let expr: A.Expr | undefined;
      if (!this.isPunct(";")) expr = this.parseExpr();
      this.expectPunct(";");
      return { kind: "Return", expr, loc: this.span(start) };
    }
    const e = this.parseExpr();
    this.expectPunct(";");
    return { kind: "ExprStmt", expr: e, loc: this.span(start) };
  }

  /* ── expressions ────────────────────────────────────── */
  parseExpr(): A.Expr {
    let e = this.parseAssignExpr();
    while (this.isPunct(",")) {
      const start = e.loc;
      this.advance();
      const rhs = this.parseAssignExpr();
      e = { kind: "Binary", op: ",", left: e, right: rhs, loc: this.span(start) };
    }
    return e;
  }

  parseAssignExpr(): A.Expr {
    const start = this.cur.loc;
    const lhs = this.parseConditional();
    if (this.cur.kind === "punct" && ASSIGN_OPS.has(this.cur.text)) {
      const op = this.advance().text;
      const value = this.parseAssignExpr();
      return { kind: "Assign", op, target: lhs, value, loc: this.span(start) };
    }
    return lhs;
  }

  private parseConditional(): A.Expr {
    const start = this.cur.loc;
    const cond = this.parseBinary(1);
    if (this.matchPunct("?")) {
      const then = this.parseAssignExpr();
      this.expectPunct(":");
      const otherwise = this.parseConditional();
      return { kind: "Cond", cond, then, otherwise, loc: this.span(start) };
    }
    return cond;
  }

  private parseBinary(minPrec: number): A.Expr {
    const start = this.cur.loc;
    let left = this.parseUnary();
    let guard = 0;
    for (;;) {
      if (guard++ > 100000) break;
      const t = this.cur;
      if (t.kind !== "punct") break;
      const prec = BIN_PREC[t.text];
      if (prec === undefined || prec < minPrec) break;
      this.advance();
      const right = this.parseBinary(prec + 1);
      left = { kind: "Binary", op: t.text, left, right, loc: this.span(start) };
    }
    return left;
  }

  private parseUnary(): A.Expr {
    const start = this.cur.loc;
    if (this.cur.kind === "punct") {
      const op = this.cur.text;
      if (op === "!" || op === "~" || op === "-" || op === "+" || op === "++" || op === "--") {
        this.advance();
        const operand = this.parseUnary();
        return { kind: "Unary", op, operand, loc: this.span(start) };
      }
    }
    return this.parsePostfix();
  }

  private parsePostfix(): A.Expr {
    const start = this.cur.loc;
    let e = this.parsePrimary();
    let guard = 0;
    while (guard++ < 10000) {
      if (this.isPunct("(")) {
        this.advance();
        const args: A.Expr[] = [];
        if (!this.isPunct(")")) {
          let g2 = 0;
          do {
            args.push(this.parseAssignExpr());
          } while (this.matchPunct(",") && g2++ < 256);
        }
        this.expectPunct(")");
        e = { kind: "Call", callee: e, args, loc: this.span(start) };
      } else if (this.isPunct("[")) {
        this.advance();
        const index = this.parseExpr();
        this.expectPunct("]");
        e = { kind: "Index", base: e, index, loc: this.span(start) };
      } else if (this.isPunct(".")) {
        this.advance();
        const name = this.cur.kind === "kw" ? this.advance().text : this.expectIdent();
        e = { kind: "Member", base: e, name, loc: this.span(start) };
      } else if (this.isPunct("++") || this.isPunct("--")) {
        const op = this.advance().text;
        e = { kind: "Unary", op, operand: e, postfix: true, loc: this.span(start) };
      } else break;
    }
    return e;
  }

  private parsePrimary(): A.Expr {
    const start = this.cur.loc;
    const t = this.cur;
    switch (t.kind) {
      case "int":
        this.advance();
        return { kind: "IntLit", value: Number(t.value), loc: this.span(start) };
      case "float":
        this.advance();
        return { kind: "FloatLit", value: Number(t.value), loc: this.span(start) };
      case "char":
        this.advance();
        return { kind: "CharLit", value: Number(t.value), text: t.text, loc: this.span(start) };
      case "string": {
        this.advance();
        let value = String(t.value);
        // adjacent string literal concatenation
        while (this.cur.kind === "string") value += String(this.advance().value);
        return { kind: "StringLit", value, loc: this.span(start) };
      }
      case "ident":
        this.advance();
        return { kind: "Ident", name: t.text, loc: this.span(start) };
      case "kw":
        if (t.text === "this") {
          this.advance();
          return { kind: "This", loc: this.span(start) };
        }
        break;
      case "punct":
        if (t.text === "(") {
          this.advance();
          const e = this.parseExpr();
          this.expectPunct(")");
          return e;
        }
        break;
      default:
        break;
    }
    this.err(`expected an expression, found '${this.describe(t)}'`);
    if (!this.atEof && !this.isPunct(";") && !this.isPunct(")") && !this.isPunct("}")) this.advance();
    return { kind: "IntLit", value: 0, loc: this.span(start) };
  }
}

export function parse(tokens: Token[], diags: DiagBag, name = "source.can"): A.Unit {
  return new Parser(tokens, diags).parseUnit(name);
}
