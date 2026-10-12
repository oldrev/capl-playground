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
    active_tab: String,
    bottom_tab: String,
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
            active_tab: "engine.can".into(),
            bottom_tab: "Simulation".into(),
        }
    }

    fn select_tab(&mut self, tab: &str, cx: &mut Context<Self>) {
        self.active_tab = tab.into();
        cx.notify();
    }

    fn select_bottom_tab(&mut self, tab: &str, cx: &mut Context<Self>) {
        self.bottom_tab = tab.into();
        cx.notify();
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
        let active_tab = self.active_tab.clone();
        let bottom_tab = self.bottom_tab.clone();
        let border = rgb(0x302a27);
        let chrome = rgb(0x171311);
        let sidebar = rgb(0x211b18);
        let panel = rgb(0x1b1715);
        let editor_bg = rgb(0x181513);
        let muted = rgb(0xa89788);
        let accent = rgb(0xe27a2b);
        div()
            .size_full()
            .v_flex()
            .bg(editor_bg)
            .text_color(rgb(0xf1e7dd))
            .child(
                h_flex()
                    .h(px(44.))
                    .px_3()
                    .gap_3()
                    .items_center()
                    .bg(chrome)
                    .border_b_1()
                    .border_color(border)
                    .child(div().w(px(28.)).h(px(28.)).rounded(px(7.)).bg(accent).items_center().justify_center().font_weight(gpui::FontWeight::BOLD).text_color(rgb(0x24170f)).child("C"))
                    .child(div().v_flex().gap_0p5().child(div().font_weight(gpui::FontWeight::BOLD).child("CAPL Playground")).child(div().text_size(px(10.)).text_color(muted).child("Vector CAPL  ·  Rust compiler  ·  virtual measurement")))
                    .child(div().flex_1())
                    .child(Button::new("build").small().label("Build").on_click(cx.listener(Self::build)))
                    .child(Button::new("run").small().success().label("Run").on_click(cx.listener(Self::run)))
                    .child(Button::new("stop").small().danger().label("Stop").on_click(cx.listener(Self::stop)))
                    .child(Button::new("reset").small().ghost().label("Reset").on_click(cx.listener(Self::stop)))
                    .child(div().text_size(px(11.)).text_color(muted).child(status.clone())),
            )
            .child(
                h_flex()
                    .h(px(32.))
                    .px_3()
                    .gap_1()
                    .items_center()
                    .bg(sidebar)
                    .border_b_1()
                    .border_color(border)
                    .child(Button::new("file").small().ghost().label("File"))
                    .child(Button::new("edit").small().ghost().label("Edit"))
                    .child(Button::new("selection").small().ghost().label("Selection"))
                    .child(Button::new("view").small().ghost().label("View"))
                    .child(Button::new("run-menu").small().ghost().label("Run"))
                    .child(Button::new("help").small().ghost().label("Help"))
                    .child(div().flex_1())
                    .child(div().text_size(px(10.)).text_color(muted).child("CAPL / playground")),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(
                        div().w(px(48.)).flex_shrink_0().h_full().v_flex().items_center().gap_2().py_2().bg(chrome).border_r_1().border_color(border)
                            .child(div().w(px(30.)).h(px(30.)).rounded(px(6.)).bg(accent).items_center().justify_center().font_weight(gpui::FontWeight::BOLD).child("C"))
                            .child(Button::new("activity-explorer").small().ghost().label("▤"))
                            .child(Button::new("activity-search").small().ghost().label("⌕"))
                            .child(Button::new("activity-trace").small().ghost().label("⌁"))
                            .child(Button::new("activity-diagnostics").small().ghost().label("!"))
                            .child(div().flex_1())
                            .child(Button::new("activity-settings").small().ghost().label("⚙")),
                    )
                    .child(
                        div().w(px(214.)).flex_shrink_0().h_full().v_flex().bg(sidebar).border_r_1().border_color(border).p_3().gap_3()
                            .child(h_flex().items_center().justify_between().child(div().text_size(px(11.)).font_weight(gpui::FontWeight::BOLD).text_color(muted).child("EXPLORER")).child(div().text_color(muted).child("···")))
                            .child(div().text_size(px(10.)).font_weight(gpui::FontWeight::BOLD).text_color(muted).child("PROJECT"))
                            .child(div().v_flex().gap_1().child(div().text_color(rgb(0xd8c9ba)).child("⌄  my_project")).child(div().pl_3().text_color(muted).child("⌄  nodes")).child(div().pl_5().text_color(rgb(0xd8c9ba)).child("▣  ECU_Power")).child(div().pl_3().text_color(muted).child("⌄  databases")).child(div().pl_5().text_color(rgb(0xd8c9ba)).child("▧  J1939.dbc")))
                            .child(div().flex_1())
                            .child(div().p_2().rounded(px(5.)).bg(rgb(0x2a211d)).text_color(muted).text_size(px(11.)).child("Open a .caplproj workspace or create a new one.")),
                    )
                    .child(
                        div().flex_1().min_w_0().min_h_0().h_full().v_flex().bg(editor_bg)
                            .child(h_flex().h(px(36.)).items_stretch().bg(sidebar).border_b_1().border_color(border)
                                .child(Button::new("engine-tab").small().ghost().label(format!("  {}  ×", active_tab)).on_click(cx.listener(|this, _, _, cx| this.select_tab("engine.can", cx))))
                                .child(Button::new("brake-tab").small().ghost().label("  brake.can  ×").on_click(cx.listener(|this, _, _, cx| this.select_tab("brake.can", cx))))
                                .child(div().flex_1())
                                .child(div().px_3().items_center().text_size(px(10.)).text_color(muted).child("CAPL")))
                            .child(div().flex_1().min_h_0().p_2().child(Editor::new(&self.editor).size_full().into_any_element()))
                            .child(h_flex().h(px(34.)).items_center().gap_1().px_2().bg(sidebar).border_t_1().border_color(border)
                                .child(Button::new("simulation-tab").small().ghost().label("Simulation").on_click(cx.listener(|this, _, _, cx| this.select_bottom_tab("Simulation", cx))))
                                .child(Button::new("console-tab").small().ghost().label("Console").on_click(cx.listener(|this, _, _, cx| this.select_bottom_tab("Console", cx))))
                                .child(Button::new("runtime-tab").small().ghost().label("Runtime").on_click(cx.listener(|this, _, _, cx| this.select_bottom_tab("Runtime", cx))))
                                .child(Button::new("diagnostics-tab").small().ghost().label("Diagnostics").on_click(cx.listener(|this, _, _, cx| this.select_bottom_tab("Diagnostics", cx))))
                                .child(Button::new("trace-tab").small().ghost().label("Trace Replay").on_click(cx.listener(|this, _, _, cx| this.select_bottom_tab("Trace Replay", cx)))))
                            .child(div().h(px(150.)).p_2().v_flex().gap_2().bg(panel).border_t_1().border_color(border)
                                .child(h_flex().items_center().gap_2().child(div().text_size(px(10.)).font_weight(gpui::FontWeight::BOLD).text_color(muted).child(if bottom_tab == "Simulation" { "VIRTUAL TIME" } else { bottom_tab.as_str() })).child(div().px_2().py_1().rounded(px(4.)).bg(rgb(0x2a211d)).font_family("monospace").text_size(px(11.)).child("0 ms")).child(Button::new("plus-100").small().ghost().label("+100 ms").on_click(cx.listener(|this, _, _, cx| this.advance(100_000, cx)))).child(Button::new("plus-1s").small().ghost().label("+1 s").on_click(cx.listener(|this, _, _, cx| this.advance(1_000_000, cx)))).child(Button::new("sim-reset").small().ghost().label("Reset").on_click(cx.listener(Self::stop))))
                                .child(h_flex().items_center().gap_2().child(div().text_size(px(10.)).font_weight(gpui::FontWeight::BOLD).text_color(muted).child("CAN FRAME")).child(Input::new(&self.frame_id).w(px(72.))).child(Input::new(&self.frame_data).w(px(112.))).child(Button::new("inject-frame").small().primary().label("Inject").on_click(cx.listener(Self::replay))).child(div().w(px(1.)).h(px(18.)).bg(border)).child(div().text_size(px(10.)).font_weight(gpui::FontWeight::BOLD).text_color(muted).child("KEY")).child(Input::new(&self.key_input).w(px(40.))).child(Button::new("inject-key").small().ghost().label("Press").on_click(cx.listener(Self::inject_key)))))
                                .child(h_flex().items_center().gap_2().child(div().text_size(px(10.)).font_weight(gpui::FontWeight::BOLD).text_color(muted).child("SYSVAR")).child(Input::new(&self.sysvar_name).w(px(128.))).child(Input::new(&self.sysvar_value).w(px(48.))).child(Button::new("set-sysvar").small().ghost().label("Set").on_click(cx.listener(Self::trigger_sysvar))).child(Button::new("set-envvar").small().ghost().label("Env").on_click(cx.listener(Self::trigger_envvar))).child(Button::new("error-frame").small().ghost().label("errorFrame").on_click(cx.listener(|this, event, window, cx| this.bus_event("errorFrame", event, window, cx)))).child(Button::new("bus-off").small().ghost().label("busOff").on_click(cx.listener(|this, event, window, cx| this.bus_event("busOff", event, window, cx)))))
                    )
                    .child(
                        div().w(px(252.)).flex_shrink_0().h_full().v_flex().bg(sidebar).border_l_1().border_color(border).p_3().gap_3()
                            .child(div().text_size(px(11.)).font_weight(gpui::FontWeight::BOLD).text_color(muted).child("INSPECTOR"))
                            .child(div().p_3().rounded(px(6.)).bg(panel).v_flex().gap_2().child(div().font_weight(gpui::FontWeight::BOLD).child("engine.can")).child(div().text_size(px(11.)).text_color(muted).child("CAPL source file")).child(div().h(px(1.)).bg(border)).child(h_flex().justify_between().child(div().text_size(px(11.)).text_color(muted).child("Handlers")).child(div().text_size(px(11.)).child(self.handlers.len().to_string()))).child(h_flex().justify_between().child(div().text_size(px(11.)).text_color(muted).child("Timers")).child(div().text_size(px(11.)).child(self.timers.len().to_string()))))
                            .child(div().text_size(px(10.)).text_color(muted).child("Build artifacts")).child(Button::new("ast").small().ghost().label("AST JSON")).child(Button::new("wasm").small().ghost().label("WASM module")).child(Button::new("c").small().ghost().label("C output")),
                    ),
            )
            .child(h_flex().h(px(26.)).px_3().gap_3().items_center().bg(chrome).border_t_1().border_color(border).text_size(px(10.)).text_color(muted).child("CAPL source.can").child("Ln 1, Col 1").child(div().px_2().py_0p5().rounded(px(3.)).bg(rgb(0x2a211d)).child(status)).child(div().flex_1()).child("UTF-8").child("WASM"))
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



