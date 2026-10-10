/* capl-ast — plain AST + type lattice + JSON exporter
 * (mirror of crates/capl-ast). The parser produces this tree with *no*
 * semantic information; `sema` decorates it in place. */

import type { Loc } from "./diagnostics";

/* ── types ──────────────────────────────────────────────── */

export type TyKind =
  | "void" | "char" | "byte" | "int" | "word" | "long" | "dword" | "int64" | "qword"
  | "float" | "double" | "timer" | "msTimer" | "message" | "array" | "struct" | "enum"
  | "error";

/** one laid-out field of a struct; offsets are filled in by sema */
export interface StructMember {
  name: string;
  ty: Ty;
  offset: number;
}

export interface Ty {
  kind: TyKind;
  elem?: Ty;
  len?: number;
  /** for `message 0x123 m;` */
  msgId?: number;
  msgName?: string;
  /** tag of a `struct S` / `enum E` */
  name?: string;
  /** struct layout — carried in the type so backends need no symbol table */
  members?: StructMember[];
  /** parser-provided fields of an *anonymous* struct, laid out by sema */
  fields?: StructField[];
  /** total struct size in bytes */
  size?: number;
}

export const T = {
  void: { kind: "void" } as Ty,
  int: { kind: "int" } as Ty,
  long: { kind: "long" } as Ty,
  dword: { kind: "dword" } as Ty,
  int64: { kind: "int64" } as Ty,
  double: { kind: "double" } as Ty,
  char: { kind: "char" } as Ty,
  byte: { kind: "byte" } as Ty,
  error: { kind: "error" } as Ty,
};

export function arrayOf(elem: Ty, len: number): Ty {
  return { kind: "array", elem, len };
}

/** byte width of a scalar CAPL type (CAPL: int=2, long=4, int64=8, float=8) */
export function sizeOf(t: Ty): number {
  switch (t.kind) {
    case "char": case "byte": return 1;
    case "int": case "word": return 2;
    case "long": case "dword": case "enum": return 4;
    case "int64": case "qword": case "float": case "double": return 8;
    case "timer": case "msTimer": return 4;
    case "message": return 32;
    case "struct": return t.size ?? 0;
    case "array": return (t.len ?? 0) * sizeOf(t.elem ?? T.char);
    default: return 4;
  }
}

/** natural alignment of a type (used for struct layout) */
export function alignOf(t: Ty): number {
  if (t.kind === "array") return Math.min(8, alignOf(t.elem ?? T.char));
  if (t.kind === "struct") return Math.min(8, Math.max(1, t.size ?? 1));
  return Math.min(8, Math.max(1, sizeOf(t)));
}

export function isFloatTy(t: Ty): boolean {
  return t.kind === "float" || t.kind === "double";
}
export function is64Ty(t: Ty): boolean {
  return t.kind === "int64" || t.kind === "qword";
}
export function isUnsignedTy(t: Ty): boolean {
  return t.kind === "byte" || t.kind === "word" || t.kind === "dword" || t.kind === "qword";
}
export function isIntegralTy(t: Ty): boolean {
  return t.kind !== "void" && t.kind !== "array" && t.kind !== "message"
    && t.kind !== "error" && t.kind !== "struct" && !isFloatTy(t);
}
export function isScalarTy(t: Ty): boolean {
  return isIntegralTy(t) || isFloatTy(t);
}
/** aggregate types live in linear memory instead of a wasm local */
export function isAggregateTy(t: Ty): boolean {
  return t.kind === "array" || t.kind === "message" || t.kind === "struct";
}

export function tyName(t: Ty | undefined): string {
  if (!t) return "?";
  switch (t.kind) {
    case "array": return `${tyName(t.elem)}[${t.len ?? ""}]`;
    case "message": return t.msgName ? `message ${t.msgName}` : "message";
    case "struct": return `struct ${t.name ?? "<anonymous>"}`;
    case "enum": return `enum ${t.name ?? "<anonymous>"}`;
    default: return t.kind;
  }
}

/** `[3][4]` for a nested array type, `[]` for a scalar */
export function dimsOf(t: Ty | undefined): number[] {
  if (!t || t.kind !== "array") return [];
  return [t.len ?? 0, ...dimsOf(t.elem)];
}

