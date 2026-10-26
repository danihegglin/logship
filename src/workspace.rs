//! The window's root view: one tab per log file or container, the settings
//! panel, the container picker, and the title bar on desktops that leave it to
//! the app.
//!
//! The open tabs, recent files and settings are saved to the session file on
//! every change, and the tabs are reopened on the next start.

use std::path::{Path, PathBuf};

use gpui::{
    App, ClickEvent, Context, Entity, ExternalPaths, FocusHandle, KeyDownEvent, MouseButton,
    MouseDownEvent, PathPromptOptions, Render, SharedString, Subscription, Window, div, prelude::*,
    px,
};

use crate::{
    containers::{self, Container, LogStream},
    decorations,
    picker::{ContainerPicker, PickerEvent},
    session::{self, RecentFiles, Session},
    settings::{self, BUNDLED_FONT, MAX_FONT_SIZE, MIN_FONT_SIZE, Settings},
    theme::Theme,
    view::{
        CloseTab, LogView, LogViewEvent, NewTab, NextTab, Open, OpenContainer, OpenSettings,
        PrevTab, ToggleNewestFirst, ToggleTheme, printable,
    },
};

pub const TABBAR_H: f32 = 32.;
/// Height of the log view's toolbar, which the settings panel opens below.
const TOOLBAR_H: f32 = 44.;

struct Tab {
    view: Entity<LogView>,
    /// Set when the tab shows a container's logs; dropping it stops the stream.
    stream: Option<LogStream>,
    _events: Subscription,
}

pub struct Workspace {
    tabs: Vec<Tab>,
    active: usize,
    settings_open: bool,
    settings_focus: FocusHandle,
    font_choices: Vec<String>,
    picker: Option<(Entity<ContainerPicker>, Subscription)>,
    window_title: String,
    /// Set while the left button is held on the client-side title bar.
    pub(crate) titlebar_drag: bool,
    _settings_changed: Subscription,
    _quit: Subscription,
}

impl Workspace {
    /// Reopens the tabs saved in `session`, then opens `paths` (from the command line).
    pub fn new(
        session: &Session,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Workspace {
            tabs: Vec::new(),
            active: 0,
            settings_open: false,
            settings_focus: cx.focus_handle(),
            font_choices: Vec::new(),
            picker: None,
            window_title: String::new(),
            titlebar_drag: false,
            _settings_changed: cx.observe_global::<Settings>(|this, cx| {
                this.save(cx);
                cx.notify();
            }),
            // Stop the container log streams and delete their temporary files.
            _quit: cx.on_app_quit(|this, _| {
                for tab in &mut this.tabs {
                    tab.stream = None;
                }
                containers::remove_log_dir();
                async {}
            }),
        };
        for path in session.tabs.iter().filter(|p| p.is_file()) {
            this.add_tab(Some(path.clone()), window, cx);
        }
        this.active = session.active.min(this.tabs.len().saturating_sub(1));
        for path in paths {
            this.open_path(path, window, cx);
        }
        if this.tabs.is_empty() {
            this.add_tab(None, window, cx);
        }
        this.focus_active(window, cx);
        this
    }

    fn add_tab(
        &mut self,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let view = cx.new(LogView::new);
        let events = cx.subscribe_in(&view, window, Self::on_tab_event);
        if let Some(path) = path {
            view.update(cx, |view, cx| view.open(path, cx));
        }
        self.tabs.push(Tab {
            view,
            stream: None,
            _events: events,
        });
        self.tabs.len() - 1
    }

