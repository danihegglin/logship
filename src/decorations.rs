//! Client-side window decorations.
//!
//! Most desktops draw the title bar and resize borders for us. GNOME on Wayland
//! (the default session on RHEL, Fedora and Ubuntu) leaves them to the app, and
//! gpui doesn't notice: it asks for server-side decorations and assumes it got
//! them, so the window ends up with no title bar and can't be moved, resized or
//! closed with the mouse. [`preferred`] asks for client-side decorations on such
//! compositors, and the title bar and resize border below are drawn whenever
//! gpui reports `Decorations::Client`. Everywhere else nothing here is drawn.

use gpui::{
    AnyElement, BoxShadow, Context, CursorStyle, Decorations, HitboxBehavior, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ResizeEdge, Size, Stateful,
    Tiling, Window, WindowDecorations, canvas, div, hsla, point, prelude::*, px,
};

use crate::{theme::Theme, view::LogView};

const TITLEBAR_H: f32 = 32.;
/// Invisible border around the window that resizes it and holds its shadow.
const RESIZE_BORDER: Pixels = px(8.);
const BUTTON: f32 = 24.;

/// The decorations to request when opening the window: server-side, unless we
/// are on a Wayland compositor that can't draw them.
pub fn preferred() -> WindowDecorations {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if !wayland_server_decorations() {
        return WindowDecorations::Client;
    }
    WindowDecorations::Server
}

/// Whether the Wayland compositor gpui will connect to supports server-side
/// decorations (the `xdg-decoration` protocol). KDE, Sway and Hyprland do;
/// GNOME and Weston don't. Not on Wayland at all counts as yes.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn wayland_server_decorations() -> bool {
    use wayland_client::{
        Connection, Dispatch, QueueHandle,
        globals::{GlobalListContents, registry_queue_init},
        protocol::wl_registry::{self, WlRegistry},
    };

    struct Globals;
    impl Dispatch<WlRegistry, GlobalListContents> for Globals {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    // Same check gpui uses to pick Wayland over X11.
    if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|d| d.is_empty()) {
        return true;
    }
    let Ok(conn) = Connection::connect_to_env() else {
        return true;
    };
    let Ok((globals, _)) = registry_queue_init::<Globals>(&conn) else {
        return true;
    };
    globals.contents().with_list(|list| {
        list.iter()
            .any(|global| global.interface == "zxdg_decoration_manager_v1")
    })
}

/// Wraps the window content in a resize border and drop shadow when the app has
/// to draw its own decorations; returns the content untouched otherwise.
pub fn frame(window: &mut Window, t: &Theme, content: impl IntoElement) -> AnyElement {
    let Decorations::Client { tiling } = window.window_decorations() else {
        return content.into_any_element();
    };
    window.set_client_inset(RESIZE_BORDER);

    div()
        .id("window-frame")
        .size_full()
        .when(!tiling.top, |d| d.pt(RESIZE_BORDER))
        .when(!tiling.bottom, |d| d.pb(RESIZE_BORDER))
        .when(!tiling.left, |d| d.pl(RESIZE_BORDER))
        .when(!tiling.right, |d| d.pr(RESIZE_BORDER))
        .child(
            canvas(
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |_, hitbox, window, _| {
                    // gpui reports (0, 0) until the pointer first moves over the
                    // window, which would show a corner resize cursor in the middle
                    // of a window that opened under a still pointer.
                    let pos = window.mouse_position();
                    if pos == Point::default() {
                        return;
                    }
                    if let Some(edge) = resize_edge(pos, window.viewport_size(), tiling) {
                        window.set_cursor_style(resize_cursor(edge), &hitbox);
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        // Re-render on hover so the canvas above can update the resize cursor.
        .on_mouse_move(|_, window, _| window.refresh())
        .on_mouse_down(MouseButton::Left, move |ev, window, _| {
            if let Some(edge) = resize_edge(ev.position, window.viewport_size(), tiling) {
                window.start_window_resize(edge);
            }
        })
        .child(
            div()
                .size_full()
                .overflow_hidden()
                .border_color(t.border)
                .when(!tiling.top, |d| d.border_t_1())
                .when(!tiling.bottom, |d| d.border_b_1())
                .when(!tiling.left, |d| d.border_l_1())
                .when(!tiling.right, |d| d.border_r_1())
                .when(!tiling.is_tiled(), |d| {
                    d.shadow(vec![BoxShadow {
                        color: hsla(0., 0., 0., 0.35),
                        blur_radius: RESIZE_BORDER / 2.,
                        spread_radius: px(0.),
                        offset: point(px(0.), px(0.)),
                    }])
                })
                // Moves over the content don't need the frame's refresh.
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .child(content),
        )
        .into_any_element()
}

/// Height taken by the title bar and the top and bottom borders, which the
/// log list has to leave room for.
pub fn chrome_height(window: &Window) -> f32 {
    let Decorations::Client { tiling } = window.window_decorations() else {
        return 0.;
    };
    let edge = |tiled: bool| {
        if tiled {
            0.
        } else {
            f32::from(RESIZE_BORDER) + 1.
        }
    };
    TITLEBAR_H + edge(tiling.top) + edge(tiling.bottom)
}

/// The title bar, drawn only when the app has to draw its own decorations.
/// Drag it to move the window, double-click to maximize, right-click for the
/// window menu.
pub fn title_bar(
    title: &str,
    window: &Window,
    t: &Theme,
    cx: &mut Context<LogView>,
) -> Option<AnyElement> {
    let Decorations::Client { .. } = window.window_decorations() else {
        return None;
    };
    let controls = window.window_controls();
    let maximized = window.is_maximized();

    let mut buttons: Vec<AnyElement> = Vec::new();
    if controls.minimize {
        buttons.push(
            control("minimize", t)
                .child(div().w(px(10.)).h(px(1.5)).bg(t.text))
                .on_click(|_, window, _| window.minimize_window())
                .into_any_element(),
        );
    }
    if controls.maximize {
        let square = |size: f32| div().size(px(size)).border_1().border_color(t.text);
        let icon = if maximized {
            // Two overlapping windows: "restore".
            div()
                .relative()
                .size(px(11.))
                .child(square(8.).absolute().top_0().right_0())
                .child(square(8.).absolute().bottom_0().left_0().bg(t.surface))
        } else {
            square(10.)
        };
        buttons.push(
            control("maximize", t)
                .child(icon)
                .on_click(|_, window, _| window.zoom_window())
                .into_any_element(),
        );
    }
    buttons.push(
        control("close", t)
            .text_color(t.text)
            .child("✕")
            .on_click(|_, window, _| window.remove_window())
            .into_any_element(),
    );
    // Same width on both sides keeps the title centered.
    let side = px(buttons.len() as f32 * (BUTTON + 6.) + 8.);

    Some(
        div()
            .id("titlebar")
            .h(px(TITLEBAR_H))
            .flex_none()
            .flex()
            .items_center()
            .bg(t.surface)
            .border_b_1()
            .border_color(t.border)
            // Start the move on the first drag instead of on press, so the press
            // and release of a double-click still reach us.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, _| {
                    if ev.click_count == 2 {
                        this.titlebar_drag = false;
                        window.zoom_window();
                    } else {
                        this.titlebar_drag = true;
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.titlebar_drag = false),
            )
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, _| {
                this.titlebar_drag = false;
            }))
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, _| {
                if this.titlebar_drag {
                    this.titlebar_drag = false;
                    window.start_window_move();
                }
            }))
            .when(controls.window_menu, |d| {
                d.on_mouse_down(MouseButton::Right, |ev, window, _| {
                    window.show_window_menu(ev.position)
                })
            })
            .child(div().w(side).flex_none())
            .child(
                div()
                    .flex_1()
                    .flex()
                    .justify_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(t.text_muted)
                    .child(title.to_string()),
            )
            .child(
                div()
                    .w(side)
                    .flex_none()
                    .flex()
                    .justify_end()
                    .gap(px(6.))
                    .pr_2()
                    .children(buttons),
            )
            .into_any_element(),
    )
}

