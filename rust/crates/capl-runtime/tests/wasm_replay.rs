use capl_driver::{compile, BackendId, Options};
use capl_model::{BusFrame, Direction};
use capl_runtime::{ReplayHandler, WasmReplay};

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
        }],
    )
    .unwrap();
    let invoked = runtime
        .run_frame(&BusFrame::new(4_200, 1, 0x100, Direction::Rx, vec![1, 2]))
        .unwrap();
    assert_eq!(invoked, 1);
}
