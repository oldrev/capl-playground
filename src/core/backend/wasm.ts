/* capl-backend-wasm — emits a real WebAssembly 1.0 module from the decorated AST
 * (mirror of crates/capl-backend-wasm).
 *
 * Memory image
 *   0x0000  null guard
 *   0x0040  vararg marshalling area (32 slots × 16 B) used by write()
 *   0x0400  string pool, globals, message objects, function-static arrays
 *
 * Host interface: everything the program cannot do itself is an import from
 * module "capl"; the simulator in src/runtime/ provides them.
 */

import * as A from "../ast";
import type { DiagBag } from "../diagnostics";
import type { SemaResult } from "../sema";
import { MSG_ACCESSORS, MSG_DATA_OFFSET, MSG_SIZE } from "../sema";

/* ── value types & opcodes ──────────────────────────────── */
const I32 = 0x7f, I64 = 0x7e, F64 = 0x7c;

const VARARG_BASE = 0x40;
const VARARG_SLOTS = 32;
const DATA_BASE = 0x400;

export const TAG_I32_S = 0, TAG_I32_U = 1, TAG_I64_S = 2, TAG_I64_U = 3, TAG_F64 = 4, TAG_STR = 5;

interface ImportSig {
  field: string;
  params: number[];
  ret: number | null;
}

export const WASM_IMPORTS: ImportSig[] = [
  { field: "write", params: [I32, I32, I32], ret: null },
  { field: "setTimer", params: [I32, I32], ret: null },
  { field: "setTimerCyclic", params: [I32, I32], ret: null },
  { field: "cancelTimer", params: [I32], ret: null },
  { field: "output", params: [I32], ret: null },
  { field: "timeNow", params: [], ret: I32 },
  { field: "random", params: [I32], ret: I32 },
  { field: "strlen", params: [I32], ret: I32 },
  { field: "pow", params: [F64, F64], ret: F64 },
  { field: "sin", params: [F64], ret: F64 },
  { field: "cos", params: [F64], ret: F64 },
  { field: "canOnline", params: [], ret: null },
  { field: "canOffline", params: [], ret: null },
  { field: "stopMeasurement", params: [], ret: null },
];
const IMP = Object.fromEntries(WASM_IMPORTS.map((s, i) => [s.field, i]));

/* ── LEB128 / binary helpers ────────────────────────────── */
function uleb(v: number, out: number[]): void {
  let x = v >>> 0;
  do {
    let b = x & 0x7f;
    x >>>= 7;
    if (x !== 0) b |= 0x80;
    out.push(b);
  } while (x !== 0);
}
function sleb(value: number, out: number[]): void {
  let v = value | 0;
  for (;;) {
    const b = v & 0x7f;
    const sign = (b & 0x40) !== 0;
    v >>= 7;
    if ((v === 0 && !sign) || (v === -1 && sign)) {
      out.push(b);
      return;
    }
    out.push(b | 0x80);
  }
}
function sleb64(value: bigint, out: number[]): void {
  let v = value;
  for (;;) {
    const b = Number(v & 0x7fn);
    const sign = (b & 0x40) !== 0;
    v >>= 7n;
    if ((v === 0n && !sign) || (v === -1n && sign)) {
      out.push(b);
      return;
    }
    out.push(b | 0x80);
  }
}
function f64bytes(v: number): number[] {
  const buf = new DataView(new ArrayBuffer(8));
  buf.setFloat64(0, v, true);
  return Array.from(new Uint8Array(buf.buffer));
}
function str(s: string, out: number[]): void {
  const enc = new TextEncoder().encode(s);
  uleb(enc.length, out);
  for (const b of enc) out.push(b);
}
/** chunk-safe concatenation (spread would blow the arg limit on big modules) */
function pushAll(dst: number[], src: ArrayLike<number>): void {
  for (let i = 0; i < src.length; i++) dst.push(src[i]);
}
function section(id: number, payload: number[], out: number[]): void {
  out.push(id);
  uleb(payload.length, out);
  pushAll(out, payload);
}
function vec(items: number[][], out: number[]): void {
  uleb(items.length, out);
  for (const it of items) pushAll(out, it);
}

/* ── per-function code builder (bytes + WAT in lock-step) ── */
class Code {
  bytes: number[] = [];
  wat: string[] = [];
  extraLocals: number[] = [];
  labels: string[] = [];
  private ind = 2;

  constructor(
    readonly name: string,
    readonly params: number[],
    readonly ret: number | null,
    readonly paramNames: string[] = [],
  ) {}

  local(t: number): number {
    this.extraLocals.push(t);
    return this.params.length + this.extraLocals.length - 1;
  }
  private w(t: string): void {
    this.wat.push("  ".repeat(this.ind) + t);
  }
  op(code: number, text: string): void {
    this.bytes.push(code);
    this.w(text);
  }
  opU(code: number, imm: number, text: string): void {
    this.bytes.push(code);
    uleb(imm, this.bytes);
    this.w(text);
  }
  prefixed(sub: number, text: string): void {
    this.bytes.push(0xfc);
    uleb(sub, this.bytes);
    this.w(text);
  }
  mem(code: number, align: number, offset: number, text: string): void {
    this.bytes.push(code);
    uleb(align, this.bytes);
    uleb(offset, this.bytes);
    this.w(offset ? `${text} offset=${offset}` : text);
  }
  i32c(v: number): void {
    this.bytes.push(0x41);
    sleb(v | 0, this.bytes);
    this.w(`i32.const ${v | 0}`);
  }
  i64c(v: bigint): void {
    this.bytes.push(0x42);
    sleb64(v, this.bytes);
    this.w(`i64.const ${v}`);
  }
  f64c(v: number): void {
    this.bytes.push(0x44);
    for (const b of f64bytes(v)) this.bytes.push(b);
    this.w(`f64.const ${v}`);
  }
  blockStart(kind: "block" | "loop" | "if", label: string, result: number | null): void {
    const code = kind === "block" ? 0x02 : kind === "loop" ? 0x03 : 0x04;
    this.bytes.push(code);
    this.bytes.push(result === null ? 0x40 : result);
    this.w(`(${kind} $${label}${result !== null ? ` (result ${vtName(result)})` : ""}`);
    this.labels.push(label);
    this.ind++;
  }
  elseOp(): void {
    this.ind--;
    this.bytes.push(0x05);
    this.w("(else)");
    this.ind++;
  }
  end(): void {
    this.labels.pop();
    this.ind--;
    this.bytes.push(0x0b);
    this.w(")");
  }
  rel(label: string): number {
    for (let i = this.labels.length - 1; i >= 0; i--) {
      if (this.labels[i] === label) return this.labels.length - 1 - i;
    }
    return 0;
  }
  br(label: string): void {
    this.opU(0x0c, this.rel(label), `br $${label}`);
  }
  brIf(label: string): void {
    this.opU(0x0d, this.rel(label), `br_if $${label}`);
  }
  getLocal(i: number): void {
    this.opU(0x20, i, `local.get ${i}`);
  }
  setLocal(i: number): void {
    this.opU(0x21, i, `local.set ${i}`);
  }
  teeLocal(i: number): void {
    this.opU(0x22, i, `local.tee ${i}`);
  }
  getGlobal(i: number, n: string): void {
    this.opU(0x23, i, `global.get $${n}`);
  }
  setGlobal(i: number, n: string): void {
    this.opU(0x24, i, `global.set $${n}`);
  }
  call(i: number, n: string): void {
    this.opU(0x10, i, `call $${n} ;; #${i}`);
  }
  drop(): void {
    this.op(0x1a, "drop");
  }
}

function vtName(v: number | null): string {
  return v === I32 ? "i32" : v === I64 ? "i64" : v === F64 ? "f64" : "void";
}

function vt(t: A.Ty): number {
  if (A.isFloatTy(t)) return F64;
  if (A.is64Ty(t)) return I64;
  return I32;
}

/* ── artifact ───────────────────────────────────────────── */
export interface LayoutEntry {
  name: string;
  addr: number;
  size: number;
  ty: string;
  kind: "string" | "global" | "message" | "static" | "runtime";
}

export interface WasmArtifact {
  bytes: Uint8Array;
  wat: string;
  exports: string[];
  imports: string[];
  memoryPages: number;
  dataSize: number;
  layout: LayoutEntry[];
  msgbufAddr: number;
  funcCount: number;
}

