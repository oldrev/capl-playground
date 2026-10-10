/* capl-lexer — hand written scanner (mirror of crates/capl-lexer)
 * Produces a flat token stream; optionally keeps trivia (comments) so the
 * same scanner can drive editor syntax highlighting. */

import type { DiagBag, Loc } from "./diagnostics";

export type TokKind =
  | "ident"
  | "kw"
  | "int"
  | "float"
  | "char"
  | "string"
  | "punct"
  | "comment"
  | "preproc"
  | "eof";

export interface Token {
  kind: TokKind;
  text: string;
  /** numeric value for int/float/char, decoded text for string */
  value?: number | string;
  loc: Loc;
}

export const KEYWORDS = new Set([
  "variables", "includes", "on", "start", "preStart", "prestart", "stopMeasurement", "preStop",
  "timer", "msTimer", "message", "key", "sysvar", "sysvar_update", "envVar", "errorFrame", "busOff",
  "signal", "this", "if", "else", "while", "for", "do", "switch", "case", "default", "break",
  "continue", "return", "const", "enum", "struct", "export", "static", "testcase", "testfunction",
]);

export const TYPE_KEYWORDS = new Set([
  "int", "long", "int64", "byte", "word", "dword", "qword", "float", "double", "char", "void",
  "timer", "msTimer", "message", "struct", "enum",
]);

/** CAPL built-ins recognised by the playground runtime (used for highlighting too) */
export const BUILTINS = new Set([
  "write", "writeLineEx", "setTimer", "setTimerCyclic", "cancelTimer", "output", "timeNow",
  "random", "elCount", "abs", "sqrt", "pow", "sin", "cos", "floor", "ceil", "strlen",
  "canOnline", "canOffline", "stopMeasurement", "min", "max",
]);

const PUNCTUATORS = [
  ">>=", "<<=", "...",
  "++", "--", "->", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||",
  "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "::",
  "+", "-", "*", "/", "%", "=", "<", ">", "!", "~", "&", "|", "^", "?", ":",
  ";", ",", ".", "(", ")", "{", "}", "[", "]",
];

export interface LexOptions {
  keepTrivia?: boolean;
}

export class Lexer {
  private i = 0;
  private line = 1;
  private lineStart = 0;

  constructor(
    private readonly src: string,
    private readonly diags: DiagBag,
    private readonly opts: LexOptions = {},
  ) {}

  private loc(startPos: number, startLine: number, startCol: number): Loc {
    return { line: startLine, col: startCol, pos: startPos, end: this.i };
  }

  private col(pos: number): number {
    return pos - this.lineStart + 1;
  }

  private newline(): void {
    this.line++;
    this.lineStart = this.i;
  }

  all(): Token[] {
    const out: Token[] = [];
    for (;;) {
      const t = this.next();
      if (!t) continue;
      out.push(t);
      if (t.kind === "eof") break;
    }
    return out;
  }

  /** returns null when trivia was skipped */
  private next(): Token | null {
    const src = this.src;
    // whitespace
    while (this.i < src.length) {
      const c = src[this.i];
      if (c === "\n") {
        this.i++;
        this.newline();
      } else if (c === " " || c === "\t" || c === "\r" || c === "\f" || c === "\v") {
        this.i++;
      } else break;
    }
    const start = this.i;
    const startLine = this.line;
    const startCol = this.col(start);
    if (this.i >= src.length) {
      return { kind: "eof", text: "", loc: this.loc(start, startLine, startCol) };
    }
    const c = src[this.i];

    // comments
    if (c === "/" && src[this.i + 1] === "/") {
      while (this.i < src.length && src[this.i] !== "\n") this.i++;
      return this.trivia("comment", start, startLine, startCol);
    }
    if (c === "/" && src[this.i + 1] === "*") {
      this.i += 2;
      while (this.i < src.length && !(src[this.i] === "*" && src[this.i + 1] === "/")) {
        if (src[this.i] === "\n") {
          this.i++;
          this.newline();
        } else this.i++;
      }
      if (this.i >= src.length) {
        this.diags.error("lex", this.loc(start, startLine, startCol), "unterminated block comment");
      } else this.i += 2;
      return this.trivia("comment", start, startLine, startCol);
    }

    // preprocessor-ish line (#include / #pragma) — kept as trivia token
    if (c === "#") {
      while (this.i < src.length && src[this.i] !== "\n") this.i++;
      return this.trivia("preproc", start, startLine, startCol);
    }

    // identifiers / keywords
    if (/[A-Za-z_@]/.test(c)) {
      while (this.i < src.length && /[A-Za-z0-9_@]/.test(src[this.i])) this.i++;
      const text = src.slice(start, this.i);
      const kind: TokKind = KEYWORDS.has(text) || TYPE_KEYWORDS.has(text) ? "kw" : "ident";
      return { kind, text, loc: this.loc(start, startLine, startCol) };
    }

    // numbers
    if (/[0-9]/.test(c) || (c === "." && /[0-9]/.test(src[this.i + 1] ?? ""))) {
      return this.number(start, startLine, startCol);
    }

    // char literal
    if (c === "'") {
      this.i++;
      let v = 0;
      if (src[this.i] === "\\") {
        this.i++;
        v = this.escape(start, startLine, startCol);
      } else {
        v = src.charCodeAt(this.i);
        this.i++;
      }
      if (src[this.i] !== "'") {
        this.diags.error("lex", this.loc(start, startLine, startCol), "unterminated character literal");
      } else this.i++;
      return { kind: "char", text: src.slice(start, this.i), value: v, loc: this.loc(start, startLine, startCol) };
    }

    // string literal
    if (c === '"') {
      this.i++;
      let out = "";
      while (this.i < src.length && src[this.i] !== '"') {
        if (src[this.i] === "\n") {
          this.diags.error("lex", this.loc(start, startLine, startCol), "unterminated string literal");
          break;
        }
        if (src[this.i] === "\\") {
          this.i++;
          out += String.fromCharCode(this.escape(start, startLine, startCol));
        } else {
          out += src[this.i];
          this.i++;
        }
      }
      if (src[this.i] === '"') this.i++;
      return { kind: "string", text: src.slice(start, this.i), value: out, loc: this.loc(start, startLine, startCol) };
    }

    // punctuation
    for (const p of PUNCTUATORS) {
      if (src.startsWith(p, this.i)) {
        this.i += p.length;
        return { kind: "punct", text: p, loc: this.loc(start, startLine, startCol) };
      }
    }

    this.i++;
    this.diags.error("lex", this.loc(start, startLine, startCol), `unexpected character '${c}'`);
    return null;
  }

