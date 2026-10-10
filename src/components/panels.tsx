import { useMemo, useState } from "react";
import type { Diagnostic } from "../core/driver";
import type { BusFrame, LogEntry } from "../runtime/sim";

/* ── tiny icon set ──────────────────────────────────────── */
export const Icon = {
  play: (p: { className?: string }) => (
    <svg viewBox="0 0 24 24" className={p.className} fill="currentColor"><path d="M8 5.5v13l11-6.5z" /></svg>
  ),
  hammer: (p: { className?: string }) => (
    <svg viewBox="0 0 24 24" className={p.className} fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
      <path d="M14 3l7 7-3 3-7-7z" /><path d="M11 6L3.5 13.5 6 16l7.5-7.5" /><path d="M5 18l3 3" />
    </svg>
  ),
  stop: (p: { className?: string }) => (
    <svg viewBox="0 0 24 24" className={p.className} fill="currentColor"><rect x="6" y="6" width="12" height="12" rx="2" /></svg>
  ),
  copy: (p: { className?: string }) => (
    <svg viewBox="0 0 24 24" className={p.className} fill="none" stroke="currentColor" strokeWidth="1.7">
      <rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15V5a2 2 0 012-2h8" />
    </svg>
  ),
  download: (p: { className?: string }) => (
    <svg viewBox="0 0 24 24" className={p.className} fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round">
      <path d="M12 3v12" /><path d="M7 11l5 5 5-5" /><path d="M4 20h16" />
    </svg>
  ),
  sun: (p: { className?: string }) => (
    <svg viewBox="0 0 24 24" className={p.className} fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round">
      <circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M2 12h2M20 12h2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M19.1 4.9l-1.4 1.4M6.3 17.7l-1.4 1.4" />
    </svg>
  ),
  moon: (p: { className?: string }) => (
    <svg viewBox="0 0 24 24" className={p.className} fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round">
      <path d="M20 14.5A8.5 8.5 0 019.5 4a8.5 8.5 0 1010.5 10.5z" />
    </svg>
  ),
  monitor: (p: { className?: string }) => (
    <svg viewBox="0 0 24 24" className={p.className} fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round">
      <rect x="3" y="4" width="18" height="12" rx="2" /><path d="M8 20h8M12 16v4" />
    </svg>
  ),
  chip: (p: { className?: string }) => (
    <svg viewBox="0 0 24 24" className={p.className} fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinejoin="round">
      <rect x="6" y="6" width="12" height="12" rx="2" /><rect x="9.5" y="9.5" width="5" height="5" rx="1" />
      <path d="M9 3v3M15 3v3M9 18v3M15 18v3M3 9h3M3 15h3M18 9h3M18 15h3" />
    </svg>
  ),
};

/* ── syntax highlighting for generated artifacts ────────── */
const C_KW = /\b(void|int8_t|int16_t|int32_t|int64_t|uint8_t|uint16_t|uint32_t|uint64_t|char|double|static|const|struct|typedef|extern|if|else|for|while|do|switch|case|default|break|continue|return|sizeof)\b/;
const W_KW = /\b(module|func|param|result|local|global|memory|data|export|import|block|loop|if|else|end|br|br_if|call|drop|select|return|mut)\b/;

