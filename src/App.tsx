import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Editor, { type EditorMark } from "./components/Editor";
import About from "./components/About";
import { BusTable, CodeView, Console, Diagnostics, Icon, JsonTree, fmtTime } from "./components/panels";
import { EXAMPLES } from "./examples";
import { detectLang, makeT, type Lang } from "./i18n";
import { getCore, type CompileResult } from "./core/driver";
import type { WasmArtifact } from "./core/driver";
import { CaplSim, type BusFrame, type LogEntry } from "./runtime/sim";

type Theme = "light" | "dark" | "system";
type TabId = "console" | "bus" | "ast" | "c" | "wasm" | "diag" | "about";

const LS = { src: "capl.src", lang: "capl.lang", theme: "capl.theme" };

function download(name: string, data: BlobPart, type: string): void {
  const url = URL.createObjectURL(new Blob([data], { type }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 2000);
}

export default function App() {
  const [lang, setLang] = useState<Lang>(() => (localStorage.getItem(LS.lang) as Lang) || detectLang());
  const [theme, setTheme] = useState<Theme>(() => (localStorage.getItem(LS.theme) as Theme) || "system");
  const [src, setSrc] = useState<string>(() => localStorage.getItem(LS.src) ?? EXAMPLES[0].code);
  const [result, setResult] = useState<CompileResult | null>(null);
  const [tab, setTab] = useState<TabId>("console");
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [frames, setFrames] = useState<BusFrame[]>([]);
  const [simInfo, setSimInfo] = useState({ now: 0, running: false, pending: [] as { name: string; at: number; cyclic: boolean }[], halted: false });
  const [astMode, setAstMode] = useState<"tree" | "raw">("tree");
  const [cursor, setCursor] = useState({ line: 1, col: 1 });
  const [fontSize, setFontSize] = useState(13);
  const [copied, setCopied] = useState("");
  const [caret, setCaret] = useState<{ line: number; col: number; nonce: number } | undefined>();
  const [frameId, setFrameId] = useState("100");
  const [frameData, setFrameData] = useState("0B B8 01 00 00 00 00 00");
  const [keyChar, setKeyChar] = useState("g");
  const [sysvarName, setSysvarName] = useState("Demo::Rpm");
  const [sysvarValue, setSysvarValue] = useState("1500");
  const simRef = useRef<CaplSim | null>(null);

  const t = useMemo(() => makeT(lang), [lang]);
  const core = useMemo(() => getCore(), []);

  /* ── theme ─────────────────────────────────────────── */
  useEffect(() => {
    const mql = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const dark = theme === "dark" || (theme === "system" && mql.matches);
      document.documentElement.classList.toggle("dark", dark);
    };
    apply();
    mql.addEventListener("change", apply);
    localStorage.setItem(LS.theme, theme);
    return () => mql.removeEventListener("change", apply);
  }, [theme]);

  useEffect(() => {
    localStorage.setItem(LS.lang, lang);
    document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";
  }, [lang]);

  /* ── incremental build ─────────────────────────────── */
  useEffect(() => {
    const h = setTimeout(() => {
      setResult(core.compile(src));
      localStorage.setItem(LS.src, src);
    }, 280);
    return () => clearTimeout(h);
  }, [src, core]);

  const sync = useCallback(() => {
    const sim = simRef.current;
    if (!sim) return;
    setLogs([...sim.logs]);
    setFrames([...sim.frames]);
    setSimInfo({
      now: sim.now,
      running: sim.running,
      halted: sim.halted,
      pending: sim.pendingTimers.map((p) => ({ name: p.name, at: p.at, cyclic: p.cyclic })),
    });
  }, []);

  const run = useCallback(async () => {
    const r = core.compile(src);
    setResult(r);
    if (!r.ok || !r.wasm || !r.sema) {
      setTab("diag");
      return;
    }
    try {
      const sim = await CaplSim.create(r.wasm, r.sema.handlers, r.sema.timers, {});
      simRef.current = sim;
      setLogs([]);
      setFrames([]);
      sim.start();
      sync();
      setTab("console");
    } catch (err) {
      setLogs([{ t: 0, kind: "error", text: `instantiation failed: ${String(err)}` }]);
      setTab("console");
    }
  }, [core, src, sync]);

  const stop = useCallback(() => {
    simRef.current?.stop();
    sync();
  }, [sync]);

  const resetSim = useCallback(() => {
    simRef.current = null;
    setLogs([]);
    setFrames([]);
    setSimInfo({ now: 0, running: false, pending: [], halted: false });
  }, []);

  const advance = (ms: number) => {
    simRef.current?.advance(ms);
    sync();
  };

  const sendFrame = () => {
    const sim = simRef.current;
    if (!sim) return;
    const id = parseInt(frameId.replace(/^0x/i, ""), 16) || 0;
    const parsed = frameData.trim().split(/[\s,]+/).filter(Boolean).map((b) => parseInt(b, 16) & 0xff);
    const dlc = Math.min(8, parsed.length);
    const bytes = [...parsed];
    while (bytes.length < 8) bytes.push(0);
    sim.injectFrame(id, dlc, bytes);
    sync();
  };

  const pressKey = (k: string) => {
    simRef.current?.injectKey(k);
    sync();
  };

  const marks: EditorMark[] = useMemo(
    () => (result?.diags ?? []).map((d) => ({ line: d.loc.line, severity: d.severity })),
    [result],
  );

  const errCount = result?.diags.filter((d) => d.severity === "error").length ?? 0;
  const warnCount = result?.diags.filter((d) => d.severity === "warning").length ?? 0;

  const keyHandlers = useMemo(() => {
    const ks = new Set<string>();
    for (const h of result?.sema?.handlers ?? []) {
      if (h.event.kind === "key" && h.event.key) ks.add(h.event.key);
    }
    return [...ks];
  }, [result]);

  const msgHandlers = useMemo(() => {
    const ids = new Set<number>();
    for (const h of result?.sema?.handlers ?? []) {
      if (h.event.kind === "message" && h.event.id !== undefined) ids.add(h.event.id);
    }
    return [...ids];
  }, [result]);

  const sysvarHandlers = useMemo(() => {
    const names = new Set<string>();
    for (const h of result?.sema?.handlers ?? []) {
      if (h.event.kind === "sysvar" && h.event.name) names.add(h.event.name);
    }
    return [...names];
  }, [result]);

  const envVarHandlers = useMemo(() => {
    const names = new Set<string>();
    for (const h of result?.sema?.handlers ?? []) {
      if (h.event.kind === "envVar" && h.event.name) names.add(h.event.name);
    }
    return [...names];
  }, [result]);

  const hasErrorFrame = (result?.sema?.handlers ?? []).some((h) => h.event.kind === "errorFrame");
  const hasBusOff = (result?.sema?.handlers ?? []).some((h) => h.event.kind === "busOff");

  const doCopy = (what: string, text: string) => {
    void navigator.clipboard?.writeText(text);
    setCopied(what);
    setTimeout(() => setCopied(""), 1400);
  };

  const status = !result ? "statusReady" : !result.ok ? "statusError" : simInfo.running ? "statusRunning" : "statusBuilt";
  const statusColor = !result ? "var(--muted)" : !result.ok ? "var(--rose)" : simInfo.running ? "var(--patina)" : "var(--ember)";

  const TABS: { id: TabId; label: string; badge?: number }[] = [
    { id: "console", label: t("tabConsole") },
    { id: "bus", label: t("tabBus"), badge: frames.length || undefined },
    { id: "ast", label: t("tabAst") },
    { id: "c", label: t("tabC") },
    { id: "wasm", label: t("tabWat") },
    { id: "diag", label: t("tabDiag"), badge: errCount + warnCount || undefined },
    { id: "about", label: t("tabAbout") },
  ];

  return (
    <div className="flex h-screen min-h-0 flex-col">
      {/* ── header ─────────────────────────────────── */}
      <header className="topbar grid grid-cols-1 gap-2 border-b border-line bg-panel/80 px-4 py-2.5 backdrop-blur lg:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] lg:items-center">
        <div className="flex min-w-0 items-center gap-3">
          <div className="relative flex h-9 w-9 items-center justify-center rounded-xl hl-stripe shadow-sm">
            <span className="font-mono text-[13px] font-black text-white drop-shadow">C</span>
            <span className="absolute -bottom-1 -right-1 rounded-md border border-line bg-panel px-1 font-mono text-[8px] font-bold text-ember">
              wasm
            </span>
          </div>
          <div className="min-w-0 leading-tight">
            <h1 className="text-[15px] font-bold tracking-tight text-ink">{t("title")}</h1>
            <p className="whitespace-nowrap text-[11px] text-muted">{t("tagline")}</p>
          </div>
          <div className="hidden items-center gap-1.5 xl:flex">
            <span className="chip whitespace-nowrap" title="compiler core">
              <Icon.chip className="h-3 w-3" />
              {t("coreRust")} v{core.version}
            </span>
            <span className="chip whitespace-nowrap" style={{ color: statusColor, borderColor: statusColor }}>
              ● {t(status as "statusReady")}
            </span>
          </div>
        </div>

        <div className="flex flex-wrap items-center justify-center gap-1.5 lg:col-start-2 lg:row-start-1">
          <select
            className="inp cursor-pointer pr-6"
            value=""
            onChange={(e) => {
              const ex = EXAMPLES.find((x) => x.id === e.target.value);
              if (ex) {
                setSrc(ex.code);
                resetSim();
              }
            }}
          >
            <option value="">{t("examples")} ▾</option>
            {EXAMPLES.map((ex) => (
              <option key={ex.id} value={ex.id}>
                {ex.title[lang]}
              </option>
            ))}
          </select>

          <button className="btn" onClick={() => setResult(core.compile(src))}>
            <Icon.hammer className="h-3.5 w-3.5" />
            {t("compile")}
          </button>
          <button className="btn btn-primary" onClick={() => void run()}>
            <Icon.play className="h-3.5 w-3.5" />
            {t("run")}
          </button>
        </div>

        <div className="flex flex-wrap items-center justify-center gap-1.5 lg:col-start-3 lg:row-start-1 lg:justify-end">

          <div className="flex overflow-hidden rounded-lg border border-line">
            {(["light", "dark", "system"] as Theme[]).map((th) => (
              <button
                key={th}
                onClick={() => setTheme(th)}
                title={t(th)}
                className="flex items-center justify-center px-2 py-1.5 transition-colors"
                style={{
                  background: theme === th ? "var(--ember)" : "transparent",
                  color: theme === th ? "#fff" : "var(--muted)",
                }}
              >
                {th === "light" ? <Icon.sun className="h-3.5 w-3.5" /> : th === "dark" ? <Icon.moon className="h-3.5 w-3.5" /> : <Icon.monitor className="h-3.5 w-3.5" />}
              </button>
            ))}
          </div>

          <div className="flex overflow-hidden rounded-lg border border-line">
            {(["en", "zh"] as Lang[]).map((l) => (
              <button
                key={l}
                onClick={() => setLang(l)}
                className="px-2.5 py-1.5 text-[11px] font-bold transition-colors"
                style={{
                  background: lang === l ? "var(--patina)" : "transparent",
                  color: lang === l ? "#fff" : "var(--muted)",
                }}
              >
                {l === "en" ? "EN" : "中文"}
              </button>
            ))}
          </div>

          <a className="btn btn-ghost px-2 py-1.5" href="https://github.com/oldrev/capl-playground" target="_blank" rel="noreferrer" title="GitHub repository" aria-label="GitHub repository">
            <Icon.github className="h-4 w-4" />
          </a>

        </div>
      </header>

      {/* ── workspace ──────────────────────────────── */}
      <main className="grid min-h-0 flex-1 grid-cols-1 grid-rows-[minmax(0,1fr)_minmax(0,1fr)] gap-3 p-3 lg:grid-cols-2 lg:grid-rows-1">
        <section className="card flex min-h-0 flex-col overflow-hidden">
          <div className="flex items-center gap-2 border-b border-line px-3 py-1.5">
            <span className="font-mono text-[11px] font-semibold text-ink">source.can</span>
            <span className="chip">CAPL</span>
            <div className="ml-auto flex items-center gap-1">
              <button className="btn btn-ghost px-1.5 py-1" onClick={() => setFontSize((f) => Math.max(11, f - 1))}>A−</button>
              <button className="btn btn-ghost px-1.5 py-1" onClick={() => setFontSize((f) => Math.min(19, f + 1))}>A+</button>
              <button className="btn btn-ghost px-1.5 py-1" onClick={() => doCopy("src", src)} title={t("copy")}>
                <Icon.copy className="h-3.5 w-3.5" />
              </button>
            </div>
          </div>
          <div className="min-h-0 flex-1 overflow-hidden">
            <Editor
              value={src}
              onChange={setSrc}
              marks={marks}
              fontSize={fontSize}
              caret={caret}
              onRun={() => void run()}
              onCursor={(line, col) => setCursor({ line, col })}
            />
          </div>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-line bg-panel2/60 px-3 py-1.5 font-mono text-[10.5px] text-muted">
            <span>{t("linesCols", cursor.line, cursor.col)}</span>
            <Stage label={t("lex")} ms={result?.timings.lex} extra={`${result?.tokenCount ?? 0} ${t("tokens")}`} />
            <Stage label={t("parse")} ms={result?.timings.parse} extra={`${result?.nodeCount ?? 0} ${t("nodes")}`} />
            <Stage label={t("sema")} ms={result?.timings.sema} extra={`${result?.sema?.handlers.length ?? 0} ${t("handlers")}`} />
            <Stage label={t("codegen")} ms={result?.timings.codegen} extra={`${result?.wasm?.bytes.length ?? 0} ${t("bytes")}`} />
            {errCount > 0 && <span style={{ color: "var(--rose)" }}>{errCount} {t("errors")}</span>}
            {warnCount > 0 && <span style={{ color: "var(--gold)" }}>{warnCount} {t("warnings")}</span>}
          </div>
        </section>

        <section className="card flex min-h-0 flex-col overflow-hidden">
          <div className="flex items-center gap-0.5 overflow-x-auto border-b border-line px-1.5">
            {TABS.map((tb) => (
              <button key={tb.id} className="tab" data-active={tab === tb.id} onClick={() => setTab(tb.id)}>
                {tb.label}
                {tb.badge !== undefined && (
                  <span className="ml-1 rounded-full bg-embersoft px-1.5 py-px text-[9px] font-bold text-emberink">{tb.badge}</span>
                )}
              </button>
            ))}
            <div className="ml-auto flex items-center gap-1 pr-1">
              {tab === "console" && (
                <button className="btn btn-ghost px-2 py-1" onClick={() => setLogs([])}>{t("clear")}</button>
              )}
              {tab === "ast" && (
                <>
                  <button className="btn btn-ghost px-2 py-1" onClick={() => setAstMode(astMode === "tree" ? "raw" : "tree")}>
                    {astMode === "tree" ? t("rawJson") : t("tree")}
                  </button>
                  <button className="btn btn-ghost px-2 py-1" onClick={() => download("ast.json", result?.astJson ?? "", "application/json")}>
                    <Icon.download className="h-3.5 w-3.5" />
                  </button>
                </>
              )}
              {(tab === "c" || tab === "wasm") && (
                <button
                  className="btn btn-ghost px-2 py-1"
                  onClick={() => doCopy(tab, tab === "c" ? result?.c ?? "" : result?.wasm?.wat ?? "")}
                >
                  {copied === tab ? t("copied") : <Icon.copy className="h-3.5 w-3.5" />}
                </button>
              )}
              {tab === "c" && (
                <button className="btn btn-ghost px-2 py-1" onClick={() => download("source.c", result?.c ?? "", "text/x-c")}>
                  <Icon.download className="h-3.5 w-3.5" />
                </button>
              )}
              {tab === "wasm" && result?.wasm && (
                <button
                  className="btn btn-ghost px-2 py-1"
                  onClick={() => {
                    const bytes = result.wasm!.bytes instanceof Uint8Array ? result.wasm!.bytes : Uint8Array.from(result.wasm!.bytes);
                    download("source.wasm", bytes.buffer as ArrayBuffer, "application/wasm");
                  }}
                >
                  <Icon.download className="h-3.5 w-3.5" />
                </button>
              )}
            </div>
          </div>

          <div className="min-h-0 flex-1 overflow-auto">
            {tab === "console" && <Console logs={logs} empty={t("emptyConsole")} />}
            {tab === "bus" && (
              <BusTable
                frames={frames}
                empty={t("emptyBus")}
                labels={{ time: t("time"), dir: t("dir"), id: t("id"), dlc: t("dlc"), data: t("data"), tx: t("tx"), rx: t("rx") }}
              />
            )}
            {tab === "ast" &&
              (astMode === "tree"
                ? <JsonTree data={result ? JSON.parse(result.astJson) : {}} />
                : <CodeView code={result?.astJson ?? ""} lang="json" />)}
            {tab === "c" && <CodeView code={result?.c ?? `/* ${t("buildFirst")} */`} lang="c" />}
            {tab === "wasm" && (result?.wasm ? <WasmPanel art={result.wasm} t={t} /> : <CodeView code={`;; ${t("buildFirst")}`} lang="wat" />)}
            {tab === "diag" && (
              <Diagnostics
                diags={result?.diags ?? []}
                onJump={(line, col) => setCaret({ line, col, nonce: Date.now() })}
                emptyText={t("noDiag")}
                locale="source.can"
              />
            )}
            {tab === "about" && <About t={t} lang={lang} />}
          </div>
        </section>
      </main>

      {/* ── simulation bar ─────────────────────────── */}
      <footer className="flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-line bg-panel/80 px-4 py-2 backdrop-blur">
        <div className="flex items-center gap-2">
          <span className="text-[10px] font-bold uppercase tracking-wider text-muted">{t("simulation")}</span>
          <span className="rounded-lg bg-panel2 px-2 py-1 font-mono text-[12px] font-bold text-ember tabular-nums">
            {fmtTime(simInfo.now)} s
          </span>
          <button className="btn px-2 py-1" onClick={() => advance(100)} disabled={!simRef.current}>+100ms</button>
          <button className="btn px-2 py-1" onClick={() => advance(1000)} disabled={!simRef.current}>+1s</button>
          <button className="btn px-2 py-1" onClick={stop} disabled={!simInfo.running}>
            <Icon.stop className="h-3 w-3" />
            {t("stop")}
          </button>
          <button className="btn px-2 py-1" onClick={resetSim}>{t("reset")}</button>
        </div>

        <div className="flex items-center gap-1.5">
          <span className="text-[10px] font-bold uppercase tracking-wider text-muted">{t("sendMessage")}</span>
          <span className="font-mono text-[11px] text-muted">0x</span>
          <input className="inp w-14" value={frameId} onChange={(e) => setFrameId(e.target.value)} />
          <input className="inp w-52" value={frameData} onChange={(e) => setFrameData(e.target.value)} />
          <button className="btn px-2 py-1" onClick={sendFrame} disabled={!simRef.current}>{t("send")}</button>
          {msgHandlers.map((id) => (
            <button
              key={id}
              className="btn px-2 py-1 font-mono"
              onClick={() => {
                setFrameId(id.toString(16));
                simRef.current?.injectFrame(id, 8, frameData.trim().split(/[\s,]+/).filter(Boolean).map((b) => parseInt(b, 16) & 0xff));
                sync();
              }}
              disabled={!simRef.current}
            >
              0x{id.toString(16).toUpperCase()}
            </button>
          ))}
        </div>

        <div className="flex items-center gap-1.5">
          <span className="text-[10px] font-bold uppercase tracking-wider text-muted">{t("keyEvent")}</span>
          <input
            className="inp w-10 text-center"
            maxLength={1}
            value={keyChar}
            onChange={(e) => setKeyChar(e.target.value.slice(0, 1))}
          />
          <button className="btn px-2 py-1" onClick={() => pressKey(keyChar)} disabled={!simRef.current}>{t("press")}</button>
          {keyHandlers.map((k) => (
            <button key={k} className="btn px-2 py-1 font-mono" onClick={() => pressKey(k)} disabled={!simRef.current}>
              '{k}'
            </button>
          ))}
        </div>

        <div className="flex items-center gap-1.5">
          <span className="text-[10px] font-bold uppercase tracking-wider text-muted">{t("sysvar")}</span>
          <input
            className="inp w-32"
            placeholder="Namespace::Name"
            value={sysvarName}
            onChange={(e) => setSysvarName(e.target.value)}
          />
          <input
            className="inp w-16"
            value={sysvarValue}
            onChange={(e) => setSysvarValue(e.target.value.replace(/[^\d-]/g, ""))}
          />
          <button
            className="btn px-2 py-1"
            onClick={() => {
              simRef.current?.injectSysvar(sysvarName.trim(), parseInt(sysvarValue, 10) || 0);
              sync();
            }}
            disabled={!simRef.current}
          >
            {t("trigger")}
          </button>
          {sysvarHandlers.map((n) => (
            <button
              key={n}
              className="btn px-2 py-1 font-mono"
              onClick={() => {
                setSysvarName(n);
                simRef.current?.injectSysvar(n, parseInt(sysvarValue, 10) || 0);
                sync();
              }}
              disabled={!simRef.current}
            >
              {n}
            </button>
          ))}
          {envVarHandlers.map((n) => (
            <button
              key={n}
              className="btn px-2 py-1 font-mono"
              onClick={() => {
                simRef.current?.injectEnvVar(n, parseInt(sysvarValue, 10) || 0);
                sync();
              }}
              disabled={!simRef.current}
            >
              {t("envVar")} {n}
            </button>
          ))}
        </div>

        {(hasErrorFrame || hasBusOff) && (
          <div className="flex items-center gap-1.5">
            <span className="text-[10px] font-bold uppercase tracking-wider text-muted">{t("busEvent")}</span>
            {hasErrorFrame && (
              <button className="btn px-2 py-1 font-mono" onClick={() => { simRef.current?.injectBusEvent("errorFrame"); sync(); }} disabled={!simRef.current}>
                errorFrame
              </button>
            )}
            {hasBusOff && (
              <button className="btn px-2 py-1 font-mono" onClick={() => { simRef.current?.injectBusEvent("busOff"); sync(); }} disabled={!simRef.current}>
                busOff
              </button>
            )}
          </div>
        )}

        <div className="ml-auto flex items-center gap-1.5">
          <span className="text-[10px] font-bold uppercase tracking-wider text-muted">{t("pendingTimers")}</span>
          {simInfo.pending.length === 0 && <span className="chip">0</span>}
          {simInfo.pending.slice(0, 4).map((p, i) => (
            <span key={i} className="chip" style={{ color: "var(--patina)", borderColor: "var(--patina)" }}>
              {p.name} @ {fmtTime(p.at)}{p.cyclic ? " ↻" : ""}
            </span>
          ))}
        </div>
      </footer>
    </div>
  );
}

