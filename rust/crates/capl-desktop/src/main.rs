//! Native CAPL Playground shell.
//!
//! The UI intentionally talks to the shared Rust model/runtime crates.  It is
//! therefore also a clean home for a future `capl-web` target without bringing
//! React or TypeScript back into the application core.

use capl_ast::{Decl, EventSpec};
use capl_driver::{compile, BackendId, Options, WasmArtifact};
use capl_model::{BusFrame, Direction};
use capl_project::{decode, encode, ProjectManifest};
use capl_runtime::{ReplayHandler, WasmReplay};
use capl_trace_asc::AscReader;
use capl_trace_core::{TraceEvent, TraceReader};
use gpui::{
    div, App, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    Render, Window,
};
use gpui_base::dock::PanelEvent;
use gpui_base::StyledExt;
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    dock::{panel_handle, DockArea, DockLayout, DockPlacement, DockSkin, Panel},
    h_flex,
    input::{Editor, EditorState},
    menu::DropdownMenu as _,
    Root,
};
use gpui_kit::*;

const DOCK_ID: &str = "capl-workspace";

actions!(
    capl,
    [
        NewProject,
        OpenProject,
        SaveProject,
        OpenTrace,
        StartSimulation,
        StopSimulation,
        ReplaySample,
        ToggleTrace
    ]
);

struct TextPanel {
    name: &'static str,
    title: &'static str,
    body: String,
    focus: FocusHandle,
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
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_4()
            .v_flex()
            .gap_2()
            .child(self.body.clone())
    }
}

struct EditorPanel {
    editor: Entity<EditorState>,
    focus: FocusHandle,
}

impl EditorPanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .default_value("variables {\n  msTimer replayTimer;\n}\n\non start {\n  write(\"ready for trace replay\");\n}\n\non message 0x100 {\n  write(\"received 0x100\");\n}\n")
                .line_number(true)
                .indent_guides(true)
        });
        let focus = editor.read(cx).focus_handle(cx);
        Self { editor, focus }
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
        Editor::new(&self.editor).size_full().into_any_element()
    }
}

