//! Title, channel, description, and the up-next list.

use gpui::{Context, FontWeight, ImageSource, SharedString, div, img, prelude::*, px, rgb};

use super::{Details, WatchPage};
use crate::backend::media::{ChannelTarget, VideoDetails, VideoSummary};
use crate::settings::Settings;
use crate::ui::components::{avatar, pill, thumbnail};
use crate::ui::format;
use crate::ui::theme::{ACCENT, FAINT, HOVER, MUTED, PANEL, TEXT};

pub fn render(page: &WatchPage, cx: &mut Context<WatchPage>) -> gpui::AnyElement {
    let details = match &page.details {
        Details::Loaded(details) => details.clone(),
        Details::Loading => {
            return div().text_sm().text_color(rgb(FAINT)).child("Loading video details…").into_any_element();
        }
        Details::Failed(err) => {
            return div()
                .rounded_xl()
                .bg(rgb(PANEL))
                .p_4()
                .text_sm()
                .text_color(rgb(ACCENT))
                .child(format!("Couldn't load this video: {err}"))
                .into_any_element();
        }
    };
    let session = page.session.read(cx);
    let tech: Option<SharedString> = match (session.decoder(), session.stream_summary()) {
        (Some(decoder), Some(summary)) => Some(format!("decoder: {decoder} · {summary}").into()),
        (None, Some(summary)) => Some(summary.into()),
        _ => None,
    };
    let avatar_image = details.channel.avatar_url.as_deref().and_then(|url| page.images.read(cx).get(url));
    let stats = stats_line(&details);
    let channel = details
        .channel
        .id
        .clone()
        .map(|id| ChannelTarget::Channel(crate::backend::media::ChannelRef { source: page.video().source, id }));

    div()
        .flex()
        .flex_col()
        .gap_4()
        .pb_4()
        .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).line_height(px(28.0)).child(details.title.clone()))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .id("watch-channel")
                        .flex()
                        .items_center()
                        .gap_3()
                        .rounded_xl()
                        .when_some(channel, |link, channel| {
                            link.cursor_pointer()
                                .hover(|style| style.bg(rgb(HOVER)))
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(super::OpenChannel(channel.clone()))))
                        })
                        .child(match avatar_image {
                            Some(image) => img(image).size(px(40.0)).flex_none().rounded_full().into_any_element(),
                            None => avatar(&details.channel.name, 40.0).into_any_element(),
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(details.channel.name.clone()))
                                .when_some(details.channel.subscribers, |column, subscribers| {
                                    column.child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(MUTED))
                                            .child(format!("{} subscribers", format::compact_count(subscribers))),
                                    )
                                }),
                        ),
                )
                .child(div().flex_1())
                .when_some(details.likes, |row, likes| {
                    row.child(pill(format!("{} likes", format::compact_count(likes))))
                }),
        )
        .child(
            div()
                .rounded_xl()
                .bg(rgb(PANEL))
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(stats))
                .child(div().text_sm().text_color(rgb(MUTED)).line_height(px(21.0)).child(details.description.clone())),
        )
        .when_some(tech, |column, tech| column.child(div().text_xs().text_color(rgb(FAINT)).child(tech)))
        .into_any_element()
}

fn stats_line(details: &VideoDetails) -> String {
    [
        details.views.map(|views| format!("{} views", format::grouped_count(views))),
        details.published.as_ref().map(format::published),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

pub fn up_next(page: &WatchPage, cx: &mut Context<WatchPage>) -> impl IntoElement + use<> {
    let hide_shorts = cx.global::<Settings>().hide_shorts;
    let related: Vec<VideoSummary> = match &page.details {
        Details::Loaded(details) => {
            details.related.iter().filter(|video| !(hide_shorts && video.is_short)).cloned().collect()
        }
        _ => Vec::new(),
    };
    let loading = matches!(page.details, Details::Loading);
    let rows: Vec<_> = related.iter().enumerate().map(|(ix, video)| recommendation(page, ix, video, cx)).collect();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().px_2().pb_2().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(FAINT)).child("UP NEXT"))
        .when(loading, |list| list.child(div().px_2().text_sm().text_color(rgb(FAINT)).child("Loading…")))
        .children(rows)
}

fn recommendation(
    page: &WatchPage,
    ix: usize,
    video: &VideoSummary,
    cx: &mut Context<WatchPage>,
) -> impl IntoElement + use<> {
    let target = video.video.clone();
    let image = video.thumbnail_url.as_deref().and_then(|url| page.images.read(cx).get(url));
    let meta = [video.views.map(|views| format!("{} views", format::compact_count(views))), video.age.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    div()
        .id(("rec", ix))
        .flex()
        .gap_3()
        .p_2()
        .rounded_xl()
        .cursor_pointer()
        .hover(|style| style.bg(rgb(HOVER)))
        .on_click(cx.listener(move |_, _, _, cx| cx.emit(super::OpenVideo(target.clone()))))
        .child(thumbnail(image.map(ImageSource::from), video.duration.map(format::clock), px(176.0), px(99.0)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .line_height(px(19.0))
                        .line_clamp(2)
                        .text_ellipsis()
                        .child(video.title.clone()),
                )
                .when_some(video.channel.clone(), |column, name| {
                    let channel_ref = crate::ui::channel_link(video, cx);
                    column.child(
                        div()
                            .id(("rec-channel", ix))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .when_some(channel_ref, |link, channel| {
                                link.cursor_pointer().hover(|style| style.text_color(rgb(TEXT))).on_click(cx.listener(
                                    move |_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(super::OpenChannel(channel.clone()));
                                    },
                                ))
                            })
                            .child(name),
                    )
                })
                .when(!meta.is_empty(), |column| column.child(div().text_xs().text_color(rgb(FAINT)).child(meta))),
        )
}
