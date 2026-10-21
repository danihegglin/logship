mod decorations;
mod index;
mod search;
mod session;
mod settings;
mod theme;
mod view;
mod workspace;

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, prelude::*, px, size,
};

use session::{RecentFiles, Session};
use view::{
    CloseTab, Copy, FocusSearch, NewTab, NextMatch, NextTab, Open, OpenSettings, PrevMatch, PrevTab,
    Quit, SelectAll, ToggleCase, ToggleFilterMode, ToggleFollow, ToggleRegex, ToggleTheme,
};
use workspace::Workspace;

fn main() {
    let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let session = Session::load();

    Application::new().run(move |cx: &mut App| {
        settings::load_bundled_fonts(cx);
        cx.set_global(session.settings.clone());
        cx.set_global(RecentFiles(session.recent.clone()));

        cx.bind_keys([
            KeyBinding::new("secondary-o", Open, None),
            KeyBinding::new("secondary-t", NewTab, None),
            KeyBinding::new("secondary-w", CloseTab, None),
            KeyBinding::new("ctrl-tab", NextTab, None),
            KeyBinding::new("ctrl-shift-tab", PrevTab, None),
            KeyBinding::new("secondary-shift-]", NextTab, None),
            KeyBinding::new("secondary-shift-[", PrevTab, None),
            KeyBinding::new("secondary-,", OpenSettings, None),
            KeyBinding::new("secondary-q", Quit, None),
            KeyBinding::new("secondary-f", FocusSearch, None),
            KeyBinding::new("secondary-c", Copy, None),
            KeyBinding::new("secondary-a", SelectAll, None),
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
                items: vec![
                    MenuItem::action("Settings…", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::action("Quit logship", Quit),
                ],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("New Tab", NewTab),
                    MenuItem::action("Open…", Open),
                    MenuItem::separator(),
                    MenuItem::action("Close Tab", CloseTab),
                ],
            },
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::action("Copy", Copy),
                    MenuItem::action("Select All", SelectAll),
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
                    MenuItem::separator(),
                    MenuItem::action("Next Tab", NextTab),
                    MenuItem::action("Previous Tab", PrevTab),
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
                    // Must match packaging/linux/logship.desktop: GNOME and KDE use it
                    // (the Wayland app_id, the X11 WM_CLASS) to find the app's name and icon.
                    app_id: Some("logship".into()),
                    window_decorations: Some(decorations::preferred()),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Workspace::new(&session, paths, window, cx)),
            )
            .expect("failed to open window");
        window
            .update(cx, |_, window, _| window.activate_window())
            .ok();
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
