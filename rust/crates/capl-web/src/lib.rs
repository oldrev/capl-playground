#![cfg(target_family = "wasm")]

use capl_ast::{Decl, EventSpec};
use capl_driver::{compile, BackendId, Options, WasmArtifact};
use capl_model::{parse_hex_bytes, parse_hex_u32, BusFrame, Direction};
use capl_runtime::{ReplayHandler, ReplayTimer, WasmReplay};
use capl_sysvars::{SysVarKey, SysVarValue};
use gpui::{
    div, App, ClickEvent, Context, Entity, FocusHandle, Focusable, IntoElement, Render, Window,
};
use gpui_base::StyledExt;
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    h_flex,
    input::{Editor, EditorState, Input, InputState},
    Root,
};
use gpui_kit::*;
use wasm_bindgen::prelude::*;

struct WebWorkspace {
    editor: Entity<EditorState>,
    focus: FocusHandle,
    artifact: Option<WasmArtifact>,
    handlers: Vec<ReplayHandler>,
    timers: Vec<ReplayTimer>,
    runtime: Option<WasmReplay>,
    frame_id: Entity<InputState>,
    frame_data: Entity<InputState>,
    key_input: Entity<InputState>,
    sysvar_name: Entity<InputState>,
    sysvar_value: Entity<InputState>,
    status: String,
}

