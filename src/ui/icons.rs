//! Line icons, drawn as paths so they scale crisply.

use gpui::{Hsla, PathBuilder, canvas, div, point, prelude::*, px, rgb};

use super::theme::TEXT;

#[derive(Clone, Copy)]
pub enum Icon {
    Home,
    Settings,
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
