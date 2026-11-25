//! The watch page: player, controls, details and up next. One page per opened video, so its
//! state (details, menu, scroll) starts fresh each time.

mod controls;
pub use controls::BoundsTracker;
mod details;
mod menu;
mod stats_panel;
mod video;

use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, Subscription, Task, Window, WindowHandle,
    div, prelude::*, px, rgb,
};

use super::BackendHandle;
use super::actions::{
    Cancel, SeekBack, SeekBackLong, SeekForward, SeekForwardLong, ToggleFullscreen, TogglePause, TogglePlayerStats,
    WATCH,
};
use super::images::ImageCache;
use super::playback::{PlaybackSession, Status};
use super::theme::{ACCENT, DEEP, PAD};
use crate::backend::media::{ChannelTarget, VideoDetails, VideoRef};
use crate::desktop::Seek;
use crate::settings::Settings;
use menu::SettingsPopup;

/// Below this window width the page stacks into a single column.
const NARROW_W: f32 = 1000.0;
const FULLSCREEN_BAR_H: f32 = 52.0;

pub enum Details {
    Loading,
    Loaded(Arc<VideoDetails>),
    Failed(String),
}

/// Emitted when the user picks another video on the page.
pub struct OpenVideo(pub VideoRef);

/// Emitted when the user picks a channel on the page.
pub struct OpenChannel(pub ChannelTarget);

pub struct WatchPage {
    video: VideoRef,
    session: Entity<PlaybackSession>,
    images: Entity<ImageCache>,
    details: Details,
    menu: Option<WindowHandle<SettingsPopup>>,
    _menu_closed: Subscription,
    _window_bounds: Subscription,
    gear: BoundsTracker,
    seek_bar: BoundsTracker,
    show_stats: bool,
    focus: FocusHandle,
    _details: Task<()>,
}

impl EventEmitter<OpenVideo> for WatchPage {}
impl EventEmitter<OpenChannel> for WatchPage {}

