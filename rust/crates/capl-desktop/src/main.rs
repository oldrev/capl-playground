//! Native CAPL Playground shell.
//!
//! The UI intentionally talks to the shared Rust model/runtime crates.  It is
//! therefore also a clean home for a future `capl-web` target without bringing
//! React or TypeScript back into the application core.

use capl_ast::{Decl, EventSpec};
use capl_database_core::NetworkDatabase;
use capl_database_dbc::parse as parse_dbc;
use capl_diag::{Severity, Stage};
use capl_driver::{compile, BackendId, Options, WasmArtifact};
use capl_model::{parse_hex_bytes, parse_hex_u32, BusFrame, Direction};
use capl_project::{decode, encode, ProjectManifest};
use capl_runtime::{ReplayHandler, ReplayOutput, ReplayTimer, RuntimeEvent, WasmReplay};
use capl_sysvars::{SysVarKey, SysVarValue};
use capl_trace_asc::AscReader;
use capl_trace_core::{TraceEvent, TraceReader};
use gpui::{
    div, App, ClickEvent, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, Window,
};
use gpui_base::dock::PanelEvent;
use gpui_base::StyledExt;
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    dock::{panel_handle, DockArea, DockLayout, DockPlacement, DockSkin, Panel},
    h_flex,
    input::{Copy, Cut, Editor, EditorState, Input, InputState, Paste, Redo, SelectAll, Undo},
    menu::DropdownMenu as _,
    ActiveTheme as _, Sizable,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

const DOCK_ID: &str = "capl-workspace";

actions!(
    capl,
    [
        NewProject,
        OpenProject,
        SaveProject,
        OpenTrace,
        OpenDatabase,
        ToggleRuntimePanel,
        ToggleTracePanel,
        ToggleDatabasePanel,
        ToggleDiagnosticsPanel,
        ToggleConsolePanel,
        ToggleBusTracePanel,
        ToggleArchitecturePanel,
        ToggleAstView,
        ShowArchitecture,
        LoadExampleMinimal,
        LoadExampleTrace,
        LoadExampleEvents,
        LoadExampleGateway,
        LoadExampleState,
        LoadExampleTypes,
        LoadTraceDemo,
        ReplayNext,
        ResetTrace,
        StartSimulation,
        StopSimulation,
        ResetSimulation,
        Advance100ms,
        Advance1s,
        InjectFrame,
        InjectKey,
        SetSysvar,
        SetEnvvar,
        InjectErrorFrame,
        InjectBusOff,
        ToggleSimulationPanel,
        ExportAst,
        ExportC,
        ExportWasm,
        CopyC,
        CopyWat,
        CopySource,
        ReplaySample,
        ReplayTrace,
        ClearRuntime,
        ToggleTrace
    ]
);

struct TextPanel {
    name: &'static str,
    title: &'static str,
    body: String,
    focus: FocusHandle,
}

fn wasm_panel_body(artifact: &WasmArtifact) -> String {
    let mut body = format!(
        "WASM module\nsize: {} bytes\nmemory: {} page(s)\nexports: {}\nimports: capl host\n\n",
        artifact.bytes.len(),
        artifact.memory_pages,
        artifact.exports.len(),
    );
    body.push_str("Exports:\n");
    body.push_str(&artifact.exports.join("\n"));
    body.push_str("\n\nMemory layout:\n");
    if artifact.layout.is_empty() {
        body.push_str("(empty)");
    } else {
        for entry in &artifact.layout {
            body.push_str(&format!(
                "0x{:04X}  {:>4} B  {:<10} {}\n",
                entry.addr, entry.size, entry.ty, entry.name
            ));
        }
    }
    body.push_str("\n\nWAT:\n");
    body.push_str(&artifact.wat);
    body
}

fn ast_panel_body(ast_json: &str) -> String {
    let compact = ast_json.trim();
    if compact.is_empty() {
        return "AST is empty".into();
    }
    let mut depth = 0usize;
    let mut out = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for ch in compact.chars() {
        if quoted {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                out.push(ch);
            }
            '{' | '[' => {
                depth += 1;
                out.push(ch);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push(ch);
            }
            ',' => {
                out.push(',');
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            ':' => {
                out.push(':');
                out.push(' ');
            }
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}
impl TextPanel {
    fn new(
        name: &'static str,
        title: &'static str,
        body: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            name,
            title,
            body: body.into(),
            focus: cx.focus_handle(),
        }
    }
}
impl TextPanel {
    fn set_body(&mut self, body: impl Into<String>, cx: &mut Context<Self>) {
        self.body = body.into();
        cx.notify();
    }
}
impl gpui_base::dock::Panel for TextPanel {
    fn panel_name(&self) -> &'static str {
        self.name
    }
}
impl Panel for TextPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.title
    }
}
impl EventEmitter<PanelEvent> for TextPanel {}
impl Focusable for TextPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for TextPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_project = self.name == "project-files";
        let empty_project = is_project && self.body == "No project open";
        let mut panel = div()
            .id(self.name)
            .test_support()
            .size_full()
            .p_4()
            .v_flex()
            .gap_2()
            .bg(if is_project {
                cx.theme().sidebar
            } else {
                cx.theme().background
            })
            .text_color(if is_project {
                cx.theme().sidebar_foreground
            } else {
                cx.theme().foreground
            })
            .when(!is_project, |this| {
                this.font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(12.))
            })
            .when(is_project, |this| {
                this.child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(10.))
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(cx.theme().muted_foreground)
                                .child(if empty_project {
                                    "PROJECT"
                                } else {
                                    "WORKSPACE"
                                }),
                        )
                        .child(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new("open-project-empty")
                                        .xsmall()
                                        .ghost()
                                        .label("Open")
                                        .on_click(|_, window, cx| {
                                            window.dispatch_action(Box::new(OpenProject), cx)
                                        }),
                                )
                                .child(
                                    Button::new("new-project-empty")
                                        .xsmall()
                                        .ghost()
                                        .label("New")
                                        .on_click(|_, window, cx| {
                                            window.dispatch_action(Box::new(NewProject), cx)
                                        }),
                                ),
                        ),
                )
            });
        if empty_project {
            panel = panel
                .child(
                    div()
                        .pt_3()
                        .text_size(px(12.))
                        .text_color(cx.theme().muted_foreground)
                        .child("No project open"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(cx.theme().muted_foreground)
                        .child("Open a .caplproj workspace or create a new one."),
                );
        } else {
            panel = panel.child(
                div()
                    .id("panel-body")
                    .flex_1()
                    .overflow_y_scroll()
                    .whitespace_normal()
                    .child(self.body.clone()),
            );
        }
        panel
    }
}

struct EditorPanel {
    editor: Entity<EditorState>,
    focus: FocusHandle,
    font_size: f32,
}

impl EditorPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .default_value("variables {\n  msTimer replayTimer;\n}\n\non start {\n  write(\"ready for trace replay\");\n  setTimer(replayTimer, 100);\n}\n\non timer replayTimer {\n  write(\"timer at %d ms\", timeNow());\n}\n\non message 0x100 {\n  write(\"received 0x100\");\n}\n\non key 'a' {\n  write(\"key %c\", this);\n}\n\non sysvar Foo {\n  write(\"sysvar %d\", this);\n}\n\non envVar Foo {\n  write(\"envvar %d\", this);\n}\n\non busOff { write(\"busOff\"); }\n\non errorFrame { write(\"errorFrame\"); }\n")
                .line_number(true)
                .indent_guides(true)
        });
        let focus = editor.read(cx).focus_handle(cx);
        Self {
            editor,
            focus,
            font_size: 13.0,
        }
    }
}

impl gpui_base::dock::Panel for EditorPanel {
    fn panel_name(&self) -> &'static str {
        "source-editor"
    }
}
impl Panel for EditorPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "CAPL"
    }
}
impl EventEmitter<PanelEvent> for EditorPanel {}
impl Focusable for EditorPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for EditorPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("source-editor")
            .test_support()
            .size_full()
            .child(
                Editor::new(&self.editor)
                    .size_full()
                    .text_size(px(self.font_size)),
            )
    }
}

struct SimulationPanel {
    frame_id_input: Entity<InputState>,
    frame_data_input: Entity<InputState>,
    sysvar_name_input: Entity<InputState>,
    sysvar_value_input: Entity<InputState>,
    key_input: Entity<InputState>,
    virtual_time_us: u64,
    pending_timers: usize,
    focus: FocusHandle,
}

impl SimulationPanel {
    fn new(
        frame_id_input: Entity<InputState>,
        frame_data_input: Entity<InputState>,
        sysvar_name_input: Entity<InputState>,
        sysvar_value_input: Entity<InputState>,
        key_input: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            frame_id_input,
            frame_data_input,
            sysvar_name_input,
            sysvar_value_input,
            key_input,
            virtual_time_us: 0,
            pending_timers: 0,
            focus: cx.focus_handle(),
        }
    }

    fn set_runtime_state(&mut self, virtual_time_us: u64, pending_timers: usize) {
        self.virtual_time_us = virtual_time_us;
        self.pending_timers = pending_timers;
    }
}

