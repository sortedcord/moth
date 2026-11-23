//! Play/pause, time, seek bar, settings gear and fullscreen.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{Bounds, Context, MouseButton, MouseDownEvent, Pixels, canvas, div, prelude::*, px, rgb};

use super::WatchPage;
use crate::ui::format;
use crate::ui::icons::{Icon, fullscreen_icon, icon, pause_icon, play_icon};
use crate::ui::playback::Status;
use crate::ui::theme::{ACCENT, HOVER, MUTED, PANEL, TEXT, TRACK};

/// Remembers an element's last painted bounds (for hit-testing and anchoring popovers).
#[derive(Clone, Default)]
pub struct BoundsTracker(Rc<Cell<Option<Bounds<Pixels>>>>);

impl BoundsTracker {
    pub fn get(&self) -> Option<Bounds<Pixels>> {
        self.0.get()
    }

    /// An invisible element filling its (relative) parent that records the parent's bounds.
    pub fn element(&self) -> impl IntoElement + use<> {
        let cell = self.0.clone();
        canvas(move |bounds, _, _| cell.set(Some(bounds)), |_, _, _, _| {}).absolute().size_full()
    }
}

fn round_button(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(34.0))
        .flex_none()
        .rounded_full()
        .hover(|style| style.bg(rgb(HOVER)))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
}

pub fn render(page: &WatchPage, fullscreen: bool, cx: &mut Context<WatchPage>) -> impl IntoElement + use<> {
    let session = page.session.read(cx);
    let paused = matches!(session.status(), Status::Paused | Status::Ended);
    let position = session.position().unwrap_or_default();
    let duration = session.duration().unwrap_or_default();
    let progress =
        if duration.is_zero() { 0.0 } else { (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0) };
    let seek_bar = page.seek_bar.clone();
    let menu_open = page.menu.is_some();

    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            round_button("play-pause")
                .bg(rgb(PANEL))
                .on_click(cx.listener(|this, _, _, cx| this.session.update(cx, |session, cx| session.toggle_pause(cx))))
                .child(if paused {
                    play_icon(12.0, rgb(TEXT).into()).into_any_element()
                } else {
                    pause_icon().into_any_element()
                }),
        )
        .child(div().text_xs().text_color(rgb(MUTED)).flex_none().child(format!(
            "{} / {}",
            format::clock(position),
            format::clock(duration)
        )))
        .child(
            // Seek bar: generous hit area, thin track.
            div()
                .id("seek")
                .flex_1()
                .py_2()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        if let Some(bounds) = seek_bar.get() {
                            let fraction = (event.position.x - bounds.origin.x) / bounds.size.width;
                            this.session.update(cx, |session, cx| session.seek_fraction(fraction, cx));
                        }
                    }),
                )
                .child(
                    div()
                        .relative()
                        .h(px(5.0))
                        .rounded_full()
                        .bg(rgb(TRACK))
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .h_full()
                                .w(gpui::relative(progress))
                                .rounded_full()
                                .bg(rgb(ACCENT)),
                        )
                        .child(page.seek_bar.element()),
                ),
        )
        .child(
            round_button("player-settings")
                .relative()
                .when(menu_open, |button| button.bg(rgb(HOVER)))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.toggle_menu(window, cx)))
                .when(menu_open, |button| button.on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx))))
                .child(icon(Icon::Settings, 17.0, rgb(if menu_open { TEXT } else { MUTED }).into()))
                .child(page.gear.element()),
        )
        .child(
            round_button("fullscreen")
                .on_click(|_, window, _| window.toggle_fullscreen())
                .child(fullscreen_icon(fullscreen)),
        )
}
