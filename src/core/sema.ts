/* capl-sema — semantic passes (mirror of crates/capl-sema).
 * Pass 1: collect global declarations (variables blocks, functions, handlers)
 * Pass 2: analyse function & handler bodies, decorating every expression
 *         with a resolved symbol and a type.
 * Backends consume the decorated tree and never resolve anything. */

import type { DiagBag, Loc } from "./diagnostics";
import * as A from "./ast";

export interface BuiltinSig {
  name: string;
  ret: A.Ty;
  /** parameter types; `null` entry = any scalar; variadic when `variadic` */
  params: (A.Ty | null)[];
  variadic?: boolean;
}

const B = (name: string, ret: A.Ty, params: (A.Ty | null)[], variadic = false): BuiltinSig =>
  ({ name, ret, params, variadic });

export const BUILTIN_SIGS: Record<string, BuiltinSig> = Object.fromEntries(
  [
    B("write", A.T.void, [null], true),
    B("writeLineEx", A.T.void, [null, null, null], true),
    B("setTimer", A.T.void, [null, A.T.long]),
    B("setTimerCyclic", A.T.void, [null, A.T.long]),
    B("cancelTimer", A.T.void, [null]),
    B("output", A.T.void, [null]),
    B("timeNow", A.T.long, []),
    B("random", A.T.long, [A.T.long]),
    B("elCount", A.T.long, [null]),
    B("abs", A.T.double, [null]),
    B("sqrt", A.T.double, [null]),
    B("pow", A.T.double, [null, null]),
    B("sin", A.T.double, [null]),
    B("cos", A.T.double, [null]),
    B("floor", A.T.double, [null]),
    B("ceil", A.T.double, [null]),
    B("strlen", A.T.long, [null]),
    B("canOnline", A.T.void, []),
    B("canOffline", A.T.void, []),
    B("stopMeasurement", A.T.void, []),
    B("min", A.T.double, [null, null]),
    B("max", A.T.double, [null, null]),
  ].map((s) => [s.name, s]),
);

/** message member fields: name → [byteOffset, type] */
export const MSG_FIELDS: Record<string, [number, A.Ty]> = {
  id: [0, A.T.dword],
  dlc: [4, A.T.long],
  can: [16, A.T.long],
  dir: [20, A.T.long],
  rtr: [24, A.T.long],
  time: [28, A.T.long],
};

export const MSG_ACCESSORS: Record<string, { size: number; ty: A.Ty }> = {
  byte: { size: 1, ty: A.T.byte },
  word: { size: 2, ty: { kind: "word" } },
  dword: { size: 4, ty: A.T.dword },
  long: { size: 4, ty: A.T.long },
  qword: { size: 8, ty: { kind: "qword" } },
};

export const MSG_DATA_OFFSET = 8;
export const MSG_SIZE = 32;

export interface SemaResult {
  unit: A.Unit;
  globals: A.VarSym[];
  funcs: A.FuncSym[];
  handlers: A.HandlerDecl[];
  timers: A.VarSym[];
  messages: A.VarSym[];
  /** every function/handler's local symbols, in declaration order */
  localsOf: Map<A.FuncDecl | A.HandlerDecl, A.VarSym[]>;
}

class Scope {
  readonly map = new Map<string, A.Sym>();
  constructor(readonly parent: Scope | null) {}
  lookup(n: string): A.Sym | undefined {
    return this.map.get(n) ?? this.parent?.lookup(n);
  }
  declareHere(n: string, s: A.Sym): boolean {
    if (this.map.has(n)) return false;
    this.map.set(n, s);
    return true;
  }
}

export class Sema {
  private globalScope = new Scope(null);
  private scope = this.globalScope;
  private curRet: A.Ty = A.T.void;
  private curThis: A.Ty | null = null;
  private loopDepth = 0;
  private switchDepth = 0;
  private timerCounter = 0;

  readonly globals: A.VarSym[] = [];
  readonly funcs: A.FuncSym[] = [];
  readonly handlers: A.HandlerDecl[] = [];
  readonly timers: A.VarSym[] = [];
  readonly messages: A.VarSym[] = [];
  readonly localsOf = new Map<A.FuncDecl | A.HandlerDecl, A.VarSym[]>();
  private curLocals: A.VarSym[] = [];

