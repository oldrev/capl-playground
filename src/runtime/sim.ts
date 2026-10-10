/* capl-rt (host half) — a miniature CANoe measurement running the module that
 * the wasm backend produced. Virtual time, timer queue, bus, key events. */

import type { HandlerInfo, TimerInfo, WasmArtifact } from "../core/driver";

export type LogKind = "write" | "info" | "event" | "error" | "bus";

export interface LogEntry {
  t: number;
  kind: LogKind;
  text: string;
}

export interface BusFrame {
  t: number;
  id: number;
  dlc: number;
  data: number[];
  dir: "Tx" | "Rx";
  channel: number;
}

interface SimEvent {
  time: number;
  seq: number;
  kind: "timer" | "message" | "key" | "start" | "stop";
  timerId?: number;
  frame?: BusFrame;
  key?: string;
}

const MSG_SIZE = 32;
const FUEL = 4_000_000;
const MAX_EVENTS = 20_000;
const MAX_TIME = 600_000;

export interface SimHooks {
  onLog?: (e: LogEntry) => void;
  onFrame?: (f: BusFrame) => void;
  onUpdate?: () => void;
}

export class CaplSim {
  now = 0;
  running = false;
  /** how much virtual time a single user action is allowed to consume (ms) */
  window = 2000;
  readonly logs: LogEntry[] = [];
  readonly frames: BusFrame[] = [];
  pendingTimers: { id: number; name: string; at: number; cyclic: boolean }[] = [];

  private instance!: WebAssembly.Instance;
  private mem!: WebAssembly.Memory;
  private queue: SimEvent[] = [];
  private seq = 0;
  private cyclic = new Map<number, number>();
  private eventBudget = MAX_EVENTS;
  private stopped = false;

  private constructor(
    private readonly art: WasmArtifact,
    private readonly handlers: HandlerInfo[],
    private readonly timers: TimerInfo[],
    private readonly hooks: SimHooks,
  ) {}

  static async create(
    art: WasmArtifact,
    handlers: HandlerInfo[],
    timers: TimerInfo[],
    hooks: SimHooks = {},
  ): Promise<CaplSim> {
    const sim = new CaplSim(
      art,
      handlers.map((h) => ({ exportName: h.exportName, event: h.event })),
      timers.map((t) => ({ id: t.id, name: t.name })),
      hooks,
    );
    await sim.instantiate();
    return sim;
  }

  /* ── wasm host interface ───────────────────────────── */
  private async instantiate(): Promise<void> {
    const imports = {
      capl: {
        write: (fmt: number, argc: number, args: number) => this.hostWrite(fmt, argc, args),
        setTimer: (id: number, ms: number) => this.hostSetTimer(id, ms, false),
        setTimerCyclic: (id: number, ms: number) => this.hostSetTimer(id, ms, true),
        cancelTimer: (id: number) => this.hostCancelTimer(id),
        output: (ptr: number) => this.hostOutput(ptr),
        timeNow: () => this.now | 0,
        random: (n: number) => (n <= 0 ? 0 : Math.floor(Math.random() * n)),
        strlen: (ptr: number) => this.cstr(ptr).length,
        pow: Math.pow,
        sin: Math.sin,
        cos: Math.cos,
        canOnline: () => this.log("info", "canOnline()"),
        canOffline: () => this.log("info", "canOffline()"),
        stopMeasurement: () => {
          this.log("info", "stopMeasurement()");
          this.stopped = true;
        },
      },
    };
    const bytes = this.art.bytes instanceof Uint8Array ? this.art.bytes : Uint8Array.from(this.art.bytes);
    const instantiated = await WebAssembly.instantiate(bytes, imports);
    const source = instantiated as WebAssembly.WebAssemblyInstantiatedSource | WebAssembly.Instance;
    this.instance = "instance" in source ? source.instance : source;
    this.mem = this.instance.exports.memory as WebAssembly.Memory;
  }

  private get u8(): Uint8Array {
    return new Uint8Array(this.mem.buffer);
  }
  private get view(): DataView {
    return new DataView(this.mem.buffer);
  }

  private cstr(ptr: number): string {
    const u8 = this.u8;
    let end = ptr;
    while (end < u8.length && u8[end] !== 0) end++;
    return new TextDecoder().decode(u8.subarray(ptr, end));
  }

  private log(kind: LogKind, text: string): void {
    const e: LogEntry = { t: this.now, kind, text };
    this.logs.push(e);
    if (this.logs.length > 4000) this.logs.splice(0, 1000);
    this.hooks.onLog?.(e);
  }