struct Workspace {
    dock: Entity<DockArea>,
    editor: Entity<EditorState>,
    artifact: Option<WasmArtifact>,
    handlers: Vec<ReplayHandler>,
    runtime: Option<WasmReplay>,
    trace: Vec<TraceEvent>,
    trace_cursor: usize,
    diagnostics: Entity<TextPanel>,
    trace_panel: Entity<TextPanel>,
    project_panel: Entity<TextPanel>,
    ast_panel: Entity<TextPanel>,
    c_panel: Entity<TextPanel>,
    wasm_panel: Entity<TextPanel>,
    runtime_panel: Entity<TextPanel>,
    status: String,
    project_path: Option<std::path::PathBuf>,
    source_path: Option<std::path::PathBuf>,
    trace_path: Option<std::path::PathBuf>,
}
impl Workspace {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (dock, _skin) = DockSkin::dock_area(DOCK_ID, Some(1), window, cx);
        let source = cx.new(|cx| {
            TextPanel::new(
                "project-files",
                "Project",
                "Open a .caplproj project or create a new one.",
                cx,
            )
        });
        let editor = cx.new(|cx| EditorPanel::new(window, cx));
        let trace = cx.new(|cx| TextPanel::new("trace-replay", "Trace Replay", "No trace loaded. Use Run → Open trace or place traffic.asc in the project directory.", cx));
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
        let editor_state = editor.read(cx).editor.clone();
        let center = DockLayout::tabs().panel_view(panel_handle(editor.clone()), cx);
        let left = DockLayout::tabs().panel_view(panel_handle(source.clone()), cx);
        let bottom = DockLayout::tabs()
            .panel_view(panel_handle(diagnostics.clone()), cx)
            .panel_view(panel_handle(trace.clone()), cx)
            .panel_view(panel_handle(ast_panel.clone()), cx)
            .panel_view(panel_handle(c_panel.clone()), cx)
            .panel_view(panel_handle(wasm_panel.clone()), cx)
            .panel_view(panel_handle(runtime_panel.clone()), cx);
        dock.update(cx, |area, cx| {
            area.set_center(center, window, cx);
            area.set_dock(DockPlacement::Left, left, window, cx);
            area.set_dock_size(DockPlacement::Left, px(280.), window, cx);
            area.set_dock(DockPlacement::Bottom, bottom, window, cx);
            area.set_dock_size(DockPlacement::Bottom, px(240.), window, cx);
            area.set_dock_collapsible(DockPlacement::Left, true, window, cx);
            area.set_dock_collapsible(DockPlacement::Bottom, true, window, cx);
        });
        Self {
            dock,
            editor: editor_state,
            artifact: None,
            handlers: Vec::new(),
            runtime: None,
            trace: Vec::new(),
            trace_cursor: 0,
            diagnostics,
            trace_panel: trace,
            project_panel: source,
            ast_panel,
            c_panel,
            wasm_panel,
            runtime_panel,
            status: "Ready".into(),
            project_path: None,
            source_path: None,
            trace_path: None,
        }
    }

    fn compile_source(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let source = self.editor.read(cx).value().to_string();
        let out = compile(
            &source,
            Options {
                backends: vec![BackendId::Wasm, BackendId::C, BackendId::Ast],
                ..Options::default()
            },
        );
        if !out.ok {
            let body = out
                .diagnostics
                .iter()
                .map(|diag| diag.message.clone())
                .collect::<Vec<_>>()
                .join("\n");
            self.diagnostics
                .update(cx, |panel, cx| panel.set_body(body, cx));
            self.status = format!("Build failed: {} diagnostic(s)", out.diagnostics.len());
            self.artifact = None;
            self.handlers.clear();
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
        self.artifact = out.wasm;
        self.runtime = None;
        self.ast_panel
            .update(cx, |panel, cx| panel.set_body(out.ast_json.clone(), cx));
        self.c_panel.update(cx, |panel, cx| {
            panel.set_body(out.c.clone().unwrap_or_else(|| "No C artifact".into()), cx)
        });
        self.wasm_panel.update(cx, |panel, cx| {
            panel.set_body(
                self.artifact
                    .as_ref()
                    .map(|artifact| artifact.wat.clone())
                    .unwrap_or_else(|| "No WASM artifact".into()),
                cx,
            )
        });
        self.diagnostics.update(cx, |panel, cx| {
            panel.set_body(
                format!(
                    "Build succeeded\nhandlers: {}\nC output: {}\nWASM output: {}",
                    self.handlers.len(),
                    out.c.is_some(),
                    self.artifact.is_some()
                ),
                cx,
            )
        });
        self.status = format!("Build succeeded: {} handler(s)", self.handlers.len());
        cx.notify();
    }

    fn start_runtime(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.artifact.is_none() {
            self.compile_source(window, cx);
        }
        let Some(artifact) = self.artifact.as_ref() else {
            return;
        };
        match WasmReplay::new(&artifact.bytes, artifact.msgbuf_addr, self.handlers.clone()) {
            Ok(runtime) => {
                self.runtime = Some(runtime);
                self.status = "Running".into();
                self.refresh_runtime_panel(cx);
            }
            Err(error) => self.status = format!("Runtime error: {error}"),
        }
        cx.notify();
    }

    fn stop_runtime(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.runtime = None;
        self.status = "Stopped".into();
        self.refresh_runtime_panel(cx);
        cx.notify();
    }

    fn replay_sample(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.runtime.is_none() {
            self.start_runtime(window, cx);
        }
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        let frame = self
            .trace
            .iter()
            .skip(self.trace_cursor)
            .find_map(|event| match event {
                TraceEvent::Frame(frame) => Some(frame.clone()),
                _ => None,
            })
            .unwrap_or_else(|| BusFrame::new(100_000, 1, 0x100, Direction::Rx, vec![0x12, 0x34]));
        match runtime.run_frame(&frame) {
            Ok(count) => {
                self.status = format!("Replayed 0x100 at 100 ms; invoked {count} handler(s)")
            }
            Err(error) => self.status = format!("Replay error: {error}"),
        }
        self.trace_cursor = self.trace_cursor.saturating_add(1);
        self.refresh_runtime_panel(cx);
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

    fn open_trace(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CAN traces", &["asc", "log", "txt"])
            .pick_file()
        else {
            return;
        };
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| AscReader.read(&text).ok())
        {
            Some(trace) => {
                self.trace = trace;
                self.trace_cursor = 0;
                self.trace_path = Some(path.clone());
                self.trace_panel.update(cx, |panel, cx| {
                    panel.set_body(
                        format!(
                            "Loaded {} event(s) from {}\nClick Replay sample to dispatch the next frame.",
                            self.trace.len(), path.display()
                        ),
                        cx,
                    )
                });
                self.status = format!(
                    "Loaded {} trace event(s) from {}",
                    self.trace.len(),
                    path.display()
                );
            }
            None => self.status = format!("Could not parse trace {}", path.display()),
        }
        cx.notify();
    }

    fn refresh_project_panel(&mut self, cx: &mut Context<Self>) {
        let body = self
            .project_path
            .as_ref()
            .map(|path| {
                let mut files = vec![path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()];
                if let Some(source) = &self.source_path {
                    files.push(
                        source
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
                if let Some(trace) = &self.trace_path {
                    files.push(
                        trace
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
                format!(
                    "{}\n{}",
                    path.display(),
                    files
                        .into_iter()
                        .map(|name| format!("  {name}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            })
            .unwrap_or_else(|| "No project open\nUse File → New Project or Open Project.".into());
        self.project_panel
            .update(cx, |panel, cx| panel.set_body(body, cx));
    }

    fn refresh_runtime_panel(&mut self, cx: &mut Context<Self>) {
        let body = self
            .runtime
            .as_ref()
            .map(|runtime| {
                let output = runtime.output();
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
                format!("write():\n{writes}\n\nBus frames:\n{frames}")
            })
            .unwrap_or_else(|| "Runtime is stopped.".into());
        self.runtime_panel
            .update(cx, |panel, cx| panel.set_body(body, cx));
    }
}
impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.status.clone();
        div()
            .size_full()
            .v_flex()
            .on_action(cx.listener(|this, _: &NewProject, window, cx| this.new_project(window, cx)))
            .on_action(
                cx.listener(|this, _: &OpenProject, window, cx| this.open_project(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SaveProject, window, cx| this.save_project(window, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenTrace, window, cx| this.open_trace(window, cx)))
            .on_action(
                cx.listener(|this, _: &StartSimulation, window, cx| this.start_runtime(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &StopSimulation, window, cx| this.stop_runtime(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ReplaySample, window, cx| this.replay_sample(window, cx)),
            )
            .child(
                h_flex()
                    .h_10()
                    .px_2()
                    .gap_2()
                    .border_b_1()
                    .child("CAPL Playground")
                    .child(
                        Button::new("file-menu")
                            .ghost()
                            .label("File")
                            .dropdown_menu(|menu, _, _| {
                                menu.menu("New Project", Box::new(NewProject))
                                    .menu("Open Project", Box::new(OpenProject))
                                    .menu("Save Project", Box::new(SaveProject))
                            }),
                    )
                    .child(Button::new("edit-menu").ghost().label("Edit"))
                    .child(Button::new("run-menu").ghost().label("Run").dropdown_menu(
                        |menu, _, _| {
                            menu.menu("Run", Box::new(StartSimulation))
                                .menu("Stop", Box::new(StopSimulation))
                                .menu("Replay sample", Box::new(ReplaySample))
                                .menu("Open trace", Box::new(OpenTrace))
                        },
                    ))
                    .child(
                        Button::new("build")
                            .primary()
                            .label("Build")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.compile_source(window, cx)
                            })),
                    )
                    .child(
                        Button::new("run")
                            .success()
                            .label("Run")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.start_runtime(window, cx)
                            })),
                    )
                    .child(
                        Button::new("stop")
                            .danger()
                            .label("Stop")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.stop_runtime(window, cx)
                            })),
                    )
                    .child(
                        Button::new("replay")
                            .outline()
                            .label("Replay sample")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.replay_sample(window, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(status),
            )
            .child(div().flex_1().child(self.dock.clone()))
    }
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

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| {
                let workspace = cx.new(|cx| Workspace::new(window, cx));
                Root::new(workspace, window, cx)
            })
        })
        .expect("failed to open CAPL Playground window");
    });
}
