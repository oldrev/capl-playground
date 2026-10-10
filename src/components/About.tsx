import type { TFn } from "../i18n";
import { CodeView } from "./panels";

const CRATES: { name: string; en: string; zh: string; dep: string }[] = [
  { name: "capl-diag", en: "source locations, spans, diagnostic bag", zh: "源码位置、区间与诊断集合", dep: "—" },
  { name: "capl-lexer", en: "hand-written scanner; also drives this editor's highlighter", zh: "手写扫描器；同时驱动编辑器高亮", dep: "capl-diag" },
  { name: "capl-ast", en: "nodes, type lattice, visitor, JSON export", zh: "节点、类型格、访问者与 JSON 导出", dep: "capl-diag" },
  { name: "capl-parse", en: "recursive descent, error recovery, zero semantics", zh: "递归下降、错误恢复，不含语义", dep: "capl-lexer, capl-ast" },
  { name: "capl-sema", en: "scopes, symbols, types — decorates the AST in place", zh: "作用域、符号、类型 —— 就地标注 AST", dep: "capl-ast" },
  { name: "capl-backend", en: "the Backend trait + artifact types", zh: "Backend trait 与产物类型", dep: "capl-sema" },
  { name: "capl-backend-wasm", en: "WebAssembly binary encoder + code generator", zh: "WebAssembly 二进制编码与代码生成", dep: "capl-backend" },
  { name: "capl-backend-c", en: "portable C99 emitter", zh: "可移植 C99 生成器", dep: "capl-backend" },
  { name: "capl-driver", en: "stage orchestration + capl CLI", zh: "阶段编排与 capl 命令行", dep: "all of the above" },
  { name: "capl-wasm", en: "wasm-bindgen facade consumed by this page", zh: "供本页面使用的 wasm-bindgen 外观层", dep: "capl-driver" },
];

const TRAIT_SNIPPET = `/// crates/capl-backend/src/lib.rs
pub trait Backend {
    /// stable id used by \`--backend=<id>\`
    fn id(&self) -> &'static str;

    /// consume the *decorated* AST; never resolve a symbol here
    fn emit(&mut self, unit: &Unit, sema: &SemaResult, diag: &mut DiagBag)
        -> Result<Artifact, BackendError>;
}

pub enum Artifact {
    Binary { bytes: Vec<u8>, listing: String },
    Source { text: String, extension: &'static str },
    Json(serde_json::Value),
}

/// Backends register themselves, so an embedder can add its own
/// (LLVM IR, C#, a bytecode VM ...) without touching the front end.
inventory::submit!(BackendEntry::new("wasm", || Box::new(WasmBackend::default())));
inventory::submit!(BackendEntry::new("c",    || Box::new(CBackend::default())));`;

const SUBSET = {
  en: [
    "variables{} blocks, globals, const, arrays and brace initialisers",
    "scalar types: byte word dword qword int long int64 float double char",
    "enum with implicit and explicit values, usable in switch labels",
    "struct with computed member layout (offsets, alignment, padding)",
    "multi-dimensional arrays, arrays of structs, struct members of array type",
    "message objects: .id .dlc .can .byte(n) .word(n) .dword(n), this",
    "timers: timer / msTimer, setTimer, setTimerCyclic, cancelTimer",
    "events: preStart start stopMeasurement preStop timer message key sysvar envVar errorFrame busOff",
    "statements: if else while do for switch-fallthrough break continue return",
    "full C expression grammar incl. ?:, short-circuit && ||, compound assignment",
    "user functions with recursion and array parameters",
    "built-ins: write (printf), output, elCount, timeNow, random, abs, min, max, sqrt, pow, sin, cos, floor, ceil, strlen",
  ],
  zh: [
    "variables{} 块、全局变量、const、数组与花括号初始化",
    "标量类型：byte word dword qword int long int64 float double char",
    "enum：支持隐式与显式取值，可直接用于 switch 标签",
    "struct：自动计算成员布局（偏移、对齐与填充）",
    "多维数组、结构体数组、以及数组类型的结构体成员",
    "报文对象：.id .dlc .can .byte(n) .word(n) .dword(n)，以及 this",
    "定时器：timer / msTimer、setTimer、setTimerCyclic、cancelTimer",
    "事件：preStart start stopMeasurement preStop timer message key sysvar envVar errorFrame busOff",
    "语句：if else while do for switch（支持贯穿）break continue return",
    "完整的 C 表达式文法，含 ?:、短路 && ||、复合赋值",
    "用户函数，支持递归与数组参数",
    "内建函数：write（printf）、output、elCount、timeNow、random、abs、min、max、sqrt、pow、sin、cos、floor、ceil、strlen",
  ],
};

export default function About({ t, lang }: { t: TFn; lang: "en" | "zh" }) {
  const stages = ["capl-lexer", "capl-parse", "capl-sema", "capl-backend-*"];
  return (
    <div className="space-y-6 p-5">
      <section>
        <h2 className="text-base font-semibold text-ink">{t("aboutTitle")}</h2>
        <p className="mt-2 max-w-3xl text-sm leading-relaxed text-inksoft">{t("aboutBody")}</p>
      </section>

      <section className="flex flex-wrap items-center gap-2">
        {stages.map((s, i) => (
          <div key={s} className="flex items-center gap-2">
            <div className="rounded-lg border border-line bg-panel2 px-3 py-1.5 font-mono text-[11px] font-semibold text-ink">
              {s}
            </div>
            {i < stages.length - 1 && <span className="text-ember">→</span>}
          </div>
        ))}
        <span className="ml-1 text-xs text-muted">{t("aboutPipelineNote")}</span>
      </section>

      <section>
        <h3 className="mb-2 text-xs font-bold uppercase tracking-wider text-muted">{t("aboutCrates")}</h3>
        <div className="overflow-hidden rounded-xl border border-line">
          <table className="w-full text-left text-[12px]">
            <tbody>
              {CRATES.map((c, i) => (
                <tr key={c.name} className={i % 2 ? "bg-panel2/60" : ""}>
                  <td className="whitespace-nowrap px-3 py-1.5 font-mono font-semibold text-ember">{c.name}</td>
                  <td className="px-3 py-1.5 text-inksoft">{lang === "zh" ? c.zh : c.en}</td>
                  <td className="whitespace-nowrap px-3 py-1.5 font-mono text-[11px] text-muted">{c.dep}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>

      <section>
        <h3 className="mb-2 text-xs font-bold uppercase tracking-wider text-muted">Backend trait</h3>
        <div className="overflow-hidden rounded-xl border border-line bg-panel2/40">
          <CodeView code={TRAIT_SNIPPET} lang="c" />
        </div>
      </section>

      <section>
        <h3 className="mb-2 text-xs font-bold uppercase tracking-wider text-muted">{t("aboutLimits")}</h3>
        <ul className="grid gap-1.5 md:grid-cols-2">
          {SUBSET[lang].map((s) => (
            <li key={s} className="flex items-start gap-2 text-[12.5px] leading-relaxed text-inksoft">
              <span className="mt-[7px] h-1.5 w-1.5 shrink-0 rounded-full" style={{ background: "var(--patina)" }} />
              {s}
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}