    fn on_tab_event(
        &mut self,
        _: &Entity<LogView>,
        event: &LogViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            LogViewEvent::Opened(path) if containers::is_stream_file(path) => cx.notify(),
            LogViewEvent::Opened(path) => {
                session::push_recent(&mut cx.global_mut::<RecentFiles>().0, path);
                self.save(cx);
                cx.notify();
            }
            LogViewEvent::OpenRequested(path) => self.open_path(path.clone(), window, cx),
        }
    }

    fn tab_path<'a>(&self, tab: &Tab, cx: &'a App) -> Option<&'a Path> {
        tab.view.read(cx).path()
    }

    /// Shows `path`: switches to its tab if it is already open, otherwise opens it
    /// in the current tab if that is empty, or in a new tab.
    pub fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if let Some(i) = self
            .tabs
            .iter()
            .position(|tab| self.tab_path(tab, cx) == Some(path.as_path()))
        {
            self.activate(i, window, cx);
            return;
        }
        let current_is_empty = self
            .tabs
            .get(self.active)
            .is_some_and(|tab| self.tab_path(tab, cx).is_none());
        if current_is_empty {
            self.tabs[self.active]
                .view
                .update(cx, |view, cx| view.open(path, cx));
        } else {
            self.active = self.add_tab(Some(path), window, cx);
        }
        self.focus_active(window, cx);
        self.save(cx);
        cx.notify();
    }

    fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Open Log".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update_in(cx, |this, window, cx| {
                    for path in paths {
                        this.open_path(path, window, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn toggle_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            self.focus_active(window, cx);
        } else {
            self.settings_open = false;
            let picker = cx.new(ContainerPicker::new);
            let events =
                cx.subscribe_in(&picker, window, |this, _, event, window, cx| match event {
                    PickerEvent::Picked(container) => this.open_container(container, window, cx),
                    PickerEvent::Dismissed => this.toggle_picker(window, cx),
                });
            window.focus(picker.read(cx).focus_handle());
            self.picker = Some((picker, events));
        }
        cx.notify();
    }

    /// Shows a container's logs: switches to its tab if it is already open,
    /// otherwise starts streaming them into the current tab if that is empty, or
    /// into a new tab.
    fn open_container(
        &mut self,
        container: &Container,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.tabs.iter().position(|tab| {
            tab.stream.as_ref().is_some_and(|s| {
                s.container.engine == container.engine && s.container.id == container.id
            })
        });
        if let Some(i) = open {
            self.picker = None;
            self.activate(i, window, cx);
            return;
        }
        let stream = match LogStream::start(container) {
            Ok(stream) => stream,
            Err(err) => {
                if let Some((picker, _)) = &self.picker {
                    let msg = format!("Could not open the logs of {}: {err}", container.name);
                    picker.update(cx, |picker, cx| picker.set_error(msg, cx));
                }
                return;
            }
        };
        self.picker = None;
        let path = stream.path().to_path_buf();
        let current_is_empty = self
            .tabs
            .get(self.active)
            .is_some_and(|tab| self.tab_path(tab, cx).is_none());
        if !current_is_empty {
            self.active = self.add_tab(None, window, cx);
        }
        let tab = &mut self.tabs[self.active];
        tab.stream = Some(stream);
        tab.view.update(cx, |view, cx| view.open(path, cx));
        self.focus_active(window, cx);
        self.save(cx);
        cx.notify();
    }

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active = self.add_tab(None, window, cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn activate(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i < self.tabs.len() {
            self.active = i;
            self.focus_active(window, cx);
            self.save(cx);
            cx.notify();
        }
    }

    fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len() as isize;
        self.activate(
            (self.active as isize + delta).rem_euclid(n) as usize,
            window,
            cx,
        );
    }

    fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.add_tab(None, window, cx);
        }
        if i < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.focus_active(window, cx);
        self.save(cx);
        cx.notify();
    }

    fn focus_active(&self, window: &mut Window, cx: &App) {
        let focus = self.tabs[self.active].view.read(cx).list_focus().clone();
        window.focus(&focus);
    }

    fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = !self.settings_open;
        self.picker = None;
        if self.settings_open {
            let current = cx.global::<Settings>().font_family.clone();
            self.font_choices = settings::font_choices(&current, cx);
            window.focus(&self.settings_focus);
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn save(&self, cx: &App) {
        let mut tabs = Vec::new();
        let mut active = 0;
        for (i, tab) in self.tabs.iter().enumerate() {
            // Container logs live in temporary files that are gone after a restart.
            if tab.stream.is_some() {
                continue;
            }
            if let Some(path) = self.tab_path(tab, cx) {
                if i < self.active {
                    active += 1;
                }
                tabs.push(path.to_path_buf());
            }
        }
        Session {
            settings: cx.global::<Settings>().clone(),
            tabs,
            active,
            recent: cx.global::<RecentFiles>().0.clone(),
        }
        .save();
    }

    fn tab_name(&self, tab: &Tab, cx: &App) -> String {
        if let Some(stream) = &tab.stream {
            return printable(&stream.container.label());
        }
        match self.tab_path(tab, cx) {
            Some(path) => printable(
                &path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy(),
            ),
            None => "New Tab".into(),
        }
    }

    fn render_tab_bar(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let hover = t.surface_hover;
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let active = i == self.active;
                div()
                    .id(("tab", i))
                    .flex()
                    .flex_shrink()
                    .min_w(px(90.))
                    .max_w(px(240.))
                    .h(px(TABBAR_H - 6.))
                    .items_center()
                    .gap_2()
                    .pl_3()
                    .pr_1()
                    .rounded_t_md()
                    .cursor_pointer()
                    .when(active, |d| {
                        d.bg(t.bg)
                            .text_color(t.text)
                            .border_1()
                            .border_b_0()
                            .border_color(t.border)
                    })
                    .when(!active, |d| {
                        d.text_color(t.text_muted).hover(move |s| s.bg(hover))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            this.activate(i, window, cx)
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            this.close_tab(i, window, cx)
                        }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_xs()
                            .child(self.tab_name(tab, cx)),
                    )
                    .child(
                        div()
                            .id(("close-tab", i))
                            .flex_none()
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .text_xs()
                            .text_color(t.text_muted)
                            .hover(move |s| s.bg(hover))
                            .child("×")
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.close_tab(i, window, cx)
                            })),
                    )
            })
            .collect();

        div()
            .h(px(TABBAR_H))
            .flex_none()
            .flex()
            .items_end()
            .gap_1()
            .px_2()
            .bg(t.surface)
            .border_b_1()
            .border_color(t.border)
            .overflow_hidden()
            .children(tabs)
            .child(
                div()
                    .id("new-tab")
                    .flex_none()
                    .mb(px(3.))
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .text_color(t.text_muted)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child("+")
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.new_tab(window, cx)),
                    ),
            )
    }

    fn render_settings(
        &self,
        s: &Settings,
        top: f32,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let hover = t.surface_hover;
        let label = |text: &'static str| div().text_xs().text_color(t.text_muted).child(text);
        let button = |id: &'static str, text: SharedString, active: bool| {
            div()
                .id(id)
                .px_3()
                .h(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .border_1()
                .border_color(t.border)
                .cursor_pointer()
                .when(active, |d| d.bg(t.surface_hover).text_color(t.text))
                .when(!active, |d| d.text_color(t.text_muted))
                .hover(move |st| st.bg(hover))
                .child(text)
        };

        let fonts = self.font_choices.iter().enumerate().map(|(i, name)| {
            let selected = *name == s.font_family;
            let pick = name.clone();
            div()
                .id(("font", i))
                .flex()
                .items_center()
                .justify_between()
                .px_2()
                .h(px(26.))
                .rounded_md()
                .cursor_pointer()
                .when(selected, |d| d.bg(t.surface_hover))
                .hover(move |st| st.bg(hover))
                .child(
                    div()
                        .font_family(SharedString::from(name.clone()))
                        .text_color(t.text)
                        .child(name.clone()),
                )
                .child(div().text_xs().text_color(t.text_muted).child(if selected {
                    "✓"
                } else if name == BUNDLED_FONT {
                    "built in"
                } else {
                    ""
                }))
                .on_click(move |_, _, cx| {
                    let pick = pick.clone();
                    cx.update_global::<Settings, _>(|s, _| s.font_family = pick);
                })
        });

        let step_size = |delta: f32| {
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                cx.update_global::<Settings, _>(|s, _| {
                    s.font_size = (s.font_size + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
                })
            }
        };

        div()
            .id("settings-panel")
            .track_focus(&self.settings_focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    this.toggle_settings(window, cx);
                }
            }))
            .absolute()
            .top(px(top))
            .right(px(12.))
            .w(px(300.))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .bg(t.surface)
            .border_1()
            .border_color(t.border)
            .shadow_lg()
            .occlude()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_color(t.text).child("Settings"))
                    .child(
                        button("close-settings", "×".into(), false)
                            .border_0()
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.toggle_settings(window, cx)
                            })),
                    ),
            )
            .child(label("Font"))
            .child(div().flex().flex_col().children(fonts))
            .child(label("Size"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(button("font-smaller", "−".into(), false).on_click(step_size(-0.5)))
                    .child(
                        div()
                            .w(px(64.))
                            .text_center()
                            .text_color(t.text)
                            .child(format!("{} px", s.font_size)),
                    )
                    .child(button("font-larger", "+".into(), false).on_click(step_size(0.5))),
            )
            .child(label("Order"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button("oldest-first", "Oldest first".into(), !s.newest_first).on_click(
                            |_, _, cx| {
                                cx.update_global::<Settings, _>(|s, _| s.newest_first = false)
                            },
                        ),
                    )
                    .child(
                        button("newest-first", "Newest first".into(), s.newest_first).on_click(
                            |_, _, cx| {
                                cx.update_global::<Settings, _>(|s, _| s.newest_first = true)
                            },
                        ),
                    ),
            )
            .child(label("Theme"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button("theme-dark", "Dark".into(), s.dark).on_click(|_, _, cx| {
                            cx.update_global::<Settings, _>(|s, _| s.dark = true)
                        }),
                    )
                    .child(
                        button("theme-light", "Light".into(), !s.dark).on_click(|_, _, cx| {
                            cx.update_global::<Settings, _>(|s, _| s.dark = false)
                        }),
                    ),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<Settings>().clone();
        let t = settings.theme();

        let title = match self.tab_path(&self.tabs[self.active], cx) {
            Some(_) => format!("{} — logship", self.tab_name(&self.tabs[self.active], cx)),
            None => "logship".into(),
        };
        if title != self.window_title {
            window.set_window_title(&title);
            self.window_title = title;
        }

        let title_bar = decorations::title_bar(&self.window_title, window, &t, cx);
        let tab_bar = self.render_tab_bar(&t, cx);
        let settings_panel = self.settings_open.then(|| {
            let top = decorations::title_bar_height(window) + TABBAR_H + TOOLBAR_H - 4.;
            // Clicking anywhere else closes the panel.
            let backdrop = div()
                .id("settings-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.toggle_settings(window, cx)
                    }),
                );
            (backdrop, self.render_settings(&settings, top, &t, cx))
        });
        let picker = self.picker.as_ref().map(|(picker, _)| {
            let top = decorations::title_bar_height(window) + TABBAR_H + TOOLBAR_H + 24.;
            // Clicking outside the picker closes it.
            div()
                .id("picker-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(top))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.toggle_picker(window, cx)
                    }),
                )
                .child(picker.clone())
        });
        let active = self.tabs[self.active].view.clone();

        let root =
            div()
                .id("workspace")
                .key_context("Workspace")
                .relative()
                .size_full()
                .flex()
                .flex_col()
                .bg(t.bg)
                .text_color(t.text)
                .text_sm()
                .on_action(cx.listener(|this, _: &Open, window, cx| this.prompt_open(window, cx)))
                .on_action(cx.listener(|this, _: &OpenContainer, window, cx| {
                    this.toggle_picker(window, cx)
                }))
                .on_action(cx.listener(|this, _: &NewTab, window, cx| this.new_tab(window, cx)))
                .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                    this.close_tab(this.active, window, cx)
                }))
                .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle(1, window, cx)))
                .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle(-1, window, cx)))
                .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                    this.toggle_settings(window, cx)
                }))
                .on_action(|_: &ToggleTheme, _, cx| {
                    cx.update_global::<Settings, _>(|s, _| s.dark = !s.dark)
                })
                .on_action(|_: &ToggleNewestFirst, _, cx| {
                    cx.update_global::<Settings, _>(|s, _| s.newest_first = !s.newest_first)
                })
                .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                    for path in paths.paths() {
                        this.open_path(path.clone(), window, cx);
                    }
                }))
                .drag_over::<ExternalPaths>({
                    let accent = t.accent;
                    move |style, _, _, _| style.border_2().border_color(accent)
                })
                .children(title_bar)
                .child(tab_bar)
                .child(active)
                .when_some(settings_panel, |d, (backdrop, panel)| {
                    d.child(backdrop).child(panel)
                })
                .children(picker);
        decorations::frame(window, &t, root)
    }
}