impl Focusable for WatchPage {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl WatchPage {
    pub fn new(
        video: VideoRef,
        session: Entity<PlaybackSession>,
        images: Entity<ImageCache>,
        show_stats: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let fetch = cx.global::<BackendHandle>().0.details(&video);
        let details_task = cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |this, cx| this.details_loaded(result, cx)).ok();
        });
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        cx.observe(&images, |_, _, cx| cx.notify()).detach();
        session.update(cx, |session, cx| session.set_collect_stats(show_stats, cx));
        let weak = cx.weak_entity();
        let menu_closed = cx.on_window_closed(move |cx, id| {
            // Removing the popup can emit this event while the watch page is being updated.
            let weak = weak.clone();
            cx.defer(move |cx| {
                weak.update(cx, |this, cx| {
                    if this.menu.is_some_and(|menu| menu.window_id() == id) {
                        this.menu = None;
                        cx.notify();
                    }
                })
                .ok();
            });
        });
        // The popup is anchored to a snapshot of the gear bounds. A resize (including a
        // fullscreen transition) can move the gear, so dismiss rather than leave it adrift.
        let window_bounds = cx.observe_window_bounds(window, |this, _, cx| this.close_menu(cx));
        cx.on_release(|this, cx| this.remove_menu(cx)).detach();
        Self {
            video,
            session,
            images,
            details: Details::Loading,
            menu: None,
            _menu_closed: menu_closed,
            _window_bounds: window_bounds,
            gear: BoundsTracker::default(),
            seek_bar: BoundsTracker::default(),
            show_stats,
            focus: cx.focus_handle(),
            _details: details_task,
        }
    }

    pub fn video(&self) -> &VideoRef {
        &self.video
    }

    pub fn details(&self) -> Option<&VideoDetails> {
        match &self.details {
            Details::Loaded(details) => Some(details),
            _ => None,
        }
    }

    fn details_loaded(&mut self, result: anyhow::Result<VideoDetails>, cx: &mut Context<Self>) {
        self.details = match result {
            Ok(details) => {
                let urls: Vec<String> = details
                    .related
                    .iter()
                    .filter_map(|video| video.thumbnail_url.clone())
                    .chain(details.channel.avatar_url.clone())
                    .collect();
                self.images.update(cx, |images, cx| urls.into_iter().for_each(|url| images.load(url, cx)));
                Details::Loaded(Arc::new(details))
            }
            Err(err) => Details::Failed(format!("{err:#}")),
        };
        cx.notify();
    }

    fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.close_menu(cx);
        } else if let Some(gear) = self.gear.get() {
            match SettingsPopup::open(self.session.clone(), gear, window, cx) {
                Ok(menu) => {
                    self.menu = Some(menu);
                    cx.notify();
                }
                Err(err) => eprintln!("moth: failed to open player settings: {err:#}"),
            }
        }
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.remove_menu(cx);
            cx.notify();
        }
    }

    fn remove_menu(&mut self, cx: &mut App) {
        if let Some(menu) = self.menu.take() {
            menu.update(cx, |_, window, _| window.remove_window()).ok();
        }
    }

    // ── Actions ──────────────────────────────────────────────────────────────

    fn seek_by(&mut self, seconds: f64, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| session.seek(Seek::By(seconds), cx));
    }

    fn on_toggle_pause(&mut self, _: &TogglePause, _: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| session.toggle_pause(cx));
    }

    fn on_seek_back(&mut self, _: &SeekBack, _: &mut Window, cx: &mut Context<Self>) {
        let step = cx.global::<Settings>().seek_step;
        self.seek_by(-f64::from(step), cx);
    }

    fn on_seek_forward(&mut self, _: &SeekForward, _: &mut Window, cx: &mut Context<Self>) {
        let step = cx.global::<Settings>().seek_step;
        self.seek_by(f64::from(step), cx);
    }

    fn on_seek_back_long(&mut self, _: &SeekBackLong, _: &mut Window, cx: &mut Context<Self>) {
        let step = cx.global::<Settings>().seek_step_long;
        self.seek_by(-f64::from(step), cx);
    }

    fn on_seek_forward_long(&mut self, _: &SeekForwardLong, _: &mut Window, cx: &mut Context<Self>) {
        let step = cx.global::<Settings>().seek_step_long;
        self.seek_by(f64::from(step), cx);
    }

    fn on_toggle_fullscreen(&mut self, _: &ToggleFullscreen, window: &mut Window, _: &mut Context<Self>) {
        window.toggle_fullscreen();
    }

    fn on_toggle_stats(&mut self, _: &TogglePlayerStats, _: &mut Window, cx: &mut Context<Self>) {
        self.show_stats = !self.show_stats;
        let show = self.show_stats;
        self.session.update(cx, |session, cx| session.set_collect_stats(show, cx));
    }

    fn on_cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.close_menu(cx);
        } else if window.is_fullscreen() {
            window.toggle_fullscreen();
        } else {
            cx.propagate();
        }
    }

    // ── Layout ───────────────────────────────────────────────────────────────

    fn player_column(&self, column_w: f32, viewport_h: f32, cx: &mut Context<Self>) -> gpui::Div {
        let session = self.session.read(cx);
        let (frame_w, frame_h) = video::fit(session.aspect(), column_w, viewport_h);
        let status = session.status().clone();
        let stats = self.show_stats.then(|| stats_panel::render(session)).flatten();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().justify_center().child(video::surface(session.anchor(), &status, frame_w, frame_h, cx)))
            .child(controls::render(self, false, cx))
            .when_some(playback_error(&status), |column, error| column.child(error))
            .when_some(stats, |column, panel| column.child(panel))
    }

    fn wide(&self, viewport_w: f32, viewport_h: f32, cx: &mut Context<Self>) -> AnyElement {
        // The player column takes half the window width.
        let column_w = ((viewport_w - 2.0 * PAD) * 0.5).floor();
        div()
            .size_full()
            .flex()
            .flex_row()
            .gap_6()
            .child(
                self.player_column(column_w, viewport_h, cx)
                    .w(px(column_w))
                    .flex_none()
                    .h_full()
                    .child(div().id("details").flex_1().min_h_0().overflow_y_scroll().child(details::render(self, cx))),
            )
            .child(div().id("recs").flex_1().min_w_0().h_full().overflow_y_scroll().child(details::up_next(self, cx)))
            .into_any_element()
    }

    /// Narrow windows (like YouTube's own layout): full-width player, then details and up next
    /// in one scrolling column.
    fn narrow(&self, viewport_w: f32, viewport_h: f32, cx: &mut Context<Self>) -> AnyElement {
        let column_w = (viewport_w - 2.0 * PAD).floor();
        self.player_column(column_w, viewport_h, cx)
            .size_full()
            .child(div().id("details").flex_1().min_h_0().overflow_y_scroll().child(
                div().flex().flex_col().gap_4().child(details::render(self, cx)).child(details::up_next(self, cx)),
            ))
            .into_any_element()
    }

    /// Video fills the screen above a slim control bar.
    fn fullscreen(&self, w: f32, h: f32, cx: &mut Context<Self>) -> AnyElement {
        let session = self.session.read(cx);
        let status = session.status().clone();
        let anchor = session.anchor();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(video::surface(anchor, &status, w, (h - FULLSCREEN_BAR_H).max(0.0), cx))
            .child(
                div()
                    .h(px(FULLSCREEN_BAR_H))
                    .flex_none()
                    .px_4()
                    .flex()
                    .items_center()
                    .bg(rgb(DEEP))
                    .child(div().flex_1().child(controls::render(self, true, cx)))
                    .when_some(playback_error(&status), |bar, error| bar.child(error)),
            )
            .into_any_element()
    }
}

fn playback_error(status: &Status) -> Option<impl IntoElement + use<>> {
    let Status::Error(err) = status else { return None };
    Some(div().text_xs().text_color(rgb(ACCENT)).line_clamp(2).text_ellipsis().child(format!("Playback error: {err}")))
}

impl Render for WatchPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let (w, h) = (f32::from(viewport.width), f32::from(viewport.height));
        let body = if window.is_fullscreen() {
            self.fullscreen(w, h, cx)
        } else if w < NARROW_W {
            self.narrow(w, h, cx)
        } else {
            self.wide(w, h, cx)
        };
        div()
            .id("watch")
            .key_context(WATCH)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_toggle_pause))
            .on_action(cx.listener(Self::on_seek_back))
            .on_action(cx.listener(Self::on_seek_forward))
            .on_action(cx.listener(Self::on_seek_back_long))
            .on_action(cx.listener(Self::on_seek_forward_long))
            .on_action(cx.listener(Self::on_toggle_fullscreen))
            .on_action(cx.listener(Self::on_toggle_stats))
            .on_action(cx.listener(Self::on_cancel))
            .size_full()
            .child(body)
    }
}
