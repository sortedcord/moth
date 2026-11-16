//! Small reusable elements.

use gpui::{ElementId, FontWeight, ImageSource, ObjectFit, Pixels, SharedString, div, img, prelude::*, px, rgb, rgba};

use super::theme::{ACCENT, BG_BOTTOM, DEEP, HOVER, MUTED, ON_ACCENT, TEXT, TRACK};

pub fn thumbnail(source: Option<ImageSource>, duration: Option<String>, w: Pixels, h: Pixels) -> impl IntoElement {
    div()
        .relative()
        .flex_none()
        .w(w)
        .h(h)
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(DEEP))
        .when_some(source, |frame, source| {
            frame.child(img(source).size_full().rounded_lg().object_fit(ObjectFit::Cover))
        })
        .when_some(duration, |frame, duration| {
            frame.child(
                div()
                    .absolute()
                    .bottom(px(6.0))
                    .right(px(6.0))
                    .px_1p5()
                    .rounded_md()
                    .bg(rgba(0x0b0f1acc))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(duration),
            )
        })
}

/// Initial on a color derived from the name, for channels without an avatar image.
pub fn avatar(name: &str, diameter: f32) -> impl IntoElement {
    let initial: SharedString = name.chars().next().unwrap_or('?').to_uppercase().to_string().into();
    let hue = name.bytes().fold(0u32, |hash, byte| hash.wrapping_mul(31).wrapping_add(byte as u32)) % 360;
    div()
        .size(px(diameter))
        .flex_none()
        .rounded_full()
        .bg(gpui::hsla(hue as f32 / 360.0, 0.55, 0.55, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(diameter * 0.45))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(ON_ACCENT))
        .child(initial)
}

/// A rounded label for read-only facts (e.g. like counts).
pub fn pill(text: String) -> impl IntoElement {
    div()
        .px_3()
        .py_1p5()
        .rounded_full()
        .bg(rgb(super::theme::PANEL))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(MUTED))
        .child(text)
}

/// One option in a segmented control.
pub fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_full()
        .cursor_pointer()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .map(|chip| {
            if selected {
                chip.bg(rgb(ACCENT)).text_color(rgb(ON_ACCENT))
            } else {
                chip.text_color(rgb(MUTED)).hover(|style| style.bg(rgb(HOVER)).text_color(rgb(TEXT)))
            }
        })
        .child(label.into())
}

/// The track a row of [`chip`]s sits in.
pub fn chip_row() -> gpui::Div {
    div().flex().flex_wrap().gap_1().p_1().rounded_full().bg(rgb(BG_BOTTOM))
}

pub fn toggle(id: impl Into<ElementId>, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(44.0))
        .h(px(24.0))
        .flex_none()
        .rounded_full()
        .cursor_pointer()
        .bg(rgb(if on { ACCENT } else { TRACK }))
        .p(px(3.0))
        .flex()
        .when(on, |track| track.justify_end())
        .child(div().size(px(18.0)).rounded_full().bg(rgb(ON_ACCENT)))
}