  constructor(private readonly diags: DiagBag) {}

  run(unit: A.Unit): SemaResult {
    // ── pass 1: declarations ──
    // struct and enum definitions come first so later declarations can use them
    const declareType = (d: A.Decl): void => {
      if (d.kind === "StructDecl") {
        d.ty = this.layoutStruct(d.fields, d.name);
        if (d.name) {
          if (this.structDefs.has(d.name)) {
            this.diags.error("sema", d.loc, `struct '${d.name}' is already declared`);
          } else this.structDefs.set(d.name, d);
        }
      } else if (d.kind === "EnumDecl") {
        this.declareEnum(d);
      }
    };
    // every type definition is published before any variable is declared
    for (const d of unit.decls) {
      declareType(d);
      if (d.kind === "VariablesBlock") for (const td of d.types ?? []) declareType(td);
    }
    for (const d of unit.decls) {
      if (d.kind === "VariablesBlock") {
        for (const v of d.decls) this.declareGlobal(v);
      } else if (d.kind === "VarDecl") {
        this.diags.warn("sema", d.loc, `'${d.name}' declared outside a variables{} block`);
        this.declareGlobal(d);
      } else if (d.kind === "FuncDecl") {
        const sym: A.FuncSym = {
          kind: "func",
          name: d.name,
          ret: d.retTy,
          params: d.params.map((p) => p.declTy),
          decl: d,
        };
        d.sym = sym;
        if (BUILTIN_SIGS[d.name]) {
          this.diags.error("sema", d.loc, `'${d.name}' shadows a CAPL built-in function`);
        } else if (!this.globalScope.declareHere(d.name, sym)) {
          this.diags.error("sema", d.loc, `function '${d.name}' is already declared`);
        }
        this.funcs.push(sym);
      } else if (d.kind === "HandlerDecl") {
        d.exportName = this.mangleHandler(d);
        if (this.handlers.some((h) => h.exportName === d.exportName)) {
          this.diags.error("sema", d.loc, `duplicate event handler '${d.exportName.replace(/_/g, " ")}'`);
        }
        this.handlers.push(d);
      } else if (d.kind === "IncludesBlock") {
        if (d.text.trim().length > 0) {
          this.diags.warn("sema", d.loc, "includes{} is ignored: the playground compiles a single translation unit");
        }
      }
    }

    // ── pass 2: bodies ──
    for (const d of unit.decls) {
      if (d.kind === "FuncDecl") this.analyzeFunction(d);
      else if (d.kind === "HandlerDecl") this.analyzeHandler(d);
    }

    if (this.handlers.length === 0 && this.funcs.length === 0) {
      this.diags.warn("sema", { line: 1, col: 1, pos: 0, end: 0 }, "translation unit declares no event handler");
    }

    return {
      unit,
      globals: this.globals,
      funcs: this.funcs,
      handlers: this.handlers,
      timers: this.timers,
      messages: this.messages,
      localsOf: this.localsOf,
    };
  }

  private mangleHandler(d: A.HandlerDecl): string {
    const e = d.event;
    switch (e.kind) {
      case "timer": return `on_timer_${e.name ?? "anon"}`;
      case "message":
        if (e.wildcard) return "on_message_any";
        if (e.id !== undefined) return `on_message_0x${e.id.toString(16)}`;
        return `on_message_${e.name ?? "anon"}`;
      case "key":
        if (e.wildcard) return "on_key_any";
        return `on_key_${(e.key ?? "?").charCodeAt(0)}`;
      case "sysvar": return `on_sysvar_${(e.name ?? "v").replace(/[^A-Za-z0-9_]/g, "_")}`;
      case "envVar": return `on_envvar_${e.name ?? "v"}`;
      default: return `on_${e.kind}`;
    }
  }

  /* ── type resolution (struct tags, enums) ──────────── */
  private structDefs = new Map<string, A.StructDecl>();
  private enumDefs = new Map<string, A.EnumDecl>();

