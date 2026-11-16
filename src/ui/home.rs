//! The home page: a wireframe grid of the sample videos. Cards are absolutely positioned at
//! their spring-animated geometry (see `layout.rs`), and only the ones near the scroll viewport
//! are built.

use gpui::{Context, div, prelude::*, px, rgb, rgba};

use super::AppView;
use super::format;
use super::layout::{self, HomeLayout};
use super::theme::{ACCENT, FAINT, HOVER, MUTED, TRACK};
use crate::backend::media::VideoSummary;

pub fn render(
    layout: &HomeLayout,
    feed: &[VideoSummary],
    viewport_h: f32,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let show_targets = layout.show_targets;
    let visible: Vec<usize> = layout.visible(viewport_h).collect();
    let hud = format!(
        "{} cols · card {:.1}px · {}/{} visible · {} moving · step {:.1}ms{}   [F9 targets{}] [F10 slow-mo] [F12 frame stats]",
        layout.spec.columns,
        layout.spec.card_w,
        visible.len(),
        layout.cards.len(),
        layout.moving,
        layout.dt * 1000.0,
        if layout.slow_motion { " · SLOW-MO" } else { "" },
        if show_targets { " on" } else { "" },
    );

    let mut ghosts = Vec::new();
    let mut cards = Vec::with_capacity(visible.len());
    for ix in visible {
        let motion = layout.cards[ix];
        let moving = motion.is_moving();
        if show_targets && moving {
            ghosts.push(
                div()
                    .absolute()
                    .left(px(motion.x.target()))
                    .top(px(motion.y.target()))
                    .w(px(motion.w.target()))
                    .h(px(layout::card_h(motion.w.target())))
                    .border_1()
                    .border_dashed()
                    .border_color(rgba(0xe94b5a80)),
            );
        }
        let Some(video) = feed.get(ix % feed.len().max(1)) else { break };
        let highlight = show_targets && moving;
        cards.push(card(ix, video, motion.x.value(), motion.y.value(), motion.w.value(), highlight, cx));
    }

    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().flex_none().whitespace_nowrap().overflow_hidden().text_xs().text_color(rgb(MUTED)).child(hud))
        .child(
            div()
                .id("home-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&layout.scroll)
                .child(div().relative().w_full().h(px(layout.content_height())).children(ghosts).children(cards)),
        )
}

/// Wireframe card: outlined boxes and single-line text, so width changes never re-wrap lines.
fn card(
    ix: usize,
    video: &VideoSummary,
    x: f32,
    y: f32,
    w: f32,
    highlight: bool,
    cx: &mut Context<AppView>,
) -> impl IntoElement + use<> {
    let line = || div().px_2().whitespace_nowrap().overflow_hidden().text_ellipsis();
    let target = video.video.clone();
    let meta = [
        video.channel.clone(),
        video.views.map(|views| format!("{} views", format::compact_count(views))),
        video.age.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    div()
        .id(("card", ix))
        .absolute()
        .left(px(x))
        .top(px(y))
        .w(px(w))
        .h(px(layout::card_h(w)))
        .overflow_hidden()
        .border_1()
        .border_color(rgb(if highlight { ACCENT } else { TRACK }))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(HOVER)))
        .flex()
        .flex_col()
        .on_click(cx.listener(move |this, _, window, cx| this.open_video(target.clone(), window, cx)))
        .child(
            div()
                .relative()
                .flex_none()
                .h(px(layout::thumb_h(w)))
                .border_b_1()
                .border_color(rgb(TRACK))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(FAINT))
                .child(format!("#{ix}"))
                .when_some(video.duration, |thumb, duration| {
                    thumb.child(div().absolute().bottom_1().right_2().text_xs().child(format::clock(duration)))
                }),
        )
        .child(line().pt_1().text_sm().child(video.title.clone()))
        .child(line().text_xs().text_color(rgb(MUTED)).child(meta))
}
