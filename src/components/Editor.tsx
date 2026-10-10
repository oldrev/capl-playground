import { useEffect, useMemo, useRef } from "react";
import { BUILTINS, TYPE_KEYWORDS, tokenize } from "../core/lexer";
import { DiagBag } from "../core/diagnostics";

export interface EditorMark {
  line: number;
  severity: "error" | "warning" | "note";
}

interface Props {
  value: string;
  onChange: (v: string) => void;
  marks: EditorMark[];
  fontSize: number;
  onRun?: () => void;
  onCursor?: (line: number, col: number) => void;
  /** bump `nonce` to move the caret (used by the diagnostics panel) */
  caret?: { line: number; col: number; nonce: number };
}

interface Piece {
  text: string;
  cls: string;
}

function classify(src: string): Piece[] {
  const bag = new DiagBag();
  let toks;
  try {
    toks = tokenize(src, bag, { keepTrivia: true });
  } catch {
    return [{ text: src, cls: "" }];
  }
  const out: Piece[] = [];
  let pos = 0;
  for (const t of toks) {
    if (t.kind === "eof") break;
    if (t.loc.pos > pos) out.push({ text: src.slice(pos, t.loc.pos), cls: "" });
    const text = src.slice(t.loc.pos, t.loc.end);
    let cls = "";
    switch (t.kind) {
      case "kw": cls = TYPE_KEYWORDS.has(t.text) ? "tk-type" : "tk-key"; break;
      case "ident": cls = BUILTINS.has(t.text) ? "tk-builtin" : ""; break;
      case "int": case "float": cls = "tk-num"; break;
      case "char": case "string": cls = "tk-str"; break;
      case "comment": case "preproc": cls = "tk-com"; break;
      case "punct": cls = "tk-punct"; break;
      default: cls = "";
    }
    out.push({ text, cls });
    pos = t.loc.end;
  }
  if (pos < src.length) out.push({ text: src.slice(pos), cls: "" });
  return out;
}

export default function Editor({ value, onChange, marks, fontSize, onRun, onCursor, caret }: Props) {
  const taRef = useRef<HTMLTextAreaElement>(null);
  const hlRef = useRef<HTMLPreElement>(null);
  const gutterRef = useRef<HTMLDivElement>(null);

  const pieces = useMemo(() => classify(value), [value]);
  const lineCount = useMemo(() => value.split("\n").length, [value]);
  const markMap = useMemo(() => {
    const m = new Map<number, string>();
    for (const k of marks) if (k.severity === "error") m.set(k.line, "gl-err");
    return m;
  }, [marks]);

  const sync = () => {
    const ta = taRef.current;
    if (!ta) return;
    if (hlRef.current) {
      hlRef.current.scrollTop = ta.scrollTop;
      hlRef.current.scrollLeft = ta.scrollLeft;
    }
    if (gutterRef.current) gutterRef.current.scrollTop = ta.scrollTop;
  };

  useEffect(sync, [value, fontSize]);

  // jump to a diagnostic
  useEffect(() => {
    if (!caret) return;
    const ta = taRef.current;
    if (!ta) return;
    const lines = value.split("\n");
    let off = 0;
    for (let i = 0; i < Math.min(caret.line - 1, lines.length); i++) off += lines[i].length + 1;
    off += Math.max(0, caret.col - 1);
    ta.focus();
    ta.setSelectionRange(off, off);
    const lh = Math.round(fontSize * 1.55);
    ta.scrollTop = Math.max(0, (caret.line - 1) * lh - ta.clientHeight / 2);
    sync();
    onCursor?.(caret.line, caret.col);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [caret?.nonce]);

  const reportCursor = () => {
    const ta = taRef.current;
    if (!ta || !onCursor) return;
    const upto = ta.value.slice(0, ta.selectionStart);
    const lines = upto.split("\n");
    onCursor(lines.length, lines[lines.length - 1].length + 1);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    const ta = e.currentTarget;
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      onRun?.();
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      const s = ta.selectionStart;
      const en = ta.selectionEnd;
      const next = `${value.slice(0, s)}  ${value.slice(en)}`;
      onChange(next);
      requestAnimationFrame(() => {
        ta.selectionStart = ta.selectionEnd = s + 2;
      });
    }
    if (e.key === "Enter") {
      // keep indentation of the current line
      const s = ta.selectionStart;
      const lineStart = value.lastIndexOf("\n", s - 1) + 1;
      const indentMatch = /^[ \t]*/.exec(value.slice(lineStart, s));
      const indent = indentMatch ? indentMatch[0] : "";
      const extra = /[{(]\s*$/.test(value.slice(lineStart, s)) ? "  " : "";
      if (indent || extra) {
        e.preventDefault();
        const next = `${value.slice(0, s)}\n${indent}${extra}${value.slice(ta.selectionEnd)}`;
        onChange(next);
        const caret = s + 1 + indent.length + extra.length;
        requestAnimationFrame(() => {
          ta.selectionStart = ta.selectionEnd = caret;
        });
      }
    }
  };

  const style = { fontSize: `${fontSize}px`, lineHeight: `${Math.round(fontSize * 1.55)}px` };

  return (
    <div className="flex h-full min-h-0 overflow-hidden">
      <div
        ref={gutterRef}
        className="gutter shrink-0 overflow-hidden select-none"
        style={{ ...style, width: `${String(lineCount).length * 0.62 + 2.2}em` }}
      >
        {Array.from({ length: lineCount }, (_, i) => (
          <div key={i} className={markMap.get(i + 1) ?? ""}>
            {markMap.get(i + 1) ? "●" : i + 1}
          </div>
        ))}
      </div>
      <div className="ed-wrap relative min-w-0 flex-1" style={style}>
        <pre ref={hlRef} className="ed-hl ed-layer" style={style} aria-hidden>
          {pieces.map((p, i) => (p.cls ? <span key={i} className={p.cls}>{p.text}</span> : <span key={i}>{p.text}</span>))}
          {"\n\n"}
        </pre>
        <textarea
          ref={taRef}
          className="ed-ta ed-layer"
          style={style}
          value={value}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          onChange={(e) => {
            onChange(e.target.value);
            reportCursor();
          }}
          onScroll={sync}
          onKeyUp={reportCursor}
          onClick={reportCursor}
          onKeyDown={onKeyDown}
        />
      </div>
    </div>
  );
}