  /** lays out a struct: assigns member offsets and the total size */
  private layoutStruct(fields: A.StructField[], name: string): A.Ty {
    const members: A.StructMember[] = [];
    let off = 0;
    let maxAlign = 1;
    for (const f of fields) {
      let ty = this.resolveTy(f.declTy, f.loc);
      for (let i = f.dims.length - 1; i >= 0; i--) {
        const n = this.constFold(f.dims[i]);
        if (n === null || n <= 0) {
          this.diags.error("sema", f.loc, `array length of '${f.name}' must be a positive constant`);
          ty = A.arrayOf(ty, 1);
        } else ty = A.arrayOf(ty, n);
      }
      const size = A.sizeOf(ty);
      const align = A.alignOf(ty);
      off = Math.ceil(off / align) * align;
      members.push({ name: f.name, ty, offset: off });
      off += size;
      maxAlign = Math.max(maxAlign, align);
    }
    const size = Math.ceil(Math.max(off, 1) / maxAlign) * maxAlign;
    return { kind: "struct", name, members, size };
  }

  /** resolves `struct S` tags and validates enums; everything else is returned as-is */
  private resolveTy(ty: A.Ty, loc: Loc): A.Ty {
    if (ty.kind === "struct") {
      // anonymous `struct { … } v;` — the fields travel with the type
      if (ty.fields && ty.fields.length > 0) return this.layoutStruct(ty.fields, ty.name ?? "");
      if (ty.members && ty.members.length > 0) {
        const src = this.structDefs.get(ty.name ?? "");
        if (src && src.ty) return src.ty;
        return ty;
      }
      const def = this.structDefs.get(ty.name ?? "");
      if (!def) {
        this.diags.error("sema", loc, `unknown struct type '${ty.name ?? "<anonymous>"}'`);
        return A.T.error;
      }
      return def.ty ?? A.T.error;
    }
    if (ty.kind === "enum") {
      if (ty.name && !this.enumDefs.has(ty.name)) {
        this.diags.error("sema", loc, `unknown enum type '${ty.name}'`);
        return A.T.error;
      }
      return ty;
    }
    return ty;
  }

  /** declares every enum member as a compile-time constant */
  private declareEnum(d: A.EnumDecl): void {
    if (d.name) {
      if (this.enumDefs.has(d.name)) {
        this.diags.error("sema", d.loc, `enum '${d.name}' is already declared`);
      } else this.enumDefs.set(d.name, d);
    }
    let next = 0;
    const seen = new Set<string>();
    for (const it of d.items) {
      if (it.init) {
        const v = this.constFold(it.init);
        if (v === null) {
          this.diags.error("sema", it.loc, `value of '${it.name}' must be a constant expression`);
          next = 0;
        } else next = v;
      }
      it.resolved = next;
      next++;
      if (!seen.has(it.name)) seen.add(it.name);
      else this.diags.error("sema", it.loc, `duplicate enum member '${it.name}'`);
      this.declareConst(it.name, it.resolved, it.loc);
    }
  }

  private declareConst(name: string, value: number, loc: Loc): void {
    const decl: A.VarDecl = {
      kind: "VarDecl",
      name,
      declTy: A.T.long,
      dims: [],
      isConst: true,
      init: { kind: "IntLit", value, loc },
      loc,
    };
    const sym: A.VarSym = {
      kind: "var", name, ty: A.T.long, storage: "global", decl,
      constValue: value,
    };
    decl.sym = sym;
    if (!this.globalScope.declareHere(name, sym)) {
      this.diags.error("sema", loc, `'${name}' is already declared`);
    }
    this.globals.push(sym);
  }

  /* ── declarations ───────────────────────────────────── */
  private declareGlobal(v: A.VarDecl): void {
    const sym = this.makeVarSym(v, "global");
    if (!this.globalScope.declareHere(v.name, sym)) {
      this.diags.error("sema", v.loc, `'${v.name}' is already declared`);
    }
    this.globals.push(sym);
    if (sym.ty.kind === "timer" || sym.ty.kind === "msTimer") {
      sym.timerId = this.timerCounter++;
      this.timers.push(sym);
    }
    if (sym.ty.kind === "message") this.messages.push(sym);
    this.checkInit(v, sym);
  }