  /* ── printf-ish formatting for write() ─────────────── */
  private hostWrite(fmtPtr: number, argc: number, argsPtr: number): void {
    const fmt = this.cstr(fmtPtr);
    const dv = this.view;
    const args: { tag: number; i: bigint; f: number }[] = [];
    for (let k = 0; k < argc; k++) {
      const base = argsPtr + k * 16;
      const tag = dv.getInt32(base, true);
      args.push({
        tag,
        i: tag === 4 ? 0n : dv.getBigInt64(base + 8, true),
        f: tag === 4 ? dv.getFloat64(base + 8, true) : 0,
      });
    }
    this.log("write", this.format(fmt, args));
  }

  private format(fmt: string, args: { tag: number; i: bigint; f: number }[]): string {
    let out = "";
    let ai = 0;
    for (let i = 0; i < fmt.length; i++) {
      const c = fmt[i];
      if (c !== "%") {
        out += c;
        continue;
      }
      if (fmt[i + 1] === "%") {
        out += "%";
        i++;
        continue;
      }
      const m = /^%([-+ 0#]*)(\d+)?(?:\.(\d+))?(hh|h|ll|l|I64|L)?([diuxXofgeEcs])/.exec(fmt.slice(i));
      if (!m) {
        out += c;
        continue;
      }
      i += m[0].length - 1;
      const [, flags, widthS, precS, , conv] = m;
      const width = widthS ? parseInt(widthS, 10) : 0;
      const prec = precS !== undefined ? parseInt(precS, 10) : undefined;
      const a = args[ai++] ?? { tag: 0, i: 0n, f: 0 };
      let s: string;
      const unsigned = a.tag === 1 || a.tag === 3;
      const asInt = (): bigint => {
        if (a.tag === 4) return BigInt(Math.trunc(a.f));
        if (unsigned) return a.i < 0n ? a.i + (1n << 64n) : a.i;
        return a.i;
      };
      switch (conv) {
        case "d": case "i": {
          const v = a.tag === 4 ? BigInt(Math.trunc(a.f)) : a.i;
          s = v.toString();
          if (flags.includes("+") && v >= 0n) s = `+${s}`;
          break;
        }
        case "u": s = asInt().toString(); break;
        case "x": s = (asInt() & 0xffffffffffffffffn).toString(16); break;
        case "X": s = (asInt() & 0xffffffffffffffffn).toString(16).toUpperCase(); break;
        case "o": s = (asInt() & 0xffffffffffffffffn).toString(8); break;
        case "c": s = String.fromCharCode(Number(a.i & 0xffn)); break;
        case "s": s = a.tag === 5 ? this.cstr(Number(a.i)) : String(a.i); break;
        case "f": {
          const v = a.tag === 4 ? a.f : Number(a.i);
          s = v.toFixed(prec ?? 6);
          if (flags.includes("+") && v >= 0) s = `+${s}`;
          break;
        }
        case "e": case "E": {
          const v = a.tag === 4 ? a.f : Number(a.i);
          s = v.toExponential(prec ?? 6);
          if (conv === "E") s = s.toUpperCase();
          break;
        }
        case "g": {
          const v = a.tag === 4 ? a.f : Number(a.i);
          s = String(prec !== undefined ? Number(v.toPrecision(prec)) : v);
          break;
        }
        default: s = "";
      }
      if ((conv === "x" || conv === "X" || conv === "o") && flags.includes("#") && s !== "0") {
        s = (conv === "o" ? "0" : conv === "x" ? "0x" : "0X") + s;
      }
      if (width > s.length) {
        if (flags.includes("-")) s = s.padEnd(width, " ");
        else if (flags.includes("0") && "diuxXofgeE".includes(conv)) {
          const neg = s.startsWith("-") || s.startsWith("+");
          s = neg ? s[0] + s.slice(1).padStart(width - 1, "0") : s.padStart(width, "0");
        } else s = s.padStart(width, " ");
      }
      out += s;
    }
    return out;
  }

  /* ── timers ────────────────────────────────────────── */
  private timerName(id: number): string {
    return this.timers.find((t) => t.id === id)?.name ?? `timer#${id}`;
  }

  private hostSetTimer(id: number, ms: number, cyclic: boolean): void {
    const delay = Math.max(0, ms | 0);
    this.queue = this.queue.filter((e) => !(e.kind === "timer" && e.timerId === id));
    if (cyclic) this.cyclic.set(id, delay);
    this.push({ time: this.now + delay, seq: this.seq++, kind: "timer", timerId: id });
    this.log("event", `setTimer(${this.timerName(id)}, ${delay} ms)${cyclic ? " [cyclic]" : ""}`);
  }

  private hostCancelTimer(id: number): void {
    this.queue = this.queue.filter((e) => !(e.kind === "timer" && e.timerId === id));
    this.cyclic.delete(id);
    this.log("event", `cancelTimer(${this.timerName(id)})`);
  }

  private hostOutput(ptr: number): void {
    const dv = this.view;
    const frame: BusFrame = {
      t: this.now,
      id: dv.getUint32(ptr, true),
      dlc: dv.getInt32(ptr + 4, true),
      data: Array.from(this.u8.subarray(ptr + 8, ptr + 16)),
      dir: "Tx",
      channel: dv.getInt32(ptr + 16, true) || 1,
    };
    this.frames.push(frame);
    this.hooks.onFrame?.(frame);
    this.log("bus", `output 0x${frame.id.toString(16).toUpperCase()} [${frame.dlc}] ${frame.data.slice(0, frame.dlc).map(hex2).join(" ")}`);
    // a transmitted frame is received by the node itself, exactly like CANoe
    this.push({ time: this.now, seq: this.seq++, kind: "message", frame: { ...frame, dir: "Tx" } });
  }

  /* ── event queue ───────────────────────────────────── */
  private push(e: SimEvent): void {
    this.queue.push(e);
    this.queue.sort((a, b) => a.time - b.time || a.seq - b.seq);
  }

  private exportFn(name: string): ((...a: number[]) => unknown) | undefined {
    const f = this.instance.exports[name];
    return typeof f === "function" ? (f as (...a: number[]) => unknown) : undefined;
  }

  private callHandler(name: string, label: string): void {
    const fn = this.exportFn(name);
    if (!fn) return;
    this.exportFn("__set_fuel")?.(FUEL);
    try {
      fn();
    } catch (err) {
      const msg = String(err);
      this.log("error", `trap in '${label}': ${msg.includes("unreachable") ? "execution budget exhausted (endless loop?)" : msg}`);
      this.stopped = true;
      this.running = false;
    }
  }

  private setThisFrame(f: BusFrame): void {
    const addr = this.art.msgbufAddr;
    const dv = this.view;
    const u8 = this.u8;
    u8.fill(0, addr, addr + MSG_SIZE);
    dv.setUint32(addr, f.id >>> 0, true);
    dv.setInt32(addr + 4, f.dlc, true);
    for (let i = 0; i < 8; i++) u8[addr + 8 + i] = f.data[i] ?? 0;
    dv.setInt32(addr + 16, f.channel, true);
    dv.setInt32(addr + 20, f.dir === "Tx" ? 1 : 0, true);
    dv.setInt32(addr + 28, this.now | 0, true);
    this.exportFn("__set_this")?.(addr);
  }

  /* ── public controls ───────────────────────────────── */
  start(): void {
    if (this.running) return;
    this.running = true;
    this.stopped = false;
    this.eventBudget = MAX_EVENTS;
    this.exportFn("__set_fuel")?.(FUEL);
    this.exportFn("__init")?.();
    this.log("info", "measurement started");
    this.callHandler("on_preStart", "on preStart");
    this.callHandler("on_start", "on start");
    this.drain(this.now + this.window);
  }

  stop(): void {
    if (!this.running) return;
    this.callHandler("on_preStop", "on preStop");
    this.callHandler("on_stopMeasurement", "on stopMeasurement");
    this.running = false;
    this.log("info", "measurement stopped");
    this.refreshPending();
    this.hooks.onUpdate?.();
  }

  injectFrame(id: number, dlc: number, data: number[], channel = 1): void {
    const frame: BusFrame = { t: this.now, id, dlc, data, dir: "Rx", channel };
    this.frames.push(frame);
    this.hooks.onFrame?.(frame);
    this.log("bus", `rx 0x${id.toString(16).toUpperCase()} [${dlc}] ${data.slice(0, dlc).map(hex2).join(" ")}`);
    this.push({ time: this.now, seq: this.seq++, kind: "message", frame });
    this.drain(this.now + this.window);
  }

  injectKey(key: string): void {
    this.log("event", `key '${key}'`);
    this.push({ time: this.now, seq: this.seq++, kind: "key", key });
    this.drain(this.now + this.window);
  }

  /** `on sysvar Namespace::Name` — `this` carries the new value */
  injectSysvar(name: string, value: number): void {
    const hit = this.handlers.find((h) => h.event.kind === "sysvar" && h.event.name === name);
    if (!hit) {
      this.log("info", `no 'on sysvar ${name}' handler in this node`);
      return;
    }
    this.log("event", `sysvar ${name} = ${value}`);
    this.exportFn("__set_this")?.(value | 0);
    this.callHandler(hit.exportName, `on sysvar ${name}`);
    this.hooks.onUpdate?.();
  }

  injectEnvVar(name: string, value: number): void {
    const hit = this.handlers.find((h) => h.event.kind === "envVar" && h.event.name === name);
    if (!hit) {
      this.log("info", `no 'on envVar ${name}' handler in this node`);
      return;
    }
    this.log("event", `envVar ${name} = ${value}`);
    this.exportFn("__set_this")?.(value | 0);
    this.callHandler(hit.exportName, `on envVar ${name}`);
    this.hooks.onUpdate?.();
  }

  /** `on errorFrame` / `on busOff` */
  injectBusEvent(kind: "errorFrame" | "busOff"): void {
    const hit = this.handlers.find((h) => h.event.kind === kind);
    if (!hit) {
      this.log("info", `no 'on ${kind}' handler in this node`);
      return;
    }
    this.log("event", kind);
    this.exportFn("__set_this")?.(0);
    this.callHandler(hit.exportName, `on ${kind}`);
    this.hooks.onUpdate?.();
  }

  /** advance virtual time by `ms`, running everything scheduled in between */
  advance(ms: number): void {
    const target = this.now + ms;
    this.drain(target);
    if (!this.stopped) this.now = Math.max(this.now, target);
    this.refreshPending();
    this.hooks.onUpdate?.();
  }

  private drain(until = Number.POSITIVE_INFINITY): void {
    let guard = 0;
    while (this.queue.length && !this.stopped && guard++ < MAX_EVENTS) {
      const next = this.queue[0];
      if (next.time > until || next.time > MAX_TIME) break;
      if (--this.eventBudget <= 0) {
        this.log("error", "event budget exhausted — simulation halted");
        this.stopped = true;
        break;
      }
      this.queue.shift();
      this.now = Math.max(this.now, next.time);
      this.dispatch(next);
    }
    this.refreshPending();
    this.hooks.onUpdate?.();
  }

  private dispatch(e: SimEvent): void {
    switch (e.kind) {
      case "timer": {
        const name = this.timerName(e.timerId ?? 0);
        const period = this.cyclic.get(e.timerId ?? -1);
        this.callHandler(`on_timer_${name}`, `on timer ${name}`);
        if (period !== undefined && !this.stopped) {
          this.push({ time: this.now + period, seq: this.seq++, kind: "timer", timerId: e.timerId });
        }
        break;
      }
      case "message": {
        const f = e.frame!;
        this.setThisFrame(f);
        for (const h of this.handlers) {
          if (h.event.kind !== "message") continue;
          // `on message CAN2.0x100` only sees channel 2
          if (h.event.channel !== undefined && h.event.channel !== f.channel) continue;
          const match = h.event.wildcard || h.event.id === f.id;
          if (!match) continue;
          this.setThisFrame(f);
          this.callHandler(h.exportName, `on message 0x${f.id.toString(16)}`);
        }
        break;
      }
      case "key": {
        const code = (e.key ?? " ").charCodeAt(0);
        for (const h of this.handlers) {
          if (h.event.kind !== "key") continue;
          const match = h.event.wildcard || h.event.key?.charCodeAt(0) === code;
          if (!match) continue;
          this.exportFn("__set_this")?.(code);
          this.callHandler(h.exportName, `on key '${e.key}'`);
        }
        break;
      }
      default:
        break;
    }
  }

  private refreshPending(): void {
    this.pendingTimers = this.queue
      .filter((e) => e.kind === "timer")
      .map((e) => ({
        id: e.timerId ?? 0,
        name: this.timerName(e.timerId ?? 0),
        at: e.time,
        cyclic: this.cyclic.has(e.timerId ?? -1),
      }));
  }

  get queueLength(): number {
    return this.queue.length;
  }
  get halted(): boolean {
    return this.stopped;
  }
}

function hex2(b: number): string {
  return b.toString(16).toUpperCase().padStart(2, "0");
}