fn control(id: &'static str, t: &Theme) -> Stateful<gpui::Div> {
    let hover = t.surface_hover;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(BUTTON))
        .rounded_full()
        .hover(move |s| s.bg(hover))
        .cursor_pointer()
        // Keep the title bar's drag handler from seeing button presses.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// The window edge under `pos`, if it is inside the resize border. Tiled
/// (maximized or snapped) edges have no border.
fn resize_edge(pos: Point<Pixels>, size: Size<Pixels>, tiling: Tiling) -> Option<ResizeEdge> {
    let top = !tiling.top && pos.y < RESIZE_BORDER;
    let bottom = !tiling.bottom && pos.y >= size.height - RESIZE_BORDER;
    let left = !tiling.left && pos.x < RESIZE_BORDER;
    let right = !tiling.right && pos.x >= size.width - RESIZE_BORDER;
    Some(match (top, bottom, left, right) {
        (true, _, true, _) => ResizeEdge::TopLeft,
        (true, _, _, true) => ResizeEdge::TopRight,
        (_, true, true, _) => ResizeEdge::BottomLeft,
        (_, true, _, true) => ResizeEdge::BottomRight,
        (true, ..) => ResizeEdge::Top,
        (_, true, ..) => ResizeEdge::Bottom,
        (_, _, true, _) => ResizeEdge::Left,
        (_, _, _, true) => ResizeEdge::Right,
        _ => return None,
    })
}

fn resize_cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(x: f32, y: f32, tiling: Tiling) -> Option<ResizeEdge> {
        resize_edge(point(px(x), px(y)), Size::new(px(400.), px(300.)), tiling)
    }

    #[test]
    fn detects_edges_and_corners() {
        let free = Tiling::default();
        assert_eq!(edge(200., 150., free), None);
        assert_eq!(edge(200., 2., free), Some(ResizeEdge::Top));
        assert_eq!(edge(200., 298., free), Some(ResizeEdge::Bottom));
        assert_eq!(edge(2., 150., free), Some(ResizeEdge::Left));
        assert_eq!(edge(398., 150., free), Some(ResizeEdge::Right));
        assert_eq!(edge(2., 2., free), Some(ResizeEdge::TopLeft));
        assert_eq!(edge(398., 298., free), Some(ResizeEdge::BottomRight));
        assert_eq!(edge(9., 150., free), None);
    }

    #[test]
    fn tiled_edges_do_not_resize() {
        let maximized = Tiling {
            top: true,
            bottom: true,
            left: true,
            right: true,
        };
        assert_eq!(edge(2., 2., maximized), None);
        assert_eq!(edge(398., 298., maximized), None);

        let left_half = Tiling {
            left: true,
            top: true,
            bottom: true,
            right: false,
        };
        assert_eq!(edge(2., 150., left_half), None);
        assert_eq!(edge(398., 150., left_half), Some(ResizeEdge::Right));
    }
}