/* ── expressions ────────────────────────────────────────── */

export type Expr =
  | IntLit | FloatLit | CharLit | StringLit | Ident | ThisExpr
  | Unary | Binary | AssignExpr | CondExpr | CallExpr | IndexExpr | MemberExpr | InitList;

interface Base {
  loc: Loc;
  ty?: Ty;
}

export interface IntLit extends Base { kind: "IntLit"; value: number }
export interface FloatLit extends Base { kind: "FloatLit"; value: number }
export interface CharLit extends Base { kind: "CharLit"; value: number; text: string }
export interface StringLit extends Base { kind: "StringLit"; value: string }
export interface Ident extends Base { kind: "Ident"; name: string; sym?: Sym }
export interface ThisExpr extends Base { kind: "This" }
export interface Unary extends Base { kind: "Unary"; op: string; operand: Expr; postfix?: boolean }
export interface Binary extends Base { kind: "Binary"; op: string; left: Expr; right: Expr }
export interface AssignExpr extends Base { kind: "Assign"; op: string; target: Expr; value: Expr }
export interface CondExpr extends Base { kind: "Cond"; cond: Expr; then: Expr; otherwise: Expr }
export interface CallExpr extends Base { kind: "Call"; callee: Expr; args: Expr[]; sym?: Sym }
export interface IndexExpr extends Base { kind: "Index"; base: Expr; index: Expr }
export interface MemberExpr extends Base {
  kind: "Member";
  base: Expr;
  name: string;
  /** byte offset of the member inside its aggregate, filled in by sema */
  offset?: number;
}
export interface InitList extends Base { kind: "InitList"; items: Expr[] }

/* ── statements ─────────────────────────────────────────── */

export type Stmt =
  | Block | ExprStmt | DeclStmt | IfStmt | WhileStmt | DoWhileStmt | ForStmt
  | SwitchStmt | BreakStmt | ContinueStmt | ReturnStmt | EmptyStmt;

export interface Block extends Base { kind: "Block"; stmts: Stmt[] }
export interface ExprStmt extends Base { kind: "ExprStmt"; expr: Expr }
/** local declaration; `types` carries any enum/struct definitions in the block */
export interface DeclStmt extends Base {
  kind: "DeclStmt";
  decls: VarDecl[];
  types?: (EnumDecl | StructDecl)[];
}
export interface IfStmt extends Base { kind: "If"; cond: Expr; then: Stmt; otherwise?: Stmt }
export interface WhileStmt extends Base { kind: "While"; cond: Expr; body: Stmt }
export interface DoWhileStmt extends Base { kind: "DoWhile"; body: Stmt; cond: Expr }
export interface ForStmt extends Base { kind: "For"; init?: Stmt; cond?: Expr; step?: Expr; body: Stmt }
export interface SwitchClause { test: Expr | null; body: Stmt[]; loc: Loc }
export interface SwitchStmt extends Base { kind: "Switch"; disc: Expr; clauses: SwitchClause[] }
export interface BreakStmt extends Base { kind: "Break" }
export interface ContinueStmt extends Base { kind: "Continue" }
export interface ReturnStmt extends Base { kind: "Return"; expr?: Expr }
export interface EmptyStmt extends Base { kind: "Empty" }

/* ── declarations ───────────────────────────────────────── */

export interface VarDecl extends Base {
  kind: "VarDecl";
  name: string;
  declTy: Ty;
  /** every `[...]` group, outermost first; empty for scalars */
  dims: Expr[];
  isArray?: boolean;
  init?: Expr;
  isConst?: boolean;
  sym?: VarSym;
}

export interface Param extends Base { kind: "Param"; name: string; declTy: Ty; sym?: VarSym }

export interface FuncDecl extends Base {
  kind: "FuncDecl";
  name: string;
  retTy: Ty;
  params: Param[];
  body: Block;
  isExport?: boolean;
  sym?: FuncSym;
}

export type EventKind =
  | "start" | "preStart" | "stopMeasurement" | "preStop" | "timer" | "message"
  | "key" | "sysvar" | "envVar" | "errorFrame" | "busOff";