  private makeVarSym(v: A.VarDecl, storage: "global" | "local"): A.VarSym {
    let ty: A.Ty = this.resolveTy(v.declTy, v.loc);
    if (v.dims.length > 0) {
      // outermost dimension may be inferred from the initialiser
      const dims = [...v.dims];
      if (dims[0] === undefined) dims[0] = { kind: "IntLit", value: 1, loc: v.loc };
      const first = this.constFold(dims[0]);
      let firstLen = 0;
      if (first !== null && first > 0) firstLen = first;
      else if (v.init?.kind === "InitList") firstLen = v.init.items.length;
      else if (v.init?.kind === "StringLit") firstLen = v.init.value.length + 1;

      if (firstLen <= 0) {
        this.diags.error("sema", v.loc, `cannot deduce the length of array '${v.name}'`);
        firstLen = 1;
      }
      ty = A.arrayOf(ty, firstLen);
      for (let i = dims.length - 1; i >= 1; i--) {
        const n = this.constFold(dims[i]);
        if (n === null || n <= 0) {
          this.diags.error("sema", v.loc, `array length of '${v.name}' must be a positive constant`);
          ty = A.arrayOf(ty, 1);
        } else ty = A.arrayOf(ty, n);
      }
    }
    const sym: A.VarSym = { kind: "var", name: v.name, ty, storage, decl: v };
    v.sym = sym;
    return sym;
  }

  private checkInit(v: A.VarDecl, sym: A.VarSym): void {
    if (!v.init) return;
    if (v.init.kind === "InitList") {
      if (sym.ty.kind === "array") {
        const max = sym.ty.len ?? 0;
        if (v.init.items.length > max) {
          this.diags.error("sema", v.loc, `too many initialisers for '${v.name}' (${v.init.items.length} > ${max})`);
        }
      } else if (sym.ty.kind === "struct") {
        const max = sym.ty.members?.length ?? 0;
        if (v.init.items.length > max) {
          this.diags.error("sema", v.loc, `too many initialisers for '${v.name}' (${v.init.items.length} > ${max})`);
        }
      } else {
        this.diags.error("sema", v.loc, `'${v.name}' is not an aggregate but has a brace initialiser`);
        return;
      }
      for (const it of v.init.items) this.expr(it);
      v.init.ty = sym.ty;
      return;
    }
    if (v.init.kind === "StringLit") {
      if (sym.ty.kind !== "array" || (sym.ty.elem?.kind !== "char" && sym.ty.elem?.kind !== "byte")) {
        this.diags.error("sema", v.loc, `string initialiser requires a char array`);
      } else if (v.init.value.length + 1 > (sym.ty.len ?? 0)) {
        this.diags.error("sema", v.loc, `string initialiser does not fit into '${v.name}'`);
      }
      this.expr(v.init);
      return;
    }
    const t = this.expr(v.init);
    if (sym.ty.kind === "array") {
      this.diags.error("sema", v.loc, `cannot initialise array '${v.name}' from a scalar`);
    } else if (!A.isScalarTy(t) && t.kind !== "error") {
      this.diags.error("sema", v.loc, `cannot initialise '${v.name}' from a value of type ${A.tyName(t)}`);
    }
  }

  /* ── bodies ─────────────────────────────────────────── */
  private analyzeFunction(d: A.FuncDecl): void {
    this.curRet = d.retTy;
    this.curThis = null;
    this.curLocals = [];
    this.scope = new Scope(this.globalScope);
    for (const p of d.params) {
      const sym: A.VarSym = { kind: "var", name: p.name, ty: p.declTy, storage: "param", decl: p };
      p.sym = sym;
      if (!this.scope.declareHere(p.name, sym)) {
        this.diags.error("sema", p.loc, `duplicate parameter '${p.name}'`);
      }
    }
    this.block(d.body);
    this.localsOf.set(d, this.curLocals);
    this.scope = this.globalScope;
  }

  private analyzeHandler(d: A.HandlerDecl): void {
    this.curRet = A.T.void;
    this.curThis =
      d.event.kind === "message" ? { kind: "message" }
      : d.event.kind === "key" ? A.T.dword
      : d.event.kind === "sysvar" || d.event.kind === "envVar" ? A.T.long
      : null;
    this.curLocals = [];
    this.scope = new Scope(this.globalScope);
    this.block(d.body);
    this.localsOf.set(d, this.curLocals);
    this.scope = this.globalScope;
  }

  private block(b: A.Block): void {
    const saved = this.scope;
    this.scope = new Scope(saved);
    for (const s of b.stmts) this.stmt(s);
    this.scope = saved;
  }

