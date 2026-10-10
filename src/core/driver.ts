/* capl-driver — glues the stages together (mirror of crates/capl-driver).
 * This is the only place that knows the order of the pipeline. */

import { DiagBag, type Diagnostic } from "./diagnostics";
import { tokenize } from "./lexer";
import { parse } from "./parser";
import { analyze, type SemaResult } from "./sema";
import { astToJson, countNodes, type Unit } from "./ast";
import { emitC } from "./backend/c";
import { emitWasm, type WasmArtifact } from "./backend/wasm";

export type BackendId = "wasm" | "c" | "ast";

export interface CompileOptions {
  fileName?: string;
  backends?: BackendId[];
}

export interface Timings {
  lex: number;
  parse: number;
  sema: number;
  codegen: number;
  total: number;
}

export interface CompileResult {
  ok: boolean;
  diags: Diagnostic[];
  tokenCount: number;
  nodeCount: number;
  unit: Unit;
  sema?: SemaResult;
  astJson: string;
  c?: string;
  wasm?: WasmArtifact;
  timings: Timings;
}

const now = (): number => (typeof performance !== "undefined" ? performance.now() : Date.now());

export function compile(src: string, opts: CompileOptions = {}): CompileResult {
  const backends = opts.backends ?? ["wasm", "c", "ast"];
  const file = opts.fileName ?? "source.can";
  const diags = new DiagBag();
  const t0 = now();

  const tokens = tokenize(src, diags);
  const t1 = now();

  const unit = parse(tokens, diags, file);
  const t2 = now();

  const sema = analyze(unit, diags);
  const t3 = now();

  let c: string | undefined;
  let wasm: WasmArtifact | undefined;
  if (!diags.hasErrors) {
    if (backends.includes("c")) {
      try {
        c = emitC(sema);
      } catch (err) {
        diags.error("codegen", unit.decls[0]?.loc ?? { line: 1, col: 1, pos: 0, end: 0 }, `C backend: ${String(err)}`);
      }
    }
    if (backends.includes("wasm")) {
      try {
        wasm = emitWasm(sema, diags);
      } catch (err) {
        diags.error("codegen", unit.decls[0]?.loc ?? { line: 1, col: 1, pos: 0, end: 0 }, `wasm backend: ${String(err)}`);
      }
    }
  }
  const t4 = now();

  return {
    ok: !diags.hasErrors,
    diags: diags.items,
    tokenCount: tokens.length,
    nodeCount: countNodes(unit),
    unit,
    sema,
    astJson: JSON.stringify(astToJson(unit), null, 2),
    c,
    wasm,
    timings: { lex: t1 - t0, parse: t2 - t1, sema: t3 - t2, codegen: t4 - t3, total: t4 - t0 },
  };
}

/* ── core selection ─────────────────────────────────────────
 * If a Rust-built `capl_wasm` module has been loaded (it registers itself on
 * `globalThis.__CAPL_CORE__`), the playground uses it; otherwise it falls back
 * to this TypeScript mirror of the same crates. Both expose `compile()`.
 */
export interface CaplCore {
  id: "rust-wasm" | "ts-reference";
  version: string;
  compile(src: string, opts?: CompileOptions): CompileResult;
}

declare global {
  // eslint-disable-next-line no-var
  var __CAPL_CORE__: CaplCore | undefined;
}

export const TS_CORE: CaplCore = {
  id: "ts-reference",
  version: "0.4.0",
  compile,
};

export function getCore(): CaplCore {
  const injected = typeof globalThis !== "undefined" ? globalThis.__CAPL_CORE__ : undefined;
  return injected ?? TS_CORE;
}