  private trivia(kind: TokKind, start: number, line: number, col: number): Token | null {
    if (!this.opts.keepTrivia) return null;
    return { kind, text: this.src.slice(start, this.i), loc: this.loc(start, line, col) };
  }

  private escape(start: number, line: number, col: number): number {
    const src = this.src;
    const c = src[this.i];
    this.i++;
    switch (c) {
      case "n": return 10;
      case "t": return 9;
      case "r": return 13;
      case "0": return 0;
      case "b": return 8;
      case "f": return 12;
      case "v": return 11;
      case "a": return 7;
      case "\\": return 92;
      case "'": return 39;
      case '"': return 34;
      case "x": {
        let h = "";
        while (this.i < src.length && /[0-9a-fA-F]/.test(src[this.i]) && h.length < 2) {
          h += src[this.i];
          this.i++;
        }
        return parseInt(h || "0", 16);
      }
      default:
        this.diags.warn("lex", this.loc(start, line, col), `unknown escape sequence '\\${c}'`);
        return c?.charCodeAt(0) ?? 0;
    }
  }

  private number(start: number, line: number, col: number): Token {
    const src = this.src;
    if (src[this.i] === "0" && (src[this.i + 1] === "x" || src[this.i + 1] === "X")) {
      this.i += 2;
      while (this.i < src.length && /[0-9a-fA-F]/.test(src[this.i])) this.i++;
      const text = src.slice(start, this.i);
      return { kind: "int", text, value: Number(text), loc: this.loc(start, line, col) };
    }
    if (src[this.i] === "0" && (src[this.i + 1] === "b" || src[this.i + 1] === "B")) {
      this.i += 2;
      const bstart = this.i;
      while (this.i < src.length && /[01]/.test(src[this.i])) this.i++;
      const text = src.slice(start, this.i);
      return { kind: "int", text, value: parseInt(src.slice(bstart, this.i) || "0", 2), loc: this.loc(start, line, col) };
    }
    let isFloat = false;
    while (this.i < src.length && /[0-9]/.test(src[this.i])) this.i++;
    if (src[this.i] === "." && /[0-9]/.test(src[this.i + 1] ?? "")) {
      isFloat = true;
      this.i++;
      while (this.i < src.length && /[0-9]/.test(src[this.i])) this.i++;
    } else if (src[this.i] === "." && !/[A-Za-z_]/.test(src[this.i + 1] ?? "")) {
      isFloat = true;
      this.i++;
    }
    if (src[this.i] === "e" || src[this.i] === "E") {
      const save = this.i;
      this.i++;
      if (src[this.i] === "+" || src[this.i] === "-") this.i++;
      if (/[0-9]/.test(src[this.i] ?? "")) {
        isFloat = true;
        while (this.i < src.length && /[0-9]/.test(src[this.i])) this.i++;
      } else this.i = save;
    }
    const text = src.slice(start, this.i);
    return {
      kind: isFloat ? "float" : "int",
      text,
      value: Number(text),
      loc: this.loc(start, line, col),
    };
  }
}

export function tokenize(src: string, diags: DiagBag, opts?: LexOptions): Token[] {
  return new Lexer(src, diags, opts).all();
}
