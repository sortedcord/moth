//! Music's now-playing bar, along the bottom of every page while a queue plays, and the
//! expanded player it opens: the artwork (or the video, with Song/Video set to Video) and the
//! queue.

use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, FontWeight, ImageSource, MouseButton, MouseDownEvent, SharedString, canvas, div,
    prelude::*, px, relative, rgb,
};

use super::components::{chip, chip_row, thumbnail};
use super::format;
use super::icons::{Icon, icon, play_icon};
use super::images::ImageCache;
use super::music::Repeat;
use super::playback::{PlaybackSession, Status, VideoAnchor};
use super::theme::{ACCENT, HOVER, MUTED, PANEL, TEXT, TRACK, VIDEO_BG};
use super::watch::BoundsTracker;
use super::{AppView, Route};
use crate::backend::music::{Rating, Track};
use crate::backend::player::Rect;

pub const BAR_H: f32 = 76.0;

fn button(id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(36.0))
        .flex_none()
        .rounded_full()
        .hover(|style| style.bg(rgb(HOVER)))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
}

fn line(text: impl Into<SharedString>) -> gpui::Div {
    div().whitespace_nowrap().overflow_hidden().text_ellipsis().child(text.into())
}

fn track_image(track: &Track, images: &Entity<ImageCache>, cx: &mut Context<AppView>) -> Option<ImageSource> {
    track.thumbnail_url.as_deref().and_then(|url| images.read(cx).get(url)).map(ImageSource::from)
}

/// The bar, if music is playing.
pub fn bar(
    session: &Entity<PlaybackSession>,
    images: &Entity<ImageCache>,
    seek_bar: &BoundsTracker,
    expanded: bool,
    cx: &mut Context<AppView>,
) -> Option<AnyElement> {
    let state = session.read(cx);
    let queue = state.music()?;
    let track = queue.current()?.clone();
    let paused = matches!(state.status(), Status::Paused | Status::Ended);
    let loading = matches!(state.status(), Status::Resolving | Status::Buffering);
    let position = state.position().unwrap_or_default();
    let duration = state.duration().or(track.duration).unwrap_or_default();
    let progress =
        if duration.is_zero() { 0.0 } else { (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0) };
    let (shuffle, repeat, rating) = (queue.shuffle, queue.repeat, queue.rating);
    let (has_previous, has_next) = (queue.has_previous(), queue.has_next());
    let error = match state.status() {
        Status::Error(err) => Some(err.clone()),
        _ => None,
    };
    let image = track_image(&track, images, cx);
    let session = session.clone();
    let bounds = seek_bar.clone();
    let on = |active: bool| rgb(if active { ACCENT } else { MUTED });

    let seek = div()
        .id("music-seek")
        .w_full()
        .py_1()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, {
            let session = session.clone();
            move |event: &MouseDownEvent, _, cx| {
                if let Some(bounds) = bounds.get() {
                    let fraction = (event.position.x - bounds.origin.x) / bounds.size.width;
                    session.update(cx, |session, cx| session.seek_fraction(fraction, cx));
                }
            }
        })
        .child(
            div()
                .relative()
                .h(px(3.0))
                .rounded_full()
                .bg(rgb(TRACK))
                .child(div().absolute().left_0().top_0().h_full().w(relative(progress)).rounded_full().bg(rgb(ACCENT)))
                .child(seek_bar.element()),
        );

    let transport = div()
        .flex()
        .items_center()
        .gap_1()
        .flex_none()
        .child(
            button("music-previous")
                .when(!has_previous, |button| button.opacity(0.6))
                .on_click(cx.listener(|this, _, _, cx| this.session.update(cx, |session, cx| session.previous(cx))))
                .child(icon(Icon::Previous, 18.0, rgb(TEXT).into())),
        )
        .child(
            div()
                .id("music-play")
                .size(px(42.0))
                .flex_none()
                .rounded_full()
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(TEXT))
                .hover(|style| style.bg(rgb(MUTED)))
                .on_click(cx.listener(|this, _, _, cx| this.session.update(cx, |session, cx| session.toggle_pause(cx))))
                .child(if paused {
                    play_icon(13.0, rgb(PANEL).into()).into_any_element()
                } else {
                    dark_pause().into_any_element()
                }),
        )
        .child(
            button("music-next")
                .when(!has_next, |button| button.opacity(0.6))
                .on_click(cx.listener(|this, _, _, cx| this.session.update(cx, |session, cx| session.next(cx))))
                .child(icon(Icon::Next, 18.0, rgb(TEXT).into())),
        )
        .child(div().w(px(96.0)).pl_2().text_xs().text_color(rgb(MUTED)).child(if loading {
            "Loading…".to_string()
        } else {
            format!("{} / {}", format::clock(position), format::clock(duration))
        }));

    let byline = [Some(track.artists.clone()), track.detail.clone()].into_iter().flatten().collect::<Vec<_>>();
    let info = div()
        .id("music-info")
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_3()
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, window, cx| this.toggle_player(window, cx)))
        .child(thumbnail(image, None, px(48.0), px(48.0)))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(line(track.title.clone()).text_sm().font_weight(FontWeight::SEMIBOLD))
                .child(match error {
                    Some(err) => line(err).text_xs().text_color(rgb(ACCENT)),
                    None => line(byline.join(" • ")).text_xs().text_color(rgb(MUTED)),
                }),
        );

    let rate = |id: &'static str, kind: Icon, value: Rating, cx: &mut Context<AppView>| {
        button(id)
            .on_click(
                cx.listener(move |this, _, _, cx| this.session.update(cx, |session, cx| session.rate_track(value, cx))),
            )
            .child(icon(kind, 18.0, on(rating == value).into()))
    };
    let actions = div()
        .flex()
        .items_center()
        .gap_1()
        .flex_none()
        .child(rate("music-like", Icon::ThumbUp, Rating::Like, cx))
        .child(rate("music-dislike", Icon::ThumbDown, Rating::Dislike, cx))
        .child(div().w(px(12.0)))
        .child(
            button("music-shuffle")
                .on_click(
                    cx.listener(|this, _, _, cx| this.session.update(cx, |session, cx| session.toggle_shuffle(cx))),
                )
                .child(icon(Icon::Shuffle, 18.0, on(shuffle).into())),
        )
        .child(
            button("music-repeat")
                .on_click(cx.listener(|this, _, _, cx| this.session.update(cx, |session, cx| session.cycle_repeat(cx))))
                .child(icon(
                    if repeat == Repeat::One { Icon::RepeatOne } else { Icon::Repeat },
                    18.0,
                    on(repeat != Repeat::Off).into(),
                )),
        )
        .child(
            button("music-expand")
                .on_click(cx.listener(|this, _, window, cx| this.toggle_player(window, cx)))
                .child(icon(if expanded { Icon::ChevronDown } else { Icon::ChevronUp }, 18.0, rgb(TEXT).into())),
        );

    Some(
        div()
            .flex_none()
            .h(px(BAR_H))
            .rounded(px(18.0))
            .bg(rgb(PANEL))
            .px_4()
            .flex()
            .flex_col()
            .justify_center()
            .gap_1()
            .child(seek)
            .child(div().flex().items_center().gap_4().child(transport).child(info).child(actions))
            .into_any_element(),
    )
}

