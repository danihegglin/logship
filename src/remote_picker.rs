//! The remote picker: connects to a host over SSH, then browses its files or
//! lists its Docker and Podman containers, and opens the picked one in a tab.

use gpui::{
    ClickEvent, Context, EventEmitter, FocusHandle, KeyDownEvent, Render, ScrollHandle,
    SharedString, Task, Window, div, prelude::*, px,
};

use crate::{
    containers::{self, Container},
    picker::{PickerEvent, button, container_row},
    session::RecentHosts,
    settings::Settings,
    ssh::{self, Entry, Listing},
    stream::Source,
    theme::Theme,
    view::printable,
};

/// Where a newly connected host's files are first shown.
const START_DIR: &str = "/var/log";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Typing or picking a host.
    Host,
    Files,
    Containers,
}

pub struct RemotePicker {
    focus: FocusHandle,
    scroll: ScrollHandle,
    stage: Stage,
    /// The host field, and the hosts from `~/.ssh/config` and recent use.
    host_input: String,
    known_hosts: Vec<String>,
    /// The host being browsed, once one is picked.
    host: Option<String>,
    /// Set after the host answered once, so a failure later is about a path,
    /// not the connection.
    connected: bool,
    /// The path field. It starts as the listed directory followed by `/`, and
    /// what is typed after that filters the entries.
    path_input: String,
    dir: Option<String>,
    entries: Vec<Entry>,
    containers: Vec<Container>,
    show_stopped: bool,
    loading: bool,
    error: Option<String>,
    /// The highlighted row, once the arrow keys moved it.
    selected: Option<usize>,
    _task: Option<Task<()>>,
}

impl EventEmitter<PickerEvent> for RemotePicker {}

/// A row of the file list.
enum Row<'a> {
    Up,
    Entry(&'a Entry),
}