impl gpui_base::dock::Panel for SimulationPanel {
    fn panel_name(&self) -> &'static str {
        "simulation-controls"
    }
}
impl Panel for SimulationPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Simulation"
    }
}
impl EventEmitter<PanelEvent> for SimulationPanel {}
impl Focusable for SimulationPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for SimulationPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("simulation-controls")
            .test_support()
            .size_full()
            .v_flex()
            .gap_2()
            .p_2()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_size(px(10.)).font_weight(gpui::FontWeight::BOLD).text_color(cx.theme().muted_foreground).child("VIRTUAL TIME"))
                    .child(div().px_2().py_1().rounded(cx.theme().radius).bg(cx.theme().secondary).text_color(cx.theme().secondary_foreground).font_family(cx.theme().mono_font_family.clone()).text_size(px(11.)).child(format!("{:0.3} s", self.virtual_time_us as f64 / 1_000_000.)))
                    .child(Button::new("sim-advance-100").small().ghost().label("+100 ms").on_click(|_, window, cx| window.dispatch_action(Box::new(Advance100ms), cx)))
                    .child(Button::new("sim-advance-1000").small().ghost().label("+1 s").on_click(|_, window, cx| window.dispatch_action(Box::new(Advance1s), cx)))
                    .child(Button::new("sim-reset-simulation").small().ghost().label("Reset").on_click(|_, window, cx| window.dispatch_action(Box::new(ResetSimulation), cx)))
                    .child(div().flex_1())
                    .child(div().text_size(px(10.)).text_color(cx.theme().muted_foreground).child(format!("PENDING TIMERS  {}", self.pending_timers))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_size(px(10.)).font_weight(gpui::FontWeight::BOLD).text_color(cx.theme().muted_foreground).child("CAN FRAME"))
                    .child(Input::new(&self.frame_id_input).small().w(px(78.)))
                    .child(Input::new(&self.frame_data_input).small().w(px(140.)))
                    .child(Button::new("sim-inject").small().primary().label("Inject").on_click(|_, window, cx| window.dispatch_action(Box::new(InjectFrame), cx)))
                    .child(div().w(px(1.)).h(px(20.)).bg(cx.theme().border))
                    .child(div().text_size(px(10.)).font_weight(gpui::FontWeight::BOLD).text_color(cx.theme().muted_foreground).child("KEY"))
                    .child(Input::new(&self.key_input).small().w(px(42.)))
                    .child(Button::new("sim-press-key").small().ghost().label("Press").on_click(|_, window, cx| window.dispatch_action(Box::new(InjectKey), cx))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_size(px(10.)).font_weight(gpui::FontWeight::BOLD).text_color(cx.theme().muted_foreground).child("SYSVAR"))
                    .child(Input::new(&self.sysvar_name_input).small().w(px(120.)))
                    .child(Input::new(&self.sysvar_value_input).small().w(px(48.)))
                    .child(Button::new("sim-trigger-sysvar").small().ghost().label("Set").on_click(|_, window, cx| window.dispatch_action(Box::new(SetSysvar), cx)))
                    .child(Button::new("sim-trigger-envvar").small().ghost().label("Env").on_click(|_, window, cx| window.dispatch_action(Box::new(SetEnvvar), cx)))
                    .child(Button::new("sim-error-frame").small().ghost().label("errorFrame").on_click(|_, window, cx| window.dispatch_action(Box::new(InjectErrorFrame), cx)))
                    .child(Button::new("sim-bus-off").small().ghost().label("busOff").on_click(|_, window, cx| window.dispatch_action(Box::new(InjectBusOff), cx))),
            )
    }
}

