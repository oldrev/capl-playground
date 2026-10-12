use capl_desktop::Workspace;
use gpui_kit::base::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{px, size};
use gpui_kit::{AppContext, TestAppContext};

#[gpui_kit::test]
fn workspace_regions_fill_the_available_height(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        capl_desktop::set_playground_theme(cx);
    });
    for (width, height) in [(960., 640.), (1536., 1096.)] {
        let (window, _) = cx.update(|cx| {
            gpui_kit::open_window(
                gpui_kit::WindowOptions {
                    window_bounds: Some(gpui_kit::WindowBounds::Windowed(
                        gpui_kit::Bounds::centered(None, size(px(width), px(height)), cx),
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
            let shell = window.find("workspace-shell").bounds();
            let status_bar = window.find("status-bar").bounds();
            let dock = window.find("main-dock").bounds();
            let explorer = window.find("project-files").bounds();
            let editor = window.find("source-editor").bounds();
            let simulation = window.find("simulation-controls").bounds();
            assert_eq!(dock.top(), shell.top());
            assert_eq!(dock.bottom(), shell.bottom());
            assert!(explorer.top() < shell.top() + px(40.));
            assert_eq!(explorer.bottom(), shell.bottom());
            assert!(editor.top() < shell.top() + px(40.));
            assert!(editor.size.height > px(150.));
            assert!(editor.bottom() <= simulation.top());
            assert_eq!(editor.left(), simulation.left());
            assert_eq!(simulation.bottom(), shell.bottom());
            assert_eq!(status_bar.bottom(), window.visual_viewport_bounds().bottom());
            assert!(explorer.right() <= editor.left());
        })
        .unwrap();
    }
}

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
            "open-project-empty",
            "new-project-empty",
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
        window.click("stop", cx);
        assert_eq!(workspace.read(cx).status(), "Stopped");
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
        assert_eq!(workspace.read(cx).trace_cursor(), 0);
    })
    .unwrap();
}