impl RemotePicker {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut known_hosts = cx.global::<RecentHosts>().0.clone();
        for host in ssh::config_hosts() {
            if !known_hosts.contains(&host) {
                known_hosts.push(host);
            }
        }
        RemotePicker {
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            stage: Stage::Host,
            host_input: String::new(),
            known_hosts,
            host: None,
            connected: false,
            path_input: String::new(),
            dir: None,
            entries: Vec::new(),
            containers: Vec::new(),
            show_stopped: true,
            loading: false,
            error: None,
            selected: None,
            _task: None,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn set_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    // ------------------------------------------------------------ hosts

    fn host_suggestions(&self) -> Vec<&String> {
        let typed = self.host_input.trim().to_lowercase();
        self.known_hosts
            .iter()
            .filter(|h| h.to_lowercase().contains(&typed))
            .collect()
    }

    fn connect(&mut self, text: &str, cx: &mut Context<Self>) {
        let host = match ssh::parse_destination(text) {
            Ok(host) => host,
            Err(err) => {
                self.error = Some(err);
                cx.notify();
                return;
            }
        };
        self.host_input = host.clone();
        self.host = Some(host);
        self.connected = false;
        self.dir = None;
        self.entries.clear();
        self.containers.clear();
        self.stage = Stage::Files;
        self.list_dir(START_DIR.to_string(), cx);
    }

    fn change_host(&mut self, cx: &mut Context<Self>) {
        self.stage = Stage::Host;
        self.host = None;
        self.error = None;
        self.loading = false;
        self.selected = None;
        self._task = None;
        cx.notify();
    }

    /// The host could not be reached: back to the host field, with the reason.
    fn connection_failed(&mut self, err: String) {
        self.stage = Stage::Host;
        self.host = None;
        self.error = Some(err);
        self.selected = None;
    }

    // ------------------------------------------------------------ files

    fn list_dir(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        self.loading = true;
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let (host, path, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = ssh::list(&host, &path);
                    (host, path, result)
                })
                .await;
            this.update(cx, |this, cx| this.listed(host, path, result, cx))
                .ok();
        }));
        cx.notify();
    }

    fn listed(
        &mut self,
        host: String,
        path: String,
        result: Result<Listing, String>,
        cx: &mut Context<Self>,
    ) {
        if self.host.as_deref() != Some(host.as_str()) {
            return;
        }
        self.loading = false;
        match result {
            Ok(Listing::Dir { path, entries }) => {
                self.connected = true;
                self.path_input = if path.ends_with('/') {
                    path.clone()
                } else {
                    format!("{path}/")
                };
                self.dir = Some(path);
                self.entries = entries;
                self.selected = None;
                self.scroll.scroll_to_item(0);
            }
            Ok(Listing::File { path }) => {
                self.connected = true;
                cx.emit(PickerEvent::Picked(Source::RemoteFile { host, path }));
            }
            // The host answered but has no /var/log for us: start at home.
            Err(_) if !self.connected && path == START_DIR => {
                self.list_dir("~".into(), cx);
                return;
            }
            Err(err) if !self.connected => self.connection_failed(err),
            Err(err) => self.error = Some(err),
        }
        cx.notify();
    }

    /// What is typed after the listed directory, which filters its entries.
    fn file_filter(&self) -> Option<&str> {
        let dir = self.dir.as_deref()?;
        let rest = self.path_input.strip_prefix(dir)?;
        let rest = if dir.ends_with('/') {
            rest
        } else {
            rest.strip_prefix('/')?
        };
        (!rest.contains('/')).then_some(rest)
    }

    fn file_rows(&self) -> Vec<Row<'_>> {
        let Some(dir) = self.dir.as_deref() else {
            return Vec::new();
        };
        let filter = self.file_filter();
        let mut rows = Vec::new();
        if dir != "/" && filter.is_none_or(str::is_empty) {
            rows.push(Row::Up);
        }
        let typed = filter.unwrap_or("").to_lowercase();
        rows.extend(
            self.entries
                .iter()
                .filter(|e| filter.is_none() || e.name.to_lowercase().contains(&typed))
                .map(Row::Entry),
        );
        rows
    }

    fn open_row(&mut self, i: usize, cx: &mut Context<Self>) {
        let (Some(host), Some(dir)) = (self.host.clone(), self.dir.clone()) else {
            return;
        };
        let target = match self.file_rows().get(i) {
            Some(Row::Up) => Some((ssh::parent(&dir), true)),
            Some(Row::Entry(e)) => Some((ssh::join(&dir, &e.name), e.dir)),
            None => None,
        };
        match target {
            Some((path, true)) => self.list_dir(path, cx),
            Some((path, false)) => cx.emit(PickerEvent::Picked(Source::RemoteFile { host, path })),
            None => {}
        }
    }

    // ------------------------------------------------------------ containers

    fn show_containers(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        self.stage = Stage::Containers;
        self.selected = None;
        self.loading = true;
        self.error = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let (host, (containers, errors)) = cx
                .background_executor()
                .spawn(async move {
                    let found = containers::list_all(Some(&host));
                    (host, found)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host.as_deref() != Some(host.as_str()) {
                    return;
                }
                this.loading = false;
                if containers.is_empty() && !errors.is_empty() {
                    this.error = Some(errors.join("\n"));
                } else {
                    this.connected = true;
                }
                this.containers = containers;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn show_files(&mut self, cx: &mut Context<Self>) {
        self.stage = Stage::Files;
        self.selected = None;
        self.error = None;
        if self.dir.is_none() {
            self.list_dir(START_DIR.into(), cx);
        }
        cx.notify();
    }

    fn visible_containers(&self) -> Vec<&Container> {
        self.containers
            .iter()
            .filter(|c| self.show_stopped || c.running)
            .collect()
    }

    // ------------------------------------------------------------ input

    fn row_count(&self) -> usize {
        match self.stage {
            Stage::Host => self.host_suggestions().len(),
            Stage::Files => self.file_rows().len(),
            Stage::Containers => self.visible_containers().len(),
        }
    }

    fn input(&mut self) -> Option<&mut String> {
        match self.stage {
            Stage::Host => Some(&mut self.host_input),
            Stage::Files => Some(&mut self.path_input),
            Stage::Containers => None,
        }
    }

    fn edited(&mut self) {
        self.selected = None;
        if self.stage == Stage::Host {
            self.error = None;
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        match self.stage {
            Stage::Host => {
                let suggestions = self.host_suggestions();
                let host = match self.selected.and_then(|i| suggestions.get(i)) {
                    Some(host) => (*host).clone(),
                    None if self.host_input.trim().is_empty() => match suggestions.first() {
                        Some(host) => (*host).clone(),
                        None => String::new(),
                    },
                    None => self.host_input.clone(),
                };
                self.connect(&host, cx);
            }
            Stage::Files => {
                let rows = self.file_rows();
                let only_match =
                    self.file_filter().is_some_and(|f| !f.is_empty()) && rows.len() == 1;
                match self.selected {
                    Some(i) => self.open_row(i, cx),
                    None if only_match => self.open_row(0, cx),
                    // A typed path, or the directory itself again to refresh it.
                    None => self.list_dir(self.path_input.trim().to_string(), cx),
                }
            }
            Stage::Containers => {
                let picked = self
                    .visible_containers()
                    .get(self.selected.unwrap_or(0))
                    .map(|c| (*c).clone());
                if let Some(c) = picked {
                    cx.emit(PickerEvent::Picked(Source::Container(c)));
                }
            }
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        let count = self.row_count();
        match ks.key.as_str() {
            "escape" => cx.emit(PickerEvent::Dismissed),
            "enter" => self.submit(cx),
            "up" if count > 0 => {
                let i = match self.selected {
                    Some(i) => i.checked_sub(1).unwrap_or(count - 1),
                    None => count - 1,
                };
                self.selected = Some(i);
                self.scroll.scroll_to_item(i);
            }
            "down" if count > 0 => {
                let i = self.selected.map_or(0, |i| (i + 1) % count);
                self.selected = Some(i);
                self.scroll.scroll_to_item(i);
            }
            "tab" if self.host.is_some() => {
                if self.stage == Stage::Files {
                    self.show_containers(cx);
                } else {
                    self.show_files(cx);
                }
            }
            "backspace" => {
                let Some(input) = self.input() else { return };
                if m.platform {
                    input.clear();
                } else if m.alt {
                    let trimmed = input.trim_end_matches(|c: char| !c.is_alphanumeric());
                    let keep = trimmed.trim_end_matches(char::is_alphanumeric).len();
                    input.truncate(keep);
                } else {
                    input.pop();
                }
                self.edited();
            }
            "v" if m.platform || (m.control && !cfg!(target_os = "macos")) => {
                let text = cx.read_from_clipboard().and_then(|item| item.text());
                let Some(input) = self.input() else { return };
                if let Some(text) = text {
                    input.push_str(text.lines().next().unwrap_or_default().trim());
                    self.edited();
                }
            }
            _ if m.platform || m.control => return,
            _ => {
                let Some(ch) = ks
                    .key_char
                    .clone()
                    .filter(|c| !c.chars().any(char::is_control))
                else {
                    return;
                };
                let Some(input) = self.input() else { return };
                input.push_str(&ch);
                self.edited();
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
}

/// A one-line text field. It is drawn only; the picker gets the keys.
fn field(text: &str, placeholder: &str, focused: bool, t: &Theme) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .h(px(28.))
        .px_2()
        .rounded_md()
        .border_1()
        .border_color(if focused { t.accent } else { t.border })
        .bg(t.bg)
        .overflow_hidden()
        .whitespace_nowrap()
        .when(text.is_empty(), |d| {
            d.text_color(t.text_muted).child(placeholder.to_string())
        })
        .when(!text.is_empty(), |d| {
            d.text_color(t.text)
                .child(SharedString::from(printable(text)))
        })
        .when(focused, |d| {
            d.child(div().w(px(1.5)).h(px(16.)).bg(t.accent))
        })
}

impl Render for RemotePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Settings>().theme();
        let hover = t.surface_hover;
        let focused = self.focus.is_focused(window);
        let count = self.row_count();
        let selected = self.selected.filter(|&i| i < count);

        let title = match &self.host {
            Some(host) => format!("SSH: {}", printable(host)),
            None => "Open logs on an SSH host".to_string(),
        };
        let mut header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().truncate().text_color(t.text).child(title));
        if self.host.is_some() {
            header = header
                .child(
                    button("remote-files", "Files", self.stage == Stage::Files, &t)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.show_files(cx))),
                )
                .child(
                    button(
                        "remote-containers",
                        "Containers",
                        self.stage == Stage::Containers,
                        &t,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.show_containers(cx))),
                )
                .child(
                    button("remote-host", "Change host", false, &t)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.change_host(cx))),
                );
        }
        header = header.child(
            button("remote-close", "×", false, &t)
                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(PickerEvent::Dismissed))),
        );

        let input = match self.stage {
            Stage::Host => Some(field(
                &self.host_input,
                "user@host, or a host from ~/.ssh/config",
                focused,
                &t,
            )),
            Stage::Files => Some(field(&self.path_input, "/var/log", focused, &t)),
            Stage::Containers => None,
        };

        let row = |id: (&'static str, usize), i: usize| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(selected == Some(i), |d| d.bg(t.surface_hover))
                .hover(move |s| s.bg(hover))
        };

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        match self.stage {
            Stage::Host => {
                for (i, host) in self.host_suggestions().into_iter().enumerate() {
                    let host = host.clone();
                    rows.push(
                        row(("remote-host-row", i), i)
                            .text_color(t.text)
                            .child(printable(&host))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.connect(&host, cx)
                            }))
                            .into_any_element(),
                    );
                }
            }
            Stage::Files => {
                for (i, r) in self.file_rows().into_iter().enumerate() {
                    let (icon, name, dir) = match r {
                        Row::Up => ("↰", "..".to_string(), true),
                        Row::Entry(e) => (if e.dir { "▸" } else { " " }, printable(&e.name), e.dir),
                    };
                    rows.push(
                        row(("remote-file", i), i)
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(12.))
                                    .text_color(t.text_muted)
                                    .child(icon),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .text_color(t.text)
                                    .when(dir, |d| d.font_weight(gpui::FontWeight::MEDIUM))
                                    .child(if dir && name != ".." {
                                        format!("{name}/")
                                    } else {
                                        name
                                    }),
                            )
                            .on_click(
                                cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.open_row(i, cx)
                                }),
                            )
                            .into_any_element(),
                    );
                }
            }
            Stage::Containers => {
                for (i, c) in self.visible_containers().into_iter().enumerate() {
                    let c2 = c.clone();
                    rows.push(
                        container_row(c, ("remote-container", i).into(), selected == Some(i), &t)
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                cx.emit(PickerEvent::Picked(Source::Container(c2.clone())))
                            }))
                            .into_any_element(),
                    );
                }
            }
        }

        let note = if self.loading {
            Some(match self.stage {
                Stage::Containers => "Looking for containers…",
                _ if !self.connected => "Connecting…",
                _ => "Listing…",
            })
        } else if rows.is_empty() && self.error.is_none() {
            Some(match self.stage {
                Stage::Host => {
                    "Type a host and press Enter. logship uses your ssh setup, keys and agent."
                }
                Stage::Files => "Nothing here",
                Stage::Containers if self.containers.is_empty() => "No containers",
                Stage::Containers => "No running containers",
            })
        } else {
            None
        };

        let footer = match self.stage {
            Stage::Host => {
                "Needs a key in the SSH agent or an unencrypted key, and a known host key."
                    .to_string()
            }
            Stage::Files => {
                "Type to filter · Enter opens · Tab: containers · files keep following".to_string()
            }
            Stage::Containers => "Uses docker or podman on the host · Tab: files".to_string(),
        };

        div()
            .id("remote-picker")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .w(px(560.))
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
            .child(header)
            .when(self.stage == Stage::Containers, |d| {
                d.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(button("remote-all", "All", self.show_stopped, &t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.show_stopped = true;
                                this.selected = None;
                                cx.notify();
                            }),
                        ))
                        .child(
                            button("remote-running", "Running", !self.show_stopped, &t).on_click(
                                cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.show_stopped = false;
                                    this.selected = None;
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(button("remote-refresh", "↻", false, &t).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.show_containers(cx)),
                        )),
                )
            })
            .children(input)
            .when_some(self.error.clone(), |d, err| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(t.error_text)
                        .children(err.lines().map(|l| div().child(printable(l)))),
                )
            })
            .child(
                div()
                    .id("remote-list")
                    .track_scroll(&self.scroll)
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(rows)
                    .when_some(note, |d, note| {
                        d.child(
                            div()
                                .px_2()
                                .py_3()
                                .text_xs()
                                .text_color(t.text_muted)
                                .child(note),
                        )
                    }),
            )
            .child(div().text_xs().text_color(t.text_muted).child(footer))
    }
}
