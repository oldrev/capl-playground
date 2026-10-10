/* capl-diag — source locations + diagnostic bag (mirror of crates/capl-diag) */

export interface Loc {
  /** 1-based line */
  line: number;
  /** 1-based column */
  col: number;
  /** byte offset of first char */
  pos: number;
  /** byte offset one past last char */
  end: number;
}

export const NO_LOC: Loc = { line: 1, col: 1, pos: 0, end: 0 };

export type Severity = "error" | "warning" | "note";
export type Stage = "lex" | "parse" | "sema" | "codegen" | "link" | "runtime";

export interface Diagnostic {
  severity: Severity;
  stage: Stage;
  message: string;
  loc: Loc;
  /** Short diagnostic code, e.g. "E0014" */
  code?: string;
}

export class DiagBag {
  readonly items: Diagnostic[] = [];
  private counter = 0;

  error(stage: Stage, loc: Loc, message: string): void {
    this.counter++;
    this.items.push({ severity: "error", stage, loc, message, code: `E${String(this.counter).padStart(4, "0")}` });
  }

  warn(stage: Stage, loc: Loc, message: string): void {
    this.items.push({ severity: "warning", stage, loc, message });
  }

  note(stage: Stage, loc: Loc, message: string): void {
    this.items.push({ severity: "note", stage, loc, message });
  }

  get errorCount(): number {
    return this.items.filter((d) => d.severity === "error").length;
  }
  get warningCount(): number {
    return this.items.filter((d) => d.severity === "warning").length;
  }
  get hasErrors(): boolean {
    return this.errorCount > 0;
  }
}

export function formatDiagnostic(d: Diagnostic): string {
  const sev = d.severity === "error" ? "Error" : d.severity === "warning" ? "Warning" : "Note";
  return `source.can(${d.loc.line},${d.loc.col}): ${sev}: ${d.message}`;
}
