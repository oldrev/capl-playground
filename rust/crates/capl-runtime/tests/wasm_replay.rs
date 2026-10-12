use capl_driver::{compile, BackendId, Options};
use capl_model::{BusFrame, Direction};
use capl_runtime::{ReplayHandler, ReplayTimer, WasmReplay};
use capl_sysvars::{SysVarKey, SysVarValue};

#[test]
fn recorded_frame_invokes_matching_capl_handler() {
    let source = "variables { long hits; } on message 0x100 { hits++; }";
    let out = compile(
        source,
        Options {
            backends: vec![BackendId::Wasm],
            ..Options::default()
        },
    );
    assert!(out.ok, "compile diagnostics: {:?}", out.diagnostics);
    let artifact = out.wasm.expect("wasm artifact");
    let mut runtime = WasmReplay::new(
        &artifact.bytes,
        artifact.msgbuf_addr,
        vec![ReplayHandler {
            export_name: "on_message_0x100".into(),
            id: Some(0x100),
            channel: None,
            wildcard: false,
            sysvar: None,
            timer: None,
            key: None,
            envvar: None,
            special: None,
        }],
    )
    .unwrap();
    let invoked = runtime
        .run_frame(&BusFrame::new(4_200, 1, 0x100, Direction::Rx, vec![1, 2]))
        .unwrap();
    assert_eq!(invoked, 1);
}

#[test]
fn replay_captures_capl_write_output() {
    let source = "on message 0x100 { write(\"received\"); }";
    let out = compile(
        source,
        Options {
            backends: vec![BackendId::Wasm],
            ..Options::default()
        },
    );
    assert!(out.ok, "compile diagnostics: {:?}", out.diagnostics);
    let artifact = out.wasm.expect("wasm artifact");
    let mut runtime = WasmReplay::new(
        &artifact.bytes,
        artifact.msgbuf_addr,
        vec![ReplayHandler {
            export_name: "on_message_0x100".into(),
            id: Some(0x100),
            channel: None,
            wildcard: false,
            sysvar: None,
            timer: None,
            key: None,
            envvar: None,
            special: None,
        }],
    )
    .unwrap();
    runtime
        .run_frame(&BusFrame::new(1_000, 1, 0x100, Direction::Rx, vec![]))
        .unwrap();
    assert!(runtime
        .output()
        .writes
        .iter()
        .any(|write| write.contains("received")));
    assert!(runtime
        .output()
        .events
        .iter()
        .any(|event| event.kind == "message" && event.text.contains("0x100")));
    assert!(runtime
        .output()
        .events
        .iter()
        .any(|event| event.kind == "write" && event.text.contains("received")));
}

#[test]
fn starting_measurement_invokes_on_start() {
    let out = compile(
        "on start { write(\"started\"); }",
        Options {
            backends: vec![BackendId::Wasm],
            ..Options::default()
        },
    );
    assert!(out.ok, "compile diagnostics: {:?}", out.diagnostics);
    let artifact = out.wasm.expect("wasm artifact");
    let mut runtime = WasmReplay::new(&artifact.bytes, artifact.msgbuf_addr, Vec::new()).unwrap();
    runtime.start_measurement().unwrap();
    assert!(runtime
        .output()
        .writes
        .iter()
        .any(|write| write.contains("started")));
}

#[test]
fn virtual_time_fires_and_reschedules_capl_timers() {
    let source = "variables { msTimer tick; long count = 0; } on start { setTimer(tick, 100); } on timer tick { count++; write(\"tick %d\", count); if (count < 2) setTimer(tick, 100); }";
    let out = compile(
        source,
        Options {
            backends: vec![BackendId::Wasm],
            ..Options::default()
        },
    );
    assert!(out.ok, "compile diagnostics: {:?}", out.diagnostics);
    let artifact = out.wasm.expect("wasm artifact");
    let handlers = vec![ReplayHandler {
        export_name: "on_timer_tick".into(),
        id: None,
        channel: None,
        wildcard: false,
        sysvar: None,
        timer: Some("tick".into()),
        key: None,
        envvar: None,
        special: None,
    }];
    let timers = vec![ReplayTimer {
        id: 0,
        name: "tick".into(),
    }];
    let mut runtime =
        WasmReplay::new_with_timers(&artifact.bytes, artifact.msgbuf_addr, handlers, timers)
            .unwrap();
    runtime.start_measurement().unwrap();
    assert_eq!(runtime.pending_timers()[0].at_us, 100_000);
    assert_eq!(runtime.advance_time(99_999).unwrap(), 0);
    assert_eq!(runtime.advance_time(1).unwrap(), 1);
    assert!(runtime.output().writes.iter().any(|line| line == "tick 1"));
    assert_eq!(runtime.pending_timers()[0].at_us, 200_000);
    assert_eq!(runtime.advance_time(100_000).unwrap(), 1);
    assert!(runtime.output().writes.iter().any(|line| line == "tick 2"));
    assert!(runtime.pending_timers().is_empty());
}

