//! Line icons, drawn as paths so they scale crisply.

use gpui::{Hsla, PathBuilder, canvas, div, point, prelude::*, px, rgb};

use super::theme::TEXT;

#[derive(Clone, Copy)]
pub enum Icon {
    Home,
    Settings,
    Music,
    Search,
    Close,
    Previous,
    Next,
    Shuffle,
    Repeat,
    RepeatOne,
    ThumbUp,
    ThumbDown,
    ChevronUp,
    ChevronDown,
}

/// Line icons drawn on a 24×24 grid.
pub fn icon(kind: Icon, size: f32, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let unit = bounds.size.width / 24.0;
            let at = |x: f32, y: f32| point(bounds.origin.x + unit * x, bounds.origin.y + unit * y);
            let radius = |r: f32| point(unit * r, unit * r);
            let mut path = PathBuilder::stroke(px(1.7));
            match kind {
                Icon::Home => {
                    path.add_polygon(&[at(3.0, 11.0), at(12.0, 3.5), at(21.0, 11.0)], false);
                    path.add_polygon(&[at(5.5, 9.5), at(5.5, 20.0), at(18.5, 20.0), at(18.5, 9.5)], false);
                    path.add_polygon(&[at(10.0, 20.0), at(10.0, 14.0), at(14.0, 14.0), at(14.0, 20.0)], false);
                }
                Icon::Settings => {
                    // Eight-toothed gear around a hub.
                    let polar = |radius: f32, degrees: f32| {
                        let (sin, cos) = degrees.to_radians().sin_cos();
                        at(12.0 + radius * cos, 12.0 + radius * sin)
                    };
                    let mut outline = Vec::with_capacity(32);
                    for tooth in 0..8 {
                        let center = tooth as f32 * 45.0;
                        outline.extend([
                            polar(7.4, center - 15.0),
                            polar(9.8, center - 8.5),
                            polar(9.8, center + 8.5),
                            polar(7.4, center + 15.0),
                        ]);
                    }
                    path.add_polygon(&outline, true);
                    path.move_to(at(12.0, 9.0));
                    path.arc_to(radius(3.0), px(0.0), false, true, at(12.0, 15.0));
                    path.arc_to(radius(3.0), px(0.0), false, true, at(12.0, 9.0));
                }
                Icon::Music => {
                    // Two eighth notes joined by a beam.
                    path.add_polygon(&[at(9.0, 17.0), at(9.0, 5.0), at(20.0, 3.0), at(20.0, 15.0)], false);
                    path.move_to(at(9.0, 9.0));
                    path.line_to(at(20.0, 7.0));
                    for (x, y) in [(6.5, 17.0), (17.5, 15.0)] {
                        path.move_to(at(x, y - 2.5));
                        path.arc_to(radius(2.5), px(0.0), false, true, at(x, y + 2.5));
                        path.arc_to(radius(2.5), px(0.0), false, true, at(x, y - 2.5));
                    }
                }
                Icon::Search => {
                    path.move_to(at(10.5, 4.0));
                    path.arc_to(radius(6.5), px(0.0), false, true, at(10.5, 17.0));
                    path.arc_to(radius(6.5), px(0.0), false, true, at(10.5, 4.0));
                    path.move_to(at(15.2, 15.2));
                    path.line_to(at(20.5, 20.5));
                }
                Icon::Close => {
                    path.move_to(at(6.0, 6.0));
                    path.line_to(at(18.0, 18.0));
                    path.move_to(at(18.0, 6.0));
                    path.line_to(at(6.0, 18.0));
                }
                Icon::Previous | Icon::Next => {
                    let flip = |x: f32| if matches!(kind, Icon::Next) { 24.0 - x } else { x };
                    path.add_polygon(&[at(flip(19.0), 5.0), at(flip(8.0), 12.0), at(flip(19.0), 19.0)], true);
                    path.move_to(at(flip(5.5), 5.0));
                    path.line_to(at(flip(5.5), 19.0));
                }
                Icon::Shuffle => {
                    path.move_to(at(3.0, 7.0));
                    path.line_to(at(7.0, 7.0));
                    path.line_to(at(15.0, 17.0));
                    path.line_to(at(20.0, 17.0));
                    path.move_to(at(3.0, 17.0));
                    path.line_to(at(7.0, 17.0));
                    path.line_to(at(15.0, 7.0));
                    path.line_to(at(20.0, 7.0));
                    for y in [7.0, 17.0] {
                        path.add_polygon(&[at(17.5, y - 2.5), at(20.5, y), at(17.5, y + 2.5)], false);
                    }
                }
                Icon::Repeat | Icon::RepeatOne => {
                    path.add_polygon(&[at(4.0, 13.0), at(4.0, 7.0), at(19.0, 7.0)], false);
                    path.add_polygon(&[at(16.0, 4.0), at(19.0, 7.0), at(16.0, 10.0)], false);
                    path.add_polygon(&[at(20.0, 11.0), at(20.0, 17.0), at(5.0, 17.0)], false);
                    path.add_polygon(&[at(8.0, 14.0), at(5.0, 17.0), at(8.0, 20.0)], false);
                    if matches!(kind, Icon::RepeatOne) {
                        path.move_to(at(11.0, 11.0));
                        path.line_to(at(12.5, 10.0));
                        path.line_to(at(12.5, 14.5));
                    }
                }
                Icon::ThumbUp | Icon::ThumbDown => {
                    let y = |y: f32| if matches!(kind, Icon::ThumbDown) { 24.0 - y } else { y };
                    path.add_polygon(&[at(3.0, y(10.0)), at(7.0, y(10.0)), at(7.0, y(20.0)), at(3.0, y(20.0))], true);
                    path.add_polygon(
                        &[
                            at(7.0, y(10.0)),
                            at(11.0, y(3.5)),
                            at(13.0, y(4.5)),
                            at(12.0, y(9.0)),
                            at(19.5, y(9.0)),
                            at(21.0, y(11.0)),
                            at(19.0, y(19.0)),
                            at(17.0, y(20.0)),
                            at(7.0, y(20.0)),
                        ],
                        false,
                    );
                }
                Icon::ChevronUp | Icon::ChevronDown => {
                    let y = |y: f32| if matches!(kind, Icon::ChevronDown) { 24.0 - y } else { y };
                    path.add_polygon(&[at(5.0, y(15.0)), at(12.0, y(8.0)), at(19.0, y(15.0))], false);
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        },
    )
    .size(px(size))
    .flex_none()
}

