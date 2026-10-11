use capl_driver::{compile, BackendId, Options};
use capl_model::{BusFrame, Direction};
use capl_runtime::{ReplayHandler, WasmReplay};
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
}

#[test]
fn starting_measurement_invokes_on_start() {
    let out = compile("on start { write(\"started\"); }", Options { backends: vec![BackendId::Wasm], ..Options::default() });
    assert!(out.ok, "compile diagnostics: {:?}", out.diagnostics);
    let artifact = out.wasm.expect("wasm artifact");
    let mut runtime = WasmReplay::new(&artifact.bytes, artifact.msgbuf_addr, Vec::new()).unwrap();
    runtime.start_measurement().unwrap();
    assert!(runtime.output().writes.iter().any(|write| write.contains("started")));
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