/* ── the backend ────────────────────────────────────────── */
export class WasmBackend {
  private heap = DATA_BASE;
  private data: { addr: number; bytes: number[] }[] = [];
  private strPool = new Map<string, number>();
  private globalAddr = new Map<A.VarSym, number>();
  private staticAddr = new Map<A.VarSym, number>();
  private slotOf = new Map<A.VarSym, number>();
  private funcIdx = new Map<A.FuncSym, number>();
  private codes: Code[] = [];
  private layout: LayoutEntry[] = [];
  private msgbuf = 0;
  private zeroIdx = 0;
  private cur!: Code;
  private curRet: A.Ty = A.T.void;
  private labelN = 0;
  private breakStack: string[] = [];
  private continueStack: string[] = [];
  private dynInit: { sym: A.VarSym; decl: A.VarDecl }[] = [];

  /* global indices */
  private readonly G_THIS = 0;
  private readonly G_FUEL = 1;
  private readonly G_MSGBUF = 2;

  constructor(private readonly sema: SemaResult, private readonly diags: DiagBag) {}

  /* ── allocation ─────────────────────────────────────── */
  private alloc(size: number, align = 8): number {
    const a = Math.max(1, align);
    this.heap = Math.ceil(this.heap / a) * a;
    const addr = this.heap;
    this.heap += Math.max(size, 1);
    return addr;
  }

  private internString(s: string): number {
    const hit = this.strPool.get(s);
    if (hit !== undefined) return hit;
    const enc = new TextEncoder().encode(s);
    const addr = this.alloc(enc.length + 1, 4);
    this.data.push({ addr, bytes: [...enc, 0] });
    this.strPool.set(s, addr);
    this.layout.push({
      name: JSON.stringify(s.length > 24 ? `${s.slice(0, 24)}…` : s),
      addr, size: enc.length + 1, ty: "char[]", kind: "string",
    });
    return addr;
  }

  private scalarBytes(ty: A.Ty, v: number): number[] {
    const size = A.sizeOf(ty);
    const buf = new DataView(new ArrayBuffer(8));
    if (A.isFloatTy(ty)) buf.setFloat64(0, v, true);
    else if (size === 8) buf.setBigInt64(0, BigInt(Math.trunc(v)), true);
    else if (size === 4) buf.setInt32(0, v | 0, true);
    else if (size === 2) buf.setInt16(0, (v << 16) >> 16, true);
    else buf.setInt8(0, (v << 24) >> 24);
    return Array.from(new Uint8Array(buf.buffer)).slice(0, size);
  }