function Stage({ label, ms, extra }: { label: string; ms?: number; extra: string }) {
  return (
    <span className="inline-flex items-center gap-1">
      <span className="font-semibold text-inksoft">{label}</span>
      <span className="text-ember">{ms !== undefined ? `${ms.toFixed(2)}ms` : "—"}</span>
      <span className="opacity-70">· {extra}</span>
    </span>
  );
}

function WasmPanel({ art, t }: { art: WasmArtifact; t: ReturnType<typeof makeT> }) {
  const [showLayout, setShowLayout] = useState(true);
  return (
    <div className="flex min-h-full flex-col">
      <div className="grid grid-cols-2 gap-px border-b border-line bg-line md:grid-cols-4">
        {[
          [t("wasmSize"), `${art.bytes.length} B`],
          ["memory", `${art.memoryPages} × 64 KiB`],
          [t("exports"), String(art.exports.length)],
          [t("imports"), String(art.imports.length)],
        ].map(([k, v]) => (
          <div key={k} className="bg-panel px-3 py-2">
            <div className="text-[10px] uppercase tracking-wider text-muted">{k}</div>
            <div className="font-mono text-[13px] font-bold text-ink">{v}</div>
          </div>
        ))}
      </div>

      <div className="border-b border-line">
        <button
          className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-[11px] font-bold uppercase tracking-wider text-muted hover:bg-panel2"
          onClick={() => setShowLayout(!showLayout)}
        >
          <span className="text-ember">{showLayout ? "▾" : "▸"}</span> memory layout · {art.layout.length} objects
        </button>
        {showLayout && (
          <div className="max-h-56 overflow-auto">
            <table className="w-full font-mono text-[11.5px]">
              <tbody>
                {art.layout.map((l, i) => (
                  <tr key={i} className="border-t border-line/50">
                    <td className="w-24 px-3 py-0.5 text-muted">
                      {l.addr < 0 ? "host" : `0x${l.addr.toString(16).padStart(4, "0")}`}
                    </td>
                    <td className="w-16 px-2 py-0.5 text-right text-muted">{l.size} B</td>
                    <td className="w-28 px-2 py-0.5 text-patina">{l.ty}</td>
                    <td className="px-2 py-0.5 text-ink">{l.name}</td>
                    <td className="w-20 px-2 py-0.5 text-right text-muted">{l.kind}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      <div className="min-h-0 flex-1">
        <CodeView code={art.wat} lang="wat" />
      </div>
    </div>
  );
}
