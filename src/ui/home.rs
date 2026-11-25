//! The home page: a grid of the sites' recommendations (after any featured videos). Cards are
//! absolutely positioned at their spring-animated geometry (see `layout.rs`), and only the ones
//! near the scroll viewport are built.

use gpui::{Context, Entity, ImageSource, div, prelude::*, px, rgb, rgba};

use super::components::thumbnail;
use super::format;
use super::images::ImageCache;
use super::layout::{self, HomeLayout};
use super::theme::{ACCENT, HOVER, MUTED, TEXT, TRACK};
use super::{AppView, FeedStatus};
use crate::backend::media::VideoSummary;

pub fn render(
    layout: &HomeLayout,
    feed: &[VideoSummary],
    status: &FeedStatus,
    images: &Entity<ImageCache>,
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
        let image = video.thumbnail_url.as_deref().and_then(|url| images.read(cx).get(url));
        let geometry = (motion.x.value(), motion.y.value(), motion.w.value());
        cards.push(card(ix, video, image.map(ImageSource::from), geometry, highlight, cx));
    }
    let notice = match status {
        FeedStatus::Loading => Some("Loading recommendations…".to_string()),
        FeedStatus::Failed(err) => Some(format!("Couldn't load recommendations: {err}")),
        FeedStatus::Loaded if feed.is_empty() => Some("No recommendations.".to_string()),
        FeedStatus::Loaded => None,
    };

    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().flex_none().whitespace_nowrap().overflow_hidden().text_xs().text_color(rgb(MUTED)).child(hud))
        .when_some(notice, |page, notice| page.child(div().flex_none().text_sm().text_color(rgb(MUTED)).child(notice)))
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

/// A thumbnail over single-line text, so width changes never re-wrap lines.
fn card(
    ix: usize,
    video: &VideoSummary,
    image: Option<ImageSource>,
    (x, y, w): (f32, f32, f32),
    highlight: bool,
    cx: &mut Context<AppView>,
) -> impl IntoElement + use<> {
    let line = || div().px_2().whitespace_nowrap().overflow_hidden().text_ellipsis();
    let target = video.video.clone();
    let channel_ref = super::channel_link(video, cx);
    let rest = [video.views.map(|views| format!("{} views", format::compact_count(views))), video.age.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    // The channel's name opens the channel (and not the video) when clicked.
    let meta = div()
        .px_2()
        .flex()
        .whitespace_nowrap()
        .overflow_hidden()
        .text_xs()
        .text_color(rgb(MUTED))
        .when_some(video.channel.clone(), |line, name| {
            line.child(
                div()
                    .id(("card-channel", ix))
                    .min_w_0()
                    .truncate()
                    .when_some(channel_ref, |link, channel| {
                        link.cursor_pointer().hover(|style| style.text_color(rgb(TEXT))).on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_channel_target(channel.clone(), window, cx);
                            },
                        ))
                    })
                    .child(name),
            )
        })
        .child(
            div()
                .flex_none()
                .child(format!("{}{rest}", if video.channel.is_some() && !rest.is_empty() { " · " } else { "" })),
        );
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
        .child(thumbnail(image, video.duration.map(format::clock), px(w - 2.0), px(layout::thumb_h(w))))
        .child(line().pt_1().text_sm().child(video.title.clone()))
        .child(meta)
}
