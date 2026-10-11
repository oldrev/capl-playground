//! Native CAPL Playground shell.
//!
//! The UI intentionally talks to the shared Rust model/runtime crates.  It is
//! therefore also a clean home for a future `capl-web` target without bringing
//! React or TypeScript back into the application core.

use capl_ast::{Decl, EventSpec};
use capl_driver::{compile, BackendId, Options, WasmArtifact};
use capl_model::{BusFrame, Direction};
use capl_runtime::{ReplayHandler, WasmReplay};
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
        StartSimulation,
        StopSimulation,
        ReplaySample,
        ToggleTrace
    ]
);

struct TextPanel {
    name: &'static str,
    title: &'static str,
    body: &'static str,
    focus: FocusHandle,
}
impl TextPanel {
    fn new(
        name: &'static str,
        title: &'static str,
        body: &'static str,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            name,
            title,
            body,
            focus: cx.focus_handle(),
        }
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
        div().size_full().p_4().v_flex().gap_2().child(self.body)
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
    status: String,
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
        let trace = cx.new(|cx| TextPanel::new("trace-replay", "Trace Replay", "Import Vector ASC or candump text traces, then replay them against CAPL on message handlers.", cx));
        let diagnostics = cx.new(|cx| {
            TextPanel::new(
                "diagnostics",
                "Diagnostics",
                "Compiler diagnostics and runtime events will appear here.",
                cx,
            )
        });
        let editor_state = editor.read(cx).editor.clone();
        let center = DockLayout::tabs().panel_view(panel_handle(editor.clone()), cx);
        let left = DockLayout::tabs().panel_view(panel_handle(source), cx);
        let bottom = DockLayout::tabs()
            .panel_view(panel_handle(diagnostics), cx)
            .panel_view(panel_handle(trace), cx);
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
            status: "Ready".into(),
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
            }
            Err(error) => self.status = format!("Runtime error: {error}"),
        }
        cx.notify();
    }

    fn stop_runtime(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.runtime = None;
        self.status = "Stopped".into();
        cx.notify();
    }

    fn replay_sample(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.runtime.is_none() {
            self.start_runtime(window, cx);
        }
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        let frame = BusFrame::new(100_000, 1, 0x100, Direction::Rx, vec![0x12, 0x34]);
        match runtime.run_frame(&frame) {
            Ok(count) => {
                self.status = format!("Replayed 0x100 at 100 ms; invoked {count} handler(s)")
            }
            Err(error) => self.status = format!("Replay error: {error}"),
        }
        cx.notify();
    }
}
impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.status.clone();
        div()
            .size_full()
            .v_flex()
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
                            .on_click(cx.listener(Self::stop_runtime)),
                    )
                    .child(
                        Button::new("replay")
                            .outline()
                            .label("Replay sample")
                            .on_click(cx.listener(Self::replay_sample)),
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