impl WebWorkspace {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| EditorState::new(window, cx).default_value("on start {\n  write(\"ready\");\n}\n\non message 0x100 {\n  write(\"received 0x100\");\n}\n").line_number(true).indent_guides(true));
        let focus = editor.read(cx).focus_handle(cx);
        let frame_id = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("100")
                .placeholder("CAN ID hex")
        });
        let frame_data = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("12 34")
                .placeholder("Data bytes")
        });
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("a")
                .placeholder("Key")
        });
        let sysvar_name = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("Foo::Enable")
                .placeholder("Sysvar/envvar")
        });
        let sysvar_value = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("1")
                .placeholder("Value")
        });
        Self {
            editor,
            focus,
            artifact: None,
            handlers: Vec::new(),
            timers: Vec::new(),
            runtime: None,
            frame_id,
            frame_data,
            key_input,
            sysvar_name,
            sysvar_value,
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
            self.timers = out
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
            match WasmReplay::new_with_timers(
                &artifact.bytes,
                artifact.msgbuf_addr,
                self.handlers.clone(),
                self.timers.clone(),
            ) {
                Ok(runtime) => {
                    self.runtime = Some(runtime);
                    if let Some(runtime) = self.runtime.as_mut() {
                        match runtime.start_measurement() {
                            Ok(()) => {
                                self.status = runtime
                                    .output()
                                    .writes
                                    .last()
                                    .map(|line| format!("Running — {line}"))
                                    .unwrap_or_else(|| "Running".into())
                            }
                            Err(error) => self.status = format!("Start error: {error}"),
                        }
                    }
                }
                Err(error) => self.status = format!("Runtime error: {error}"),
            }
        }
        cx.notify();
    }

    fn stop(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(runtime) = self.runtime.as_mut() {
            let _ = runtime.stop_measurement();
        }
        self.runtime = None;
        self.status = "Stopped".into();
        cx.notify();
    }

    fn replay(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(runtime) = self.runtime.as_mut() {
            let id_text = self.frame_id.read(cx).value().to_string();
            let id = parse_hex_u32(&id_text);
            let data = parse_hex_bytes(&self.frame_data.read(cx).value());
            match (id, data) {
                (Ok(id), Ok(data)) if data.len() <= 8 => {
                    let _ = runtime.advance_time(100_000);
                    let frame = BusFrame::new(runtime.now_us(), 1, id, Direction::Rx, data);
                    match runtime.run_frame(&frame) {
                        Ok(count) => {
                            self.status = format!(
                                "{} ms: replayed 0x{id:X}; {count} handler(s); {}",
                                runtime.now_us() / 1000,
                                runtime.output().writes.last().cloned().unwrap_or_default()
                            )
                        }
                        Err(error) => self.status = format!("Replay error: {error}"),
                    }
                }
                _ => {
                    self.status =
                        "Invalid CAN ID or payload (use hexadecimal bytes, up to 8)".into()
                }
            }
        } else {
            self.status = "Run the measurement before replaying".into();
        }
        cx.notify();
    }

    fn advance(&mut self, delta_us: u64, cx: &mut Context<Self>) {
        if let Some(runtime) = self.runtime.as_mut() {
            match runtime.advance_time(delta_us) {
                Ok(fired) => {
                    self.status = format!(
                        "Virtual time {} ms; {} timer(s) fired",
                        runtime.now_us() / 1000,
                        fired
                    )
                }
                Err(error) => self.status = format!("Timer error: {error}"),
            }
        } else {
            self.status = "Run the measurement before advancing virtual time".into();
        }
        cx.notify();
    }

    fn inject_key(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).value().chars().next();
        match (self.runtime.as_mut(), key) {
            (Some(runtime), Some(key)) => match runtime.inject_key(key) {
                Ok(count) => self.status = format!("Key '{key}' invoked {count} handler(s)"),
                Err(error) => self.status = format!("Key error: {error}"),
            },
            _ => self.status = "Run first and enter a key character".into(),
        }
        cx.notify();
    }

    fn trigger_sysvar(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let name = self.sysvar_name.read(cx).value().to_string();
        let value = self
            .sysvar_value
            .read(cx)
            .value()
            .parse::<i64>()
            .unwrap_or(0);
        if let Some(runtime) = self.runtime.as_mut() {
            let key = SysVarKey::parse(&name);
            if runtime.sysvars.get(&key).is_none() {
                runtime
                    .sysvars
                    .declare(key, SysVarValue::Integer(0), true, None);
            }
            match runtime.set_sysvar(&name, SysVarValue::Integer(value)) {
                Ok(count) => self.status = format!("{name}={value}; invoked {count}"),
                Err(error) => self.status = format!("Sysvar error: {error}"),
            }
        } else {
            self.status = "Run before changing a sysvar".into();
        }
        cx.notify();
    }

    fn trigger_envvar(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let name = self.sysvar_name.read(cx).value().to_string();
        let value = self
            .sysvar_value
            .read(cx)
            .value()
            .parse::<i64>()
            .unwrap_or(0);
        if let Some(runtime) = self.runtime.as_mut() {
            match runtime.set_envvar(&name, SysVarValue::Integer(value)) {
                Ok(count) => self.status = format!("Envvar {name}={value}; invoked {count}"),
                Err(error) => self.status = format!("Envvar error: {error}"),
            }
        } else {
            self.status = "Run before changing an envvar".into();
        }
        cx.notify();
    }

    fn bus_event(
        &mut self,
        event: &'static str,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(runtime) = self.runtime.as_mut() {
            match runtime.inject_bus_event(event) {
                Ok(count) => self.status = format!("{event}; invoked {count}"),
                Err(error) => self.status = format!("Bus event error: {error}"),
            }
        } else {
            self.status = "Run before injecting a bus event".into();
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
                    .child(
                        Button::new("advance-100")
                            .outline()
                            .label("+100 ms")
                            .on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.advance(100_000, cx)
                                }),
                            ),
                    )
                    .child(
                        Button::new("advance-1000")
                            .outline()
                            .label("+1 s")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.advance(1_000_000, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(status),
            )
            .child(
                div()
                    .flex_1()
                    .p_2()
                    .v_flex()
                    .child(
                        h_flex()
                            .h_9()
                            .px_2()
                            .gap_2()
                            .child("CAN")
                            .child(Input::new(&self.frame_id).w(px(80.)))
                            .child(Input::new(&self.frame_data).w(px(140.)))
                            .child(
                                Button::new("inject-frame")
                                    .outline()
                                    .label("Inject")
                                    .on_click(cx.listener(Self::replay)),
                            )
                            .child("Key")
                            .child(Input::new(&self.key_input).w(px(44.)))
                            .child(
                                Button::new("inject-key")
                                    .outline()
                                    .label("Press")
                                    .on_click(cx.listener(Self::inject_key)),
                            )
                            .child("Sysvar/Env")
                            .child(Input::new(&self.sysvar_name).w(px(120.)))
                            .child(Input::new(&self.sysvar_value).w(px(48.)))
                            .child(
                                Button::new("set-sysvar")
                                    .outline()
                                    .label("Set")
                                    .on_click(cx.listener(Self::trigger_sysvar)),
                            )
                            .child(
                                Button::new("set-envvar")
                                    .outline()
                                    .label("Env")
                                    .on_click(cx.listener(Self::trigger_envvar)),
                            )
                            .child(
                                Button::new("error-frame")
                                    .outline()
                                    .label("errorFrame")
                                    .on_click(cx.listener(|this, event, window, cx| {
                                        this.bus_event("errorFrame", event, window, cx)
                                    })),
                            )
                            .child(Button::new("bus-off").outline().label("busOff").on_click(
                                cx.listener(|this, event, window, cx| {
                                    this.bus_event("busOff", event, window, cx)
                                }),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .p_2()
                            .child(Editor::new(&self.editor).size_full().into_any_element()),
                    ),
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
        EventSpec::Key { key, wildcard } => Some(ReplayHandler {
            export_name: handler.export_name.clone(),
            id: None,
            channel: None,
            wildcard: *wildcard,
            sysvar: None,
            timer: None,
            key: key.map(|value| value.to_string()),
            envvar: None,
            special: Some("key".into()),
        }),
        EventSpec::EnvVar { name } => Some(ReplayHandler {
            export_name: handler.export_name.clone(),
            id: None,
            channel: None,
            wildcard: false,
            sysvar: None,
            timer: None,
            key: None,
            envvar: Some(name.clone()),
            special: None,
        }),
        EventSpec::BusOff => Some(ReplayHandler {
            export_name: handler.export_name.clone(),
            id: None,
            channel: None,
            wildcard: false,
            sysvar: None,
            timer: None,
            key: None,
            envvar: None,
            special: Some("busOff".into()),
        }),
        EventSpec::ErrorFrame => Some(ReplayHandler {
            export_name: handler.export_name.clone(),
            id: None,
            channel: None,
            wildcard: false,
            sysvar: None,
            timer: None,
            key: None,
            envvar: None,
            special: Some("errorFrame".into()),
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