export function CodeView({ code, lang }: { code: string; lang: "c" | "wat" | "json" }) {
  const pieces = useMemo(() => {
    const re = new RegExp(
      [
        "(/\\*[\\s\\S]*?\\*/|//[^\\n]*|;;[^\\n]*)",
        '("(?:[^"\\\\]|\\\\.)*")',
        "\\b(0x[0-9a-fA-F]+|\\d+\\.?\\d*)\\b",
        lang === "wat" ? `\\b${W_KW.source.slice(2, -2)}\\b` : `\\b${C_KW.source.slice(2, -2)}\\b`,
        "(\\$[A-Za-z0-9_.]+|[A-Za-z_][A-Za-z0-9_]*\\s*(?=\\())",
      ].join("|"),
      "g",
    );
    const out: { t: string; c: string }[] = [];
    let last = 0;
    for (const m of code.matchAll(re)) {
      const idx = m.index ?? 0;
      if (idx > last) out.push({ t: code.slice(last, idx), c: "" });
      const cls = m[1] ? "tk-com" : m[2] ? "tk-str" : m[3] ? "tk-num" : m[4] ? "tk-key" : "tk-fn";
      out.push({ t: m[0], c: cls });
      last = idx + m[0].length;
    }
    if (last < code.length) out.push({ t: code.slice(last), c: "" });
    return out;
  }, [code, lang]);

  return (
    <pre className="min-h-full overflow-auto p-4 font-mono text-[12px] leading-[1.6]">
      {pieces.map((p, i) => (p.c ? <span key={i} className={p.c}>{p.t}</span> : <span key={i}>{p.t}</span>))}
    </pre>
  );
}

/* ── collapsible JSON / AST tree ────────────────────────── */
function TreeNode({ label, value, depth, openDepth }: { label: string; value: unknown; depth: number; openDepth: number }) {
  const [open, setOpen] = useState(depth < openDepth);
  if (value === null || typeof value !== "object") {
    const v = typeof value === "string" ? `"${value}"` : String(value);
    return (
      <div className="whitespace-pre" style={{ paddingLeft: depth * 12 }}>
        <span className="text-muted">{label}</span>
        <span className="text-muted">: </span>
        <span className={typeof value === "number" ? "tk-num" : typeof value === "string" ? "tk-str" : "tk-key"}>{v}</span>
      </div>
    );
  }
  const isArr = Array.isArray(value);
  const entries = isArr
    ? (value as unknown[]).map((v, i) => [String(i), v] as const)
    : Object.entries(value as Record<string, unknown>);
  const kindVal = !isArr ? (value as Record<string, unknown>).kind : undefined;
  const summary = isArr ? `[${entries.length}]` : typeof kindVal === "string" ? kindVal : `{${entries.length}}`;

  return (
    <div>
      <div
        className="cursor-pointer whitespace-pre hover:bg-panel2"
        style={{ paddingLeft: depth * 12 }}
        onClick={() => setOpen(!open)}
      >
        <span className="text-ember select-none">{open ? "▾ " : "▸ "}</span>
        <span className="text-muted">{label}</span>
        <span className="text-muted">: </span>
        <span className="font-semibold text-patina">{summary}</span>
      </div>
      {open &&
        entries
          .filter(([k]) => !(k === "kind" && !isArr))
          .map(([k, v]) => <TreeNode key={k} label={k} value={v} depth={depth + 1} openDepth={openDepth} />)}
    </div>
  );
}

export function JsonTree({ data, openDepth = 3 }: { data: unknown; openDepth?: number }) {
  return (
    <div className="p-3 font-mono text-[12px] leading-[1.55]">
      <TreeNode label="unit" value={data} depth={0} openDepth={openDepth} />
    </div>
  );
}

/* ── console ────────────────────────────────────────────── */
const LOG_COLOR: Record<string, string> = {
  write: "var(--ink)",
  info: "var(--muted)",
  event: "var(--patina)",
  bus: "var(--gold)",
  error: "var(--rose)",
};

export function Console({ logs, empty }: { logs: LogEntry[]; empty: string }) {
  if (!logs.length) return <Empty text={empty} />;
  return (
    <div className="py-2">
      {logs.map((l, i) => (
        <div key={i} className="log-line anim-in" style={{ borderLeftColor: l.kind === "error" ? "var(--rose)" : "transparent" }}>
          <span className="text-right text-muted opacity-70">{fmtTime(l.t)}</span>
          <span style={{ color: LOG_COLOR[l.kind] ?? "var(--ink)" }} className="whitespace-pre-wrap break-words">
            {l.text}
          </span>
        </div>
      ))}
    </div>
  );
}

