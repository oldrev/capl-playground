//! Native CAPL Playground shell.
//!
//! The UI intentionally talks to the shared Rust model/runtime crates.  It is
//! therefore also a clean home for a future `capl-web` target without bringing
//! React or TypeScript back into the application core.

use gpui::{
    div, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Window,
};
use gpui_base::dock::PanelEvent;
use gpui_base::StyledExt;
use gpui_kit::component::{
    dock::{panel_handle, DockArea, DockLayout, DockPlacement, DockSkin, Panel},
    input::{Editor, EditorState},
    Root,
};
use gpui_kit::*;

const DOCK_ID: &str = "capl-workspace";

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
        let center = DockLayout::tabs().panel_view(panel_handle(editor), cx);
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
        Self { dock }
    }
}
impl Render for Workspace {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.dock.clone()
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
