use capl_desktop::Workspace;
use gpui_kit::base::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{px, size};
use gpui_kit::{AppContext, TestAppContext};

#[gpui_kit::test]
fn menus_build_run_inject_and_stop_work_in_a_real_gpui_test_window(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        capl_desktop::set_playground_theme(cx);
        assert!(gpui_kit::component::Theme::global(cx).is_dark());
    });
    let (window, workspace) = cx.update(|cx| {
        gpui_kit::open_window(
            gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(
                    gpui_kit::Bounds::centered(None, size(px(900.), px(700.)), cx),
                )),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Workspace::new(window, cx)),
        )
        .expect("open CAPL workspace")
    });
    let window = window.downcast::<Root>().expect("Kit window Root");

    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("simulation-controls").bounds().size.width <= window.viewport_size().width
        );
        for id in [
            "file-menu",
            "edit-menu",
            "run-menu",
            "build",
            "run",
            "stop",
            "open-trace",
            "examples",
            "replay-sample",
            "replay-trace",
            "replay-next",
            "reset-trace",
            "clear-runtime",
            "open-project-empty",
            "new-project-empty",
            "inject",
            "trigger-sysvar",
        ] {
            assert!(window.find(id).visible(), "missing visible control {id}");
        }

        window.click("file-menu", cx);
        assert!(
            window.find("popup-menu").visible(),
            "File menu did not open"
        );
        window.press("escape", cx);

        window.click("build", cx);
        assert!(
            workspace.read(cx).status().starts_with("Build succeeded"),
            "{}",
            workspace.read(cx).status()
        );
        window.click("run-menu", cx);
        assert!(window.find("run-menu").visible(), "Run menu did not open");
        window.press("escape", cx);
        window.click("run", cx);
        assert_eq!(workspace.read(cx).status(), "Running");
        assert_eq!(workspace.read(cx).pending_timer_count(), 1);
        window.click("advance-100", cx);
        assert!(workspace.read(cx).status().contains("fired 1 timer"));
        window.click("inject", cx);
        assert!(workspace.read(cx).status().contains("invoked 1 handler"));
        window.click("clear-runtime", cx);
        assert_eq!(workspace.read(cx).status(), "Runtime output cleared");
        window.click("press-key", cx);
        assert!(workspace.read(cx).status().contains("invoked 1 handler"));
        window.click("trigger-sysvar", cx);
        assert!(workspace.read(cx).status().contains("invoked 1 handler"));
        window.click("trigger-envvar", cx);
        assert!(workspace.read(cx).status().contains("invoked 1 handler"));
        window.click("bus-off", cx);
        assert!(workspace.read(cx).status().contains("invoked 1 handler"));
        window.click("error-frame", cx);
        assert!(workspace.read(cx).status().contains("invoked 1 handler"));
        window.click("replay-sample", cx);
        assert!(workspace.read(cx).status().contains("Replayed 0x100"));
        window.click("stop", cx);
        assert_eq!(workspace.read(cx).status(), "Stopped");
        assert!(workspace.read(cx).has_stopped_write("received 0x100"));
        assert!(workspace.read(cx).has_stopped_write("timer at 100 ms"));
        assert!(workspace.read(cx).has_stopped_write("key a"));
        assert!(workspace.read(cx).has_stopped_write("busOff"));
        assert!(workspace.read(cx).has_stopped_write("errorFrame"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn asc_trace_is_loaded_and_replayed_against_virtual_time(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        capl_desktop::set_playground_theme(cx);
    });
    let (window, workspace) = cx.update(|cx| {
        gpui_kit::open_window(
            gpui_kit::WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(
                    gpui_kit::Bounds::centered(None, size(px(900.), px(700.)), cx),
                )),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Workspace::new(window, cx)),
        )
        .expect("open CAPL workspace")
    });
    let window = window.downcast::<Root>().expect("Kit window Root");
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        workspace.update(cx, |workspace, cx| workspace.load_trace_demo(cx));
        assert_eq!(workspace.read(cx).trace_event_count(), 2);
        window.click("run", cx);
        window.click("replay-trace", cx);
        assert!(workspace.read(cx).status().contains("Replayed 2 frame(s)"));
        assert_eq!(workspace.read(cx).trace_cursor(), 2);
        window.click("stop", cx);
        assert!(workspace.read(cx).has_stopped_write("received 0x100"));
    })
    .unwrap();
}