export function BusTable({ frames, empty, labels }: { frames: BusFrame[]; empty: string; labels: Record<string, string> }) {
  if (!frames.length) return <Empty text={empty} />;
  return (
    <table className="w-full font-mono text-[12px]">
      <thead className="sticky top-0 bg-panel2 text-left text-[11px] uppercase tracking-wide text-muted">
        <tr>
          <th className="px-3 py-1.5 font-semibold">{labels.time}</th>
          <th className="px-3 py-1.5 font-semibold">{labels.dir}</th>
          <th className="px-3 py-1.5 font-semibold">{labels.id}</th>
          <th className="px-3 py-1.5 font-semibold">{labels.dlc}</th>
          <th className="px-3 py-1.5 font-semibold">{labels.data}</th>
        </tr>
      </thead>
      <tbody>
        {frames.map((f, i) => (
          <tr key={i} className="border-t border-line/60 hover:bg-panel2">
            <td className="px-3 py-1 text-muted">{fmtTime(f.t)}</td>
            <td className="px-3 py-1">
              <span className="chip" style={{ color: f.dir === "Tx" ? "var(--ember)" : "var(--patina)" }}>
                {f.dir === "Tx" ? labels.tx : labels.rx}
              </span>
            </td>
            <td className="px-3 py-1 font-semibold text-ink">0x{f.id.toString(16).toUpperCase().padStart(3, "0")}</td>
            <td className="px-3 py-1 text-muted">{f.dlc}</td>
            <td className="px-3 py-1 tracking-wide">
              {f.data.slice(0, Math.max(0, Math.min(8, f.dlc))).map((b) => b.toString(16).toUpperCase().padStart(2, "0")).join(" ")}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function Diagnostics({
  diags, onJump, emptyText, locale,
}: {
  diags: Diagnostic[];
  onJump: (line: number, col: number) => void;
  emptyText: string;
  locale: string;
}) {
  if (!diags.length) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-8 text-center">
        <div className="flex h-12 w-12 items-center justify-center rounded-full" style={{ background: "var(--patina-soft)" }}>
          <svg viewBox="0 0 24 24" className="h-6 w-6" fill="none" stroke="var(--patina)" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M4 12.5l5 5L20 6.5" />
          </svg>
        </div>
        <p className="max-w-sm text-sm text-muted">{emptyText}</p>
      </div>
    );
  }
  return (
    <div className="divide-y divide-line/60">
      {diags.map((d, i) => (
        <button
          key={i}
          onClick={() => onJump(d.loc.line, d.loc.col)}
          className="flex w-full items-start gap-3 px-4 py-2.5 text-left hover:bg-panel2"
        >
          <span
            className="mt-0.5 rounded px-1.5 py-0.5 font-mono text-[10px] font-bold uppercase"
            style={{
              color: d.severity === "error" ? "#fff" : "var(--ink)",
              background: d.severity === "error" ? "var(--rose)" : d.severity === "warning" ? "var(--gold)" : "var(--line)",
            }}
          >
            {d.severity}
          </span>
          <span className="min-w-0 flex-1">
            <span className="block font-mono text-[12px] text-ink">{d.message}</span>
            <span className="mt-0.5 block font-mono text-[11px] text-muted">
              {locale}({d.loc.line},{d.loc.col}) · {d.stage}
              {d.code ? ` · ${d.code}` : ""}
            </span>
          </span>
        </button>
      ))}
    </div>
  );
}

export function Empty({ text }: { text: string }) {
  return (
    <div className="flex h-full items-center justify-center p-8">
      <p className="max-w-xs text-center text-sm text-muted">{text}</p>
    </div>
  );
}

export function fmtTime(ms: number): string {
  const s = Math.floor(ms / 1000);
  const r = ms % 1000;
  return `${String(s).padStart(3, " ")}.${String(Math.floor(r)).padStart(3, "0")}`;
}