  private stmt(s: A.Stmt): void {
    switch (s.kind) {
      case "Block": this.block(s); break;
      case "ExprStmt": this.expr(s.expr); break;
      case "Empty": break;
      case "DeclStmt":
        // local `enum`/`struct` definitions are visible from here on
        for (const td of s.types ?? []) {
          if (td.kind === "StructDecl") {
            td.ty = this.layoutStruct(td.fields, td.name);
            if (td.name) {
              if (this.structDefs.has(td.name)) {
                this.diags.error("sema", td.loc, `struct '${td.name}' hides an outer declaration`);
              } else this.structDefs.set(td.name, td);
            }
          } else if (td.kind === "EnumDecl") {
            this.declareEnum(td);
          }
        }
        for (const v of s.decls) {
          const sym = this.makeVarSym(v, "local");
          if (!this.scope.declareHere(v.name, sym)) {
            this.diags.error("sema", v.loc, `'${v.name}' is already declared in this scope`);
          }
          if (sym.ty.kind === "timer" || sym.ty.kind === "msTimer") {
            sym.timerId = this.timerCounter++;
            this.timers.push(sym);
          }
          this.curLocals.push(sym);
          this.checkInit(v, sym);
        }
        break;
      case "If":
        this.condition(s.cond);
        this.stmt(s.then);
        if (s.otherwise) this.stmt(s.otherwise);
        break;
      case "While":
        this.condition(s.cond);
        this.loopDepth++;
        this.stmt(s.body);
        this.loopDepth--;
        break;
      case "DoWhile":
        this.loopDepth++;
        this.stmt(s.body);
        this.loopDepth--;
        this.condition(s.cond);
        break;
      case "For": {
        const saved = this.scope;
        this.scope = new Scope(saved);
        if (s.init) this.stmt(s.init);
        if (s.cond) this.condition(s.cond);
        if (s.step) this.expr(s.step);
        this.loopDepth++;
        this.stmt(s.body);
        this.loopDepth--;
        this.scope = saved;
        break;
      }
      case "Switch": {
        const dt = this.expr(s.disc);
        if (!A.isIntegralTy(dt) && dt.kind !== "error") {
          this.diags.error("sema", s.loc, `switch expression must be integral, got ${A.tyName(dt)}`);
        }
        let defaults = 0;
        const seen = new Set<number>();
        this.switchDepth++;
        for (const c of s.clauses) {
          if (c.test) {
            const v = this.constFold(c.test);
            this.expr(c.test);
            if (v === null) this.diags.error("sema", c.loc, "case label must be a constant expression");
            else if (seen.has(v)) this.diags.error("sema", c.loc, `duplicate case label ${v}`);
            else seen.add(v);
          } else defaults++;
          const saved = this.scope;
          this.scope = new Scope(saved);
          for (const st of c.body) this.stmt(st);
          this.scope = saved;
        }
        this.switchDepth--;
        if (defaults > 1) this.diags.error("sema", s.loc, "more than one 'default' label in switch");
        break;
      }
      case "Break":
        if (this.loopDepth === 0 && this.switchDepth === 0) {
          this.diags.error("sema", s.loc, "'break' outside of a loop or switch");
        }
        break;
      case "Continue":
        if (this.loopDepth === 0) this.diags.error("sema", s.loc, "'continue' outside of a loop");
        break;
      case "Return":
        if (s.expr) {
          const t = this.expr(s.expr);
          if (this.curRet.kind === "void") {
            this.diags.error("sema", s.loc, "cannot return a value from a void function");
          } else if (!A.isScalarTy(t) && t.kind !== "error") {
            this.diags.error("sema", s.loc, `cannot return a value of type ${A.tyName(t)}`);
          }
        } else if (this.curRet.kind !== "void") {
          this.diags.error("sema", s.loc, `function must return a value of type ${A.tyName(this.curRet)}`);
        }
        break;
    }
  }

  private condition(e: A.Expr): void {
    const t = this.expr(e);
    if (!A.isScalarTy(t) && t.kind !== "error") {
      this.diags.error("sema", e.loc, `condition must be a scalar, got ${A.tyName(t)}`);
    }
  }

  /* ── expressions ────────────────────────────────────── */
  expr(e: A.Expr): A.Ty {
    const t = this.exprImpl(e);
    e.ty = t;
    return t;
  }