pub fn play_icon(size: f32, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let (x, y, w, h) = (bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height);
            let mut path = PathBuilder::fill();
            path.move_to(point(x + w * 0.12, y));
            path.line_to(point(x + w, y + h / 2.0));
            path.line_to(point(x + w * 0.12, y + h));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        },
    )
    .size(px(size))
}

/// Four corner brackets: pointing outwards to enter fullscreen, inwards to exit.
pub fn fullscreen_icon(exit: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let (x, y, s) = (bounds.origin.x, bounds.origin.y, bounds.size.width);
            let k = s * 0.36;
            let mut path = PathBuilder::stroke(px(1.8));
            // (corner, horizontal direction into the icon, vertical direction into the icon)
            for (cx, cy, dx, dy) in
                [(x, y, 1.0, 1.0), (x + s, y, -1.0, 1.0), (x, y + s, 1.0, -1.0), (x + s, y + s, -1.0, -1.0)]
            {
                if exit {
                    path.move_to(point(cx + k * dx, cy));
                    path.line_to(point(cx + k * dx, cy + k * dy));
                    path.line_to(point(cx, cy + k * dy));
                } else {
                    path.move_to(point(cx, cy + k * dy));
                    path.line_to(point(cx, cy));
                    path.line_to(point(cx + k * dx, cy));
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(TEXT));
            }
        },
    )
    .size(px(14.0))
}

pub fn pause_icon() -> impl IntoElement {
    let bar = || div().w(px(3.5)).h(px(12.0)).rounded_sm().bg(rgb(TEXT));
    div().flex().gap(px(3.0)).child(bar()).child(bar())
}
