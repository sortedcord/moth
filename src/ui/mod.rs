//! The gpui frontend. Talks to sites only through [`crate::backend::Backend`] and to the player
//! through [`playback::PlaybackSession`]; reports to the desktop through [`crate::desktop`].
//!
//! [`AppView`] is the window's root: it owns navigation (home, watch, settings), the top bar and
//! the sidebar, and routes external commands. Pages with state of their own are entities.

mod actions;
mod components;
mod format;
mod home;
mod icons;
mod images;
mod layout;
mod playback;
mod presence;
mod settings_page;
mod sidebar;
mod text_input;
mod theme;
mod watch;

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Bounds, ClickEvent, Context, Entity, FocusHandle, Focusable as _, FontWeight, Global, SharedString,
    Subscription, Task, Window, WindowBounds, WindowDecorations, WindowOptions, div, linear_color_stop,
    linear_gradient, prelude::*, px, rgb, size,
};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

use crate::backend::Backend;
use crate::backend::media::{Link, VideoRef, VideoSummary};
use crate::backend::player::WaylandTarget;
use crate::desktop::status::{Hub, NowPlaying, Page, VideoInfo};
use crate::desktop::{Command, socket};
use crate::dev::DevOptions;
use crate::settings::Settings;
use actions::{APP, Cancel, CycleFrameOverlay, FocusAddress, OpenSettings, ToggleLayoutTargets, ToggleSlowMotion};
use images::ImageCache;
use layout::HomeLayout;
use playback::PlaybackSession;
use text_input::{Submit, TextInput};
use theme::{ACCENT, BG_BOTTOM, BG_TOP, FAINT, FOCUS_RING, FONT, HOVER, PAD, PANEL, TEXT, VIDEO_BG};
use watch::{OpenVideo, WatchPage};

impl Global for Settings {}
impl Global for DevOptions {}

/// The backend, as a gpui global.
pub struct BackendHandle(pub Arc<Backend>);

impl Global for BackendHandle {}

/// Start the app. `open` is a link to open right away (from the command line).
pub fn run(
    backend: Arc<Backend>,
    hub: Arc<Hub>,
    commands: async_channel::Receiver<Command>,
    open: Option<String>,
    dev: DevOptions,
) {
    let app = gpui::Application::with_platform(gpui_linux::linux_platform(gpui::WindowingModes::WAYLAND));
    app.run(move |cx: &mut App| {
        if let Err(err) = cx.text_system().add_fonts(theme::FONT_FILES.map(Cow::Borrowed).to_vec()) {
            eprintln!("moth: failed to load fonts: {err:#}");
        }
        text_input::bind_keys(cx);
        actions::bind(cx);
        cx.set_global(Settings::load());
        cx.set_global(BackendHandle(backend));
        cx.set_global(dev);

        let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("moth".into()),
            window_decorations: Some(WindowDecorations::Server),
            window_min_size: Some(size(px(640.0), px(480.0))),
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| cx.new(|cx| AppView::new(hub, commands, open, window, cx)));
        if let Err(err) = opened {
            eprintln!("moth: failed to open window: {err:#}");
            cx.quit();
            return;
        }
        cx.on_window_closed(|cx, _| quit(cx)).detach();
        cx.activate(true);
    });
}

fn quit(cx: &mut App) {
    socket::remove();
    cx.quit();
}

/// The window's Wayland handles, for embedding the video.
fn wayland_target(window: &Window) -> Result<WaylandTarget, String> {
    let surface = match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Wayland(handle)) => handle.surface,
        Ok(_) => return Err("not a Wayland window".into()),
        Err(err) => return Err(format!("no window handle: {err}")),
    };
    let display = match HasDisplayHandle::display_handle(window).map(|handle| handle.as_raw()) {
        Ok(RawDisplayHandle::Wayland(handle)) => handle.display,
        Ok(_) => return Err("not a Wayland display".into()),
        Err(err) => return Err(format!("no display handle: {err}")),
    };
    // SAFETY: the window owns both and outlives the session (which lives inside the window).
    Ok(unsafe { WaylandTarget::new(display, surface) })
}

enum Route {
    Home,
    Watch(Entity<WatchPage>),
    Settings,
}

