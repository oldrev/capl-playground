/** Frontend contract for the Rust CAPL compiler loaded from WebAssembly. */
import type { Diagnostic } from "./diagnostics";
export type { Diagnostic } from "./diagnostics";

export interface Loc { line: number; col: number; pos: number; end: number }
export interface EventSpec { kind: string; name?: string; id?: number; wildcard?: boolean; channel?: number; key?: string }
export interface HandlerInfo { exportName: string; event: EventSpec }
export interface TimerInfo { id: number; name: string }
export interface WasmArtifact {
  bytes: Uint8Array | number[];
  wat: string;
  exports: string[];
  imports: string[];
  memoryPages: number;
  msgbufAddr: number;
  layout: { name: string; addr: number; size: number; ty: string; kind: string }[];
}
export interface CompileResult {
  ok: boolean;
  diags: Diagnostic[];
  tokenCount: number;
  nodeCount: number;
  astJson: string;
  c?: string;
  wasm?: WasmArtifact;
  sema: { handlers: HandlerInfo[]; timers: TimerInfo[] };
  timings: { lex: number; parse: number; sema: number; codegen: number; total?: number };
}
export interface CaplCore { id: "rust-wasm"; version: string; compile(src: string): CompileResult }
declare global {
  // eslint-disable-next-line no-var
  var __CAPL_CORE__: CaplCore | undefined;
}
export function getCore(): CaplCore {
  const core = globalThis.__CAPL_CORE__;
  if (!core) throw new Error("Rust CAPL compiler is not loaded");
  return core;
}