/// A pause glyph in the panel colour, for the light play button.
fn dark_pause() -> impl IntoElement {
    let bar = || div().w(px(3.5)).h(px(13.0)).rounded_sm().bg(rgb(PANEL));
    div().flex().gap(px(3.0)).child(bar()).child(bar())
}

/// The expanded player: artwork or video on the left, the queue on the right.
pub fn expanded(
    session: &Entity<PlaybackSession>,
    anchor: Rc<VideoAnchor>,
    images: &Entity<ImageCache>,
    viewport_h: f32,
    cx: &mut Context<AppView>,
) -> AnyElement {
    let state = session.read(cx);
    let Some(queue) = state.music() else { return div().into_any_element() };
    let Some(current) = queue.current().cloned() else { return div().into_any_element() };
    let show_video = queue.show_video;
    let has_video = state.has_video();
    let upcoming: Vec<(usize, Track)> =
        queue.upcoming().take(60).map(|(place, track)| (place, track.clone())).collect();
    let art_size = (viewport_h - 260.0).clamp(220.0, 560.0);
    let aspect = state.aspect().unwrap_or(16.0 / 9.0);
    // The current track's streams (in its audio track), to pick one for this track.
    let preferences = cx.global::<crate::settings::Settings>().stream_preferences();
    let state = session.read(cx);
    let current_audio =
        state.audio_stream().map(|audio| (audio.id, audio.track.as_ref().map(|track| track.id.clone())));
    let streams: Vec<(crate::backend::media::StreamId, String)> = match (state.catalog(), &current_audio) {
        (Some(catalog), Some((_, track))) => catalog
            .audio_in_track(track.as_deref(), &preferences)
            .into_iter()
            .map(|audio| (audio.id, audio.label()))
            .collect(),
        _ => Vec::new(),
    };
    let selected_audio = current_audio.map(|(id, _)| id);
    let image = current
        .artwork_url
        .as_deref()
        .and_then(|url| images.read(cx).get(url))
        .map(ImageSource::from)
        .or_else(|| track_image(&current, images, cx));

    let audio_chips = chip_row().children(streams.into_iter().enumerate().map(|(ix, (id, label))| {
        chip(("music-audio", ix), label, Some(id) == selected_audio)
            .on_click(cx.listener(move |this, _, _, cx| this.session.update(cx, |s, cx| s.choose_audio(id, cx))))
    }));

    let toggle = chip_row()
        .child(
            chip("music-song", "Song", !show_video)
                .on_click(cx.listener(|this, _, _, cx| this.session.update(cx, |s, cx| s.set_show_video(false, cx)))),
        )
        .child(
            chip("music-video", "Video", show_video)
                .on_click(cx.listener(|this, _, _, cx| this.session.update(cx, |s, cx| s.set_show_video(true, cx)))),
        );

    let stage = if show_video {
        let (w, h) = (art_size * aspect.max(1.0), art_size);
        div()
            .relative()
            .w(px(w))
            .h(px(h))
            .flex_none()
            .bg(rgb(VIDEO_BG))
            .rounded_lg()
            .flex()
            .items_center()
            .justify_center()
            .when(!has_video, |stage| stage.child(div().text_sm().text_color(rgb(MUTED)).child("Loading video…")))
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
            .into_any_element()
    } else {
        thumbnail(image, None, px(art_size), px(art_size)).into_any_element()
    };

    let row = |id: usize, track: &Track, place: Option<usize>, cx: &mut Context<AppView>| {
        let image = track_image(track, images, cx);
        div()
            .id(("queue", id))
            .flex()
            .items_center()
            .gap_3()
            .p_1p5()
            .rounded_lg()
            .when(place.is_none(), |row| row.bg(rgb(HOVER)))
            .when_some(place, |row, place| {
                row.cursor_pointer().hover(|style| style.bg(rgb(HOVER))).on_click(cx.listener(move |this, _, _, cx| {
                    this.session.update(cx, |session, cx| session.play_queued(place, cx));
                }))
            })
            .child(thumbnail(image, None, px(44.0), px(44.0)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(line(track.title.clone()).text_sm().font_weight(FontWeight::SEMIBOLD))
                    .child(line(track.artists.clone()).text_xs().text_color(rgb(MUTED))),
            )
            .when_some(track.duration, |row, duration| {
                row.child(div().flex_none().text_xs().text_color(rgb(MUTED)).child(format::clock(duration)))
            })
    };
    let mut rows = vec![row(usize::MAX, &current, None, cx).into_any_element()];
    rows.extend(upcoming.iter().map(|(place, track)| row(*place, track, Some(*place), cx).into_any_element()));

    div()
        .flex_1()
        .min_h_0()
        .flex()
        .gap_8()
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .items_center()
                .gap_4()
                .child(toggle)
                .child(stage)
                .child(
                    div()
                        .max_w(px(art_size.max(320.0)))
                        .flex()
                        .flex_col()
                        .items_center()
                        .child(line(current.title.clone()).text_xl().font_weight(FontWeight::BOLD))
                        .child(line(current.artists.clone()).text_sm().text_color(rgb(MUTED))),
                )
                .child(audio_chips),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_sm().font_weight(FontWeight::BOLD).text_color(rgb(MUTED)).child("UP NEXT"))
                .child(div().id("queue-scroll").flex_1().min_h_0().overflow_y_scroll().children(rows)),
        )
        .into_any_element()
}

impl AppView {
    /// Open or close the expanded player.
    pub(super) fn toggle_player(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.close_suggestions(cx);
        if matches!(self.route, Route::Player) {
            self.route = self.before_player.take().unwrap_or(Route::Music);
            // Video only plays where it can be seen.
            self.session.update(cx, |session, cx| session.set_music_visible(false, cx));
        } else {
            let previous = std::mem::replace(&mut self.route, Route::Player);
            if !matches!(previous, Route::Watch(_)) {
                self.before_player = Some(previous);
            }
            self.session.update(cx, |session, cx| session.set_music_visible(true, cx));
        }
        self.sync_address_placeholder(cx);
        self.publish(window, cx);
        cx.notify();
    }
}
