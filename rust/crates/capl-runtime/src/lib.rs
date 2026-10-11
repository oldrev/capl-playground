use capl_model::BusFrame;
use capl_sysvars::{SysVarError, SysVarKey, SysVarStore, SysVarValue};
use capl_trace_core::TraceEvent;
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
}

#[derive(Clone, Debug, Default)]
pub struct ReplayOutput {
    pub frames: Vec<BusFrame>,
    pub writes: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Default)]
struct HostState {
    now: u64,
    message_addr: u32,
    output: ReplayOutput,
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
        let engine = Engine::default();
        let module = Module::new(&engine, &mut &bytes[..])
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        let mut store = Store::new(
            &engine,
            HostState {
                message_addr,
                ..Default::default()
            },
        );
        let mut linker = Linker::new(&engine);
        linker
            .func_wrap(
                "capl",
                "write",
                |mut caller: wasmi::Caller<'_, HostState>, fmt: i32, _count: i32, _base: i32| {
                    let Some(Extern::Memory(memory)) = caller.get_export("memory") else {
                        return;
                    };
                    let mut bytes = vec![0u8; 512];
                    if memory
                        .read(&caller, fmt.max(0) as usize, &mut bytes)
                        .is_ok()
                    {
                        let end = bytes
                            .iter()
                            .position(|byte| *byte == 0)
                            .unwrap_or(bytes.len());
                        caller
                            .data_mut()
                            .output
                            .writes
                            .push(String::from_utf8_lossy(&bytes[..end]).into_owned());
                    }
                },
            )
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        for name in ["setTimer", "setTimerCyclic"] {
            linker
                .func_wrap("capl", name, |_timer: i32, _ms: i32| {})
                .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        }
        linker
            .func_wrap("capl", "cancelTimer", |_timer: i32| {})
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
                    caller.data_mut().output.frames.push(BusFrame::new(
                        now,
                        channel,
                        id,
                        capl_model::Direction::Tx,
                        data[..dlc].to_vec(),
                    ));
                },
            )
            .map_err(|e| WasmReplayError::Wasm(e.to_string()))?;
        linker
            .func_wrap("capl", "timeNow", |caller: wasmi::Caller<'_, HostState>| {
                caller.data().now as i32
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
    pub fn set_time(&mut self, time: VirtualTime) {
        self.store.data_mut().now = time.0;
    }

    /// Change a system variable and invoke matching `on sysvar` handlers.
    pub fn set_sysvar(&mut self, path: &str, value: SysVarValue) -> Result<usize, SysVarError> {
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
    pub fn run_frame(&mut self, frame: &BusFrame) -> Result<usize, WasmReplayError> {
        self.set_time(VirtualTime(frame.timestamp_us));
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