pub struct Workspace {
    dock: Entity<DockArea>,
    simulation_panel: Entity<SimulationPanel>,
    editor: Entity<EditorState>,
    frame_id_input: Entity<InputState>,
    frame_data_input: Entity<InputState>,
    sysvar_name_input: Entity<InputState>,
    sysvar_value_input: Entity<InputState>,
    key_input: Entity<InputState>,
    artifact: Option<WasmArtifact>,
    ast_output: Option<String>,
    c_output: Option<String>,
    handlers: Vec<ReplayHandler>,
    timers: Vec<ReplayTimer>,
    runtime: Option<WasmReplay>,
    runtime_history: Option<ReplayOutput>,
    last_virtual_time_us: u64,
    trace: Vec<TraceEvent>,
    trace_cursor: usize,
    diagnostics: Entity<TextPanel>,
    trace_panel: Entity<TextPanel>,
    database_panel: Entity<TextPanel>,
    project_panel: Entity<TextPanel>,
    ast_panel: Entity<TextPanel>,
    c_panel: Entity<TextPanel>,
    wasm_panel: Entity<TextPanel>,
    runtime_panel: Entity<TextPanel>,
    console_panel: Entity<TextPanel>,
    bus_trace_panel: Entity<TextPanel>,
    architecture_panel: Entity<TextPanel>,
    status: String,
    project_path: Option<std::path::PathBuf>,
    source_path: Option<std::path::PathBuf>,
    trace_path: Option<std::path::PathBuf>,
    database_path: Option<std::path::PathBuf>,
    database: Option<NetworkDatabase>,
    compile_metrics: String,
    ast_tree_view: bool,
}
impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (dock, _skin) = DockSkin::dock_area(DOCK_ID, Some(1), window, cx);
        let source =
            cx.new(|cx| TextPanel::new("project-files", "Explorer", "No project open", cx));
        let editor = cx.new(|cx| EditorPanel::new(window, cx));
        let frame_id_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("100")
                .placeholder("CAN ID (hex)")
        });
        let frame_data_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("12 34")
                .placeholder("Data bytes (hex)")
        });
        let sysvar_name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("Foo")
                .placeholder("Sysvar")
        });
        let sysvar_value_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("1")
                .placeholder("Value")
        });
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("a")
                .placeholder("Key")
        });
        let simulation_panel = cx.new(|cx| {
            SimulationPanel::new(
                frame_id_input.clone(),
                frame_data_input.clone(),
                sysvar_name_input.clone(),
                sysvar_value_input.clone(),
                key_input.clone(),
                cx,
            )
        });
        let trace = cx.new(|cx| TextPanel::new("trace-replay", "Trace Replay", "No trace loaded. Use Run → Open trace or place traffic.asc in the project directory.", cx));
        let database_panel = cx.new(|cx| {
            TextPanel::new(
                "database",
                "Database",
                "Open a DBC file to decode trace frames.",
                cx,
            )
        });
        let diagnostics = cx.new(|cx| {
            TextPanel::new(
                "diagnostics",
                "Diagnostics",
                "Compiler diagnostics and runtime events will appear here.",
                cx,
            )
        });
        let ast_panel =
            cx.new(|cx| TextPanel::new("ast-output", "AST", "Build to inspect the AST JSON.", cx));
        let c_panel =
            cx.new(|cx| TextPanel::new("c-output", "C", "Build to inspect generated C.", cx));
        let wasm_panel = cx
            .new(|cx| TextPanel::new("wasm-output", "WASM", "Build to inspect generated WAT.", cx));
        let runtime_panel = cx.new(|cx| {
            TextPanel::new(
                "runtime-output",
                "Runtime",
                "Run the measurement to see write() calls and transmitted frames.",
                cx,
            )
        });
        let console_panel = cx.new(|cx| {
            TextPanel::new(
                "console-output",
                "Console",
                "Build diagnostics and runtime writes will appear here.",
                cx,
            )
        });
        let bus_trace_panel = cx.new(|cx| {
            TextPanel::new(
                "bus-trace",
                "Bus Trace",
                "Injected and replayed CAN frames will appear here.",
                cx,
            )
        });
        let architecture_panel = cx.new(|cx| {
            TextPanel::new(
                "architecture",
                "Architecture",
                "Use Run -> Architecture to inspect the compiler and simulation graph.",
                cx,
            )
        });
        let editor_state = editor.read(cx).editor.clone();
        let center = DockLayout::tabs().panel_view(panel_handle(editor.clone()), cx);
        let left = DockLayout::tabs().panel_view(panel_handle(source.clone()), cx);
        let bottom = DockLayout::tabs()
            .panel_view(panel_handle(simulation_panel.clone()), cx)
            .panel_view(panel_handle(console_panel.clone()), cx)
            .panel_view(panel_handle(runtime_panel.clone()), cx)
            .panel_view(panel_handle(diagnostics.clone()), cx)
            .panel_view(panel_handle(trace.clone()), cx)
            .panel_view(panel_handle(bus_trace_panel.clone()), cx)
            .panel_view(panel_handle(database_panel.clone()), cx)
            .panel_view(panel_handle(ast_panel.clone()), cx)
            .panel_view(panel_handle(c_panel.clone()), cx)
            .panel_view(panel_handle(wasm_panel.clone()), cx)
            .panel_view(panel_handle(architecture_panel.clone()), cx);
        let viewport_width = window.viewport_size().width;
        let project_width = if viewport_width < px(1_000.) {
            px(190.)
        } else if viewport_width < px(1_350.) {
            px(210.)
        } else {
            px(230.)
        };
        dock.update(cx, |area, cx| {
            area.set_center(center, window, cx);
            area.set_dock(DockPlacement::Left, left, window, cx);
            area.set_dock_size(DockPlacement::Left, project_width, window, cx);
            area.set_dock(DockPlacement::Bottom, bottom, window, cx);
            let simulation_panel_id =
                gpui_base::dock::PanelView::panel_id(&simulation_panel, cx);
            area.select_panel(simulation_panel_id, window, cx);
            area.set_dock_size(DockPlacement::Bottom, px(260.), window, cx);
            area.set_dock_collapsible(DockPlacement::Left, true, window, cx);
            area.set_dock_collapsible(DockPlacement::Bottom, true, window, cx);
        });
        Self {
            dock,
            simulation_panel,
            editor: editor_state,
            frame_id_input,
            frame_data_input,
            sysvar_name_input,
            sysvar_value_input,
            key_input,
            artifact: None,
            ast_output: None,
            c_output: None,
            handlers: Vec::new(),
            timers: Vec::new(),
            runtime: None,
            runtime_history: None,
            last_virtual_time_us: 0,
            trace: Vec::new(),
            trace_cursor: 0,
            diagnostics,
            trace_panel: trace,
            database_panel,
            project_panel: source,
            ast_panel,
            c_panel,
            wasm_panel,
            runtime_panel,
            console_panel,
            bus_trace_panel,
            architecture_panel,
            status: "Ready".into(),
            project_path: None,
            source_path: None,
            trace_path: None,
            database_path: None,
            database: None,
            compile_metrics: "lex: --  parse: --  sema: --  codegen: --".into(),
            ast_tree_view: true,
        }
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn has_stopped_write(&self, needle: &str) -> bool {
        self.runtime_history
            .as_ref()
            .is_some_and(|output| output.writes.iter().any(|line| line.contains(needle)))
    }

    pub fn stopped_writes(&self) -> Vec<String> {
        self.runtime_history
            .as_ref()
            .map(|output| output.writes.clone())
            .unwrap_or_default()
    }

    pub fn pending_timer_count(&self) -> usize {
        self.runtime
            .as_ref()
            .map_or(0, |runtime| runtime.pending_timers().len())
    }

    pub fn trace_event_count(&self) -> usize {
        self.trace.len()
    }

    pub fn trace_cursor(&self) -> usize {
        self.trace_cursor
    }

    pub fn ast_tree_view(&self) -> bool {
        self.ast_tree_view
    }

    pub fn load_trace_text(&mut self, text: &str, cx: &mut Context<Self>) -> Result<usize, String> {
        let trace = AscReader
            .read(text)
            .map_err(|error| format!("could not parse trace: {error}"))?;
        self.trace = trace;
        self.trace_cursor = 0;
        self.trace_path = None;
        let count = self.trace.len();
        self.refresh_trace_panel(cx);
        self.refresh_project_panel(cx);
        self.status = format!("Loaded {count} trace event(s)");
        cx.notify();
        Ok(count)
    }

    pub fn load_trace_demo(&mut self, cx: &mut Context<Self>) {
        let text =
            "date 2026-01-01\n   0.100000 1  100 Rx d 2 12 34\n   0.250000 1  200 Rx d 1 AA\n";
        match AscReader.read(text) {
            Ok(trace) => {
                self.trace = trace;
                self.trace_cursor = 0;
                self.trace_path = None;
                self.refresh_trace_panel(cx);
                self.refresh_project_panel(cx);
                self.status = format!("Loaded trace demo: {} event(s)", self.trace.len());
                cx.notify();
            }
            Err(error) => {
                self.status = format!("Trace demo failed: {error}");
                cx.notify();
            }
        }
    }

    fn compile_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.editor.read(cx).value().to_string();
        let out = compile(
            &source,
            Options {
                backends: vec![BackendId::Wasm, BackendId::C, BackendId::Ast],
                ..Options::default()
            },
        );
        self.compile_metrics = format!(
            "lex: --  parse: {:.2} ms  sema: {:.2} ms  codegen: {:.2} ms",
            out.timings.parse_us as f64 / 1000.0,
            out.timings.sema_us as f64 / 1000.0,
            out.timings.codegen_us as f64 / 1000.0
        );
        if !out.ok {
            let body = out
                .diagnostics
                .iter()
                .map(|diag| {
                    let severity = match diag.severity {
                        Severity::Error => "error",
                        Severity::Warning => "warning",
                        Severity::Note => "note",
                    };
                    let stage = match diag.stage {
                        Stage::Lex => "lex",
                        Stage::Parse => "parse",
                        Stage::Sema => "sema",
                        Stage::Codegen => "codegen",
                        Stage::Link => "link",
                    };
                    format!(
                        "{}:{}:{} [{} / {}] {}",
                        diag.loc.line, diag.loc.col, diag.loc.pos, severity, stage, diag.message
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.diagnostics
                .update(cx, |panel, cx| panel.set_body(body, cx));
            self.status = format!(
                "Build failed: {}",
                out.diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            self.artifact = None;
            self.handlers.clear();
            self.select_output_panel(self.diagnostics.clone(), window, cx);
            cx.notify();
            return;
        }
        self.handlers = out
            .unit
            .as_ref()
            .into_iter()
            .flat_map(|unit| unit.decls.iter())
            .filter_map(replay_handler)
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
        self.ast_output = Some(out.ast_json.clone());
        self.c_output = out.c.clone();
        self.runtime = None;
        self.ast_panel.update(cx, |panel, cx| {
            panel.set_body(
                if self.ast_tree_view {
                    ast_panel_body(&out.ast_json)
                } else {
                    out.ast_json.clone()
                },
                cx,
            )
        });
        self.c_panel.update(cx, |panel, cx| {
            panel.set_body(out.c.clone().unwrap_or_else(|| "No C artifact".into()), cx)
        });
        self.wasm_panel.update(cx, |panel, cx| {
            panel.set_body(
                self.artifact
                    .as_ref()
                    .map(wasm_panel_body)
                    .unwrap_or_else(|| "No WASM artifact".into()),
                cx,
            )
        });
        self.diagnostics.update(cx, |panel, cx| {
            panel.set_body(
                format!(
                    "Build succeeded\nhandlers: {}\nC output: {}\nWASM output: {}\n\n{}",
                    self.handlers.len(),
                    out.c.is_some(),
                    self.artifact.is_some(),
                    self.compile_metrics
                ),
                cx,
            )
        });
        self.status = format!("Build succeeded: {} handler(s)", self.handlers.len());
        self.refresh_diagnostics(cx);
        cx.notify();
    }

    fn start_runtime(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.artifact.is_none() {
            self.compile_source(window, cx);
        }
        let Some(artifact) = self.artifact.as_ref() else {
            self.select_output_panel(self.diagnostics.clone(), window, cx);
            return;
        };
        match WasmReplay::new_with_timers(
            &artifact.bytes,
            artifact.msgbuf_addr,
            self.handlers.clone(),
            self.timers.clone(),
        ) {
            Ok(runtime) => {
                self.runtime = Some(runtime);
                self.runtime_history = None;
                self.last_virtual_time_us = 0;
                self.trace_cursor = 0;
                if let Some(runtime) = self.runtime.as_mut() {
                    match runtime.start_measurement() {
                        Ok(()) => {
                            self.status = "Running".into();
                            self.select_output_panel(self.console_panel.clone(), window, cx);
                        }
                        Err(error) => {
                            self.status = format!("Start error: {error}");
                            self.select_output_panel(self.diagnostics.clone(), window, cx);
                        }
                    }
                }
                self.refresh_runtime_panel(cx);
            }
            Err(error) => {
                self.status = format!("Runtime error: {error}");
                self.select_output_panel(self.diagnostics.clone(), window, cx);
            }
        }
        cx.notify();
    }

    fn stop_runtime(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(runtime) = self.runtime.as_mut() {
            let _ = runtime.stop_measurement();
            self.runtime_history = Some(runtime.output().clone());
            self.last_virtual_time_us = runtime.now_us();
        }
        self.runtime = None;
        self.status = "Stopped".into();
        self.refresh_diagnostics(cx);
        self.refresh_runtime_panel(cx);
        cx.notify();
    }

    fn advance_time(&mut self, delta_us: u64, cx: &mut Context<Self>) {
        if let Some(runtime) = self.runtime.as_mut() {
            match runtime.advance_time(delta_us) {
                Ok(fired) => {
                    self.status = format!(
                        "Virtual time: {} ms; fired {} timer(s)",
                        runtime.now_us() / 1000,
                        fired
                    )
                }
                Err(error) => self.status = format!("Timer error: {error}"),
            };
            self.refresh_runtime_panel(cx);
        } else {
            self.status = "Run the measurement before advancing time".into();
        }
        cx.notify();
    }

    fn reset_runtime(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.runtime = None;
        self.runtime_history = None;
        self.last_virtual_time_us = 0;
        self.trace_cursor = 0;
        self.status = "Simulation reset".into();
        self.refresh_diagnostics(cx);
        self.refresh_runtime_panel(cx);
        cx.notify();
    }

    fn clear_runtime_output(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.runtime_history = Some(ReplayOutput::default());
        self.status = "Runtime output cleared".into();
        self.refresh_runtime_panel(cx);
        self.refresh_console_panel(cx);
        self.refresh_bus_trace_panel(cx);
        cx.notify();
    }

    fn select_output_panel<T: Panel>(
        &mut self,
        panel: Entity<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel_id = gpui_base::dock::PanelView::panel_id(&panel, cx);
        self.dock
            .update(cx, |dock, cx| dock.select_panel(panel_id, window, cx));
    }

    fn toggle_ast_view(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.ast_tree_view = !self.ast_tree_view;
        if let Some(ast) = self.ast_output.as_deref() {
            let body = if self.ast_tree_view {
                ast_panel_body(ast)
            } else {
                ast.to_string()
            };
            self.ast_panel
                .update(cx, |panel, cx| panel.set_body(body, cx));
        }
        self.status = if self.ast_tree_view {
            "AST view: tree".into()
        } else {
            "AST view: raw JSON".into()
        };
        cx.notify();
    }

    fn show_architecture(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let body = format!(
            "CAPL WORKSPACE\n\nSource editor\n  handlers: {}\n  timers: {}\n\nCompiler pipeline\n  parse -> semantic analysis -> AST\n  AST -> C backend\n  AST -> WASM backend\n\nSimulation\n  virtual time: {} ms\n  trace events: {}\n  database: {}\n\nRuntime\n  status: {}\n  pending timers: {}",
            self.handlers.len(),
            self.timers.len(),
            self.last_virtual_time_us / 1000,
            self.trace.len(),
            self.database_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none".into()),
            self.status,
            self.pending_timer_count()
        );
        self.diagnostics
            .update(cx, |panel, cx| panel.set_body(body.clone(), cx));
        self.architecture_panel
            .update(cx, |panel, cx| panel.set_body(body, cx));
        self.status = "Architecture refreshed".into();
        cx.notify();
    }

    fn refresh_diagnostics(&mut self, cx: &mut Context<Self>) {
        let body = format!(
            "Status: {}\n\nHandlers: {}\nTimers: {}\nTrace events: {}\nDatabase: {}\nWASM: {}\nC backend: {}\n\n{}",
            self.status,
            self.handlers.len(),
            self.timers.len(),
            self.trace.len(),
            self.database_path.is_some(),
            self.artifact.is_some(),
            self.c_output.is_some(),
            self.compile_metrics
        );
        self.diagnostics
            .update(cx, |panel, cx| panel.set_body(body, cx));
        self.refresh_console_panel(cx);
    }

    fn refresh_console_panel(&mut self, cx: &mut Context<Self>) {
        let output_snapshot = self
            .runtime
            .as_ref()
            .map(|runtime| runtime.output())
            .or(self.runtime_history.as_ref());
        let mut body = format!("Status: {}\n\n", self.status);
        body.push_str(&format!("compile: {}\n\n", self.compile_metrics));
        if let Some(output) = output_snapshot {
            if output.events.is_empty() {
                body.push_str("(no runtime events)");
            } else {
                body.push_str("time         kind       event\n");
                body.push_str("----------------------------------------\n");
                body.push_str(
                    &output
                        .events
                        .iter()
                        .map(format_runtime_event)
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
        } else {
            body.push_str("(no runtime output)");
        }
        self.console_panel
            .update(cx, |panel, cx| panel.set_body(body, cx));
    }

    fn refresh_bus_trace_panel(&mut self, cx: &mut Context<Self>) {
        let output_snapshot = self
            .runtime
            .as_ref()
            .map(|runtime| runtime.output())
            .or(self.runtime_history.as_ref());
        let mut rows = Vec::new();
        rows.extend(
            self.trace
                .iter()
                .take(self.trace_cursor)
                .filter_map(|event| match event {
                    TraceEvent::Frame(frame) => Some(format!(
                        "REPLAY  {}",
                        format_frame_row(
                            frame.timestamp_us,
                            frame.channel,
                            frame.direction,
                            frame.id,
                            &frame.data
                        )
                    )),
                    _ => None,
                }),
        );
        if let Some(output) = output_snapshot {
            rows.extend(output.frames.iter().map(|frame| {
                format!(
                    "RUNTIME {}",
                    format_frame_row(
                        frame.timestamp_us,
                        frame.channel,
                        frame.direction,
                        frame.id,
                        &frame.data,
                    )
                )
            }));
        }
        let body = if rows.is_empty() {
            "No runtime bus frames yet. Inject a frame or replay a trace.".into()
        } else {
            format!("Runtime Bus Trace\n\n{}", rows.join("\n"))
        };
        self.bus_trace_panel
            .update(cx, |panel, cx| panel.set_body(body, cx));
    }

    fn inject_sample(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.runtime.is_none() {
            self.start_runtime(window, cx);
        }
        if let Some(runtime) = self.runtime.as_mut() {
            let id_text = self.frame_id_input.read(cx).value().to_string();
            let id = match parse_hex_u32(&id_text) {
                Ok(id) => id,
                Err(_) => {
                    self.status = "Invalid CAN identifier; enter hexadecimal, e.g. 100".into();
                    cx.notify();
                    return;
                }
            };
            let data_text = self.frame_data_input.read(cx).value().to_string();
            let data = parse_hex_bytes(&data_text);
            let Ok(data) = data else {
                self.status = "Invalid data byte; enter hex bytes, e.g. 12 34 AB".into();
                cx.notify();
                return;
            };
            if data.len() > 8 {
                self.status = "Classic CAN frame payload is limited to 8 bytes".into();
                cx.notify();
                return;
            }
            let frame = BusFrame::new(runtime.now_us(), 1, id, Direction::Rx, data);
            match runtime.run_frame(&frame) {
                Ok(count) => self.status = format!("Injected 0x{id:X}; invoked {count} handler(s)"),
                Err(error) => self.status = format!("Inject error: {error}"),
            };
            self.refresh_runtime_panel(cx);
        }
        cx.notify();
    }

    fn inject_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.runtime.is_none() {
            self.start_runtime(window, cx);
        }
        let key = self.key_input.read(cx).value().chars().next();
        let Some(key) = key else {
            self.status = "Enter a key character".into();
            cx.notify();
            return;
        };
        if let Some(runtime) = self.runtime.as_mut() {
            match runtime.inject_key(key) {
                Ok(count) => self.status = format!("Key '{key}' invoked {count} handler(s)"),
                Err(error) => self.status = format!("Key event error: {error}"),
            }
            self.refresh_runtime_panel(cx);
        }
        cx.notify();
    }

    fn trigger_sysvar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.runtime.is_none() {
            self.start_runtime(window, cx);
        }
        let name = self.sysvar_name_input.read(cx).value().to_string();
        let value = self
            .sysvar_value_input
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
                Ok(count) => {
                    self.status = format!("Sysvar {name}={value}; invoked {count} handler(s)")
                }
                Err(error) => self.status = format!("Sysvar error: {error}"),
            }
            self.refresh_runtime_panel(cx);
        }
        cx.notify();
    }

    fn trigger_envvar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.runtime.is_none() {
            self.start_runtime(window, cx);
        }
        let name = self.sysvar_name_input.read(cx).value().to_string();
        let value = self
            .sysvar_value_input
            .read(cx)
            .value()
            .parse::<i64>()
            .unwrap_or(0);
        if let Some(runtime) = self.runtime.as_mut() {
            match runtime.set_envvar(&name, SysVarValue::Integer(value)) {
                Ok(count) => {
                    self.status = format!("Envvar {name}={value}; invoked {count} handler(s)")
                }
                Err(error) => self.status = format!("Envvar error: {error}"),
            };
            self.refresh_runtime_panel(cx);
        }
        cx.notify();
    }

    fn inject_bus_event(
        &mut self,
        event: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.runtime.is_none() {
            self.start_runtime(window, cx);
        }
        if let Some(runtime) = self.runtime.as_mut() {
            match runtime.inject_bus_event(event) {
                Ok(count) => self.status = format!("{event} invoked {count} handler(s)"),
                Err(error) => self.status = format!("Bus event error: {error}"),
            };
            self.refresh_runtime_panel(cx);
        }
        cx.notify();
    }

    fn replay_sample(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.runtime.is_none() {
            self.start_runtime(window, cx);
        }
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        let (frame_index, frame) = self
            .trace
            .iter()
            .enumerate()
            .skip(self.trace_cursor)
            .find_map(|(index, event)| match event {
                TraceEvent::Frame(frame) => Some((index, frame.clone())),
                _ => None,
            })
            .unwrap_or_else(|| {
                (
                    self.trace_cursor,
                    BusFrame::new(100_000, 1, 0x100, Direction::Rx, vec![0x12, 0x34]),
                )
            });
        if frame.timestamp_us > runtime.now_us() {
            if let Err(error) = runtime.advance_time(frame.timestamp_us - runtime.now_us()) {
                self.status = format!("Timer error during trace replay: {error}");
            }
        }
        match runtime.run_frame(&frame) {
            Ok(count) => {
                self.status = format!(
                    "Replayed 0x{:X} at {} ms; invoked {count} handler(s)",
                    frame.id,
                    runtime.now_us() / 1000
                )
            }
            Err(error) => self.status = format!("Replay error: {error}"),
        }
        self.trace_cursor = frame_index.saturating_add(1);
        self.refresh_trace_panel(cx);
        self.refresh_runtime_panel(cx);
        cx.notify();
    }

    fn load_example(&mut self, example: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let source = match example {
            "trace" => "variables {\n  msTimer replayTimer;\n}\n\non start { write(\"ready for trace replay\"); setTimer(replayTimer, 100); }\n\non timer replayTimer { write(\"timer at %d ms\", timeNow()); }\n\non message 0x100 { write(\"received 0x100\"); }\n",
            "events" => "on start { write(\"event playground\"); }\n\non key 'a' { write(\"key %c\", this); }\n\non sysvar Foo { write(\"sysvar %d\", this); }\n\non envVar Foo { write(\"envvar %d\", this); }\n\non busOff { write(\"busOff\"); }\n\non errorFrame { write(\"errorFrame\"); }\n",
            "gateway" => "on message 0x100 { write(\"gateway rx\"); output(0x200); }\n\non message 0x200 { write(\"gateway tx\"); }\n",
            "state" => "variables { int state; }\n\non start { state = 0; write(\"state ready\"); }\n\non message 0x100 { state = state + 1; write(\"state %d\", state); }\n",
            "types" => "variables { int counter; float gain; }\n\non start { counter = 1; gain = 1.5; write(\"types ready\"); }\n",
            "minimal" => "on start { write(\"hello CAPL\"); }\n\non message 0x100 { write(\"received 0x100\"); }\n",
            _ => return,
        };
        self.editor
            .update(cx, |editor, cx| editor.set_value(source, window, cx));
        self.artifact = None;
        self.runtime = None;
        self.runtime_history = None;
        self.trace_cursor = 0;
        self.refresh_runtime_panel(cx);
        self.refresh_trace_panel(cx);
        self.status = format!("Loaded {example} example");
        cx.notify();
    }

    fn replay_trace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.runtime.is_none() {
            self.start_runtime(window, cx);
        }
        let frames = self
            .trace
            .iter()
            .filter_map(|event| match event {
                TraceEvent::Frame(frame) => Some(frame.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if frames.is_empty() {
            self.status = "Load a trace before replaying it".into();
            cx.notify();
            return;
        }
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        let mut invoked = 0;
        let mut fired_timers = 0;
        let mut failed = None;
        for frame in &frames {
            if frame.timestamp_us > runtime.now_us() {
                match runtime.advance_time(frame.timestamp_us - runtime.now_us()) {
                    Ok(count) => fired_timers += count,
                    Err(error) => {
                        failed = Some(error.to_string());
                        break;
                    }
                }
            }
            match runtime.run_frame(frame) {
                Ok(count) => invoked += count,
                Err(error) => {
                    failed = Some(error.to_string());
                    break;
                }
            }
        }
        self.trace_cursor = self.trace.len();
        self.status = failed
            .map(|error| format!("Trace replay failed: {error}"))
            .unwrap_or_else(|| {
                format!(
                    "Replayed {} frame(s); {} message and {} timer handlers",
                    frames.len(),
                    invoked,
                    fired_timers
                )
            });
        self.refresh_runtime_panel(cx);
        self.refresh_trace_panel(cx);
        cx.notify();
    }

    fn replay_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replay_sample(window, cx);
    }

    fn reset_trace(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.trace_cursor = 0;
        self.status = "Trace cursor reset".into();
        self.refresh_trace_panel(cx);
        cx.notify();
    }

    fn open_database(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CAN database", &["dbc"])
            .pick_file()
        else {
            return;
        };
        match std::fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|text| parse_dbc(&text).map_err(|error| error.to_string()))
        {
            Ok(database) => {
                let body = database
                    .messages
                    .iter()
                    .map(|message| {
                        format!(
                            "0x{:X} {} [{}]\n{}",
                            message.id,
                            message.name,
                            message.length,
                            message
                                .signals
                                .iter()
                                .map(|signal| format!(
                                    "  {} ({} bit, ×{} + {}) {}",
                                    signal.name,
                                    signal.length,
                                    signal.factor,
                                    signal.offset,
                                    signal.unit.as_deref().unwrap_or("").trim()
                                ))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                self.database = Some(database);
                self.database_path = Some(path.clone());
                self.database_panel
                    .update(cx, |panel, cx| panel.set_body(body, cx));
                self.refresh_project_panel(cx);
                self.status = format!("Loaded DBC {}", path.display());
            }
            Err(error) => self.status = format!("DBC load failed: {error}"),
        }
        cx.notify();
    }

    fn new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CAPL Project", &["caplproj"])
            .set_file_name("project.caplproj")
            .save_file()
        else {
            return;
        };
        let source_path = path.with_extension("can");
        let source = "on start {\n  write(\"new CAPL project\");\n}\n";
        if let Err(error) = std::fs::write(&source_path, source) {
            self.status = format!("Create source failed: {error}");
            cx.notify();
            return;
        }
        let source_name = source_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let manifest = ProjectManifest {
            source_files: vec![source_name.clone()],
            active_source: Some(source_name),
            ..ProjectManifest::new(path.file_stem().unwrap_or_default().to_string_lossy())
        };
        match encode(&manifest).and_then(|bytes| std::fs::write(&path, bytes).map_err(Into::into)) {
            Ok(()) => {
                self.project_path = Some(path.clone());
                self.source_path = Some(source_path);
                self.editor
                    .update(cx, |editor, cx| editor.set_value(source, window, cx));
                self.refresh_project_panel(cx);
                self.status = format!("Created {}", path.display());
            }
            Err(error) => self.status = format!("Save project failed: {error}"),
        }
        cx.notify();
    }

    fn open_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CAPL Project", &["caplproj"])
            .pick_file()
        else {
            return;
        };
        self.load_project_file(path, window, cx);
    }

    fn load_project_file(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = std::fs::read(&path)
            .map_err(capl_project::ProjectError::from)
            .and_then(|bytes| decode(&bytes));
        match result {
            Ok(manifest) => {
                let root = path.parent().unwrap_or_else(|| std::path::Path::new("."));
                let source_name = manifest
                    .active_source
                    .clone()
                    .or_else(|| manifest.source_files.first().cloned());
                if let Some(source_name) = source_name {
                    let source_path = root.join(source_name);
                    match std::fs::read_to_string(&source_path) {
                        Ok(source) => {
                            self.editor
                                .update(cx, |editor, cx| editor.set_value(source, window, cx));
                            self.source_path = Some(source_path);
                        }
                        Err(error) => {
                            self.status = format!("Load source failed: {error}");
                            cx.notify();
                            return;
                        }
                    }
                }
                self.project_path = Some(path.clone());
                self.trace_path = manifest.traces.first().map(|p| root.join(p));
                self.database_path = manifest.databases.first().map(|p| root.join(p));
                if let Some(database_path) = self.database_path.clone() {
                    if let Ok(text) = std::fs::read_to_string(&database_path) {
                        if let Ok(database) = parse_dbc(&text) {
                            let body = database
                                .messages
                                .iter()
                                .map(|message| {
                                    format!(
                                        "0x{:X} {} [{}]",
                                        message.id, message.name, message.length
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            self.database = Some(database);
                            self.database_panel
                                .update(cx, |panel, cx| panel.set_body(body, cx));
                        }
                    }
                }
                if let Some(trace_path) = self.trace_path.clone() {
                    if let Ok(text) = std::fs::read_to_string(&trace_path) {
                        if let Ok(trace) = AscReader.read(&text) {
                            self.trace = trace;
                            self.trace_cursor = 0;
                        }
                    }
                }
                self.refresh_trace_panel(cx);
                self.refresh_project_panel(cx);
                self.status = format!("Opened {}", path.display());
            }
            Err(error) => self.status = format!("Open project failed: {error}"),
        }
        cx.notify();
    }

    fn save_project(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.project_path.clone().or_else(|| {
            rfd::FileDialog::new()
                .add_filter("CAPL Project", &["caplproj"])
                .set_file_name("project.caplproj")
                .save_file()
        }) else {
            return;
        };
        let source_path = self
            .source_path
            .clone()
            .unwrap_or_else(|| path.with_extension("can"));
        let source_name = source_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let source = self.editor.read(cx).value().to_string();
        let result = std::fs::write(&source_path, source)
            .map_err(capl_project::ProjectError::from)
            .and_then(|_| {
                let manifest = ProjectManifest {
                    source_files: vec![source_name.clone()],
                    active_source: Some(source_name),
                    traces: self
                        .trace_path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|p| vec![p.to_string_lossy().into_owned()])
                        .unwrap_or_default(),
                    databases: self
                        .database_path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|p| vec![p.to_string_lossy().into_owned()])
                        .unwrap_or_default(),
                    ..ProjectManifest::new(path.file_stem().unwrap_or_default().to_string_lossy())
                };
                encode(&manifest).and_then(|bytes| std::fs::write(&path, bytes).map_err(Into::into))
            });
        match result {
            Ok(()) => {
                self.project_path = Some(path.clone());
                self.source_path = Some(source_path);
                self.refresh_project_panel(cx);
                self.status = format!("Saved {}", path.display());
            }
            Err(error) => self.status = format!("Save project failed: {error}"),
        }
        cx.notify();
    }

    fn export_ast(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.ast_output.as_ref() else {
            self.status = "Build before exporting AST".into();
            cx.notify();
            return;
        };
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .set_file_name("ast.json")
            .save_file()
        {
            match std::fs::write(&path, data) {
                Ok(()) => self.status = format!("Exported {}", path.display()),
                Err(error) => self.status = format!("AST export failed: {error}"),
            }
        }
        cx.notify();
    }

    fn export_c(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.c_output.as_ref() else {
            self.status = "Build before exporting C".into();
            cx.notify();
            return;
        };
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("C source", &["c"])
            .set_file_name("source.c")
            .save_file()
        {
            match std::fs::write(&path, data) {
                Ok(()) => self.status = format!("Exported {}", path.display()),
                Err(error) => self.status = format!("C export failed: {error}"),
            }
        }
        cx.notify();
    }

    fn export_wasm(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(artifact) = self.artifact.as_ref() else {
            self.status = "Build before exporting WASM".into();
            cx.notify();
            return;
        };
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("WebAssembly", &["wasm"])
            .set_file_name("source.wasm")
            .save_file()
        {
            match std::fs::write(&path, &artifact.bytes) {
                Ok(()) => self.status = format!("Exported {}", path.display()),
                Err(error) => self.status = format!("WASM export failed: {error}"),
            }
        }
        cx.notify();
    }

    fn copy_c(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(source) = &self.c_output {
            cx.write_to_clipboard(ClipboardItem::new_string(source.clone()));
            self.status = "Copied generated C".into();
        } else {
            self.status = "Build before copying C".into();
        }
        cx.notify();
    }

    fn copy_wat(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(wasm) = &self.artifact {
            cx.write_to_clipboard(ClipboardItem::new_string(wasm.wat.clone()));
            self.status = "Copied generated WAT".into();
        } else {
            self.status = "Build before copying WAT".into();
        }
        cx.notify();
    }

    fn copy_source(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(
            self.editor.read(cx).value().to_string(),
        ));
        self.status = "Copied CAPL source".into();
        cx.notify();
    }

    fn open_trace(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CAN traces", &["asc", "log", "txt"])
            .pick_file()
        else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match AscReader.read(&text) {
                Ok(trace) => {
                    self.trace = trace;
                    self.trace_cursor = 0;
                    self.trace_path = Some(path.clone());
                    self.refresh_trace_panel(cx);
                    self.trace_panel.update(cx, |panel, cx| {
                        panel.set_body(
                            format!(
                                "Loaded {} event(s) from {}\nClick Replay sample to dispatch the next frame.\n\n{}",
                                self.trace.len(), path.display(),
                                trace_summary(&self.trace)
                            ),
                            cx,
                        )
                    });
                    self.status = format!(
                        "Loaded {} trace event(s) from {}",
                        self.trace.len(),
                        path.display()
                    );
                    self.refresh_project_panel(cx);
                }
                Err(error) => {
                    self.status = format!("Could not parse trace {}: {error}", path.display())
                }
            },
            Err(error) => self.status = format!("Could not read trace {}: {error}", path.display()),
        }
        cx.notify();
    }

    fn refresh_project_panel(&mut self, cx: &mut Context<Self>) {
        let mut lines = Vec::new();
        if let Some(path) = &self.project_path {
            lines.push(path.display().to_string());
        }
        if let Some(source) = &self.source_path {
            lines.push(format!(
                "  ◇ {}",
                source.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        if let Some(trace) = &self.trace_path {
            lines.push(format!(
                "  ▤ {}",
                trace.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        if let Some(database) = &self.database_path {
            lines.push(format!(
                "  ◈ {}",
                database.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
        let body = if lines.is_empty() {
            "No project open".into()
        } else {
            lines.join("\n")
        };
        self.project_panel
            .update(cx, |panel, cx| panel.set_body(body, cx));
    }

    fn refresh_runtime_panel(&mut self, cx: &mut Context<Self>) {
        let output_snapshot = self
            .runtime
            .as_ref()
            .map(|runtime| runtime.output())
            .or(self.runtime_history.as_ref());
        let virtual_time_us = self
            .runtime
            .as_ref()
            .map_or(self.last_virtual_time_us, |runtime| runtime.now_us());
        let pending_timers = self
            .runtime
            .as_ref()
            .map(|runtime| runtime.pending_timers());
        let body = output_snapshot.map(|output| {
                let writes = if output.writes.is_empty() {
                    "(no write calls)".into()
                } else {
                    output.writes.join("\n")
                };
                let frames = if output.frames.is_empty() {
                    "(no transmitted frames)".into()
                } else {
                    output
                        .frames
                        .iter()
                        .map(|frame| {
                            format!(
                                "{} us  {}  0x{:X} [{}]",
                                frame.timestamp_us,
                                if matches!(frame.direction, Direction::Tx) {
                                    "Tx"
                                } else {
                                    "Rx"
                                },
                                frame.id,
                                frame
                                    .data
                                    .iter()
                                    .map(|byte| format!("{:02X}", byte))
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                let timers = if pending_timers.is_none_or(|timers| timers.is_empty()) {
                    "(none)".into()
                } else {
                    pending_timers.unwrap().iter().map(|timer| format!("{}  due {} ms{}", timer.name, timer.at_us / 1000, if timer.cyclic { "  cyclic" } else { "" })).collect::<Vec<_>>().join("\n")
                };
                format!("Virtual time: {} ms\n\nwrite():\n{writes}\n\nBus frames:\n{frames}\n\nPending timers:\n{timers}", virtual_time_us / 1000)
            })
            .unwrap_or_else(|| "No simulation output yet.".into());
        self.runtime_panel
            .update(cx, |panel, cx| panel.set_body(body, cx));
        self.refresh_console_panel(cx);
        self.refresh_bus_trace_panel(cx);
    }

    fn refresh_trace_panel(&mut self, cx: &mut Context<Self>) {
        let total_frames = self
            .trace
            .iter()
            .filter(|event| matches!(event, TraceEvent::Frame(_)))
            .count();
        let replayed_frames = self
            .trace
            .iter()
            .take(self.trace_cursor)
            .filter(|event| matches!(event, TraceEvent::Frame(_)))
            .count();
        let header = format!(
            "Trace Replay\nframes: {replayed_frames}/{total_frames}  |  cursor: {}",
            self.trace_cursor
        );
        let body = self
            .trace
            .iter()
            .map(|event| match event {
                TraceEvent::Frame(frame) => {
                    let direction = if matches!(frame.direction, Direction::Tx) {
                        "Tx"
                    } else {
                        "Rx"
                    };
                    let data = frame
                        .data
                        .iter()
                        .map(|byte| format!("{byte:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let mut row = format!(
                        "{:>12} us  CH{} {direction}  0x{:X}  [{data}]",
                        frame.timestamp_us, frame.channel, frame.id
                    );
                    if let Some(database) = &self.database {
                        if let Some(values) = database.decode(frame) {
                            for value in values {
                                row.push_str(&format!("\n    {} = {}", value.name, value.physical));
                            }
                        }
                    }
                    row
                }
                TraceEvent::Comment(comment) => format!("; {comment}"),
                TraceEvent::Error { line, message } => format!("line {line}: {message}"),
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.trace_panel.update(cx, |panel, cx| {
            panel.set_body(
                if body.is_empty() {
                    "No trace events loaded.".into()
                } else {
                    format!("{header}\n\n{body}")
                },
                cx,
            )
        });
    }
}

fn format_runtime_event(event: &RuntimeEvent) -> String {
    format!(
        "{:>8} ms  {:<10} {}",
        event.timestamp_us / 1000,
        event.kind,
        event.text
    )
}
impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let virtual_time_us = self
            .runtime
            .as_ref()
            .map_or(self.last_virtual_time_us, |runtime| runtime.now_us());
        let pending_timers = self.pending_timer_count();
        self.simulation_panel.update(cx, |panel, _| {
            panel.set_runtime_state(virtual_time_us, pending_timers)
        });
        let status = self.status.clone();
        div()
            .size_full()
            .v_flex()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &NewProject, window, cx| this.new_project(window, cx)))
            .on_action(
                cx.listener(|this, _: &OpenProject, window, cx| this.open_project(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SaveProject, window, cx| this.save_project(window, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenTrace, window, cx| this.open_trace(window, cx)))
            .on_action(
                cx.listener(|this, _: &OpenDatabase, window, cx| this.open_database(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &StartSimulation, window, cx| this.start_runtime(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &StopSimulation, window, cx| this.stop_runtime(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ResetSimulation, window, cx| this.reset_runtime(window, cx)),
            )
            .on_action(cx.listener(|this, _: &Advance100ms, _, cx| this.advance_time(100_000, cx)))
            .on_action(cx.listener(|this, _: &Advance1s, _, cx| this.advance_time(1_000_000, cx)))
            .on_action(cx.listener(|this, _: &InjectFrame, window, cx| this.inject_sample(window, cx)))
            .on_action(cx.listener(|this, _: &InjectKey, window, cx| this.inject_key(window, cx)))
            .on_action(cx.listener(|this, _: &SetSysvar, window, cx| this.trigger_sysvar(window, cx)))
            .on_action(cx.listener(|this, _: &SetEnvvar, window, cx| this.trigger_envvar(window, cx)))
            .on_action(cx.listener(|this, _: &InjectErrorFrame, window, cx| this.inject_bus_event("errorFrame", window, cx)))
            .on_action(cx.listener(|this, _: &InjectBusOff, window, cx| this.inject_bus_event("busOff", window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSimulationPanel, window, cx| {
                this.select_output_panel(this.simulation_panel.clone(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ExportAst, window, cx| this.export_ast(window, cx)))
            .on_action(cx.listener(|this, _: &ExportC, window, cx| this.export_c(window, cx)))
            .on_action(cx.listener(|this, _: &ExportWasm, window, cx| this.export_wasm(window, cx)))
            .on_action(cx.listener(|this, _: &CopyC, window, cx| this.copy_c(window, cx)))
            .on_action(cx.listener(|this, _: &CopyWat, window, cx| this.copy_wat(window, cx)))
            .on_action(cx.listener(|this, _: &CopySource, window, cx| this.copy_source(window, cx)))
            .on_action(
                cx.listener(|this, _: &ReplaySample, window, cx| this.replay_sample(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ReplayTrace, window, cx| this.replay_trace(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ClearRuntime, window, cx| {
                this.clear_runtime_output(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleRuntimePanel, window, cx| {
                this.select_output_panel(this.runtime_panel.clone(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleTracePanel, window, cx| {
                this.select_output_panel(this.trace_panel.clone(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleDatabasePanel, window, cx| {
                this.select_output_panel(this.database_panel.clone(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleDiagnosticsPanel, window, cx| {
                this.select_output_panel(this.diagnostics.clone(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleConsolePanel, window, cx| {
                this.select_output_panel(this.console_panel.clone(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleBusTracePanel, window, cx| {
                this.select_output_panel(this.bus_trace_panel.clone(), window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleArchitecturePanel, window, cx| {
                    this.select_output_panel(this.architecture_panel.clone(), window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ShowArchitecture, window, cx| {
                this.show_architecture(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleAstView, window, cx| this.toggle_ast_view(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ReplayNext, window, cx| this.replay_next(window, cx)))
            .on_action(cx.listener(|this, _: &ResetTrace, window, cx| this.reset_trace(window, cx)))
            .on_action(cx.listener(|this, _: &LoadExampleMinimal, window, cx| {
                this.load_example("minimal", window, cx)
            }))
            .on_action(cx.listener(|this, _: &LoadExampleTrace, window, cx| {
                this.load_example("trace", window, cx)
            }))
            .on_action(cx.listener(|this, _: &LoadExampleEvents, window, cx| {
                this.load_example("events", window, cx)
            }))
            .on_action(cx.listener(|this, _: &LoadExampleGateway, window, cx| {
                this.load_example("gateway", window, cx)
            }))
            .on_action(cx.listener(|this, _: &LoadExampleState, window, cx| {
                this.load_example("state", window, cx)
            }))
            .on_action(cx.listener(|this, _: &LoadExampleTypes, window, cx| {
                this.load_example("types", window, cx)
            }))
            .on_action(cx.listener(|this, _: &LoadTraceDemo, _, cx| this.load_trace_demo(cx)))
            .child(
                h_flex()
                    .id("titlebar")
                    .h_12()
                    .px_3()
                    .gap_3()
                    .bg(cx.theme().title_bar)
                    .border_b_1()
                    .border_color(cx.theme().title_bar_border)
                    .child(
                        div()
                            .id("brand-mark")
                            .w_6()
                            .h_6()
                            .rounded(px(7.))
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .items_center()
                            .justify_center()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("C"),
                    )
                    .child(
                        div()
                            .v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("CAPL Playground"),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Vector CAPL  ·  Rust compiler  ·  virtual measurement"),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("build")
                            .small()
                            .label("Build")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.compile_source(window, cx)
                            })),
                    )
                    .child(
                        Button::new("run")
                            .small()
                            .success()
                            .label("Run")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start_runtime(window, cx)
                            })),
                    )
                    .child(Button::new("stop").small().danger().label("Stop").on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.stop_runtime(window, cx)
                        }),
                    ))
                    .child(
                        Button::new("reset-top")
                            .small()
                            .ghost()
                            .label("Reset")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.reset_runtime(window, cx)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .id("menu-strip")
                    .h_8()
                    .px_3()
                    .gap_1()
                    .bg(cx.theme().tab_bar)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .w_5()
                            .h_5()
                            .mr_1()
                            .rounded(px(4.))
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .items_center()
                            .justify_center()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_size(px(10.))
                            .child("C"),
                    )
                    .child(
                        Button::new("file-menu")
                            .small()
                            .ghost()
                            .label("File")
                            .dropdown_menu(|menu, _, _| {
                                menu.menu("New Project", Box::new(NewProject))
                                    .menu("Open Project", Box::new(OpenProject))
                                    .menu("Save Project", Box::new(SaveProject))
                                    .separator()
                                    .menu("Open Trace", Box::new(OpenTrace))
                                    .menu("Open DBC", Box::new(OpenDatabase))
                                    .separator()
                                    .menu("Example: Minimal", Box::new(LoadExampleMinimal))
                                    .menu("Example: Trace replay", Box::new(LoadExampleTrace))
                                    .menu("Example: Events", Box::new(LoadExampleEvents))
                                    .menu("Example: Gateway", Box::new(LoadExampleGateway))
                                    .menu("Example: State machine", Box::new(LoadExampleState))
                                    .menu("Example: Types", Box::new(LoadExampleTypes))
                                    .menu("Load trace demo", Box::new(LoadTraceDemo))
                                    .separator()
                                    .menu("Export AST JSON", Box::new(ExportAst))
                                    .menu("Export C", Box::new(ExportC))
                                    .menu("Export WASM", Box::new(ExportWasm))
                            }),
                    )
                    .child(
                        Button::new("edit-menu")
                            .small()
                            .ghost()
                            .label("Edit")
                            .dropdown_menu(|menu, _, _| {
                                menu.menu("Undo", Box::new(Undo))
                                    .menu("Redo", Box::new(Redo))
                                    .separator()
                                    .menu("Cut", Box::new(Cut))
                                    .menu("Copy", Box::new(Copy))
                                    .menu("Paste", Box::new(Paste))
                                    .menu("Select All", Box::new(SelectAll))
                                    .separator()
                                    .menu("Copy CAPL source", Box::new(CopySource))
                            }),
                    )
                    .child(
                        Button::new("run-menu")
                            .small()
                            .ghost()
                            .label("Run")
                            .dropdown_menu(|menu, _, _| {
                                menu.menu("Run", Box::new(StartSimulation))
                                    .menu("Stop", Box::new(StopSimulation))
                                    .menu("Reset", Box::new(ResetSimulation))
                                    .menu("Replay trace", Box::new(ReplayTrace))
                                    .menu("Clear runtime output", Box::new(ClearRuntime))
                                    .separator()
                                    .menu("Show Runtime", Box::new(ToggleRuntimePanel))
                                    .menu("Show Console", Box::new(ToggleConsolePanel))
                                    .menu("Show Trace Replay", Box::new(ToggleTracePanel))
                                    .menu("Show Bus Trace", Box::new(ToggleBusTracePanel))
                                    .menu("Show Database", Box::new(ToggleDatabasePanel))
                                    .menu("Show Diagnostics", Box::new(ToggleDiagnosticsPanel))
                                    .menu("Show Architecture", Box::new(ToggleArchitecturePanel))
                                    .menu("Toggle AST tree/raw", Box::new(ToggleAstView))
                                    .menu("Refresh Architecture", Box::new(ShowArchitecture))
                                    .menu("Replay next frame", Box::new(ReplayNext))
                                    .menu("Reset trace cursor", Box::new(ResetTrace))
                            }),
                    )
                    .child(
                        Button::new("open-trace")
                            .small()
                            .ghost()
                            .label("Open Trace")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.open_trace(window, cx)
                            })),
                    )
                    .child(div().flex_1()),
            )
            .child(
                h_flex()
                    .id("workspace-shell")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("activity-bar")
                            .test_support()
                            .w(px(48.))
                            .flex_shrink_0()
                            .h_full()
                            .v_flex()
                            .items_center()
                            .gap_2()
                            .py_2()
                            .bg(cx.theme().title_bar)
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .w_8()
                                    .h_8()
                                    .rounded(px(6.))
                                    .bg(cx.theme().primary)
                                    .text_color(cx.theme().primary_foreground)
                                    .items_center()
                                    .justify_center()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("C"),
                            )
                            .child(
                                Button::new("activity-explorer")
                                    .small()
                                    .ghost()
                                    .label("▤")
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(OpenProject), cx)
                                    }),
                            )
                            .child(
                                Button::new("activity-trace")
                                    .small()
                                    .ghost()
                                    .label("⌁")
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(ToggleTracePanel), cx)
                                    }),
                            )
                            .child(
                                Button::new("activity-diagnostics")
                                    .small()
                                    .ghost()
                                    .label("!")
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(ToggleDiagnosticsPanel), cx)
                                    }),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("activity-architecture")
                                    .small()
                                    .ghost()
                                    .label("⚙")
                                    .on_click(|_, window, cx| {
                                        window
                                            .dispatch_action(Box::new(ToggleArchitecturePanel), cx)
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("main-dock")
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_hidden()
                            .child(self.dock.clone()),
                    ),
            )
            .child(
                h_flex()
                    .id("editor-status")
                    .h_0()
                    .overflow_hidden()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .bg(cx.theme().muted)
                    .text_color(cx.theme().muted_foreground)
                    .child("CAPL source.can")
                    .child("Ln 1, Col 1")
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary)
                            .text_color(cx.theme().secondary_foreground)
                            .child(status.clone()),
                    )
                    .child(div().flex_1())
                    .child("A-")
                    .child("A+")
                    .child("UTF-8"),
            )
            .child(
                div()
                    .id("legacy-simulation-controls")
                    .test_support()
                    .h_0()
                    .overflow_hidden()
                    .v_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .bg(cx.theme().status_bar)
                    .border_t_1()
                    .border_color(cx.theme().status_bar_border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(cx.theme().muted_foreground)
                                    .child("VIRTUAL TIME"),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().secondary)
                                    .text_color(cx.theme().secondary_foreground)
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(px(11.))
                                    .child(format!(
                                        "{:0.3} s",
                                        self.last_virtual_time_us as f64 / 1_000_000.
                                    )),
                            )
                            .child(
                                Button::new("advance-100")
                                    .small()
                                    .ghost()
                                    .label("+100 ms")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.advance_time(100_000, cx)
                                    })),
                            )
                            .child(
                                Button::new("advance-1000")
                                    .small()
                                    .ghost()
                                    .label("+1 s")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.advance_time(1_000_000, cx)
                                    })),
                            )
                            .child(
                                Button::new("reset-simulation")
                                    .small()
                                    .ghost()
                                    .label("Reset")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.reset_runtime(window, cx)
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "PENDING TIMERS  {}",
                                        self.pending_timer_count()
                                    )),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(cx.theme().muted_foreground)
                                    .child("CAN FRAME"),
                            )
                            .child(Input::new(&self.frame_id_input).small().w(px(78.)))
                            .child(Input::new(&self.frame_data_input).small().w(px(160.)))
                            .child(
                                Button::new("inject")
                                    .small()
                                    .primary()
                                    .label("Inject")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.inject_sample(window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(cx.theme().muted_foreground)
                                    .child("KEY"),
                            )
                            .child(Input::new(&self.key_input).small().w(px(42.)))
                            .child(
                                Button::new("press-key")
                                    .small()
                                    .ghost()
                                    .label("Press")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.inject_key(window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(cx.theme().muted_foreground)
                                    .child("SYSVAR"),
                            )
                            .child(Input::new(&self.sysvar_name_input).small().w(px(120.)))
                            .child(Input::new(&self.sysvar_value_input).small().w(px(48.)))
                            .child(
                                Button::new("trigger-sysvar")
                                    .small()
                                    .ghost()
                                    .label("Set")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.trigger_sysvar(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("trigger-envvar")
                                    .small()
                                    .ghost()
                                    .label("Env")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.trigger_envvar(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("error-frame")
                                    .small()
                                    .ghost()
                                    .label("errorFrame")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.inject_bus_event("errorFrame", window, cx)
                                    })),
                            )
                            .child(
                                Button::new("bus-off")
                                    .small()
                                    .ghost()
                                    .label("busOff")
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.inject_bus_event("busOff", window, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .id("status-bar")
                    .test_support()
                    .h(px(26.))
                    .flex_shrink_0()
                    .gap_2()
                    .px_3()
                    .items_center()
                    .bg(cx.theme().muted)
                    .text_color(cx.theme().muted_foreground)
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child("CAPL source.can")
                    .child("Ln 1, Col 1")
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().secondary)
                            .text_color(cx.theme().secondary_foreground)
                            .child(status),
                    )
                    .child(div().flex_1())
                    .child("A-")
                    .child("A+")
                    .child("UTF-8"),
            )
    }
}

fn format_frame_row(
    timestamp_us: u64,
    channel: u16,
    direction: Direction,
    id: u32,
    data: &[u8],
) -> String {
    let direction = if matches!(direction, Direction::Tx) {
        "Tx"
    } else {
        "Rx"
    };
    let data = data
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{timestamp_us:>12} us  CH{channel} {direction}  0x{id:X}  [{data}]")
}

fn trace_summary(trace: &[TraceEvent]) -> String {
    trace
        .iter()
        .map(|event| match event {
            TraceEvent::Frame(frame) => format_frame_row(
                frame.timestamp_us,
                frame.channel,
                frame.direction,
                frame.id,
                &frame.data,
            ),
            TraceEvent::Comment(comment) => format!("; {comment}"),
            TraceEvent::Error { line, message } => format!("line {line}: {message}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn replay_handler(decl: &Decl) -> Option<ReplayHandler> {
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

pub fn set_playground_theme(cx: &mut App) {
    use gpui_kit::component::{Theme, ThemeMode};

    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        theme.background = gpui::rgb(0x181513).into();
        theme.foreground = gpui::rgb(0xeee4d8).into();
        theme.title_bar = gpui::rgb(0x171311).into();
        theme.title_bar_border = gpui::rgb(0x342a24).into();
        theme.border = gpui::rgb(0x342a24).into();
        theme.input = gpui::rgb(0x2a211d).into();
        theme.primary = gpui::rgb(0xe27a2b).into();
        theme.primary_foreground = gpui::rgb(0x21150e).into();
        theme.primary_hover = gpui::rgb(0xf08b38).into();
        theme.accent = gpui::rgb(0x30251e).into();
        theme.accent_foreground = gpui::rgb(0xf2d8bd).into();
        theme.secondary = gpui::rgb(0x28201c).into();
        theme.secondary_foreground = gpui::rgb(0xd8c9ba).into();
        theme.muted = gpui::rgb(0x211b18).into();
        theme.muted_foreground = gpui::rgb(0x9d8d7d).into();
        theme.sidebar = gpui::rgb(0x1d1815).into();
        theme.sidebar_foreground = gpui::rgb(0xcfc0b1).into();
        theme.sidebar_border = gpui::rgb(0x342a24).into();
        theme.tab_bar = gpui::rgb(0x1d1815).into();
        theme.tab = gpui::rgb(0x211b18).into();
        theme.tab_active = gpui::rgb(0x30251e).into();
        theme.tab_foreground = gpui::rgb(0x9d8d7d).into();
        theme.tab_active_foreground = gpui::rgb(0xf2e6d9).into();
        theme.status_bar = gpui::rgb(0x1d1815).into();
        theme.status_bar_border = gpui::rgb(0x342a24).into();
        theme.success = gpui::rgb(0x159d83).into();
        theme.success_foreground = gpui::rgb(0xf0fff9).into();
        theme.danger = gpui::rgb(0xb94b4b).into();
        theme.danger_foreground = gpui::rgb(0xfff3f3).into();
    });
}

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        set_playground_theme(cx);
        let options = WindowOptions {
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("CAPL Playground".into()),
                // Keep the native Windows chrome so minimize, maximize and close
                // remain available to the desktop build.
                appears_transparent: false,
                traffic_light_position: None,
            }),
            window_min_size: Some(gpui::size(px(960.), px(640.))),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::new(window, cx))
        })
        .expect("failed to open CAPL Playground window");
    });
}