  private exprImpl(e: A.Expr): A.Ty {
    switch (e.kind) {
      case "IntLit": return Math.abs(e.value) > 0x7fffffff ? { kind: "int64" } : A.T.long;
      case "FloatLit": return A.T.double;
      case "CharLit": return A.T.char;
      case "StringLit": return A.arrayOf(A.T.char, e.value.length + 1);
      case "InitList":
        for (const it of e.items) this.expr(it);
        return A.T.error;
      case "This":
        if (!this.curThis) {
          this.diags.error("sema", e.loc, "'this' is only valid inside a message, key or sysvar handler");
          return A.T.error;
        }
        return this.curThis;
      case "Ident": {
        const sym = this.scope.lookup(e.name);
        if (!sym) {
          if (BUILTIN_SIGS[e.name]) {
            e.sym = { kind: "builtin", name: e.name, ret: BUILTIN_SIGS[e.name].ret, arity: -1 };
            return A.T.error;
          }
          this.diags.error("sema", e.loc, `undefined identifier '${e.name}'`);
          return A.T.error;
        }
        e.sym = sym;
        if (sym.kind === "var") {
          sym.used = true;
          return sym.ty;
        }
        return A.T.error;
      }
      case "Index": {
        const bt = this.expr(e.base);
        const it = this.expr(e.index);
        if (!A.isIntegralTy(it) && it.kind !== "error") {
          this.diags.error("sema", e.index.loc, "array subscript must be an integer");
        }
        if (bt.kind !== "array") {
          if (bt.kind !== "error") this.diags.error("sema", e.loc, `cannot index a value of type ${A.tyName(bt)}`);
          return A.T.error;
        }
        return bt.elem ?? A.T.error;
      }
      case "Member": {
        const bt = this.expr(e.base);
        if (bt.kind === "message") {
          const f = MSG_FIELDS[e.name];
          if (f) {
            e.offset = f[0];
            return f[1];
          }
          if (MSG_ACCESSORS[e.name]) return A.T.error; // handled by Call
          this.diags.error("sema", e.loc, `message has no member '${e.name}'`);
          return A.T.error;
        }
        if (bt.kind === "struct") {
          const m = bt.members?.find((x) => x.name === e.name);
          if (!m) {
            this.diags.error("sema", e.loc, `struct ${bt.name ?? ""} has no member '${e.name}'`);
            return A.T.error;
          }
          e.offset = m.offset;
          return m.ty;
        }
        if (bt.kind !== "error") {
          this.diags.error("sema", e.loc, `type ${A.tyName(bt)} has no member '${e.name}'`);
        }
        return A.T.error;
      }
      case "Unary": {
        const t = this.expr(e.operand);
        if (e.op === "!") return A.T.long;
        if (e.op === "++" || e.op === "--") {
          this.requireLvalue(e.operand);
          return t;
        }
        if (e.op === "~" && A.isFloatTy(t)) {
          this.diags.error("sema", e.loc, "operator '~' requires an integral operand");
          return A.T.error;
        }
        return this.promote(t);
      }
      case "Binary": {
        const lt = this.expr(e.left);
        const rt = this.expr(e.right);
        if (e.op === ",") return rt;
        if (lt.kind === "error" || rt.kind === "error") return A.T.error;
        const cmp = ["==", "!=", "<", ">", "<=", ">=", "&&", "||"].includes(e.op);
        if (A.isAggregateTy(lt) || A.isAggregateTy(rt)) {
          this.diags.error("sema", e.loc, `operator '${e.op}' cannot be applied to ${A.tyName(lt)} and ${A.tyName(rt)}`);
          return A.T.error;
        }
        if (["%", "&", "|", "^", "<<", ">>"].includes(e.op) && (A.isFloatTy(lt) || A.isFloatTy(rt))) {
          this.diags.error("sema", e.loc, `operator '${e.op}' requires integral operands`);
          return A.T.error;
        }
        if (cmp) return A.T.long;
        return this.usualConversion(lt, rt);
      }
      case "Cond": {
        this.condition(e.cond);
        const a = this.expr(e.then);
        const b = this.expr(e.otherwise);
        if (a.kind === "error" || b.kind === "error") return A.T.error;
        return this.usualConversion(a, b);
      }
      case "Assign": {
        const lt = this.expr(e.target);
        const rt = this.expr(e.value);
        this.requireLvalue(e.target);
        if (lt.kind === "array" || lt.kind === "struct" || lt.kind === "message") {
          this.diags.error(
            "sema", e.loc,
            lt.kind === "struct"
              ? `whole-struct assignment is not supported yet — copy '${A.tyName(lt)}' member by member`
              : `cannot assign to ${A.tyName(lt)}`,
          );
          return A.T.error;
        }

        if (rt.kind !== "error" && !A.isScalarTy(rt)) {
          this.diags.error("sema", e.loc, `cannot assign a value of type ${A.tyName(rt)}`);
        }
        if (e.op !== "=" && (A.isFloatTy(lt) || A.isFloatTy(rt)) && ["%=", "&=", "|=", "^=", "<<=", ">>="].includes(e.op)) {
          this.diags.error("sema", e.loc, `operator '${e.op}' requires integral operands`);
        }
        return lt;
      }
      case "Call": return this.call(e);
    }
  }

