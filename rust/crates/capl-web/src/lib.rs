#![cfg(target_family = "wasm")]

use gpui::{div, Context, IntoElement, Render, Window};
use gpui_base::StyledExt;
use gpui_kit::*;
use wasm_bindgen::prelude::*;

struct WebWorkspace;

impl Render for WebWorkspace {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_6()
            .v_flex()
            .gap_3()
            .child("CAPL Playground")
            .child("GPUI Kit WebAssembly GUI is running.")
            .child("Use the native build for filesystem-backed projects; browser projects use localStorage.")
    }
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    gpui_platform::web_init();
    gpui_platform::single_threaded_web().run(|cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
            cx.new(|_| WebWorkspace)
        })
        .expect("failed to open GPUI web window");
    });
    Ok(())
}