export interface EventSpec {
  kind: EventKind;
  /** timer/sysvar/envVar name, or symbolic message name */
  name?: string;
  /** numeric CAN id for `on message 0x100` */
  id?: number;
  /** `on message *` / `on key *` */
  wildcard?: boolean;
  /** channel prefix: `on message CAN2.0x100` */
  channel?: number;
  /** `on key 'a'` */
  key?: string;
}

export interface HandlerDecl extends Base {
  kind: "HandlerDecl";
  event: EventSpec;
  body: Block;
  /** mangled export name, filled by sema */
  exportName?: string;
}

export interface VariablesBlock extends Base {
  kind: "VariablesBlock";
  decls: VarDecl[];
  /** enum/struct definitions written inside the block */
  types?: (EnumDecl | StructDecl)[];
}
export interface IncludesBlock extends Base { kind: "IncludesBlock"; text: string }

/* ── enums ──────────────────────────────────────────────── */

export interface EnumItem extends Base {
  kind: "EnumItem";
  name: string;
  /** explicit `= expr`, folded by sema into `resolved` */
  init?: Expr;
  resolved?: number;
}

export interface EnumDecl extends Base {
  kind: "EnumDecl";
  name: string;
  items: EnumItem[];
}

/* ── structs ────────────────────────────────────────────── */

export interface StructField extends Base {
  kind: "StructField";
  name: string;
  declTy: Ty;
  dims: Expr[];
}

export interface StructDecl extends Base {
  kind: "StructDecl";
  name: string;
  fields: StructField[];
  /** laid-out type, filled in by sema */
  ty?: Ty;
}

export type Decl =
  | VarDecl | FuncDecl | HandlerDecl | VariablesBlock | IncludesBlock | EnumDecl | StructDecl;

export interface Unit {
  kind: "Unit";
  name: string;
  decls: Decl[];
}

/* ── symbols (filled in by sema) ────────────────────────── */

export interface VarSym {
  kind: "var";
  name: string;
  ty: Ty;
  storage: "global" | "local" | "param";
  decl: VarDecl | Param;
  /** static address in linear memory (globals, arrays, messages, structs) */
  addr?: number;
  /** wasm local index for scalar locals/params */
  slot?: number;
  /** runtime id for timers */
  timerId?: number;
  used?: boolean;
  /** value of a compile-time constant (enum members, `const int N = 3`) */
  constValue?: number;
}

export interface FuncSym {
  kind: "func";
  name: string;
  ret: Ty;
  params: Ty[];
  decl: FuncDecl;
  index?: number;
}

export interface BuiltinSym {
  kind: "builtin";
  name: string;
  ret: Ty;
  /** -1 = variadic */
  arity: number;
}

export type Sym = VarSym | FuncSym | BuiltinSym;

/* ── JSON export ────────────────────────────────────────── */

const SKIP = new Set(["sym", "used"]);

/** Structural JSON dump of the decorated AST (no cycles, stable key order). */
export function astToJson(node: unknown): unknown {
  if (node === null || node === undefined) return null;
  if (Array.isArray(node)) return node.map(astToJson);
  if (typeof node !== "object") return node;
  const src = node as Record<string, unknown>;
  const out: Record<string, unknown> = {};
  if (typeof src.kind === "string") out.kind = src.kind;
  for (const [k, v] of Object.entries(src)) {
    if (k === "kind" || SKIP.has(k)) continue;
    if (k === "loc") {
      const l = v as Loc;
      out.loc = `${l.line}:${l.col}`;
      continue;
    }
    if (k === "ty" || k === "declTy" || k === "retTy") {
      out[k] = tyName(v as Ty);
      continue;
    }
    if (v === undefined) continue;
    out[k] = astToJson(v);
  }
  return out;
}

export function countNodes(node: unknown): number {
  if (node === null || node === undefined) return 0;
  if (Array.isArray(node)) return node.reduce((a: number, n) => a + countNodes(n), 0);
  if (typeof node !== "object") return 0;
  const src = node as Record<string, unknown>;
  let n = typeof src.kind === "string" ? 1 : 0;
  for (const [k, v] of Object.entries(src)) {
    if (k === "loc" || SKIP.has(k) || k === "ty" || k === "declTy" || k === "retTy") continue;
    n += countNodes(v);
  }
  return n;
}
