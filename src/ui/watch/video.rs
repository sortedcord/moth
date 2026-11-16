//! The video area. The player's subsurface is stacked above the window, at the rectangle this
//! element reports to the session's [`VideoAnchor`] as it paints.

use std::rc::Rc;

use gpui::{ClickEvent, Context, SharedString, canvas, div, prelude::*, px, rgb};

use super::WatchPage;
use crate::backend::player::Rect;
use crate::ui::playback::{Status, VideoAnchor};
use crate::ui::theme::{MUTED, VIDEO_BG};

/// Vertical space kept free besides the player (padding, top bar, controls and a peek of the
/// title), so tall videos don't push everything off-screen.
const RESERVED_H: f32 = 300.0;
const MIN_H: f32 = 180.0;

/// Player frame size: as wide as `column_w` at the video's aspect ratio (16:9 until it's known),
/// but never taller than leaves room for the controls and title; tall videos get narrower instead.
pub fn fit(aspect: Option<f32>, column_w: f32, viewport_h: f32) -> (f32, f32) {
    let aspect = aspect.unwrap_or(16.0 / 9.0).clamp(0.2, 5.0);
    let max_h = (viewport_h - RESERVED_H).max(MIN_H);
    let (w, h) = if column_w / aspect > max_h { (max_h * aspect, max_h) } else { (column_w, column_w / aspect) };
    (w.min(column_w).floor(), h.floor())
}

fn status_label(status: &Status) -> Option<SharedString> {
    match status {
        Status::Resolving => Some("Resolving stream…".into()),
        Status::Buffering => Some("Buffering…".into()),
        Status::Error(err) => Some(format!("Playback error: {err}").into()),
        _ => None,
    }
}

/// Double-click toggles fullscreen (the subsurface has an empty input region, so pointer events
/// reach this element).
pub fn surface(
    anchor: Rc<VideoAnchor>,
    status: &Status,
    w: f32,
    h: f32,
    cx: &mut Context<WatchPage>,
) -> impl IntoElement {
    div()
        .id("video")
        .relative()
        .w(px(w))
        .h(px(h))
        .flex_none()
        .bg(rgb(VIDEO_BG))
        .on_click(cx.listener(|_, event: &ClickEvent, window, _| {
            if event.click_count() == 2 {
                window.toggle_fullscreen();
            }
        }))
        .flex()
        .items_center()
        .justify_center()
        .when_some(status_label(status), |surface, label| {
            surface.child(div().px_6().text_sm().text_color(rgb(MUTED)).child(label))
        })
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, (), _, _| {
                    anchor.place(Rect {
                        x: f32::from(bounds.origin.x).round() as i32,
                        y: f32::from(bounds.origin.y).round() as i32,
                        w: f32::from(bounds.size.width).round() as i32,
                        h: f32::from(bounds.size.height).round() as i32,
                    });
                },
            )
            .absolute()
            .size_full(),
        )
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn player_takes_the_video_aspect_ratio() {
        // Spring is 2048x858: full column width, shorter than 16:9.
        assert_eq!(fit(Some(2048.0 / 858.0), 712.0, 898.0), (712.0, 298.0));
        // 16:9 at full width.
        assert_eq!(fit(Some(16.0 / 9.0), 712.0, 898.0), (712.0, 400.0));
        // Unknown yet: 16:9 placeholder.
        assert_eq!(fit(None, 712.0, 898.0), (712.0, 400.0));
        // Vertical 9:16 video: capped height, narrower frame.
        assert_eq!(fit(Some(9.0 / 16.0), 712.0, 898.0), (336.0, 598.0));
        // Tiny window: never wider than the column, never shorter than the floor.
        let (w, h) = fit(Some(9.0 / 16.0), 300.0, 400.0);
        assert!(w <= 300.0 && h == 180.0);
    }
}
