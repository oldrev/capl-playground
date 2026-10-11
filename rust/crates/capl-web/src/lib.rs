#![cfg(target_family = "wasm")]

use capl_ast::{Decl, EventSpec};
use capl_driver::{compile, BackendId, Options, WasmArtifact};
use capl_model::{BusFrame, Direction};
use capl_runtime::{ReplayHandler, WasmReplay};
use gpui::{
    div, App, ClickEvent, Context, Entity, FocusHandle, Focusable, IntoElement, Render, Window,
};
use gpui_base::StyledExt;
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Editor, EditorState},
    Root,
};
use gpui_kit::*;
use wasm_bindgen::prelude::*;

struct WebWorkspace {
    editor: Entity<EditorState>,
    focus: FocusHandle,
    artifact: Option<WasmArtifact>,
    handlers: Vec<ReplayHandler>,
    runtime: Option<WasmReplay>,
    status: String,
}

impl WebWorkspace {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| EditorState::new(window, cx).default_value("on start {\n  write(\"ready\");\n}\n\non message 0x100 {\n  write(\"received 0x100\");\n}\n").line_number(true).indent_guides(true));
        let focus = editor.read(cx).focus_handle(cx);
        Self {
            editor,
            focus,
            artifact: None,
            handlers: Vec::new(),
            runtime: None,
            status: "Ready".into(),
        }
    }

    fn build_now(&mut self, cx: &mut Context<Self>) {
        let out = compile(
            &self.editor.read(cx).value().to_string(),
            Options {
                backends: vec![BackendId::Wasm, BackendId::C, BackendId::Ast],
                ..Options::default()
            },
        );
        if !out.ok {
            self.status = format!("Build failed: {} diagnostic(s)", out.diagnostics.len());
            self.artifact = None;
            self.handlers.clear();
        } else {
            self.handlers = out
                .unit
                .as_ref()
                .into_iter()
                .flat_map(|unit| unit.decls.iter())
                .filter_map(web_handler)
                .collect();
            self.artifact = out.wasm;
            self.runtime = None;
            self.status = format!("Build succeeded: {} handler(s)", self.handlers.len());
        }
    }

    fn build(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.build_now(cx);
        cx.notify();
    }

    fn run(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.artifact.is_none() {
            self.build_now(cx);
        }
        if let Some(artifact) = &self.artifact {
            match WasmReplay::new(&artifact.bytes, artifact.msgbuf_addr, self.handlers.clone()) {
                Ok(runtime) => {
                    self.runtime = Some(runtime);
                    self.status = "Running".into();
                }
                Err(error) => self.status = format!("Runtime error: {error}"),
            }
        }
        cx.notify();
    }

    fn stop(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.runtime = None;
        self.status = "Stopped".into();
        cx.notify();
    }

    fn replay(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(runtime) = self.runtime.as_mut() {
            let frame = BusFrame::new(100_000, 1, 0x100, Direction::Rx, vec![0x12, 0x34]);
            match runtime.run_frame(&frame) {
                Ok(count) => self.status = format!("Replayed 0x100; invoked {count} handler(s)"),
                Err(error) => self.status = format!("Replay error: {error}"),
            }
        } else {
            self.status = "Run the measurement before replaying".into();
        }
        cx.notify();
    }
}

impl Focusable for WebWorkspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for WebWorkspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.status.clone();
        div()
            .size_full()
            .v_flex()
            .bg(rgb(0x202124))
            .text_color(rgb(0xffffff))
            .child(
                h_flex()
                    .h_10()
                    .px_2()
                    .gap_2()
                    .border_b_1()
                    .child("CAPL Playground")
                    .child(
                        Button::new("build")
                            .primary()
                            .label("Build")
                            .on_click(cx.listener(Self::build)),
                    )
                    .child(
                        Button::new("run")
                            .success()
                            .label("Run")
                            .on_click(cx.listener(Self::run)),
                    )
                    .child(
                        Button::new("stop")
                            .danger()
                            .label("Stop")
                            .on_click(cx.listener(Self::stop)),
                    )
                    .child(
                        Button::new("replay")
                            .outline()
                            .label("Replay sample")
                            .on_click(cx.listener(Self::replay)),
                    )
                    .child(div().flex_1())
                    .child(status),
            )
            .child(
                div()
                    .flex_1()
                    .p_2()
                    .child(Editor::new(&self.editor).size_full().into_any_element()),
            )
    }
}

fn web_handler(decl: &Decl) -> Option<ReplayHandler> {
    let Decl::Handler(handler) = decl else {
        return None;
    };
    match &handler.event {
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
        }),
        EventSpec::SysVar { name } => Some(ReplayHandler {
            export_name: handler.export_name.clone(),
            id: None,
            channel: None,
            wildcard: false,
            sysvar: Some(name.clone()),
        }),
        _ => None,
    }
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    gpui_platform::web_init();
    gpui_platform::single_threaded_web().run(|cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| {
                let workspace = cx.new(|cx| WebWorkspace::new(window, cx));
                Root::new(workspace, window, cx)
            })
        })
        .expect("failed to open GPUI web window");
    });
    Ok(())
}