pub struct AppView {
    route: Route,
    session: Entity<PlaybackSession>,
    images: Entity<ImageCache>,
    address: Entity<TextInput>,
    address_error: Option<SharedString>,
    settings_error: Option<SharedString>,
    layout: HomeLayout,
    feed: Vec<VideoSummary>,
    focus: FocusHandle,
    hub: Arc<Hub>,
    /// Whether gpui says the window is visible, and whether its frames are being shown (see
    /// [`presence`]): video is suspended unless both.
    platform_shown: bool,
    frames_shown: bool,
    _presence: Option<Task<()>>,
    _resolve: Option<Task<()>>,
    _page_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
    _commands: Task<()>,
}

impl AppView {
    fn new(
        hub: Arc<Hub>,
        commands: async_channel::Receiver<Command>,
        open: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let dev = cx.global::<DevOptions>().clone();
        if dev.frame_stats {
            window.set_debug_frame_overlay_mode(gpui::DebugFrameOverlayMode::Full);
        }
        let feed = cx.global::<BackendHandle>().0.home_feed();
        let target = wayland_target(window);
        let session = cx.new(|cx| PlaybackSession::new(target, cx));
        let address = cx.new(|cx| {
            TextInput::new(
                "Paste a video link and press Enter",
                rgb(TEXT).into(),
                rgb(FAINT).into(),
                rgb(ACCENT).into(),
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe_in(&address, window, |this, _, event: &Submit, window, cx| {
                this.submit_address(event.0.to_string(), window, cx);
            }),
            cx.observe_in(&session, window, |this, _, window, cx| {
                this.publish(window, cx);
                this.update_presence(window, cx);
                cx.notify();
            }),
            cx.observe_window_visibility(window, |this, visibility, _, cx| {
                this.platform_shown = visibility.is_visible();
                this.apply_shown(cx);
            }),
            // Fullscreen changes come with a resize.
            cx.observe_window_bounds(window, |this, window, cx| this.publish(window, cx)),
        ];
        // Links from `moth <url>` / the browser extension, and MPRIS controls.
        let commands = cx.spawn_in(window, async move |this, cx| {
            while let Ok(command) = commands.recv().await {
                if this.update_in(cx, |this, window, cx| this.handle_command(command, window, cx)).is_err() {
                    break;
                }
            }
        });

        let mut view = Self {
            route: Route::Home,
            session,
            images: cx.new(|_| ImageCache::default()),
            address,
            address_error: None,
            settings_error: None,
            layout: HomeLayout::new(dev.cards.unwrap_or(feed.len())),
            feed,
            focus,
            hub,
            platform_shown: window.visibility().is_visible(),
            frames_shown: true,
            _presence: None,
            _resolve: None,
            _page_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
            _commands: commands,
        };
        if let Some(link) = open {
            view.submit_address(link, window, cx);
        } else if let Some(target) = dev.open {
            match target.parse::<usize>() {
                Ok(ix) if !view.feed.is_empty() => {
                    let video = view.feed[ix.min(view.feed.len() - 1)].video.clone();
                    view.open_video(video, window, cx);
                }
                _ => view.submit_address(target, window, cx),
            }
        }
        view
    }

    // ── Navigation ───────────────────────────────────────────────────────────

    fn submit_address(&mut self, input: String, window: &mut Window, cx: &mut Context<Self>) {
        self.address_error = None;
        let resolve = cx.global::<BackendHandle>().0.resolve(input);
        self._resolve = Some(cx.spawn_in(window, async move |this, cx| {
            let result = resolve.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(Link { video, start }) => this.open_video_at(video, start, window, cx),
                Err(err) => {
                    this.address_error = Some(format!("{err:#}").into());
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn open_video(&mut self, video: VideoRef, window: &mut Window, cx: &mut Context<Self>) {
        self.open_video_at(video, None, window, cx);
    }

    fn open_video_at(&mut self, video: VideoRef, start: Option<Duration>, window: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| session.open(video.clone(), start, cx));
        let link = cx.global::<BackendHandle>().0.web_url(&video).unwrap_or_default();
        self.address.update(cx, |input, cx| input.set_text(link, window, cx));
        self.address_error = None;

        let show_stats = cx.global::<DevOptions>().player_stats.unwrap_or(cx.global::<Settings>().player_stats);
        let (session, images) = (self.session.clone(), self.images.clone());
        let page = cx.new(|cx| WatchPage::new(video, session, images, show_stats, cx));
        self._page_subscriptions = vec![
            cx.subscribe_in(&page, window, |this, _, event: &OpenVideo, window, cx| {
                this.open_video(event.0.clone(), window, cx);
            }),
            // Title and channel arrive with the details.
            cx.observe_in(&page, window, |this, _, window, cx| this.publish(window, cx)),
        ];
        window.focus(&page.focus_handle(cx), cx);
        self.route = Route::Watch(page);
        self.publish(window, cx);
        cx.notify();
    }

    fn leave_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| session.close(cx));
        self._page_subscriptions.clear();
        self.address.update(cx, |input, cx| input.set_text("", window, cx));
        window.focus(&self.focus, cx);
    }

    fn go_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_watch(window, cx);
        self.route = Route::Home;
        self.publish(window, cx);
        cx.notify();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.route, Route::Settings) {
            return;
        }
        if window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        self.leave_watch(window, cx);
        self.route = Route::Settings;
        self.settings_error = None;
        self.publish(window, cx);
        cx.notify();
    }

    fn update_settings(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        cx.update_global::<Settings, _>(|settings, _| change(settings));
        self.settings_error =
            cx.global::<Settings>().save().err().map(|err| format!("Couldn't save settings: {err:#}").into());
        cx.notify();
    }

    // ── Desktop integration ──────────────────────────────────────────────────

    fn handle_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        match command {
            Command::Open(link) => {
                // Wayland only lets us ask for attention here, not take focus.
                window.activate_window();
                self.submit_address(link, window, cx);
            }
            Command::Play => session.update(cx, |session, cx| session.play(cx)),
            Command::Pause => session.update(cx, |session, cx| session.pause(cx)),
            Command::PlayPause => session.update(cx, |session, cx| session.toggle_pause(cx)),
            Command::Stop => {
                if matches!(self.route, Route::Watch(_)) {
                    self.go_home(window, cx);
                }
            }
            Command::Seek(seek) => session.update(cx, |session, cx| session.seek(seek, cx)),
            Command::SetRate(rate) => session.update(cx, |session, cx| session.set_rate(rate, cx)),
            Command::Raise => window.activate_window(),
            // Quitting this way skips the window-closed hook.
            Command::Quit => quit(cx),
        }
    }

    /// Hand the current state to `moth status` / MPRIS (they only act on changes).
    /// Check whether the window is on screen while there's video to suspend, and only then: each
    /// check draws a frame.
    fn update_presence(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.session.read(cx).wants_presence(cx);
        if wanted == self._presence.is_some() {
            return;
        }
        self._presence = wanted.then(|| {
            presence::watch(window, cx, |this: &mut Self, shown, cx| {
                this.frames_shown = shown;
                this.apply_shown(cx);
            })
        });
        self.frames_shown = true;
        self.apply_shown(cx);
    }

    fn apply_shown(&mut self, cx: &mut Context<Self>) {
        let shown = self.platform_shown && self.frames_shown;
        self.session.update(cx, |session, cx| session.set_shown(shown, cx));
    }

    fn publish(&self, window: &Window, cx: &App) {
        self.hub.publish(self.now_playing(window, cx));
    }

    fn now_playing(&self, window: &Window, cx: &App) -> NowPlaying {
        let session = self.session.read(cx);
        let backend = &cx.global::<BackendHandle>().0;
        let (status, error) = session.status().report();
        let watching = match &self.route {
            Route::Watch(page) => Some(page.read(cx)),
            _ => None,
        };
        let video = watching.map(|page| {
            let details = page.details();
            VideoInfo {
                source: page.video().source,
                id: page.video().id.clone(),
                url: backend.web_url(page.video()),
                title: details.map(|details| details.title.clone()),
                channel: details.map(|details| details.channel.name.clone()),
                thumbnail: backend.thumbnail_url(page.video()),
            }
        });
        NowPlaying {
            running: true,
            page: match self.route {
                Route::Home => Page::Home,
                Route::Watch(_) => Page::Watch,
                Route::Settings => Page::Settings,
            },
            status,
            error,
            position: video.as_ref().and(session.position()).map(|position| position.as_secs_f64()),
            duration: video.as_ref().and(session.duration()).map(|duration| duration.as_secs_f64()),
            stream: video.as_ref().and(session.stream_info()),
            video,
            rate: session.rate(),
            fullscreen: window.is_fullscreen(),
        }
    }

    // ── Actions ──────────────────────────────────────────────────────────────

    fn on_focus_address(&mut self, _: &FocusAddress, window: &mut Window, cx: &mut Context<Self>) {
        self.address.update(cx, |input, cx| input.focus_and_select_all(window, cx));
    }

    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
    }

