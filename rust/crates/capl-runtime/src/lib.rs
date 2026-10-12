use capl_model::BusFrame;
use capl_sysvars::{SysVarError, SysVarKey, SysVarStore, SysVarValue};
use capl_trace_core::TraceEvent;
use std::collections::HashMap;
use std::fmt;
use wasmi::{
    core::{ValueType, F64},
    Engine, Extern, FuncType, Linker, Module, Store, Value,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct VirtualTime(pub u64);

pub trait MessageHandler {
    fn on_message(&mut self, frame: &BusFrame, now: VirtualTime);
}

pub struct ReplayEngine {
    events: Vec<TraceEvent>,
    cursor: usize,
    now: VirtualTime,
}
impl ReplayEngine {
    pub fn new(mut events: Vec<TraceEvent>) -> Self {
        events.sort_by_key(|event| match event {
            TraceEvent::Frame(frame) => frame.timestamp_us,
            _ => u64::MAX,
        });
        Self {
            events,
            cursor: 0,
            now: VirtualTime::default(),
        }
    }
    pub fn now(&self) -> VirtualTime {
        self.now
    }
    pub fn remaining(&self) -> usize {
        self.events.len().saturating_sub(self.cursor)
    }
    pub fn step<H: MessageHandler>(&mut self, handler: &mut H) -> bool {
        while let Some(event) = self.events.get(self.cursor) {
            self.cursor += 1;
            if let TraceEvent::Frame(frame) = event {
                self.now = VirtualTime(frame.timestamp_us);
                handler.on_message(frame, self.now);
                return true;
            }
        }
        false
    }
    pub fn run_until<H: MessageHandler>(&mut self, until_us: u64, handler: &mut H) {
        while self.events.get(self.cursor).is_some_and(
            |event| matches!(event, TraceEvent::Frame(frame) if frame.timestamp_us <= until_us),
        ) {
            self.step(handler);
        }
        self.now = VirtualTime(until_us);
    }
    pub fn reset(&mut self) {
        self.cursor = 0;
        self.now = VirtualTime::default();
    }
}

/// A compiled CAPL node that can consume recorded traffic in virtual time.
/// The handler metadata is deliberately small so callers can build it from
/// either `capl-ast` or the browser-facing contract without coupling this
/// runtime to a particular frontend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayHandler {
    pub export_name: String,
    pub id: Option<u32>,
    pub channel: Option<u16>,
    pub wildcard: bool,
    pub sysvar: Option<String>,
    pub timer: Option<String>,
    pub key: Option<String>,
    pub envvar: Option<String>,
    pub special: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayTimer {
    pub id: u32,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTimer {
    pub id: u32,
    pub name: String,
    pub at_us: u64,
    pub cyclic: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ReplayOutput {
    pub frames: Vec<BusFrame>,
    pub writes: Vec<String>,
    pub errors: Vec<String>,
    pub events: Vec<RuntimeEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeEvent {
    pub timestamp_us: u64,
    pub kind: String,
    pub text: String,
}

impl RuntimeEvent {
    pub fn new(timestamp_us: u64, kind: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            timestamp_us,
            kind: kind.into(),
            text: text.into(),
        }
    }
}

#[derive(Default)]
struct HostState {
    now: u64,
    message_addr: u32,
    output: ReplayOutput,
    timers: Vec<PendingTimer>,
    timer_names: HashMap<u32, String>,
    timer_periods: HashMap<u32, u64>,
}

#[derive(Debug)]
pub enum WasmReplayError {
    Wasm(String),
    MissingExport(String),
    Trap(String),
}
impl fmt::Display for WasmReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wasm(e) => write!(f, "wasm: {e}"),
            Self::MissingExport(e) => write!(f, "missing export: {e}"),
            Self::Trap(e) => write!(f, "trap: {e}"),
        }
    }
}
impl std::error::Error for WasmReplayError {}

