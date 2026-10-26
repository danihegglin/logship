//! The container picker: lists Docker and Podman containers so their logs can
//! be opened in a tab, as an alternative to picking a file.

use gpui::{
    ClickEvent, Context, EventEmitter, FocusHandle, KeyDownEvent, Render, ScrollHandle, Task,
    Window, div, prelude::*, px,
};

use crate::{
    containers::{self, Container},
    index::Level,
    settings::Settings,
    view::printable,
};

pub enum PickerEvent {
    Picked(Container),
    Dismissed,
}

pub struct ContainerPicker {
    focus: FocusHandle,
    scroll: ScrollHandle,
    containers: Vec<Container>,
    /// Engines that could not list containers, e.g. "podman is not installed".
    errors: Vec<String>,
    /// Set when the logs of the picked container could not be opened.
    error: Option<String>,
    loading: bool,
    show_stopped: bool,
    selected: usize,
    _refresh: Option<Task<()>>,
}

impl EventEmitter<PickerEvent> for ContainerPicker {}

impl ContainerPicker {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = ContainerPicker {
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            containers: Vec::new(),
            errors: Vec::new(),
            error: None,
            loading: false,
            show_stopped: true,
            selected: 0,
            _refresh: None,
        };
        this.refresh(cx);
        this
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn set_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        self._refresh = Some(cx.spawn(async move |this, cx| {
            let (containers, errors) = cx
                .background_executor()
                .spawn(async move { containers::list_all() })
                .await;
            this.update(cx, |this, cx| {
                this.containers = containers;
                this.errors = errors;
                this.loading = false;
                this.selected = 0;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn visible(&self) -> Vec<&Container> {
        self.containers
            .iter()
            .filter(|c| self.show_stopped || c.running)
            .collect()
    }

    fn pick(&mut self, i: usize, cx: &mut Context<Self>) {
        if let Some(container) = self.visible().get(i) {
            cx.emit(PickerEvent::Picked((*container).clone()));
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.visible().len();
        match ev.keystroke.key.as_str() {
            "escape" => cx.emit(PickerEvent::Dismissed),
            "enter" => self.pick(self.selected, cx),
            "up" if count > 0 => {
                self.selected = self.selected.checked_sub(1).unwrap_or(count - 1);
                self.scroll.scroll_to_item(self.selected);
            }
            "down" if count > 0 => {
                self.selected = (self.selected + 1) % count;
                self.scroll.scroll_to_item(self.selected);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for ContainerPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Settings>().theme();
        let hover = t.surface_hover;
        let running_dot = t.level(Level::Info);
        let visible = self.visible();
        let selected = self.selected.min(visible.len().saturating_sub(1));

        let button = |id: &'static str, text: &'static str, active: bool| {
            div()
                .id(id)
                .px_2()
                .h(px(24.))
                .flex()
                .items_center()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .when(active, |d| d.bg(t.surface_hover).text_color(t.text))
                .when(!active, |d| d.text_color(t.text_muted))
                .hover(move |s| s.bg(hover))
                .child(text)
        };

        let rows = visible.iter().enumerate().map(|(i, c)| {
            div()
                .id(("container", i))
                .flex()
                .items_center()
                .gap_3()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(i == selected, |d| d.bg(t.surface_hover))
                .hover(move |s| s.bg(hover))
                .child(
                    div()
                        .flex_none()
                        .size(px(7.))
                        .rounded_full()
                        .bg(if c.running { running_dot } else { t.border }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_color(t.text)
                                .child(printable(&c.name)),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(t.text_muted)
                                .child(printable(&format!("{}  ·  {}", c.image, c.status))),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .px_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(t.border)
                        .text_xs()
                        .text_color(t.text_muted)
                        .child(c.engine.name()),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.pick(i, cx)))
        });

        let empty_note = if self.loading {
            Some("Looking for containers…".to_string())
        } else if visible.is_empty() && self.containers.is_empty() && !self.errors.is_empty() {
            Some(format!(
                "No container engine could be reached.\n{}",
                self.errors.join("\n")
            ))
        } else if visible.is_empty() {
            Some(if self.containers.is_empty() {
                "No containers".to_string()
            } else {
                "No running containers".to_string()
            })
        } else {
            None
        };

        div()
            .id("container-picker")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .w(px(520.))
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
                    .gap_2()
                    .child(div().flex_1().text_color(t.text).child("Container logs"))
                    .child(button("containers-all", "All", self.show_stopped).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.show_stopped = true;
                            this.selected = 0;
                            cx.notify();
                        }),
                    ))
                    .child(
                        button("containers-running", "Running", !self.show_stopped).on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.show_stopped = false;
                                this.selected = 0;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        button("containers-refresh", "↻", false)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.refresh(cx))),
                    )
                    .child(button("containers-close", "×", false).on_click(
                        cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(PickerEvent::Dismissed)),
                    )),
            )
            .when_some(self.error.clone(), |d, err| {
                d.child(div().text_xs().text_color(t.error_text).child(err))
            })
            .child(
                div()
                    .id("container-list")
                    .track_scroll(&self.scroll)
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(rows)
                    .when_some(empty_note, |d, note| {
                        d.child(
                            div()
                                .px_2()
                                .py_3()
                                .text_xs()
                                .text_color(t.text_muted)
                                .children(note.lines().map(|l| div().child(l.to_string()))),
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(t.text_muted)
                    .child("Logs of running containers keep streaming while the tab is open."),
            )
    }
}