  /** constant folder used for static initialisers & case labels */
  private fold(e: A.Expr): number | null {
    switch (e.kind) {
      case "IntLit": case "CharLit": case "FloatLit": return e.value;
      case "Unary": {
        const v = this.fold(e.operand);
        if (v === null) return null;
        return e.op === "-" ? -v : e.op === "+" ? v : e.op === "!" ? (v ? 0 : 1) : e.op === "~" ? ~v : null;
      }
      case "Binary": {
        const l = this.fold(e.left), r = this.fold(e.right);
        if (l === null || r === null) return null;
        switch (e.op) {
          case "+": return l + r;
          case "-": return l - r;
          case "*": return l * r;
          case "/": return r === 0 ? null : A.isFloatTy(e.ty ?? A.T.long) ? l / r : Math.trunc(l / r);
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
        const s = e.sym;
        if (s?.kind === "var" && s.decl.kind === "VarDecl" && s.decl.isConst && s.decl.init) {
          return this.fold(s.decl.init);
        }
        return null;
      }
      default: return null;
    }
  }

  private layoutGlobals(): void {
    for (const g of this.sema.globals) {
      if (g.ty.kind === "timer" || g.ty.kind === "msTimer") {
        this.layout.push({ name: g.name, addr: -1, size: 0, ty: A.tyName(g.ty), kind: "runtime" });
        continue;
      }
      if (g.constValue !== undefined) {
        // enum members and other compile-time constants never occupy memory
        this.layout.push({ name: g.name, addr: -1, size: 0, ty: "const", kind: "runtime" });
        continue;
      }
      const size = A.sizeOf(g.ty);
      const align = g.ty.kind === "array" ? Math.min(8, A.sizeOf(g.ty.elem ?? A.T.char)) : Math.min(8, size);
      const addr = this.alloc(size, align);
      this.globalAddr.set(g, addr);
      this.layout.push({
        name: g.name, addr, size, ty: A.tyName(g.ty),
        kind: g.ty.kind === "message" ? "message" : "global",
      });
      this.initStatic(g, addr);
    }
    this.msgbuf = this.alloc(MSG_SIZE, 8);
    this.layout.push({ name: "__event_msg", addr: this.msgbuf, size: MSG_SIZE, ty: "message", kind: "runtime" });
  }

  /** writes the constant part of an initialiser into the data image */
  /** bytes of a fully-constant initialiser, or null when it must run at runtime */
  private constInitBytes(ty: A.Ty, init: A.Expr): number[] | null {
    if (init.kind === "InitList") {
      if (ty.kind === "array") {
        const elem = ty.elem ?? A.T.char;
        const esz = A.sizeOf(elem);
        const out: number[] = [];
        const n = Math.min(init.items.length, ty.len ?? 0);
        for (let i = 0; i < n; i++) {
          const part = this.constInitBytes(elem, init.items[i]);
          if (!part) return null;
          while (out.length < i * esz) out.push(0);
          out.push(...part);
        }
        while (out.length < (ty.len ?? 0) * esz) out.push(0);
        return out;
      }
      if (ty.kind === "struct") {
        const out = new Array(Math.max(1, A.sizeOf(ty))).fill(0);
        const members = ty.members ?? [];
        for (let i = 0; i < Math.min(init.items.length, members.length); i++) {
          const part = this.constInitBytes(members[i].ty, init.items[i]);
          if (!part) return null;
          for (let k = 0; k < part.length; k++) out[members[i].offset + k] = part[k];
        }
        return out;
      }
      return null;
    }
    if (init.kind === "StringLit" && ty.kind === "array") {
      const enc = new TextEncoder().encode(init.value);
      return [...enc, 0];
    }
    if (A.isAggregateTy(ty)) return null;
    const v = this.fold(init);
    return v === null ? null : this.scalarBytes(ty, v);
  }

  /** runtime store of an arbitrary initialiser (arrays, structs, nested) */
  private storeInit(addr: number, ty: A.Ty, init: A.Expr): void {
    if (init.kind === "InitList") {
      if (ty.kind === "array") {
        const elem = ty.elem ?? A.T.char;
        const esz = A.sizeOf(elem);
        const n = Math.min(init.items.length, ty.len ?? 0, 512);
        for (let i = 0; i < n; i++) this.storeInit(addr + i * esz, elem, init.items[i]);
        return;
      }
      if (ty.kind === "struct") {
        const members = ty.members ?? [];
        const n = Math.min(init.items.length, members.length);
        for (let i = 0; i < n; i++) this.storeInit(addr + members[i].offset, members[i].ty, init.items[i]);
        return;
      }
      return;
    }
    if (init.kind === "StringLit" && ty.kind === "array") {
      const enc = new TextEncoder().encode(init.value);
      for (let i = 0; i < Math.min(enc.length, (ty.len ?? 1) - 1); i++) {
        this.cur.i32c(addr + i);
        this.cur.i32c(enc[i]);
        this.cur.mem(0x3a, 0, 0, "i32.store8");
      }
      return;
    }
    if (A.isAggregateTy(ty)) return;
    this.cur.i32c(addr);
    const t = this.genExpr(init);
    this.conv(t, ty);
    this.store(ty);
  }

  private initStatic(sym: A.VarSym, addr: number): void {
    const d = sym.decl;
    if (sym.ty.kind === "message") {
      const bytes = new Uint8Array(MSG_SIZE);
      const dv = new DataView(bytes.buffer);
      dv.setUint32(0, (sym.ty.msgId ?? 0) >>> 0, true);
      dv.setInt32(4, 8, true);
      dv.setInt32(16, 1, true);
      this.data.push({ addr, bytes: Array.from(bytes) });
      return;
    }
    if (d.kind !== "VarDecl" || !d.init) return;
    const bytes = this.constInitBytes(sym.ty, d.init);
    if (bytes) {
      this.data.push({ addr, bytes });
      return;
    }
    if (sym.storage === "global") this.dynInit.push({ sym, decl: d });
  }

  /* ── entry point ────────────────────────────────────── */
  compile(): WasmArtifact {
    this.layoutGlobals();

    const base = WASM_IMPORTS.length;
    this.sema.funcs.forEach((f, i) => this.funcIdx.set(f, base + i));
    const handlerBase = base + this.sema.funcs.length;
    const initIdx = handlerBase + this.sema.handlers.length;

    // module order: imports · user funcs · handlers · __zero · __init · setters
    this.zeroIdx = base + this.sema.funcs.length + this.sema.handlers.length;

    for (const f of this.sema.funcs) this.genFunction(f);
    for (const h of this.sema.handlers) this.genHandler(h);
    this.genZero();
    this.genInit();
    this.genHelpers();

    const exports: string[] = ["memory", "__msgbuf", "__init", "__set_this", "__set_fuel"];
    for (const h of this.sema.handlers) exports.push(h.exportName!);
    for (const f of this.sema.funcs) exports.push(f.name);
    void initIdx;

    const bytes = this.assemble();
    return {
      bytes,
      wat: this.watModule(),
      exports,
      imports: WASM_IMPORTS.map((i) => `capl.${i.field}`),
      memoryPages: this.pages(),
      dataSize: this.data.reduce((a, d) => a + d.bytes.length, 0),
      layout: this.layout,
      msgbufAddr: this.msgbuf,
      funcCount: this.codes.length,
    };
  }

  private pages(): number {
    return Math.max(1, Math.ceil(this.heap / 65536) + 1);
  }

  /* ── function prologue ──────────────────────────────── */
  private beginFunc(
    name: string, owner: A.FuncDecl | A.HandlerDecl | null,
    params: A.Param[], ret: A.Ty,
  ): void {
    const ptypes = params.map((p) => vt(p.declTy));
    this.cur = new Code(name, ptypes, ret.kind === "void" ? null : vt(ret), params.map((p) => p.name));
    this.curRet = ret;
    params.forEach((p, i) => {
      if (p.sym) this.slotOf.set(p.sym, i);
    });
    const locals = owner ? this.sema.localsOf.get(owner) ?? [] : [];
    for (const l of locals) {
      if (l.constValue !== undefined) continue; // enum members never take storage
      if (A.isAggregateTy(l.ty)) {
        const size = A.sizeOf(l.ty);
        const align = Math.min(8, Math.max(1, A.alignOf(l.ty)));
        const addr = this.alloc(size, align);
        this.staticAddr.set(l, addr);
        this.layout.push({
          name: `${name}::${l.name}`, addr, size, ty: A.tyName(l.ty), kind: "static",
        });
      } else if (l.ty.kind === "timer" || l.ty.kind === "msTimer") {
        /* timers live in the host */
      } else {
        this.slotOf.set(l, this.cur.local(vt(l.ty)));
      }
    }
  }

  private endFunc(): void {
    if (this.curRet.kind !== "void") this.zeroOf(this.curRet);
    this.codes.push(this.cur);
  }

  private genFunction(f: A.FuncSym): void {
    this.beginFunc(f.name, f.decl, f.decl.params, f.ret);
    this.genBlock(f.decl.body);
    this.endFunc();
  }

  private genHandler(h: A.HandlerDecl): void {
    this.beginFunc(h.exportName!, h, [], A.T.void);
    this.genBlock(h.body);
    this.endFunc();
  }

  private genInit(): void {
    this.cur = new Code("__init", [], null);
    this.curRet = A.T.void;
    for (const { sym, decl } of this.dynInit) {
      const addr = this.globalAddr.get(sym);
      if (addr === undefined || !decl.init) continue;
      if (A.isAggregateTy(sym.ty)) this.storeInit(addr, sym.ty, decl.init);
      else {
        this.cur.i32c(addr);
        const t = this.genExpr(decl.init);
        this.conv(t, sym.ty);
        this.store(sym.ty);
      }
    }
    this.codes.push(this.cur);
  }

  /** `__zero(ptr, len)` — byte-wise clear, used for local aggregates */
  private genZero(): void {
    const c = new Code("__zero", [I32, I32], null, ["ptr", "len"]);
    const i = c.local(I32);
    c.i32c(0);
    c.setLocal(i);
    c.blockStart("block", "done", null);
    c.blockStart("loop", "loop", null);
    c.getLocal(i);
    c.getLocal(1);
    c.op(0x4c, "i32.ge_s");
    c.brIf("done");
    c.getLocal(0);
    c.getLocal(i);
    c.op(0x6a, "i32.add");
    c.i32c(0);
    c.mem(0x3a, 0, 0, "i32.store8");
    c.getLocal(i);
    c.i32c(1);
    c.op(0x6a, "i32.add");
    c.setLocal(i);
    c.br("loop");
    c.end();
    c.end();
    this.codes.push(c);
  }

  private genHelpers(): void {
    const setThis = new Code("__set_this", [I32], null, ["p"]);
    setThis.getLocal(0);
    setThis.setGlobal(this.G_THIS, "__this");
    this.codes.push(setThis);

    const setFuel = new Code("__set_fuel", [I32], null, ["n"]);
    setFuel.getLocal(0);
    setFuel.setGlobal(this.G_FUEL, "__fuel");
    this.codes.push(setFuel);
  }

  /* ── statements ─────────────────────────────────────── */
  private genBlock(b: A.Block): void {
    for (const s of b.stmts) this.genStmt(s);
  }

  private genStmt(s: A.Stmt): void {
    switch (s.kind) {
      case "Block": this.genBlock(s); break;
      case "Empty": break;
      case "ExprStmt": {
        const t = this.genExpr(s.expr);
        if (t.kind !== "void" && t.kind !== "error") this.cur.drop();
        break;
      }
      case "DeclStmt":
        for (const d of s.decls) this.genLocalDecl(d);
        break;
      case "If": {
        const lbl = `if${this.labelN++}`;
        this.truthy(this.genExpr(s.cond));
        this.cur.blockStart("if", lbl, null);
        this.genStmt(s.then);
        if (s.otherwise) {
          this.cur.elseOp();
          this.genStmt(s.otherwise);
        }
        this.cur.end();
        break;
      }
      case "While": {
        const n = this.labelN++;
        const brk = `brk${n}`, loop = `loop${n}`, cont = `cont${n}`;
        this.cur.blockStart("block", brk, null);
        this.cur.blockStart("loop", loop, null);
        this.burnFuel();
        this.truthy(this.genExpr(s.cond));
        this.cur.op(0x45, "i32.eqz");
        this.cur.brIf(brk);
        this.cur.blockStart("block", cont, null);
        this.breakStack.push(brk);
        this.continueStack.push(cont);
        this.genStmt(s.body);
        this.breakStack.pop();
        this.continueStack.pop();
        this.cur.end();
        this.cur.br(loop);
        this.cur.end();
        this.cur.end();
        break;
      }
      case "DoWhile": {
        const n = this.labelN++;
        const brk = `brk${n}`, loop = `loop${n}`, cont = `cont${n}`;
        this.cur.blockStart("block", brk, null);
        this.cur.blockStart("loop", loop, null);
        this.burnFuel();
        this.cur.blockStart("block", cont, null);
        this.breakStack.push(brk);
        this.continueStack.push(cont);
        this.genStmt(s.body);
        this.breakStack.pop();
        this.continueStack.pop();
        this.cur.end();
        this.truthy(this.genExpr(s.cond));
        this.cur.brIf(loop);
        this.cur.end();
        this.cur.end();
        break;
      }
      case "For": {
        if (s.init) this.genStmt(s.init);
        const n = this.labelN++;
        const brk = `brk${n}`, loop = `loop${n}`, cont = `cont${n}`;
        this.cur.blockStart("block", brk, null);
        this.cur.blockStart("loop", loop, null);
        this.burnFuel();
        if (s.cond) {
          this.truthy(this.genExpr(s.cond));
          this.cur.op(0x45, "i32.eqz");
          this.cur.brIf(brk);
        }
        this.cur.blockStart("block", cont, null);
        this.breakStack.push(brk);
        this.continueStack.push(cont);
        this.genStmt(s.body);
        this.breakStack.pop();
        this.continueStack.pop();
        this.cur.end();
        if (s.step) {
          const t = this.genExpr(s.step);
          if (t.kind !== "void" && t.kind !== "error") this.cur.drop();
        }
        this.cur.br(loop);
        this.cur.end();
        this.cur.end();
        break;
      }
      case "Switch": this.genSwitch(s); break;
      case "Break":
        if (this.breakStack.length) this.cur.br(this.breakStack[this.breakStack.length - 1]);
        break;
      case "Continue":
        if (this.continueStack.length) this.cur.br(this.continueStack[this.continueStack.length - 1]);
        break;
      case "Return":
        if (s.expr) {
          const t = this.genExpr(s.expr);
          this.conv(t, this.curRet);
        }
        this.cur.op(0x0f, "return");
        break;
    }
  }

  private genSwitch(s: A.SwitchStmt): void {
    const n = this.labelN++;
    const brk = `swbrk${n}`;
    const dt = s.disc.ty ?? A.T.long;
    const tmp = this.cur.local(vt(dt));
    const dv = this.genExpr(s.disc);
    this.conv(dv, dt);
    this.cur.setLocal(tmp);

    const caseLabels = s.clauses.map((_, i) => `case${n}_${i}`);
    const defIndex = s.clauses.findIndex((c) => c.test === null);
    const defLabel = `swdef${n}`;

    this.cur.blockStart("block", brk, null);
    this.cur.blockStart("block", defLabel, null);
    for (let i = s.clauses.length - 1; i >= 0; i--) this.cur.blockStart("block", caseLabels[i], null);

    // dispatch
    for (let i = 0; i < s.clauses.length; i++) {
      const c = s.clauses[i];
      if (!c.test) continue;
      const v = this.fold(c.test) ?? 0;
      this.cur.getLocal(tmp);
      if (vt(dt) === I64) {
        this.cur.i64c(BigInt(Math.trunc(v)));
        this.cur.op(0x51, "i64.eq");
      } else {
        this.cur.i32c(v);
        this.cur.op(0x46, "i32.eq");
      }
      this.cur.brIf(caseLabels[i]);
    }
    this.cur.br(defIndex >= 0 ? caseLabels[defIndex] : defLabel);

    this.breakStack.push(brk);
    for (let i = 0; i < s.clauses.length; i++) {
      this.cur.end(); // closes block case_i → body follows
      for (const st of s.clauses[i].body) this.genStmt(st);
    }
    this.breakStack.pop();
    this.cur.end(); // default block
    this.cur.end(); // break block
  }

  private burnFuel(): void {
    this.cur.getGlobal(this.G_FUEL, "__fuel");
    this.cur.i32c(1);
    this.cur.op(0x6b, "i32.sub");
    this.cur.setGlobal(this.G_FUEL, "__fuel");
    this.cur.getGlobal(this.G_FUEL, "__fuel");
    this.cur.i32c(0);
    this.cur.op(0x4c, "i32.le_s");
    this.cur.blockStart("if", `fuel${this.labelN++}`, null);
    this.cur.op(0x00, "unreachable");
    this.cur.end();
  }

  private genLocalDecl(d: A.VarDecl): void {
    const sym = d.sym;
    if (!sym) return;
    if (sym.ty.kind === "timer" || sym.ty.kind === "msTimer") return;
    const addr = this.staticAddr.get(sym);
    if (sym.ty.kind === "message") {
      if (addr === undefined) return;
      this.cur.i32c(addr);
      this.cur.i32c((sym.ty.msgId ?? 0) | 0);
      this.cur.mem(0x36, 2, 0, "i32.store");
      this.cur.i32c(addr + 4);
      this.cur.i32c(8);
      this.cur.mem(0x36, 2, 0, "i32.store");
      return;
    }
    if (A.isAggregateTy(sym.ty)) {
      if (addr === undefined) return;
      // CAPL clears aggregates on declaration, which keeps examples deterministic
      this.cur.i32c(addr);
      this.cur.i32c(A.sizeOf(sym.ty));
      this.cur.call(this.zeroIdx, "__zero");
      if (d.init) this.storeInit(addr, sym.ty, d.init);
      return;
    }
    const slot = this.slotOf.get(sym);
    if (slot === undefined) return;
    if (d.init && d.init.kind !== "InitList") {
      const t = this.genExpr(d.init);
      this.conv(t, sym.ty);
      this.narrow(sym.ty);
      this.cur.setLocal(slot);
    } else if (!d.init) {
      this.zeroOf(sym.ty);
      this.cur.setLocal(slot);
    }
  }

  /* ── loads / stores / conversions ───────────────────── */
  private load(ty: A.Ty): void {
    switch (ty.kind) {
      case "char": case "byte": this.cur.mem(0x2d, 0, 0, "i32.load8_u"); break;
      case "int": this.cur.mem(0x2e, 1, 0, "i32.load16_s"); break;
      case "word": this.cur.mem(0x2f, 1, 0, "i32.load16_u"); break;
      case "int64": case "qword": this.cur.mem(0x29, 3, 0, "i64.load"); break;
      case "float": case "double": this.cur.mem(0x2b, 3, 0, "f64.load"); break;
      default: this.cur.mem(0x28, 2, 0, "i32.load"); break;
    }
  }
  private store(ty: A.Ty): void {
    switch (ty.kind) {
      case "char": case "byte": this.cur.mem(0x3a, 0, 0, "i32.store8"); break;
      case "int": case "word": this.cur.mem(0x3b, 1, 0, "i32.store16"); break;
      case "int64": case "qword": this.cur.mem(0x37, 3, 0, "i64.store"); break;
      case "float": case "double": this.cur.mem(0x39, 3, 0, "f64.store"); break;
      default: this.cur.mem(0x36, 2, 0, "i32.store"); break;
    }
  }
  private zeroOf(ty: A.Ty): void {
    if (A.isFloatTy(ty)) this.cur.f64c(0);
    else if (A.is64Ty(ty)) this.cur.i64c(0n);
    else this.cur.i32c(0);
  }
  /** narrow a register value to the storage width of `ty` (locals only) */
  private narrow(ty: A.Ty): void {
    switch (ty.kind) {
      case "char": case "byte":
        this.cur.i32c(0xff);
        this.cur.op(0x71, "i32.and");
        break;
      case "word":
        this.cur.i32c(0xffff);
        this.cur.op(0x71, "i32.and");
        break;
      case "int":
        this.cur.i32c(16);
        this.cur.op(0x74, "i32.shl");
        this.cur.i32c(16);
        this.cur.op(0x75, "i32.shr_s");
        break;
      default: break;
    }
  }
  private conv(from: A.Ty, to: A.Ty): void {
    if (to.kind === "void" || from.kind === "error" || to.kind === "error") return;
    const a = vt(from), b = vt(to);
    if (a === b) return;
    const uns = A.isUnsignedTy(from);
    if (a === I32 && b === I64) this.cur.op(uns ? 0xad : 0xac, uns ? "i64.extend_i32_u" : "i64.extend_i32_s");
    else if (a === I32 && b === F64) this.cur.op(uns ? 0xb8 : 0xb7, uns ? "f64.convert_i32_u" : "f64.convert_i32_s");
    else if (a === I64 && b === I32) this.cur.op(0xa7, "i32.wrap_i64");
    else if (a === I64 && b === F64) this.cur.op(uns ? 0xba : 0xb9, uns ? "f64.convert_i64_u" : "f64.convert_i64_s");
    else if (a === F64 && b === I32) this.cur.prefixed(A.isUnsignedTy(to) ? 3 : 2, A.isUnsignedTy(to) ? "i32.trunc_sat_f64_u" : "i32.trunc_sat_f64_s");
    else if (a === F64 && b === I64) this.cur.prefixed(A.isUnsignedTy(to) ? 7 : 6, A.isUnsignedTy(to) ? "i64.trunc_sat_f64_u" : "i64.trunc_sat_f64_s");
  }
  private truthy(t: A.Ty): void {
    const v = vt(t);
    if (v === I64) {
      this.cur.i64c(0n);
      this.cur.op(0x52, "i64.ne");
    } else if (v === F64) {
      this.cur.f64c(0);
      this.cur.op(0x62, "f64.ne");
    }
  }

  /* ── lvalues ────────────────────────────────────────── */
  private symAddr(sym: A.VarSym): number | undefined {
    return this.globalAddr.get(sym) ?? this.staticAddr.get(sym);
  }

  /** pushes the address of an aggregate (array / message) */
  private genBaseAddr(e: A.Expr): A.Ty {
    if (e.kind === "This") {
      this.cur.getGlobal(this.G_THIS, "__this");
      return { kind: "message" };
    }
    if (e.kind === "StringLit") {
      this.cur.i32c(this.internString(e.value));
      return A.arrayOf(A.T.char, e.value.length + 1);
    }
    if (e.kind === "Ident" && e.sym?.kind === "var") {
      const sym = e.sym;
      if (sym.storage === "param") {
        const slot = this.slotOf.get(sym);
        this.cur.getLocal(slot ?? 0);
        return sym.ty;
      }
      const addr = this.symAddr(sym);
      if (addr === undefined) {
        this.diags.error("codegen", e.loc, `'${sym.name}' has no storage`);
        this.cur.i32c(0);
        return A.T.error;
      }
      this.cur.i32c(addr);
      return sym.ty;
    }
    if (e.kind === "Index" || e.kind === "Member") {
      const lv = this.lvalue(e);
      if (lv.kind === "mem") {
        lv.emit();
        return lv.ty;
      }
      this.cur.getLocal(lv.slot);
      return lv.ty;
    }
    this.diags.error("codegen", e.loc, "expression does not denote an object");
    this.cur.i32c(0);
    return A.T.error;
  }

  private lvalue(e: A.Expr): { kind: "local"; slot: number; ty: A.Ty } | { kind: "mem"; ty: A.Ty; emit: () => void } {
    if (e.kind === "Ident" && e.sym?.kind === "var") {
      const sym = e.sym;
      const slot = this.slotOf.get(sym);
      if (slot !== undefined && sym.ty.kind !== "array" && sym.ty.kind !== "message") {
        return { kind: "local", slot, ty: sym.ty };
      }
      const addr = this.symAddr(sym);
      return { kind: "mem", ty: sym.ty, emit: () => this.cur.i32c(addr ?? 0) };
    }
    if (e.kind === "Index") {
      const elem = e.ty ?? A.T.long;
      return {
        kind: "mem",
        ty: elem,
        emit: () => {
          const bt = this.genBaseAddr(e.base);
          const esz = A.sizeOf(bt.kind === "array" ? bt.elem ?? A.T.char : A.T.char);
          const it = this.genExpr(e.index);
          this.conv(it, A.T.long);
          if (esz !== 1) {
            this.cur.i32c(esz);
            this.cur.op(0x6c, "i32.mul");
          }
          this.cur.op(0x6a, "i32.add");
        },
      };
    }
    if (e.kind === "Member") {
      // sema already resolved the byte offset (message field or struct member)
      const ty = e.ty ?? A.T.long;
      const off = e.offset ?? 0;
      return {
        kind: "mem",
        ty,
        emit: () => {
          this.genBaseAddr(e.base);
          if (off !== 0) {
            this.cur.i32c(off);
            this.cur.op(0x6a, "i32.add");
          }
        },
      };
    }
    if (e.kind === "Call" && e.callee.kind === "Member") {
      const member = e.callee;
      const acc = MSG_ACCESSORS[member.name];
      const ty = acc ? acc.ty : A.T.long;
      return {
        kind: "mem",
        ty,
        emit: () => {
          this.genBaseAddr(member.base);
          this.cur.i32c(MSG_DATA_OFFSET);
          this.cur.op(0x6a, "i32.add");
          const it = this.genExpr(e.args[0] ?? { kind: "IntLit", value: 0, loc: e.loc });
          this.conv(it, A.T.long);
          const sz = acc?.size ?? 1;
          if (sz !== 1) {
            this.cur.i32c(sz);
            this.cur.op(0x6c, "i32.mul");
          }
          this.cur.op(0x6a, "i32.add");
        },
      };
    }
    this.diags.error("codegen", e.loc, "expression is not assignable");
    return { kind: "mem", ty: A.T.error, emit: () => this.cur.i32c(0) };
  }

  /* ── expressions ────────────────────────────────────── */
  private genExpr(e: A.Expr): A.Ty {
    switch (e.kind) {
      case "IntLit": {
        const t = e.ty ?? A.T.long;
        if (vt(t) === I64) this.cur.i64c(BigInt(Math.trunc(e.value)));
        else this.cur.i32c(e.value | 0);
        return t;
      }
      case "CharLit":
        this.cur.i32c(e.value & 0xff);
        return A.T.char;
      case "FloatLit":
        this.cur.f64c(e.value);
        return A.T.double;
      case "StringLit":
        this.cur.i32c(this.internString(e.value));
        return A.arrayOf(A.T.char, e.value.length + 1);
      case "This": {
        this.cur.getGlobal(this.G_THIS, "__this");
        return { kind: "message" };
      }
      case "Ident": {
        const sym = e.sym;
        if (!sym || sym.kind !== "var") {
          this.cur.i32c(0);
          return A.T.error;
        }
        if (sym.constValue !== undefined) {
          // enum members and `const int N = 3` fold into immediates
          const t = sym.ty;
          if (A.is64Ty(t)) this.cur.i64c(BigInt(sym.constValue));
          else this.cur.i32c(sym.constValue | 0);
          return t;
        }
        if (sym.ty.kind === "timer" || sym.ty.kind === "msTimer") {
          this.cur.i32c(sym.timerId ?? 0);
          return sym.ty;
        }
        if (sym.ty.kind === "array" || sym.ty.kind === "message") return this.genBaseAddr(e);
        const slot = this.slotOf.get(sym);
        if (slot !== undefined) {
          this.cur.getLocal(slot);
          return sym.ty;
        }
        const addr = this.symAddr(sym);
        this.cur.i32c(addr ?? 0);
        this.load(sym.ty);
        return sym.ty;
      }
      case "Index": case "Member": {
        const lv = this.lvalue(e);
        if (lv.kind === "mem") {
          lv.emit();
          this.load(lv.ty);
          return lv.ty;
        }
        this.cur.getLocal(lv.slot);
        return lv.ty;
      }
      case "Unary": return this.genUnary(e);
      case "Binary": return this.genBinary(e);
      case "Assign": return this.genAssign(e);
      case "Cond": {
        const ty = e.ty ?? A.T.long;
        this.truthy(this.genExpr(e.cond));
        this.cur.blockStart("if", `tern${this.labelN++}`, vt(ty));
        this.conv(this.genExpr(e.then), ty);
        this.cur.elseOp();
        this.conv(this.genExpr(e.otherwise), ty);
        this.cur.end();
        return ty;
      }
      case "Call": return this.genCall(e);
      case "InitList":
        this.cur.i32c(0);
        return A.T.error;
    }
  }

  private genUnary(e: A.Unary): A.Ty {
    if (e.op === "++" || e.op === "--") return this.genIncDec(e);
    if (e.op === "!") {
      const t = this.genExpr(e.operand);
      const v = vt(t);
      if (v === I64) this.cur.op(0x50, "i64.eqz");
      else if (v === F64) {
        this.cur.f64c(0);
        this.cur.op(0x61, "f64.eq");
      } else this.cur.op(0x45, "i32.eqz");
      return A.T.long;
    }
    if (e.op === "-") {
      const ty = e.ty ?? e.operand.ty ?? A.T.long;
      const v = vt(ty);
      if (v === F64) {
        this.conv(this.genExpr(e.operand), ty);
        this.cur.op(0x9a, "f64.neg");
        return ty;
      }
      if (v === I64) this.cur.i64c(0n);
      else this.cur.i32c(0);
      this.conv(this.genExpr(e.operand), ty);
      this.cur.op(v === I64 ? 0x7d : 0x6b, v === I64 ? "i64.sub" : "i32.sub");
      return ty;
    }
    if (e.op === "~") {
      const ty = e.ty ?? A.T.long;
      this.conv(this.genExpr(e.operand), ty);
      if (vt(ty) === I64) {
        this.cur.i64c(-1n);
        this.cur.op(0x85, "i64.xor");
      } else {
        this.cur.i32c(-1);
        this.cur.op(0x73, "i32.xor");
      }
      return ty;
    }
    // unary '+'
    return this.genExpr(e.operand);
  }

  private genIncDec(e: A.Unary): A.Ty {
    const lv = this.lvalue(e.operand);
    const ty = lv.ty;
    const v = vt(ty);
    const add = e.op === "++";
    const one = () => {
      if (v === I64) this.cur.i64c(1n);
      else if (v === F64) this.cur.f64c(1);
      else this.cur.i32c(1);
    };
    const addOp = () => {
      if (v === I64) this.cur.op(add ? 0x7c : 0x7d, add ? "i64.add" : "i64.sub");
      else if (v === F64) this.cur.op(add ? 0xa0 : 0xa1, add ? "f64.add" : "f64.sub");
      else this.cur.op(add ? 0x6a : 0x6b, add ? "i32.add" : "i32.sub");
    };
    if (lv.kind === "local") {
      if (e.postfix) this.cur.getLocal(lv.slot);
      this.cur.getLocal(lv.slot);
      one();
      addOp();
      this.narrow(ty);
      if (e.postfix) this.cur.setLocal(lv.slot);
      else this.cur.teeLocal(lv.slot);
      return ty;
    }
    const aTmp = this.cur.local(I32);
    const vTmp = this.cur.local(v);
    lv.emit();
    this.cur.setLocal(aTmp);
    this.cur.getLocal(aTmp);
    this.load(ty);
    this.cur.setLocal(vTmp);
    this.cur.getLocal(aTmp);
    this.cur.getLocal(vTmp);
    one();
    addOp();
    this.store(ty);
    if (e.postfix) this.cur.getLocal(vTmp);
    else {
      this.cur.getLocal(aTmp);
      this.load(ty);
    }
    return ty;
  }

  private genBinary(e: A.Binary): A.Ty {
    if (e.op === ",") {
      const lt = this.genExpr(e.left);
      if (lt.kind !== "void" && lt.kind !== "error") this.cur.drop();
      return this.genExpr(e.right);
    }
    if (e.op === "&&" || e.op === "||") {
      const lbl = `sc${this.labelN++}`;
      this.truthy(this.genExpr(e.left));
      this.cur.blockStart("if", lbl, I32);
      if (e.op === "&&") {
        this.truthy(this.genExpr(e.right));
        this.cur.op(0x45, "i32.eqz");
        this.cur.op(0x45, "i32.eqz");
        this.cur.elseOp();
        this.cur.i32c(0);
      } else {
        this.cur.i32c(1);
        this.cur.elseOp();
        this.truthy(this.genExpr(e.right));
        this.cur.op(0x45, "i32.eqz");
        this.cur.op(0x45, "i32.eqz");
      }
      this.cur.end();
      return A.T.long;
    }

    const lt0 = e.left.ty ?? A.T.long;
    const rt0 = e.right.ty ?? A.T.long;
    const cmp = ["==", "!=", "<", ">", "<=", ">="].includes(e.op);
    const shift = e.op === "<<" || e.op === ">>";
    const common: A.Ty = shift ? this.promoteOne(lt0) : this.commonTy(lt0, rt0);

    this.conv(this.genExpr(e.left), common);
    this.conv(this.genExpr(e.right), shift ? common : common);

    const v = vt(common);
    const uns = A.isUnsignedTy(common);
    const pick = (i32: [number, string], i64: [number, string], f64: [number, string]) => {
      const sel = v === I64 ? i64 : v === F64 ? f64 : i32;
      this.cur.op(sel[0], sel[1]);
    };
    switch (e.op) {
      case "+": pick([0x6a, "i32.add"], [0x7c, "i64.add"], [0xa0, "f64.add"]); break;
      case "-": pick([0x6b, "i32.sub"], [0x7d, "i64.sub"], [0xa1, "f64.sub"]); break;
      case "*": pick([0x6c, "i32.mul"], [0x7e, "i64.mul"], [0xa2, "f64.mul"]); break;
      case "/":
        if (v === F64) this.cur.op(0xa3, "f64.div");
        else if (v === I64) this.cur.op(uns ? 0x80 : 0x7f, uns ? "i64.div_u" : "i64.div_s");
        else this.cur.op(uns ? 0x6e : 0x6d, uns ? "i32.div_u" : "i32.div_s");
        break;
      case "%":
        if (v === I64) this.cur.op(uns ? 0x82 : 0x81, uns ? "i64.rem_u" : "i64.rem_s");
        else this.cur.op(uns ? 0x70 : 0x6f, uns ? "i32.rem_u" : "i32.rem_s");
        break;
      case "&": pick([0x71, "i32.and"], [0x83, "i64.and"], [0x71, "i32.and"]); break;
      case "|": pick([0x72, "i32.or"], [0x84, "i64.or"], [0x72, "i32.or"]); break;
      case "^": pick([0x73, "i32.xor"], [0x85, "i64.xor"], [0x73, "i32.xor"]); break;
      case "<<": pick([0x74, "i32.shl"], [0x86, "i64.shl"], [0x74, "i32.shl"]); break;
      case ">>":
        if (v === I64) this.cur.op(uns ? 0x88 : 0x87, uns ? "i64.shr_u" : "i64.shr_s");
        else this.cur.op(uns ? 0x76 : 0x75, uns ? "i32.shr_u" : "i32.shr_s");
        break;
      case "==": pick([0x46, "i32.eq"], [0x51, "i64.eq"], [0x61, "f64.eq"]); break;
      case "!=": pick([0x47, "i32.ne"], [0x52, "i64.ne"], [0x62, "f64.ne"]); break;
      case "<":
        if (v === F64) this.cur.op(0x63, "f64.lt");
        else if (v === I64) this.cur.op(uns ? 0x54 : 0x53, uns ? "i64.lt_u" : "i64.lt_s");
        else this.cur.op(uns ? 0x49 : 0x48, uns ? "i32.lt_u" : "i32.lt_s");
        break;
      case ">":
        if (v === F64) this.cur.op(0x64, "f64.gt");
        else if (v === I64) this.cur.op(uns ? 0x56 : 0x55, uns ? "i64.gt_u" : "i64.gt_s");
        else this.cur.op(uns ? 0x4b : 0x4a, uns ? "i32.gt_u" : "i32.gt_s");
        break;
      case "<=":
        if (v === F64) this.cur.op(0x65, "f64.le");
        else if (v === I64) this.cur.op(uns ? 0x58 : 0x57, uns ? "i64.le_u" : "i64.le_s");
        else this.cur.op(uns ? 0x4d : 0x4c, uns ? "i32.le_u" : "i32.le_s");
        break;
      case ">=":
        if (v === F64) this.cur.op(0x66, "f64.ge");
        else if (v === I64) this.cur.op(uns ? 0x5a : 0x59, uns ? "i64.ge_u" : "i64.ge_s");
        else this.cur.op(uns ? 0x4f : 0x4e, uns ? "i32.ge_u" : "i32.ge_s");
        break;
      default:
        this.diags.error("codegen", e.loc, `unsupported operator '${e.op}'`);
        break;
    }
    return cmp ? A.T.long : common;
  }

  private promoteOne(t: A.Ty): A.Ty {
    if (A.isFloatTy(t)) return A.T.double;
    if (A.is64Ty(t)) return t;
    if (t.kind === "dword") return A.T.dword;
    return A.T.long;
  }
  private commonTy(a: A.Ty, b: A.Ty): A.Ty {
    if (A.isFloatTy(a) || A.isFloatTy(b)) return A.T.double;
    if (A.is64Ty(a) || A.is64Ty(b)) {
      return a.kind === "qword" || b.kind === "qword" ? { kind: "qword" } : { kind: "int64" };
    }
    if (a.kind === "dword" || b.kind === "dword") return A.T.dword;
    return A.T.long;
  }

  private genAssign(e: A.AssignExpr): A.Ty {
    const lv = this.lvalue(e.target);
    const ty = lv.ty;
    if (e.op === "=") {
      if (lv.kind === "local") {
        this.conv(this.genExpr(e.value), ty);
        this.narrow(ty);
        this.cur.teeLocal(lv.slot);
        return ty;
      }
      const vTmp = this.cur.local(vt(ty));
      lv.emit();
      this.conv(this.genExpr(e.value), ty);
      this.cur.teeLocal(vTmp);
      this.store(ty);
      this.cur.getLocal(vTmp);
      return ty;
    }
    const binOp = e.op.slice(0, -1);
    const synth: A.Binary = {
      kind: "Binary", op: binOp,
      left: { ...e.target, ty },
      right: e.value,
      ty: this.commonTy(ty, e.value.ty ?? ty),
      loc: e.loc,
    };
    if (lv.kind === "local") {
      this.conv(this.genBinaryWithLhs(synth, () => this.cur.getLocal(lv.slot), ty), ty);
      this.narrow(ty);
      this.cur.teeLocal(lv.slot);
      return ty;
    }
    const aTmp = this.cur.local(I32);
    const vTmp = this.cur.local(vt(ty));
    lv.emit();
    this.cur.setLocal(aTmp);
    this.conv(
      this.genBinaryWithLhs(synth, () => {
        this.cur.getLocal(aTmp);
        this.load(ty);
      }, ty),
      ty,
    );
    this.cur.setLocal(vTmp);
    this.cur.getLocal(aTmp);
    this.cur.getLocal(vTmp);
    this.store(ty);
    this.cur.getLocal(vTmp);
    return ty;
  }

  /** like genBinary, but the left operand is produced by a callback */
  private genBinaryWithLhs(e: A.Binary, pushLhs: () => void, lhsTy: A.Ty): A.Ty {
    const shift = e.op === "<<" || e.op === ">>";
    const common: A.Ty = shift ? this.promoteOne(lhsTy) : this.commonTy(lhsTy, e.right.ty ?? lhsTy);
    pushLhs();
    this.conv(lhsTy, common);
    this.conv(this.genExpr(e.right), common);
    const v = vt(common);
    const uns = A.isUnsignedTy(common);
    switch (e.op) {
      case "+": this.cur.op(v === I64 ? 0x7c : v === F64 ? 0xa0 : 0x6a, `${vtName(v)}.add`); break;
      case "-": this.cur.op(v === I64 ? 0x7d : v === F64 ? 0xa1 : 0x6b, `${vtName(v)}.sub`); break;
      case "*": this.cur.op(v === I64 ? 0x7e : v === F64 ? 0xa2 : 0x6c, `${vtName(v)}.mul`); break;
      case "/":
        if (v === F64) this.cur.op(0xa3, "f64.div");
        else if (v === I64) this.cur.op(uns ? 0x80 : 0x7f, uns ? "i64.div_u" : "i64.div_s");
        else this.cur.op(uns ? 0x6e : 0x6d, uns ? "i32.div_u" : "i32.div_s");
        break;
      case "%":
        if (v === I64) this.cur.op(uns ? 0x82 : 0x81, uns ? "i64.rem_u" : "i64.rem_s");
        else this.cur.op(uns ? 0x70 : 0x6f, uns ? "i32.rem_u" : "i32.rem_s");
        break;
      case "&": this.cur.op(v === I64 ? 0x83 : 0x71, `${vtName(v)}.and`); break;
      case "|": this.cur.op(v === I64 ? 0x84 : 0x72, `${vtName(v)}.or`); break;
      case "^": this.cur.op(v === I64 ? 0x85 : 0x73, `${vtName(v)}.xor`); break;
      case "<<": this.cur.op(v === I64 ? 0x86 : 0x74, `${vtName(v)}.shl`); break;
      case ">>":
        if (v === I64) this.cur.op(uns ? 0x88 : 0x87, uns ? "i64.shr_u" : "i64.shr_s");
        else this.cur.op(uns ? 0x76 : 0x75, uns ? "i32.shr_u" : "i32.shr_s");
        break;
      default:
        this.diags.error("codegen", e.loc, `unsupported compound operator '${e.op}='`);
        break;
    }
    return common;
  }

  /* ── calls ──────────────────────────────────────────── */
  private genCall(e: A.CallExpr): A.Ty {
    if (e.callee.kind === "Member") {
      const lv = this.lvalue(e);
      if (lv.kind === "mem") {
        lv.emit();
        this.load(lv.ty);
        return lv.ty;
      }
    }
    if (e.callee.kind !== "Ident") {
      this.cur.i32c(0);
      return A.T.error;
    }
    const name = e.callee.name;
    const sym = e.sym;

    if (sym?.kind === "func") {
      const idx = this.funcIdx.get(sym);
      sym.params.forEach((pt, i) => {
        const a = e.args[i];
        if (!a) {
          this.zeroOf(pt);
          return;
        }
        if (pt.kind === "array") this.genBaseAddr(a);
        else this.conv(this.genExpr(a), pt);
      });
      this.cur.call(idx ?? 0, sym.name);
      return sym.ret;
    }

    switch (name) {
      case "write": return this.genWrite(e);
      case "setTimer": case "setTimerCyclic": case "cancelTimer": {
        this.genTimerRef(e.args[0]);
        if (name !== "cancelTimer") this.conv(this.genExpr(e.args[1] ?? intLit(e)), A.T.long);
        this.cur.call(IMP[name], name);
        return A.T.void;
      }
      case "output":
        this.genBaseAddr(e.args[0]);
        this.cur.call(IMP.output, "output");
        return A.T.void;
      case "timeNow":
        this.cur.call(IMP.timeNow, "timeNow");
        return A.T.long;
      case "random":
        this.conv(this.genExpr(e.args[0]), A.T.long);
        this.cur.call(IMP.random, "random");
        return A.T.long;
      case "strlen":
        this.genBaseAddr(e.args[0]);
        this.cur.call(IMP.strlen, "strlen");
        return A.T.long;
      case "canOnline": case "canOffline": case "stopMeasurement":
        this.cur.call(IMP[name], name);
        return A.T.void;
      case "elCount": {
        const t = e.args[0]?.ty;
        this.cur.i32c(t?.kind === "array" ? t.len ?? 0 : 0);
        return A.T.long;
      }
      case "sqrt": case "floor": case "ceil": {
        this.conv(this.genExpr(e.args[0]), A.T.double);
        const op = name === "sqrt" ? [0x9f, "f64.sqrt"] : name === "floor" ? [0x9c, "f64.floor"] : [0x9b, "f64.ceil"];
        this.cur.op(op[0] as number, op[1] as string);
        return A.T.double;
      }
      case "pow":
        this.conv(this.genExpr(e.args[0]), A.T.double);
        this.conv(this.genExpr(e.args[1]), A.T.double);
        this.cur.call(IMP.pow, "pow");
        return A.T.double;
      case "sin": case "cos":
        this.conv(this.genExpr(e.args[0]), A.T.double);
        this.cur.call(IMP[name], name);
        return A.T.double;
      case "abs": {
        const t = e.args[0]?.ty ?? A.T.long;
        if (A.isFloatTy(t)) {
          this.conv(this.genExpr(e.args[0]), A.T.double);
          this.cur.op(0x99, "f64.abs");
          return A.T.double;
        }
        const ty = A.is64Ty(t) ? t : A.T.long;
        const tmp = this.cur.local(vt(ty));
        this.conv(this.genExpr(e.args[0]), ty);
        this.cur.setLocal(tmp);
        this.cur.getLocal(tmp);
        this.cur.getLocal(tmp);
        if (vt(ty) === I64) {
          this.cur.i64c(63n);
          this.cur.op(0x87, "i64.shr_s");
          this.cur.op(0x85, "i64.xor");
          this.cur.getLocal(tmp);
          this.cur.i64c(63n);
          this.cur.op(0x87, "i64.shr_s");
          this.cur.op(0x7d, "i64.sub");
        } else {
          this.cur.i32c(31);
          this.cur.op(0x75, "i32.shr_s");
          this.cur.op(0x73, "i32.xor");
          this.cur.getLocal(tmp);
          this.cur.i32c(31);
          this.cur.op(0x75, "i32.shr_s");
          this.cur.op(0x6b, "i32.sub");
        }
        return ty;
      }
      case "min": case "max": {
        const ty = this.commonTy(e.args[0]?.ty ?? A.T.long, e.args[1]?.ty ?? A.T.long);
        const v = vt(ty);
        if (v === F64) {
          this.conv(this.genExpr(e.args[0]), ty);
          this.conv(this.genExpr(e.args[1]), ty);
          this.cur.op(name === "min" ? 0xa4 : 0xa5, name === "min" ? "f64.min" : "f64.max");
          return ty;
        }
        const a = this.cur.local(v), b = this.cur.local(v);
        this.conv(this.genExpr(e.args[0]), ty);
        this.cur.setLocal(a);
        this.conv(this.genExpr(e.args[1]), ty);
        this.cur.setLocal(b);
        this.cur.getLocal(a);
        this.cur.getLocal(b);
        this.cur.getLocal(a);
        this.cur.getLocal(b);
        const uns = A.isUnsignedTy(ty);
        if (v === I64) this.cur.op(name === "min" ? (uns ? 0x54 : 0x53) : (uns ? 0x56 : 0x55), `i64.${name === "min" ? "lt" : "gt"}_${uns ? "u" : "s"}`);
        else this.cur.op(name === "min" ? (uns ? 0x49 : 0x48) : (uns ? 0x4b : 0x4a), `i32.${name === "min" ? "lt" : "gt"}_${uns ? "u" : "s"}`);
        this.cur.op(0x1b, "select");
        return ty;
      }
      default:
        this.diags.error("codegen", e.loc, `built-in '${name}' is not available in the playground runtime`);
        this.cur.i32c(0);
        return A.T.error;
    }
  }

  private genTimerRef(a: A.Expr | undefined): void {
    if (a && a.kind === "Ident" && a.sym?.kind === "var" && a.sym.timerId !== undefined) {
      this.cur.i32c(a.sym.timerId);
      return;
    }
    if (a) this.conv(this.genExpr(a), A.T.long);
    else this.cur.i32c(0);
  }

  private genWrite(e: A.CallExpr): A.Ty {
    const fmtTmp = this.cur.local(I32);
    const fmt = e.args[0];
    if (fmt) this.genBaseAddr(fmt);
    else this.cur.i32c(this.internString(""));
    this.cur.setLocal(fmtTmp);

    const extra = e.args.slice(1);
    if (extra.length > VARARG_SLOTS) {
      this.diags.error("codegen", e.loc, `write() supports at most ${VARARG_SLOTS} arguments`);
    }
    extra.slice(0, VARARG_SLOTS).forEach((a, i) => {
      const slot = VARARG_BASE + i * 16;
      const ty = a.ty ?? A.T.long;
      let tag = TAG_I32_S;
      if (ty.kind === "array") tag = TAG_STR;
      else if (A.isFloatTy(ty)) tag = TAG_F64;
      else if (A.is64Ty(ty)) tag = A.isUnsignedTy(ty) ? TAG_I64_U : TAG_I64_S;
      else tag = A.isUnsignedTy(ty) ? TAG_I32_U : TAG_I32_S;

      this.cur.i32c(slot);
      this.cur.i32c(tag);
      this.cur.mem(0x36, 2, 0, "i32.store");

      this.cur.i32c(slot + 8);
      if (tag === TAG_STR) {
        this.genBaseAddr(a);
        this.cur.op(0xad, "i64.extend_i32_u");
        this.cur.mem(0x37, 3, 0, "i64.store");
      } else if (tag === TAG_F64) {
        this.conv(this.genExpr(a), A.T.double);
        this.cur.mem(0x39, 3, 0, "f64.store");
      } else {
        const t = this.genExpr(a);
        this.conv(t, A.isUnsignedTy(ty) ? { kind: "qword" } : { kind: "int64" });
        this.cur.mem(0x37, 3, 0, "i64.store");
      }
    });

    this.cur.getLocal(fmtTmp);
    this.cur.i32c(Math.min(extra.length, VARARG_SLOTS));
    this.cur.i32c(VARARG_BASE);
    this.cur.call(IMP.write, "write");
    return A.T.void;
  }

  /* ── module assembly ────────────────────────────────── */
  private sigKey(params: number[], ret: number | null): string {
    return `${params.join(",")}->${ret ?? "v"}`;
  }

  private assemble(): Uint8Array {
    const out: number[] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    // types
    const typeMap = new Map<string, number>();
    const types: { params: number[]; ret: number | null }[] = [];
    const typeOf = (params: number[], ret: number | null): number => {
      const k = this.sigKey(params, ret);
      let i = typeMap.get(k);
      if (i === undefined) {
        i = types.length;
        types.push({ params, ret });
        typeMap.set(k, i);
      }
      return i;
    };
    const importTypes = WASM_IMPORTS.map((s) => typeOf(s.params, s.ret));
    const funcTypes = this.codes.map((c) => typeOf(c.params, c.ret));

    const typeSec: number[] = [];
    uleb(types.length, typeSec);
    for (const t of types) {
      typeSec.push(0x60);
      uleb(t.params.length, typeSec);
      pushAll(typeSec, t.params);
      if (t.ret === null) uleb(0, typeSec);
      else {
        uleb(1, typeSec);
        typeSec.push(t.ret);
      }
    }
    section(1, typeSec, out);

    // imports
    const impSec: number[] = [];
    uleb(WASM_IMPORTS.length, impSec);
    WASM_IMPORTS.forEach((s, i) => {
      str("capl", impSec);
      str(s.field, impSec);
      impSec.push(0x00);
      uleb(importTypes[i], impSec);
    });
    section(2, impSec, out);

    // functions
    const fnSec: number[] = [];
    uleb(funcTypes.length, fnSec);
    for (const t of funcTypes) uleb(t, fnSec);
    section(3, fnSec, out);

    // memory
    const memSec: number[] = [];
    uleb(1, memSec);
    memSec.push(0x01);
    uleb(this.pages(), memSec);
    uleb(Math.max(this.pages(), 64), memSec);
    section(5, memSec, out);

    // globals: __this (mut i32), __fuel (mut i32), __msgbuf (const i32)
    const glbSec: number[] = [];
    uleb(3, glbSec);
    glbSec.push(I32, 0x01, 0x41);
    sleb(0, glbSec);
    glbSec.push(0x0b);
    glbSec.push(I32, 0x01, 0x41);
    sleb(0, glbSec);
    glbSec.push(0x0b);
    glbSec.push(I32, 0x00, 0x41);
    sleb(this.msgbuf, glbSec);
    glbSec.push(0x0b);
    section(6, glbSec, out);

    // exports
    const expEntries: number[][] = [];
    const pushExport = (name: string, kind: number, idx: number) => {
      const b: number[] = [];
      str(name, b);
      b.push(kind);
      uleb(idx, b);
      expEntries.push(b);
    };
    pushExport("memory", 0x02, 0);
    pushExport("__msgbuf", 0x03, this.G_MSGBUF);
    const impCount = WASM_IMPORTS.length;
    this.codes.forEach((c, i) => pushExport(c.name, 0x00, impCount + i));
    const expSec: number[] = [];
    vec(expEntries, expSec);
    section(7, expSec, out);

    // code
    const codeEntries: number[][] = this.codes.map((c) => {
      const body: number[] = [];
      // compress locals
      const runs: [number, number][] = [];
      for (const t of c.extraLocals) {
        const last = runs[runs.length - 1];
        if (last && last[1] === t) last[0]++;
        else runs.push([1, t]);
      }
      uleb(runs.length, body);
      for (const [n, t] of runs) {
        uleb(n, body);
        body.push(t);
      }
      pushAll(body, c.bytes);
      body.push(0x0b);
      const entry: number[] = [];
      uleb(body.length, entry);
      pushAll(entry, body);
      return entry;
    });
    const codeSec: number[] = [];
    vec(codeEntries, codeSec);
    section(10, codeSec, out);

    // data
    const dataEntries: number[][] = this.data
      .filter((d) => d.bytes.length > 0)
      .map((d) => {
        const b: number[] = [0x00, 0x41];
        sleb(d.addr, b);
        b.push(0x0b);
        uleb(d.bytes.length, b);
        pushAll(b, d.bytes);
        return b;
      });
    if (dataEntries.length) {
      const dataSec: number[] = [];
      vec(dataEntries, dataSec);
      section(11, dataSec, out);
    }

    return new Uint8Array(out);
  }

  private watModule(): string {
    const L: string[] = ["(module"];
    for (const s of WASM_IMPORTS) {
      const ps = s.params.length ? ` (param ${s.params.map(vtName).join(" ")})` : "";
      const r = s.ret !== null ? ` (result ${vtName(s.ret)})` : "";
      L.push(`  (import "capl" "${s.field}" (func $${s.field}${ps}${r}))`);
    }
    L.push(`  (memory (export "memory") ${this.pages()} ${Math.max(this.pages(), 64)})`);
    L.push(`  (global $__this (mut i32) (i32.const 0))`);
    L.push(`  (global $__fuel (mut i32) (i32.const 0))`);
    L.push(`  (global $__msgbuf (export "__msgbuf") i32 (i32.const ${this.msgbuf}))`);
    for (const d of this.data) {
      const esc = d.bytes
        .map((b) => (b >= 0x20 && b < 0x7f && b !== 0x22 && b !== 0x5c
          ? String.fromCharCode(b)
          : `\\${b.toString(16).padStart(2, "0")}`))
        .join("");
      L.push(`  (data (i32.const ${d.addr}) "${esc}")`);
    }
    for (const c of this.codes) {
      const ps = c.params.length
        ? ` ${c.params.map((p, i) => `(param $${c.paramNames[i] ?? i} ${vtName(p)})`).join(" ")}`
        : "";
      const r = c.ret !== null ? ` (result ${vtName(c.ret)})` : "";
      L.push(`  (func $${c.name} (export "${c.name}")${ps}${r}`);
      if (c.extraLocals.length) {
        L.push(`    (local ${c.extraLocals.map(vtName).join(" ")})`);
      }
      L.push(...c.wat);
      L.push("  )");
    }
    L.push(")");
    return L.join("\n");
  }
}

function intLit(e: A.CallExpr): A.Expr {
  return { kind: "IntLit", value: 0, loc: e.loc, ty: A.T.long };
}

export function emitWasm(sema: SemaResult, diags: DiagBag): WasmArtifact {
  return new WasmBackend(sema, diags).compile();
}
