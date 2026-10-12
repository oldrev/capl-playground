use capl_ast::{Decl, EventSpec};
use capl_driver::{compile, BackendId, Options};
use capl_runtime::{ReplayHandler, ReplayTimer, WasmReplay};
use capl_trace_asc::AscReader;
use capl_trace_core::TraceReader;
use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<_> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: capl-replay <node.can> <trace.asc|candump.log>");
        return ExitCode::from(2);
    }
    let source = match fs::read_to_string(&args[1]) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("read source: {e}");
            return ExitCode::from(1);
        }
    };
    let trace = match fs::read_to_string(&args[2]) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("read trace: {e}");
            return ExitCode::from(1);
        }
    };
    let out = compile(
        &source,
        Options {
            file_name: args[1].clone(),
            backends: vec![BackendId::Wasm],
            ..Options::default()
        },
    );
    if !out.ok {
        for diag in out.diagnostics {
            eprintln!("{}", diag.message);
        }
        return ExitCode::from(1);
    }
    let handlers = out
        .unit
        .as_ref()
        .into_iter()
        .flat_map(|unit| unit.decls.iter())
        .filter_map(|decl| match decl {
            Decl::Handler(handler) => match &handler.event {
                EventSpec::Message {
                    id,
                    channel,
                    wildcard,
                    ..
                } => Some(ReplayHandler {
                    export_name: handler.export_name.clone(),
                    id: *id,
                    channel: channel.map(u16::from),
                    wildcard: *wildcard,
                    sysvar: None,
                    timer: None,
                    key: None,
                    envvar: None,
                    special: None,
                }),
                EventSpec::SysVar { name } => Some(ReplayHandler {
                    export_name: handler.export_name.clone(),
                    id: None,
                    channel: None,
                    wildcard: false,
                    sysvar: Some(name.clone()),
                    timer: None,
                    key: None,
                    envvar: None,
                    special: None,
                }),
                EventSpec::Timer { name } => Some(ReplayHandler {
                    export_name: handler.export_name.clone(),
                    id: None,
                    channel: None,
                    wildcard: false,
                    sysvar: None,
                    timer: Some(name.clone()),
                    key: None,
                    envvar: None,
                    special: None,
                }),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let events = match AscReader.read(&trace) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("trace: {e}");
            return ExitCode::from(1);
        }
    };
    let artifact = out.wasm.expect("wasm backend requested");
    let timers = out
        .sema
        .as_ref()
        .map(|sema| {
            sema.timers
                .iter()
                .filter_map(|id| sema.var(*id))
                .filter_map(|var| {
                    var.timer_id.map(|id| ReplayTimer {
                        id,
                        name: var.name.clone(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let mut runtime = match WasmReplay::new_with_timers(
        &artifact.bytes,
        artifact.msgbuf_addr,
        handlers,
        timers,
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("runtime: {e}");
            return ExitCode::from(1);
        }
    };
    let mut frames = 0;
    let mut invoked = 0;
    let mut timer_handlers = 0;
    if let Err(error) = runtime.start_measurement() {
        eprintln!("start measurement: {error}");
        return ExitCode::from(1);
    }
    for event in events {
        if let capl_trace_core::TraceEvent::Frame(frame) = event {
            frames += 1;
            if frame.timestamp_us > runtime.now_us() {
                match runtime.advance_time(frame.timestamp_us - runtime.now_us()) {
                    Ok(count) => timer_handlers += count,
                    Err(error) => {
                        eprintln!("timer before {}us: {error}", frame.timestamp_us);
                        return ExitCode::from(1);
                    }
                }
            }
            match runtime.run_frame(&frame) {
                Ok(n) => invoked += n,
                Err(e) => {
                    eprintln!("replay at {}us: {e}", frame.timestamp_us);
                    return ExitCode::from(1);
                }
            }
        }
    }
    println!(
        "replayed {frames} frame(s), invoked {invoked} message handler(s) and {timer_handlers} timer handler(s), emitted {} frame(s)",
        runtime.output().frames.len()
    );
    ExitCode::SUCCESS
}