fn schedule_timer(state: &mut HostState, id: i32, ms: i32, cyclic: bool) {
    let id = id.max(0) as u32;
    let delay = ms.max(0) as u64 * 1000;
    let name = state
        .timer_names
        .get(&id)
        .cloned()
        .unwrap_or_else(|| format!("timer#{id}"));
    state.timers.retain(|timer| timer.id != id);
    if cyclic {
        state.timer_periods.insert(id, delay);
    } else {
        state.timer_periods.remove(&id);
    }
    schedule_timer_at(state, id, name, state.now.saturating_add(delay), cyclic);
}

fn schedule_timer_at(state: &mut HostState, id: u32, name: String, at_us: u64, cyclic: bool) {
    state.timers.push(PendingTimer {
        id,
        name,
        at_us,
        cyclic,
    });
    state.timers.sort_by_key(|timer| (timer.at_us, timer.id));
}

fn read_c_string(
    memory: &wasmi::Memory,
    caller: &wasmi::Caller<'_, HostState>,
    ptr: i32,
) -> Option<String> {
    let mut bytes = vec![0u8; 4096];
    memory.read(caller, ptr.max(0) as usize, &mut bytes).ok()?;
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

#[derive(Clone, Copy)]
struct WriteArg {
    tag: i32,
    integer: i64,
    unsigned: u64,
    float: f64,
}

fn format_capl_write(
    fmt: &str,
    count: usize,
    args_ptr: usize,
    memory: &wasmi::Memory,
    caller: &wasmi::Caller<'_, HostState>,
) -> String {
    let mut args = Vec::with_capacity(count.min(64));
    for index in 0..count.min(64) {
        let mut raw = [0u8; 16];
        if memory
            .read(caller, args_ptr.saturating_add(index * 16), &mut raw)
            .is_err()
        {
            break;
        }
        let tag = i32::from_le_bytes(raw[0..4].try_into().unwrap());
        let bits = u64::from_le_bytes(raw[8..16].try_into().unwrap());
        args.push(WriteArg {
            tag,
            integer: bits as i64,
            unsigned: bits,
            float: f64::from_bits(bits),
        });
    }
    let chars: Vec<char> = fmt.chars().collect();
    let mut output = String::new();
    let mut index = 0;
    let mut arg_index = 0;
    while index < chars.len() {
        if chars[index] != '%' {
            output.push(chars[index]);
            index += 1;
            continue;
        }
        if chars.get(index + 1) == Some(&'%') {
            output.push('%');
            index += 2;
            continue;
        }
        index += 1;
        let mut flags = String::new();
        while index < chars.len() && "-+ 0#".contains(chars[index]) {
            flags.push(chars[index]);
            index += 1;
        }
        let mut width_s = String::new();
        while index < chars.len() && chars[index].is_ascii_digit() {
            width_s.push(chars[index]);
            index += 1;
        }
        let width = width_s.parse::<usize>().unwrap_or(0).min(256);
        let mut precision = None;
        if chars.get(index) == Some(&'.') {
            index += 1;
            let mut value = String::new();
            while index < chars.len() && chars[index].is_ascii_digit() {
                value.push(chars[index]);
                index += 1;
            }
            precision = Some(value.parse::<usize>().unwrap_or(0).min(32));
        }
        while index < chars.len() && matches!(chars[index], 'h' | 'l' | 'L' | 'I') {
            index += 1;
        }
        let Some(&conversion) = chars.get(index) else {
            output.push('%');
            break;
        };
        index += 1;
        let arg = args.get(arg_index).copied().unwrap_or(WriteArg {
            tag: 0,
            integer: 0,
            unsigned: 0,
            float: 0.0,
        });
        arg_index += 1;
        let mut value = match conversion {
            'd' | 'i' => {
                if arg.tag == 4 {
                    (arg.float.trunc() as i64).to_string()
                } else {
                    arg.integer.to_string()
                }
            }
            'u' => arg.unsigned.to_string(),
            'x' => format!("{:x}", arg.unsigned),
            'X' => format!("{:X}", arg.unsigned),
            'o' => format!("{:o}", arg.unsigned),
            'c' => char::from_u32(arg.integer as u32 & 0xff)
                .unwrap_or('\u{FFFD}')
                .to_string(),
            's' => read_c_string(memory, caller, arg.integer as i32).unwrap_or_default(),
            'f' => format!(
                "{:.*}",
                precision.unwrap_or(6),
                if arg.tag == 4 {
                    arg.float
                } else {
                    arg.integer as f64
                }
            ),
            'e' | 'E' => {
                let text = format!(
                    "{:.*e}",
                    precision.unwrap_or(6),
                    if arg.tag == 4 {
                        arg.float
                    } else {
                        arg.integer as f64
                    }
                );
                if conversion == 'E' {
                    text.to_uppercase()
                } else {
                    text
                }
            }
            'g' | 'G' => {
                let n = if arg.tag == 4 {
                    arg.float
                } else {
                    arg.integer as f64
                };
                let p = precision.unwrap_or(6).max(1);
                let text = format!("{:.*}", p, n);
                if conversion == 'G' {
                    text.to_uppercase()
                } else {
                    text
                }
            }
            _ => {
                output.push('%');
                output.push(conversion);
                continue;
            }
        };
        if flags.contains('+')
            && !value.starts_with('-')
            && matches!(conversion, 'd' | 'i' | 'f' | 'e' | 'E' | 'g' | 'G')
        {
            value.insert(0, '+');
        }
        if width > value.len() {
            let padding = width - value.len();
            if flags.contains('-') {
                value.push_str(&" ".repeat(padding));
            } else if flags.contains('0') && !matches!(conversion, 's' | 'c') {
                let prefix = if value.starts_with('-') || value.starts_with('+') {
                    value.remove(0).to_string()
                } else {
                    String::new()
                };
                value = format!("{prefix}{}{value}", "0".repeat(padding));
            } else {
                value = format!("{}{value}", " ".repeat(padding));
            }
        }
        output.push_str(&value);
    }
    output
}

pub struct WasmReplay {
    store: Store<HostState>,
    instance: wasmi::Instance,
    handlers: Vec<ReplayHandler>,
    pub sysvars: SysVarStore,
}

impl WasmReplay {
    pub fn new(
        bytes: &[u8],
        message_addr: u32,
        handlers: Vec<ReplayHandler>,
    ) -> Result<Self, WasmReplayError> {
        Self::new_with_timers(bytes, message_addr, handlers, Vec::new())
    }

    pub fn new_with_timers(
        bytes: &[u8],
        message_addr: u32,
        handlers: Vec<ReplayHandler>,
        timers: Vec<ReplayTimer>,
    ) -> Result<Self, WasmReplayError> {
        let engine = Engine::default();
        let module = Module::new(&engine, &mut &bytes[..])
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        let mut store = Store::new(
            &engine,
            HostState {
                message_addr,
                timer_names: timers
                    .iter()
                    .map(|timer| (timer.id, timer.name.clone()))
                    .collect(),
                ..Default::default()
            },
        );
        let mut linker = Linker::new(&engine);
        linker
            .func_wrap(
                "capl",
                "write",
                |mut caller: wasmi::Caller<'_, HostState>, fmt: i32, count: i32, base: i32| {
                    let Some(Extern::Memory(memory)) = caller.get_export("memory") else {
                        return;
                    };
                    let format = read_c_string(&memory, &caller, fmt).unwrap_or_default();
                    let rendered = format_capl_write(
                        &format,
                        count.max(0) as usize,
                        base.max(0) as usize,
                        &memory,
                        &caller,
                    );
                    let state = caller.data_mut();
                    state.output.writes.push(rendered.clone());
                    state
                        .output
                        .events
                        .push(RuntimeEvent::new(state.now, "write", rendered));
                },
            )
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        linker
            .func_wrap(
                "capl",
                "setTimer",
                |mut caller: wasmi::Caller<'_, HostState>, id: i32, ms: i32| {
                    schedule_timer(caller.data_mut(), id, ms, false)
                },
            )
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        linker
            .func_wrap(
                "capl",
                "setTimerCyclic",
                |mut caller: wasmi::Caller<'_, HostState>, id: i32, ms: i32| {
                    schedule_timer(caller.data_mut(), id, ms, true)
                },
            )
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        linker
            .func_wrap(
                "capl",
                "cancelTimer",
                |mut caller: wasmi::Caller<'_, HostState>, id: i32| {
                    let id = id.max(0) as u32;
                    caller.data_mut().timers.retain(|timer| timer.id != id);
                    caller.data_mut().timer_periods.remove(&id);
                },
            )
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        linker
            .func_wrap(
                "capl",
                "output",
                |mut caller: wasmi::Caller<'_, HostState>, ptr: i32| {
                    let Some(Extern::Memory(memory)) = caller.get_export("memory") else {
                        return;
                    };
                    let mut raw = [0u8; 24];
                    if memory.read(&caller, ptr.max(0) as usize, &mut raw).is_err() {
                        return;
                    }
                    let id = u32::from_le_bytes(raw[0..4].try_into().unwrap());
                    let dlc =
                        (i32::from_le_bytes(raw[4..8].try_into().unwrap())).clamp(0, 8) as usize;
                    let data = raw[8..16].to_vec();
                    let channel = i32::from_le_bytes(raw[16..20].try_into().unwrap()).max(1) as u16;
                    let now = caller.data().now;
                    let output_frame = BusFrame::new(
                        now,
                        channel,
                        id,
                        capl_model::Direction::Tx,
                        data[..dlc].to_vec(),
                    );
                    let state = caller.data_mut();
                    state.output.frames.push(output_frame.clone());
                    state.output.events.push(RuntimeEvent::new(
                        now,
                        "tx",
                        format!(
                            "0x{:X} [{}]",
                            output_frame.id,
                            output_frame
                                .data
                                .iter()
                                .map(|byte| format!("{byte:02X}"))
                                .collect::<Vec<_>>()
                                .join(" ")
                        ),
                    ));
                },
            )
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        linker
            .func_wrap("capl", "timeNow", |caller: wasmi::Caller<'_, HostState>| {
                (caller.data().now / 1000) as i32
            })
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        linker
            .func_wrap("capl", "random", |_n: i32| 0i32)
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        linker
            .func_wrap("capl", "strlen", |_ptr: i32| 0i32)
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        linker
            .func_new(
                "capl",
                "pow",
                FuncType::new([ValueType::F64, ValueType::F64], [ValueType::F64]),
                |_caller, _params, results| {
                    results[0] = Value::F64(F64::from(0.0));
                    Ok(())
                },
            )
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        for name in ["sin", "cos"] {
            linker
                .func_new(
                    "capl",
                    name,
                    FuncType::new([ValueType::F64], [ValueType::F64]),
                    |_caller, _params, results| {
                        results[0] = Value::F64(F64::from(0.0));
                        Ok(())
                    },
                )
                .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        }
        for name in ["canOnline", "canOffline", "stopMeasurement"] {
            linker
                .func_wrap("capl", name, || {})
                .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        }
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?
            .start(&mut store)
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        Ok(Self {
            store,
            instance,
            handlers,
            sysvars: SysVarStore::default(),
        })
    }

    pub fn output(&self) -> &ReplayOutput {
        &self.store.data().output
    }
    pub fn output_mut(&mut self) -> &mut ReplayOutput {
        &mut self.store.data_mut().output
    }
    pub fn now_us(&self) -> u64 {
        self.store.data().now
    }
    pub fn pending_timers(&self) -> &[PendingTimer] {
        &self.store.data().timers
    }
    pub fn set_time(&mut self, time: VirtualTime) {
        self.store.data_mut().now = time.0;
    }

    pub fn advance_time(&mut self, delta_us: u64) -> Result<usize, WasmReplayError> {
        let target = self.store.data().now.saturating_add(delta_us);
        let mut fired = 0;
        loop {
            let index = self
                .store
                .data()
                .timers
                .iter()
                .enumerate()
                .filter(|(_, timer)| timer.at_us <= target)
                .min_by_key(|(_, timer)| (timer.at_us, timer.id))
                .map(|(index, _)| index);
            let Some(index) = index else {
                break;
            };
            let timer = self.store.data_mut().timers.remove(index);
            self.set_time(VirtualTime(timer.at_us));
            self.store.data_mut().output.events.push(RuntimeEvent::new(
                timer.at_us,
                "timer",
                format!("{} fired", timer.name),
            ));
            let export_name = format!("on_timer_{}", timer.name);
            if let Ok(handler) = self
                .instance
                .get_typed_func::<(), ()>(&self.store, &export_name)
            {
                handler
                    .call(&mut self.store, ())
                    .map_err(|e| WasmReplayError::Trap(e.to_string()))?;
                fired += 1;
            }
            if timer.cyclic
                && !self
                    .store
                    .data()
                    .timers
                    .iter()
                    .any(|pending| pending.id == timer.id)
            {
                let period = self
                    .store
                    .data()
                    .timer_periods
                    .get(&timer.id)
                    .copied()
                    .unwrap_or(0);
                if period > 0 {
                    schedule_timer_at(
                        self.store.data_mut(),
                        timer.id,
                        timer.name,
                        timer.at_us.saturating_add(period),
                        true,
                    );
                }
            }
        }
        self.set_time(VirtualTime(target));
        Ok(fired)
    }

    pub fn start_measurement(&mut self) -> Result<(), WasmReplayError> {
        let now = self.now_us();
        self.store
            .data_mut()
            .output
            .events
            .push(RuntimeEvent::new(now, "lifecycle", "start"));
        if let Ok(init) = self
            .instance
            .get_typed_func::<(), ()>(&self.store, "__init")
        {
            init.call(&mut self.store, ())
                .map_err(|e| WasmReplayError::Trap(e.to_string()))?;
        }
        for export in ["on_preStart", "on_start"] {
            if let Ok(handler) = self.instance.get_typed_func::<(), ()>(&self.store, export) {
                handler
                    .call(&mut self.store, ())
                    .map_err(|e| WasmReplayError::Trap(e.to_string()))?;
            }
        }
        Ok(())
    }

    pub fn stop_measurement(&mut self) -> Result<(), WasmReplayError> {
        let now = self.now_us();
        self.store
            .data_mut()
            .output
            .events
            .push(RuntimeEvent::new(now, "lifecycle", "stop"));
        for export in ["on_preStop", "on_stopMeasurement"] {
            if let Ok(handler) = self.instance.get_typed_func::<(), ()>(&self.store, export) {
                handler
                    .call(&mut self.store, ())
                    .map_err(|e| WasmReplayError::Trap(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Change a system variable and invoke matching `on sysvar` handlers.
    pub fn set_sysvar(&mut self, path: &str, value: SysVarValue) -> Result<usize, SysVarError> {
        let now = self.now_us();
        self.store.data_mut().output.events.push(RuntimeEvent::new(
            now,
            "sysvar",
            format!("{path} = {value:?}"),
        ));
        let change = self.sysvars.set_path(path, value)?;
        let this_value = change.new.as_i64().unwrap_or(0) as i32;
        if let Ok(set_this) = self
            .instance
            .get_typed_func::<i32, ()>(&self.store, "__set_this")
        {
            set_this
                .call(&mut self.store, this_value)
                .map_err(|e| SysVarError::Missing(SysVarKey::new("runtime", e.to_string())))?;
        }
        let mut invoked = 0;
        for handler in self.handlers.clone() {
            if handler.sysvar.as_deref() != Some(path) {
                continue;
            }
            let func = self
                .instance
                .get_typed_func::<(), ()>(&self.store, &handler.export_name)
                .map_err(|_| SysVarError::Missing(SysVarKey::parse(path)))?;
            func.call(&mut self.store, ())
                .map_err(|e| SysVarError::Missing(SysVarKey::new("runtime", e.to_string())))?;
            invoked += 1;
        }
        Ok(invoked)
    }

    pub fn set_envvar(&mut self, path: &str, value: SysVarValue) -> Result<usize, SysVarError> {
        let now = self.now_us();
        self.store.data_mut().output.events.push(RuntimeEvent::new(
            now,
            "envvar",
            format!("{path} = {value:?}"),
        ));
        let value_i32 = value.as_i64().unwrap_or(0) as i32;
        let set_this = self
            .instance
            .get_typed_func::<i32, ()>(&self.store, "__set_this")
            .ok();
        if let Some(set_this) = set_this {
            set_this.call(&mut self.store, value_i32).map_err(|error| {
                SysVarError::Missing(SysVarKey::new("runtime", error.to_string()))
            })?;
        }
        let mut invoked = 0;
        for handler in self.handlers.clone() {
            if handler.envvar.as_deref() == Some(path) {
                let func = self
                    .instance
                    .get_typed_func::<(), ()>(&self.store, &handler.export_name)
                    .map_err(|_| SysVarError::Missing(SysVarKey::parse(path)))?;
                func.call(&mut self.store, ()).map_err(|error| {
                    SysVarError::Missing(SysVarKey::new("runtime", error.to_string()))
                })?;
                invoked += 1;
            }
        }
        Ok(invoked)
    }

    pub fn inject_key(&mut self, key: char) -> Result<usize, WasmReplayError> {
        let now = self.now_us();
        self.store
            .data_mut()
            .output
            .events
            .push(RuntimeEvent::new(now, "key", format!("'{key}'")));
        let code = key as i32;
        if let Ok(set_this) = self
            .instance
            .get_typed_func::<i32, ()>(&self.store, "__set_this")
        {
            set_this
                .call(&mut self.store, code)
                .map_err(|error| WasmReplayError::Trap(error.to_string()))?;
        }
        let handlers = self.handlers.clone();
        let mut invoked = 0;
        for handler in handlers {
            if handler
                .key
                .as_deref()
                .is_some_and(|name| name == key.to_string())
                || (handler.key.is_none()
                    && handler.wildcard
                    && handler.special.as_deref() == Some("key"))
            {
                let func = self
                    .instance
                    .get_typed_func::<(), ()>(&self.store, &handler.export_name)
                    .map_err(|_| WasmReplayError::MissingExport(handler.export_name.clone()))?;
                func.call(&mut self.store, ())
                    .map_err(|error| WasmReplayError::Trap(error.to_string()))?;
                invoked += 1;
            }
        }
        Ok(invoked)
    }

    pub fn inject_bus_event(&mut self, event: &str) -> Result<usize, WasmReplayError> {
        let now = self.now_us();
        self.store
            .data_mut()
            .output
            .events
            .push(RuntimeEvent::new(now, "bus", event));
        let mut invoked = 0;
        for handler in self.handlers.clone() {
            if handler.special.as_deref() == Some(event) {
                let func = self
                    .instance
                    .get_typed_func::<(), ()>(&self.store, &handler.export_name)
                    .map_err(|_| WasmReplayError::MissingExport(handler.export_name.clone()))?;
                func.call(&mut self.store, ())
                    .map_err(|error| WasmReplayError::Trap(error.to_string()))?;
                invoked += 1;
            }
        }
        Ok(invoked)
    }
    pub fn run_frame(&mut self, frame: &BusFrame) -> Result<usize, WasmReplayError> {
        let mut queue = std::collections::VecDeque::from([frame.clone()]);
        let mut invoked = 0;
        let mut dispatched = 0;
        while let Some(frame) = queue.pop_front() {
            dispatched += 1;
            if dispatched > 20_000 {
                return Err(WasmReplayError::Trap(
                    "event budget exhausted during bus loopback".into(),
                ));
            }
            let previous_tx_count = self.output().frames.len();
            invoked += self.dispatch_frame(&frame)?;
            let tx_frames = self
                .output()
                .frames
                .get(previous_tx_count..)
                .unwrap_or_default()
                .to_vec();
            queue.extend(tx_frames);
        }
        Ok(invoked)
    }

    fn dispatch_frame(&mut self, frame: &BusFrame) -> Result<usize, WasmReplayError> {
        self.set_time(VirtualTime(frame.timestamp_us));
        self.store.data_mut().output.events.push(RuntimeEvent::new(
            frame.timestamp_us,
            "message",
            format!(
                "{} 0x{:X}",
                if matches!(frame.direction, capl_model::Direction::Tx) {
                    "Tx"
                } else {
                    "Rx"
                },
                frame.id
            ),
        ));
        let memory = self
            .instance
            .get_memory(&self.store, "memory")
            .ok_or_else(|| WasmReplayError::MissingExport("memory".into()))?;
        let addr = self.store.data().message_addr as usize;
        let mut raw = [0u8; 32];
        raw[0..4].copy_from_slice(&frame.id.to_le_bytes());
        raw[4..8].copy_from_slice(&(frame.data.len() as i32).to_le_bytes());
        raw[8..8 + frame.data.len().min(8)].copy_from_slice(&frame.data[..frame.data.len().min(8)]);
        raw[16..20].copy_from_slice(&(frame.channel as i32).to_le_bytes());
        raw[20..24].copy_from_slice(
            &(i32::from(matches!(frame.direction, capl_model::Direction::Tx))).to_le_bytes(),
        );
        raw[28..32].copy_from_slice(&((frame.timestamp_us / 1000) as i32).to_le_bytes());
        memory
            .write(&mut self.store, addr, &raw)
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        let message_addr = self.store.data().message_addr as i32;
        if let Ok(set_this) = self
            .instance
            .get_typed_func::<i32, ()>(&self.store, "__set_this")
        {
            set_this
                .call(&mut self.store, message_addr)
                .map_err(|e| WasmReplayError::Trap(e.to_string()))?;
        }
        let mut invoked = 0;
        for handler in self.handlers.clone() {
            if (handler.wildcard || handler.id.is_some_and(|id| id == frame.id))
                && handler.channel.is_none_or(|ch| ch == frame.channel)
            {
                let func = self
                    .instance
                    .get_typed_func::<(), ()>(&self.store, &handler.export_name)
                    .map_err(|_| WasmReplayError::MissingExport(handler.export_name.clone()))?;
                func.call(&mut self.store, ())
                    .map_err(|e| WasmReplayError::Trap(e.to_string()))?;
                invoked += 1;
            }
        }
        Ok(invoked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capl_model::{BusFrame, Direction};
    struct C(u32);
    impl MessageHandler for C {
        fn on_message(&mut self, _: &BusFrame, _: VirtualTime) {
            self.0 += 1;
        }
    }
    #[test]
    fn replays_in_virtual_time() {
        let e = vec![
            TraceEvent::Frame(BusFrame::new(20, 1, 2, Direction::Rx, vec![])),
            TraceEvent::Frame(BusFrame::new(10, 1, 1, Direction::Rx, vec![])),
        ];
        let mut r = ReplayEngine::new(e);
        let mut c = C(0);
        r.run_until(15, &mut c);
        assert_eq!(c.0, 1);
        assert_eq!(r.now(), VirtualTime(15));
    }
}