    /// Esc that nothing closer handled: leave the address bar.
    fn on_cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.address.focus_handle(cx).is_focused(window) {
            match &self.route {
                Route::Watch(page) => window.focus(&page.focus_handle(cx), cx),
                _ => window.focus(&self.focus, cx),
            }
        }
    }

    fn on_toggle_layout_targets(&mut self, _: &ToggleLayoutTargets, _: &mut Window, cx: &mut Context<Self>) {
        self.layout.show_targets = !self.layout.show_targets;
        cx.notify();
    }

    fn on_toggle_slow_motion(&mut self, _: &ToggleSlowMotion, _: &mut Window, cx: &mut Context<Self>) {
        self.layout.slow_motion = !self.layout.slow_motion;
        cx.notify();
    }

    fn on_cycle_frame_overlay(&mut self, _: &CycleFrameOverlay, window: &mut Window, _: &mut Context<Self>) {
        window.cycle_debug_frame_overlay_mode();
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    fn top_bar(&self, back: bool, address_focused: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .flex()
            .items_center()
            .gap_4()
            .h(px(44.0))
            .flex_none()
            .when(back, |bar| {
                bar.child(
                    div()
                        .id("back")
                        .px_4()
                        .h(px(38.0))
                        .rounded_full()
                        .bg(rgb(PANEL))
                        .hover(|style| style.bg(rgb(HOVER)))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child("← Home")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go_home(window, cx))),
                )
            })
            .child(
                div()
                    .flex_1()
                    .max_w(px(560.0))
                    .h(px(38.0))
                    .rounded_full()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(if address_focused { FOCUS_RING } else { PANEL }))
                    .px_4()
                    .flex()
                    .items_center()
                    .text_sm()
                    .line_height(px(20.0))
                    .child(self.address.clone()),
            )
            .when_some(self.address_error.clone(), |bar, error| {
                bar.child(div().text_xs().text_color(rgb(ACCENT)).max_w(px(320.0)).truncate().child(error))
            })
    }

    fn root(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .id("root")
            .key_context(APP)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_focus_address))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_cancel))
            .on_action(cx.listener(Self::on_toggle_layout_targets))
            .on_action(cx.listener(Self::on_toggle_slow_motion))
            .on_action(cx.listener(Self::on_cycle_frame_overlay))
            .size_full()
            .font_family(FONT)
            .text_color(rgb(TEXT))
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let (viewport_w, viewport_h) = (f32::from(viewport.width), f32::from(viewport.height));
        let address_focused = self.address.focus_handle(cx).is_focused(window);

        let body = match &self.route {
            Route::Watch(page) if window.is_fullscreen() => {
                return self.root(cx).bg(rgb(VIDEO_BG)).child(page.clone());
            }
            Route::Watch(page) => div()
                .size_full()
                .flex()
                .flex_col()
                .gap_4()
                .child(self.top_bar(true, address_focused, cx))
                .child(page.clone()),
            Route::Home | Route::Settings => {
                let animate = cx.global::<Settings>().animations_enabled(cx.reduce_motion());
                if self.layout.step(viewport_w, animate) {
                    window.request_animation_frame();
                }
                let (entry, content) = match self.route {
                    Route::Settings => {
                        let settings = cx.global::<Settings>().clone();
                        let error = self.settings_error.clone();
                        (sidebar::Entry::Settings, settings_page::render(&settings, error, cx).into_any_element())
                    }
                    _ => (
                        sidebar::Entry::Home,
                        home::render(&self.layout, &self.feed, viewport_h, cx).into_any_element(),
                    ),
                };
                div()
                    .size_full()
                    .flex()
                    .flex_row()
                    .gap(px(PAD))
                    .child(sidebar::render(self.layout.sidebar_w.value(), entry, cx))
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(self.top_bar(false, address_focused, cx))
                            .child(content),
                    )
            }
        };

        self.root(cx)
            .p(px(PAD))
            .bg(linear_gradient(160.0, linear_color_stop(rgb(BG_TOP), 0.0), linear_color_stop(rgb(BG_BOTTOM), 1.0)))
            .child(body)
    }
}
