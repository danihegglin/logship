mod index;
mod search;
mod theme;
mod view;

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, prelude::*, px, size,
};

use view::{
    CopyLine, FocusSearch, LogView, NextMatch, Open, PrevMatch, Quit, ToggleCase, ToggleFilterMode,
    ToggleFollow, ToggleRegex, ToggleTheme,
};

fn main() {
    let path = std::env::args_os().nth(1).map(PathBuf::from);

    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("secondary-o", Open, None),
            KeyBinding::new("secondary-q", Quit, None),
            KeyBinding::new("secondary-f", FocusSearch, None),
            KeyBinding::new("secondary-c", CopyLine, None),
            KeyBinding::new("secondary-g", NextMatch, None),
            KeyBinding::new("secondary-shift-g", PrevMatch, None),
            KeyBinding::new("secondary-shift-d", ToggleTheme, None),
            KeyBinding::new("secondary-shift-f", ToggleFollow, None),
            KeyBinding::new("secondary-alt-c", ToggleCase, None),
            KeyBinding::new("secondary-alt-r", ToggleRegex, None),
            KeyBinding::new("secondary-alt-f", ToggleFilterMode, None),
        ]);
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.set_menus(vec![
            Menu {
                name: "logship".into(),
                items: vec![MenuItem::action("Quit logship", Quit)],
            },
            Menu {
                name: "File".into(),
                items: vec![MenuItem::action("Open…", Open)],
            },
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::action("Copy Line", CopyLine),
                    MenuItem::separator(),
                    MenuItem::action("Find", FocusSearch),
                    MenuItem::action("Next Match", NextMatch),
                    MenuItem::action("Previous Match", PrevMatch),
                    MenuItem::separator(),
                    MenuItem::action("Toggle Case Sensitive", ToggleCase),
                    MenuItem::action("Toggle Regex", ToggleRegex),
                    MenuItem::action("Toggle Filter / Highlight", ToggleFilterMode),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Follow Tail", ToggleFollow),
                    MenuItem::action("Toggle Light / Dark", ToggleTheme),
                ],
            },
        ]);

        let bounds = Bounds::centered(None, size(px(1400.), px(900.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("logship".into()),
                        ..Default::default()
                    }),
                    focus: true,
                    ..Default::default()
                },
                |window, cx| {
                    cx.new(|cx| {
                        let mut view = LogView::new(cx);
                        window.focus(view.list_focus());
                        if let Some(path) = path {
                            view.open(path, cx);
                        }
                        view
                    })
                },
            )
            .expect("failed to open window");
        window
            .update(cx, |_, window, _| window.activate_window())
            .ok();
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