  private call(e: A.CallExpr): A.Ty {
    // message accessor: m.byte(0) / this.dword(4)
    if (e.callee.kind === "Member") {
      const acc = MSG_ACCESSORS[e.callee.name];
      const bt = this.expr(e.callee.base);
      if (bt.kind === "message" && acc) {
        e.callee.ty = bt;
        if (e.args.length !== 1) {
          this.diags.error("sema", e.loc, `'${e.callee.name}()' takes exactly one index`);
        }
        for (const a of e.args) this.expr(a);
        return acc.ty;
      }
      if (bt.kind !== "error") {
        this.diags.error("sema", e.loc, `type ${A.tyName(bt)} has no method '${e.callee.name}'`);
      }
      for (const a of e.args) this.expr(a);
      return A.T.error;
    }

    if (e.callee.kind !== "Ident") {
      this.diags.error("sema", e.loc, "call target is not a function");
      for (const a of e.args) this.expr(a);
      return A.T.error;
    }
    const name = e.callee.name;
    const sym = this.scope.lookup(name);
    const bi = BUILTIN_SIGS[name];

    if (!sym && bi) {
      const bsym: A.BuiltinSym = { kind: "builtin", name, ret: bi.ret, arity: bi.variadic ? -1 : bi.params.length };
      e.sym = bsym;
      e.callee.sym = bsym;
      e.callee.ty = bi.ret;
      const argTys = e.args.map((a) => this.expr(a));
      if (!bi.variadic && argTys.length !== bi.params.length) {
        this.diags.error("sema", e.loc, `'${name}' expects ${bi.params.length} argument(s), got ${argTys.length}`);
      }
      if (bi.variadic && argTys.length < bi.params.length) {
        this.diags.error("sema", e.loc, `'${name}' expects at least ${bi.params.length} argument(s)`);
      }
      // a few hand-checked signatures
      if (name === "write" && e.args[0] && e.args[0].kind !== "StringLit" && argTys[0]?.kind !== "array") {
        this.diags.error("sema", e.args[0].loc, "the first argument of write() must be a format string");
      }
      if ((name === "setTimer" || name === "setTimerCyclic" || name === "cancelTimer")) {
        const t0 = argTys[0];
        if (t0 && t0.kind !== "timer" && t0.kind !== "msTimer" && t0.kind !== "error") {
          this.diags.error("sema", e.args[0].loc, `'${name}' expects a timer, got ${A.tyName(t0)}`);
        }
      }
      if (name === "output") {
        const t0 = argTys[0];
        if (t0 && t0.kind !== "message" && t0.kind !== "error") {
          this.diags.error("sema", e.args[0].loc, `'output' expects a message, got ${A.tyName(t0)}`);
        }
      }
      if (name === "elCount") {
        const t0 = argTys[0];
        if (t0 && t0.kind !== "array" && t0.kind !== "error") {
          this.diags.error("sema", e.args[0].loc, "'elCount' expects an array");
        }
      }
      if (name === "abs" || name === "min" || name === "max") {
        const ts = argTys.filter((t) => t.kind !== "error");
        if (ts.length && ts.every((t) => A.isIntegralTy(t))) return this.promote(ts[0]);
      }
      return bi.ret;
    }

    if (!sym) {
      this.diags.error("sema", e.loc, `call to undeclared function '${name}'`);
      for (const a of e.args) this.expr(a);
      return A.T.error;
    }
    if (sym.kind !== "func") {
      this.diags.error("sema", e.loc, `'${name}' is not a function`);
      for (const a of e.args) this.expr(a);
      return A.T.error;
    }
    e.sym = sym;
    e.callee.sym = sym;
    const argTys = e.args.map((a) => this.expr(a));
    if (argTys.length !== sym.params.length) {
      this.diags.error(
        "sema", e.loc,
        `'${name}' expects ${sym.params.length} argument(s), got ${argTys.length}`,
      );
    } else {
      sym.params.forEach((pt, i) => {
        const at = argTys[i];
        if (at.kind === "error") return;
        if (pt.kind === "struct" || pt.kind === "message") {
          this.diags.error("sema", e.args[i].loc, `passing ${A.tyName(pt)} by value is not supported yet`);
        } else if (pt.kind === "array" && at.kind !== "array") {
          this.diags.error("sema", e.args[i].loc, `argument ${i + 1} of '${name}' must be an array`);
        } else if (pt.kind !== "array" && !A.isScalarTy(at) && pt.kind !== at.kind) {
          this.diags.error("sema", e.args[i].loc, `argument ${i + 1} of '${name}': cannot convert ${A.tyName(at)} to ${A.tyName(pt)}`);
        }
      });
    }
    return sym.ret;
  }