#[test]
fn output_frames_loop_back_into_message_handlers() {
    let source = "variables { message 0x101 forwarded; } on message 0x100 { forwarded.dlc = 1; forwarded.byte(0) = this.byte(0); output(forwarded); } on message 0x101 { write(\"loopback %d\", this.byte(0)); }";
    let out = compile(
        source,
        Options {
            backends: vec![BackendId::Wasm],
            ..Options::default()
        },
    );
    assert!(out.ok, "compile diagnostics: {:?}", out.diagnostics);
    let artifact = out.wasm.expect("wasm artifact");
    let handlers = vec![
        ReplayHandler {
            export_name: "on_message_0x100".into(),
            id: Some(0x100),
            channel: None,
            wildcard: false,
            sysvar: None,
            timer: None,
            key: None,
            envvar: None,
            special: None,
        },
        ReplayHandler {
            export_name: "on_message_0x101".into(),
            id: Some(0x101),
            channel: None,
            wildcard: false,
            sysvar: None,
            timer: None,
            key: None,
            envvar: None,
            special: None,
        },
    ];
    let mut runtime = WasmReplay::new(&artifact.bytes, artifact.msgbuf_addr, handlers).unwrap();
    assert_eq!(
        runtime
            .run_frame(&BusFrame::new(0, 1, 0x100, Direction::Rx, vec![0x2a]))
            .unwrap(),
        2
    );
    assert_eq!(runtime.output().frames[0].id, 0x101);
    assert!(runtime
        .output()
        .writes
        .iter()
        .any(|line| line == "loopback 42"));
}

#[test]
fn key_environment_and_bus_events_invoke_their_handlers() {
    let source = "on key 'a' { write(\"key %c\", this); } on envVar Foo { write(\"env %d\", this); } on busOff { write(\"bus off\"); }";
    let out = compile(
        source,
        Options {
            backends: vec![BackendId::Wasm],
            ..Options::default()
        },
    );
    assert!(out.ok, "compile diagnostics: {:?}", out.diagnostics);
    let artifact = out.wasm.expect("wasm artifact");
    let handlers = vec![
        ReplayHandler {
            export_name: "on_key_97".into(),
            id: None,
            channel: None,
            wildcard: false,
            sysvar: None,
            timer: None,
            key: Some("a".into()),
            envvar: None,
            special: Some("key".into()),
        },
        ReplayHandler {
            export_name: "on_envvar_Foo".into(),
            id: None,
            channel: None,
            wildcard: false,
            sysvar: None,
            timer: None,
            key: None,
            envvar: Some("Foo".into()),
            special: None,
        },
        ReplayHandler {
            export_name: "on_busOff".into(),
            id: None,
            channel: None,
            wildcard: false,
            sysvar: None,
            timer: None,
            key: None,
            envvar: None,
            special: Some("busOff".into()),
        },
    ];
    let mut runtime = WasmReplay::new(&artifact.bytes, artifact.msgbuf_addr, handlers).unwrap();
    assert_eq!(runtime.inject_key('a').unwrap(), 1);
    assert_eq!(
        runtime.set_envvar("Foo", SysVarValue::Integer(7)).unwrap(),
        1
    );
    assert_eq!(runtime.inject_bus_event("busOff").unwrap(), 1);
    assert!(runtime.output().writes.iter().any(|line| line == "key a"));
    assert!(runtime.output().writes.iter().any(|line| line == "env 7"));
    assert!(runtime.output().writes.iter().any(|line| line == "bus off"));
}

#[test]
fn system_variable_changes_invoke_capl_handlers() {
    let out = compile(
        "on sysvar Foo::Enable { write(\"changed\"); }",
        Options {
            backends: vec![BackendId::Wasm],
            ..Options::default()
        },
    );
    assert!(out.ok, "compile diagnostics: {:?}", out.diagnostics);
    let artifact = out.wasm.expect("wasm artifact");
    let mut runtime = WasmReplay::new(
        &artifact.bytes,
        artifact.msgbuf_addr,
        vec![ReplayHandler {
            export_name: "on_sysvar_Foo_Enable".into(),
            id: None,
            channel: None,
            wildcard: false,
            sysvar: Some("Foo::Enable".into()),
            timer: None,
            key: None,
            envvar: None,
            special: None,
        }],
    )
    .unwrap();
    runtime.sysvars.declare(
        SysVarKey::parse("Foo::Enable"),
        SysVarValue::Boolean(false),
        true,
        None,
    );
    assert_eq!(
        runtime
            .set_sysvar("Foo::Enable", SysVarValue::Boolean(true))
            .unwrap(),
        1
    );
}