  private requireLvalue(e: A.Expr): void {
    const ok =
      (e.kind === "Ident" && e.sym?.kind === "var") ||
      e.kind === "Index" ||
      e.kind === "Member" ||
      (e.kind === "Call" && e.callee.kind === "Member") ||
      e.kind === "This";
    if (!ok) this.diags.error("sema", e.loc, "expression is not assignable");
    if (e.kind === "Ident" && e.sym?.kind === "var") {
      const d = e.sym.decl;
      if (d.kind === "VarDecl" && d.isConst) {
        this.diags.error("sema", e.loc, `cannot assign to constant '${e.name}'`);
      }
    }
  }

  private promote(t: A.Ty): A.Ty {
    if (A.isFloatTy(t)) return A.T.double;
    if (A.is64Ty(t)) return t;
    if (t.kind === "dword" || t.kind === "qword") return t;
    return A.T.long;
  }

  private usualConversion(a: A.Ty, b: A.Ty): A.Ty {
    if (A.isFloatTy(a) || A.isFloatTy(b)) return A.T.double;
    if (A.is64Ty(a) || A.is64Ty(b)) {
      return a.kind === "qword" || b.kind === "qword" ? { kind: "qword" } : { kind: "int64" };
    }
    if (a.kind === "dword" || b.kind === "dword") return A.T.dword;
    return A.T.long;
  }

  /** constant folding used for array lengths & case labels */
  constFold(e: A.Expr): number | null {
    switch (e.kind) {
      case "IntLit": case "CharLit": return e.value;
      case "FloatLit": return Math.trunc(e.value);
      case "Unary": {
        const v = this.constFold(e.operand);
        if (v === null) return null;
        switch (e.op) {
          case "-": return -v;
          case "+": return v;
          case "!": return v ? 0 : 1;
          case "~": return ~v;
          default: return null;
        }
      }
      case "Binary": {
        const l = this.constFold(e.left);
        const r = this.constFold(e.right);
        if (l === null || r === null) return null;
        switch (e.op) {
          case "+": return l + r;
          case "-": return l - r;
          case "*": return l * r;
          case "/": return r === 0 ? null : Math.trunc(l / r);
          case "%": return r === 0 ? null : l % r;
          case "<<": return l << r;
          case ">>": return l >> r;
          case "&": return l & r;
          case "|": return l | r;
          case "^": return l ^ r;
          default: return null;
        }
      }
      case "Ident": {
        const sym = this.scope.lookup(e.name);
        if (sym?.kind === "var" && sym.decl.kind === "VarDecl" && sym.decl.isConst && sym.decl.init) {
          return this.constFold(sym.decl.init);
        }
        return null;
      }
      default: return null;
    }
  }
}

export function analyze(unit: A.Unit, diags: DiagBag): SemaResult {
  return new Sema(diags).run(unit);
}

export function locKey(l: Loc): string {
  return `${l.line}:${l.col}`;
}
