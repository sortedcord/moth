mod data;
mod ipc;
mod player;
mod settings;
mod text_input;
mod youtube;

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, Focusable as _, FontWeight, Hsla, Image, ImageFormat,
    ImageSource, KeyDownEvent, MouseButton, MouseDownEvent, ObjectFit, PathBuilder, Pixels, SharedString,
    Subscription, Task, Window, WindowBounds, WindowDecorations, WindowOptions, anchored, canvas, deferred, div, img,
    linear_color_stop, linear_gradient, point, prelude::*, px, rgb, rgba, size,
};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

use data::VIDEOS;
use player::{Player, PlayerEvent, Rect, Start, StreamUrls};
use settings::{RESOLUTIONS, Settings};
use text_input::{Submit, TextInput};
use youtube::{RelatedVideo, StreamCatalog, WatchPage, YouTube};

// ── Palette (from the mock, minus the glass) ────────────────────────────────
const BG_TOP: u32 = 0x1d2540;
const BG_BOTTOM: u32 = 0x131929;
const PANEL: u32 = 0x212a43;
const HOVER: u32 = 0x29334f;
const TRACK: u32 = 0x343e5c;
const TEXT: u32 = 0xe8ebf4;
const MUTED: u32 = 0x9aa3bb;
const FAINT: u32 = 0x6b7490;
const ACCENT: u32 = 0xe94b5a;

const SIDEBAR_W: f32 = 220.0;
/// Vertical space kept free below/above the player: padding, top bar, controls and a peek of
/// the title, so tall videos don't push everything off-screen.
const PLAYER_RESERVED_H: f32 = 300.0;
/// Width of the collapsed sidebar (icon rail).
const RAIL_W: f32 = 76.0;
/// Below this window width the watch page stacks into a single column and the sidebar collapses.
const NARROW_W: f32 = 1000.0;
const SWITCHED_NOTE: &str = " · switched to web-client URLs for seeking";
const PAD: f32 = 16.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if ipc::is_native_host_invocation(&args) {
        std::process::exit(ipc::run_native_host());
    }
    // `moth <url>`: hand it to a running instance if there is one.
    let url_arg = args.into_iter().find(|arg| !arg.starts_with('-'));
    if let Some(url) = &url_arg
        && ipc::send_to_running(url).is_ok()
    {
        return;
    }
    let incoming = match ipc::listen() {
        Ok(incoming) => Some(incoming),
        Err(err) => {
            eprintln!("moth: links from other launches won't reach this window: {err:#}");
            None
        }
    };

    if let Err(err) = gstreamer::init() {
        eprintln!("moth: failed to initialize GStreamer: {err}");
        std::process::exit(1);
    }
    let youtube = match YouTube::new() {
        Ok(youtube) => youtube,
        Err(err) => {
            eprintln!("moth: failed to set up the YouTube client: {err:#}");
            std::process::exit(1);
        }
    };

    let app = gpui::Application::with_platform(gpui_linux::linux_platform(gpui::WindowingModes::WAYLAND));
    app.run(move |cx: &mut App| {
        if let Err(err) = cx.text_system().add_fonts(vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-Regular.ttf").as_slice()),
            Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-Medium.ttf").as_slice()),
            Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-SemiBold.ttf").as_slice()),
            Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-Bold.ttf").as_slice()),
        ]) {
            eprintln!("moth: failed to load fonts: {err:#}");
        }
        text_input::bind_keys(cx);

        let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("moth".into()),
                window_decorations: Some(WindowDecorations::Server),
                window_min_size: Some(size(px(640.0), px(480.0))),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let mut view = AppView::new(youtube, incoming, window, cx);
                    // Dev aid: `MOTH_OPEN=<home card index | YouTube URL | video ID>` opens it on startup.
                    if let Some(url) = url_arg {
                        view.submit_address(url, window, cx);
                    } else if let Ok(target) = std::env::var("MOTH_OPEN") {
                        match target.parse::<usize>() {
                            Ok(ix) => {
                                let id = VIDEOS[ix.min(VIDEOS.len() - 1)].id;
                                view.open_video(id.into(), window, cx);
                            }
                            Err(_) => view.submit_address(target, window, cx),
                        }
                    }
                    view
                })
            },
        );
        if let Err(err) = opened {
            eprintln!("moth: failed to open window: {err:#}");
            cx.quit();
            return;
        }
        cx.on_window_closed(|cx, _| {
            ipc::remove_socket();
            cx.quit();
        })
        .detach();
        cx.activate(true);
    });
}

// ── State ────────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq)]
enum Route {
    Home,
    Watch(SharedString),
    Settings,
}

enum Page {
    Loading,
    Loaded(Rc<WatchPage>),
    Failed(SharedString),
}

#[derive(Clone, Copy, PartialEq)]
enum MenuPage {
    Main,
    Quality,
    Format,
    Track,
    Audio,
    Speed,
}

#[derive(Clone)]
enum MenuAction {
    Open(MenuPage),
    /// `None` = automatic.
    Quality(Option<(u32, u8)>),
    Video(u32),
    Audio(u32),
    Track(String),
    Speed(f64),
}

struct MenuItem {
    label: String,
    detail: Option<String>,
    selected: bool,
    action: MenuAction,
}

const SPEEDS: [f64; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];
const MENU_W: f32 = 300.0;
const MENU_ROW_H: f32 = 38.0;
const MENU_HEADER_H: f32 = 44.0;

fn speed_label(rate: f64) -> String {
    if (rate - 1.0).abs() < 1e-9 { "Normal".to_string() } else { format!("{rate}×") }
}

#[derive(Clone, PartialEq)]
enum Status {
    Idle,
    Resolving,
    Starting,
    Playing,
    Paused,
    Ended,
    Error(String),
}

/// Shared between the view and the video canvas' paint callback, which is where we learn
/// the on-screen rectangle and have access to the raw Wayland handles.
#[derive(Default)]
struct PlayerSlot {
    pending: Option<StreamUrls>,
    /// Where the next player should begin (stream switches resume mid-video).
    start: Option<Start>,
    /// Whether web-client URLs for the same files are being resolved for `pending`.
    fallback_pending: bool,
    /// Resolved fallback (video, audio) URLs, handed to the player on the next poll.
    fallback: Option<(String, String)>,
    fallback_failed: bool,
    player: Option<Player>,
    error: Option<String>,
}

struct AppView {
    youtube: Arc<YouTube>,
    settings: Settings,
    settings_error: Option<SharedString>,
    route: Route,
    page: Page,
    /// Remote images (thumbnails, avatars) by URL.
    images: HashMap<String, Arc<Image>>,
    address: Entity<TextInput>,
    address_error: Option<SharedString>,
    address_focused: bool,
    viewport_h: f32,
    focus: FocusHandle,
    slot: Rc<RefCell<PlayerSlot>>,
    /// Bumped on every navigation so results of stale async work are dropped.
    generation: u64,
    status: Status,
    position: Option<u64>,
    duration: Option<u64>,
    stream_info: Option<String>,
    decoder: Option<String>,
    /// Width / height of the current video; the player frame takes this shape.
    video_aspect: Option<f32>,
    /// Streams available for the current video (None when yt-dlp had to resolve them).
    catalog: Option<Rc<StreamCatalog>>,
    video_itag: Option<u32>,
    audio_itag: Option<u32>,
    quality_auto: bool,
    /// Playback speed; kept across videos like YouTube does.
    rate: f64,
    menu: Option<MenuPage>,
    gear_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Bumped whenever the player is rebuilt, so late fallback URLs for old streams are dropped.
    stream_generation: u64,
    subscribed: bool,
    seek_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl AppView {
    fn new(
        youtube: Arc<YouTube>,
        incoming: Option<std::sync::mpsc::Receiver<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        let address = cx.new(|cx| {
            TextInput::new(
                "Paste a YouTube link and press Enter",
                rgb(TEXT).into(),
                rgb(FAINT).into(),
                rgb(ACCENT).into(),
                cx,
            )
        });
        let subscriptions = vec![cx.subscribe_in(&address, window, |this, _, event: &Submit, window, cx| {
            this.submit_address(event.0.to_string(), window, cx);
        })];

        // Lightweight UI tick for bus messages and the position readout.
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let tick = this.update_in(cx, |this, window, cx| {
                    // Links handed over by `moth <url>` or the browser extension.
                    let urls: Vec<String> = incoming.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
                    if let Some(url) = urls.into_iter().last() {
                        window.activate_window();
                        this.submit_address(url, window, cx);
                    }
                    this.poll(cx)
                });
                if tick.is_err() {
                    break;
                }
            }
        });

        Self {
            youtube,
            settings: Settings::load(),
            settings_error: None,
            route: Route::Home,
            page: Page::Loading,
            images: HashMap::new(),
            address,
            address_error: None,
            address_focused: false,
            viewport_h: 0.0,
            focus,
            slot: Rc::default(),
            generation: 0,
            status: Status::Idle,
            position: None,
            duration: None,
            stream_info: None,
            decoder: None,
            video_aspect: None,
            catalog: None,
            video_itag: None,
            audio_itag: None,
            quality_auto: true,
            rate: 1.0,
            menu: None,
            gear_bounds: Rc::default(),
            stream_generation: 0,
            subscribed: false,
            seek_bounds: Rc::default(),
            _poll: poll,
            _subscriptions: subscriptions,
        }
    }

    fn submit_address(&mut self, input: String, window: &mut Window, cx: &mut Context<Self>) {
        self.address_error = None;
        let resolve = self.youtube.resolve_video_id(input);
        cx.spawn_in(window, async move |this, cx| {
            let result = resolve.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(id) => {
                    window.focus(&this.focus, cx);
                    this.open_video(id.into(), window, cx);
                }
                Err(err) => {
                    this.address_error = Some(format!("{err:#}").into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn open_video(&mut self, id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.stop();
        self.route = Route::Watch(id.clone());
        self.page = Page::Loading;
        self.status = Status::Resolving;
        self.subscribed = false;
        self.address_error = None;
        self.address.update(cx, |input, cx| {
            input.set_text(format!("https://www.youtube.com/watch?v={id}"), window, cx)
        });
        let generation = self.generation;

        let details = self.youtube.watch_page(id.to_string());
        cx.spawn(async move |this, cx| {
            let result = details.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(page) => {
                        let urls = page
                            .related
                            .iter()
                            .filter_map(|video| video.thumbnail_url.clone())
                            .chain(page.channel_avatar_url.clone());
                        for url in urls.collect::<Vec<_>>() {
                            this.load_image(url, cx);
                        }
                        this.page = Page::Loaded(Rc::new(page));
                    }
                    Err(err) => this.page = Page::Failed(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();

        let max_height = self.settings.max_height;
        let streams = self.youtube.streams(id.to_string(), max_height);
        cx.spawn(async move |this, cx| {
            let result = match streams.await {
                Ok((urls, catalog)) => Ok((urls, Some(catalog))),
                Err(rustypipe_err) => {
                    eprintln!("moth: RustyPipe stream resolution failed, trying yt-dlp: {rustypipe_err:#}");
                    let id = id.to_string();
                    cx.background_executor()
                        .spawn(async move { player::resolve_streams(&id, None, max_height) })
                        .await
                        .map(|mut urls| {
                            urls.info = format!("{} · via yt-dlp (RustyPipe failed)", urls.info);
                            (urls, None)
                        })
                        .map_err(|ytdlp_err| format!("RustyPipe: {rustypipe_err:#}\nyt-dlp: {ytdlp_err:#}"))
                }
            };
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok((urls, catalog)) => {
                        this.catalog = catalog.map(Rc::new);
                        let start = (this.rate != 1.0).then_some(Start { position: 0, rate: this.rate, paused: false });
                        this.begin_playback(urls, start, cx);
                    }
                    Err(err) => {
                        this.status = Status::Error(err);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Hand stream URLs to the video canvas (which builds the player on its next paint) and, if
    /// enabled, resolve web-client URLs for the same files so seeking far ahead works.
    fn begin_playback(&mut self, urls: StreamUrls, start: Option<Start>, cx: &mut Context<Self>) {
        self.stream_generation += 1;
        self.stream_info = Some(urls.info.clone());
        self.video_itag = urls.video_itag;
        self.audio_itag = urls.audio_itag;
        self.decoder = None;
        if let (Some(w), Some(h)) = (urls.width, urls.height)
            && w > 0
            && h > 0
        {
            self.video_aspect = Some(w as f32 / h as f32);
        }
        // RustyPipe's iOS-client URLs can't seek far ahead (see player.rs); only catalog streams
        // have itags to ask yt-dlp for.
        let fallback_format = urls
            .video_itag
            .zip(urls.audio_itag)
            .filter(|_| self.settings.seek_fallback && self.catalog.is_some())
            .map(|(video, audio)| format!("{video}+{audio}"));
        // Replacing the slot drops (and stops) any current player.
        *self.slot.borrow_mut() =
            PlayerSlot { pending: Some(urls), start, fallback_pending: fallback_format.is_some(), ..Default::default() };
        self.status = Status::Starting;
        cx.notify();

        let (Some(format), Route::Watch(id)) = (fallback_format, &self.route) else { return };
        let id = id.to_string();
        let max_height = self.settings.max_height;
        let (generation, stream_generation) = (self.generation, self.stream_generation);
        cx.spawn(async move |this, cx| {
            let fallback = cx
                .background_executor()
                .spawn(async move { player::resolve_streams(&id, Some(format), max_height) })
                .await;
            this.update(cx, |this, _| {
                if this.generation != generation || this.stream_generation != stream_generation {
                    return;
                }
                let mut slot = this.slot.borrow_mut();
                match fallback {
                    Ok(urls) => slot.fallback = Some((urls.video, urls.audio)),
                    Err(err) => {
                        eprintln!("moth: couldn't resolve fallback stream URLs: {err:#}");
                        slot.fallback_failed = true;
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Rebuild playback with other streams, resuming where we are.
    fn switch_streams(&mut self, video_itag: u32, audio_itag: u32, cx: &mut Context<Self>) {
        let Some(catalog) = self.catalog.clone() else { return };
        if self.video_itag == Some(video_itag) && self.audio_itag == Some(audio_itag) {
            return;
        }
        let (Some(video), Some(audio)) = (catalog.video_by_itag(video_itag), catalog.audio_by_itag(audio_itag))
        else {
            return;
        };
        let start = Start {
            position: self.position.unwrap_or(0),
            rate: self.rate,
            paused: matches!(self.status, Status::Paused),
        };
        self.begin_playback(catalog.urls(video, audio), Some(start), cx);
    }

    fn menu_action(&mut self, action: MenuAction, cx: &mut Context<Self>) {
        let catalog = self.catalog.clone();
        let current_video = catalog.as_ref().zip(self.video_itag).and_then(|(c, itag)| c.video_by_itag(itag).cloned());
        let current_audio = catalog.as_ref().zip(self.audio_itag).and_then(|(c, itag)| c.audio_by_itag(itag).cloned());
        match action {
            MenuAction::Open(page) => {
                self.menu = Some(page);
                cx.notify();
                return;
            }
            MenuAction::Speed(rate) => {
                self.rate = rate;
                let mut slot = self.slot.borrow_mut();
                if let Some(player) = slot.player.as_mut() {
                    player.set_rate(rate);
                } else if let Some(start) = slot.start.as_mut() {
                    start.rate = rate;
                } else if slot.pending.is_some() {
                    slot.start = Some(Start { position: 0, rate, paused: false });
                }
            }
            MenuAction::Quality(choice) => {
                let (Some(catalog), Some(audio_itag)) = (catalog, self.audio_itag) else { return };
                let codec = current_video.as_ref().map(|video| video.codec);
                let video = match choice {
                    None => catalog.auto_video(self.settings.max_height, codec),
                    Some((resolution, fps)) => {
                        let variants = catalog.video_variants(resolution, fps);
                        variants.iter().find(|video| Some(video.codec) == codec).or(variants.first()).copied()
                    }
                };
                self.quality_auto = choice.is_none();
                if let Some(video) = video {
                    self.switch_streams(video.itag, audio_itag, cx);
                }
            }
            MenuAction::Video(itag) => {
                if let Some(audio_itag) = self.audio_itag {
                    self.switch_streams(itag, audio_itag, cx);
                }
            }
            MenuAction::Audio(itag) => {
                if let Some(video_itag) = self.video_itag {
                    self.switch_streams(video_itag, itag, cx);
                }
            }
            MenuAction::Track(track_id) => {
                let (Some(catalog), Some(video_itag)) = (catalog, self.video_itag) else { return };
                let codec = current_audio.as_ref().map(|audio| audio.codec);
                let options = catalog.audio_in_track(Some(&track_id));
                if let Some(audio) = options.iter().find(|audio| Some(audio.codec) == codec).or(options.first()) {
                    self.switch_streams(video_itag, audio.itag, cx);
                }
            }
        }
        self.menu = None;
        cx.notify();
    }

    fn load_image(&mut self, url: String, cx: &mut Context<Self>) {
        if self.images.contains_key(&url) {
            return;
        }
        let fetch = self.youtube.fetch_bytes(url.clone());
        cx.spawn(async move |this, cx| {
            let image = match fetch.await {
                Ok(bytes) => match image_format(&bytes) {
                    Some(format) => Arc::new(Image::from_bytes(format, bytes)),
                    None => return eprintln!("moth: unrecognized image format: {url}"),
                },
                Err(err) => return eprintln!("moth: failed to load {url}: {err:#}"),
            };
            this.update(cx, |this, cx| {
                this.images.insert(url, image);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.route == Route::Settings {
            return;
        }
        self.stop();
        self.route = Route::Settings;
        self.status = Status::Idle;
        self.settings_error = None;
        self.address.update(cx, |input, cx| input.set_text("", window, cx));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn update_settings(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        change(&mut self.settings);
        self.settings_error = self.settings.save().err().map(|err| format!("Couldn't save settings: {err:#}").into());
        cx.notify();
    }

    fn go_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop();
        self.route = Route::Home;
        self.status = Status::Idle;
        self.address.update(cx, |input, cx| input.set_text("", window, cx));
        cx.notify();
    }

    fn stop(&mut self) {
        self.generation += 1;
        *self.slot.borrow_mut() = PlayerSlot::default();
        self.position = None;
        self.duration = None;
        self.stream_info = None;
        self.decoder = None;
        self.video_aspect = None;
        self.catalog = None;
        self.video_itag = None;
        self.audio_itag = None;
        self.quality_auto = true;
        self.menu = None;
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        let paused = match self.slot.borrow_mut().player.as_mut() {
            Some(player) => {
                player.toggle_pause();
                player.is_paused()
            }
            None => return,
        };
        self.status = if paused { Status::Paused } else { Status::Playing };
        cx.notify();
    }

    fn seek_relative(&mut self, delta: i64) {
        if let (Some(player), Some(position)) = (self.slot.borrow().player.as_ref(), self.position) {
            let target = (position as i64 + delta).max(0) as u64;
            player.seek_seconds(self.duration.map_or(target, |duration| target.min(duration)));
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut slot = self.slot.borrow_mut();
        let fallback = slot.fallback.take();
        let fallback_failed = std::mem::take(&mut slot.fallback_failed);
        if let Some(player) = slot.player.as_ref() {
            if let Some((video, audio)) = fallback {
                player.set_fallback(video, audio);
            } else if fallback_failed {
                player.fallback_failed();
            }
        } else {
            // Player not created yet: keep the result for the next poll.
            slot.fallback = slot.fallback.take().or(fallback);
            slot.fallback_failed |= fallback_failed;
        }
        let mut changed = false;

        if let Some(err) = slot.error.as_ref()
            && self.status != Status::Error(err.clone())
        {
            self.status = Status::Error(err.clone());
            changed = true;
        }

        if let Some(player) = slot.player.as_mut() {
            player.tick();
            for event in player.drain_events() {
                self.status = match event {
                    PlayerEvent::Error(err) => Status::Error(err),
                    PlayerEvent::Eos => Status::Ended,
                };
                changed = true;
            }
            if self.status == Status::Starting && player.is_playing() {
                self.status = Status::Playing;
                changed = true;
            }
            let (position, duration) = player.position_duration();
            if (position, duration) != (self.position, self.duration) {
                (self.position, self.duration) = (position, duration);
                changed = true;
            }
            if player.using_fallback()
                && let Some(info) = self.stream_info.as_mut()
                && !info.ends_with(SWITCHED_NOTE)
            {
                info.push_str(SWITCHED_NOTE);
                changed = true;
            }
            // The decoded caps are authoritative (pixel aspect ratio, mid-stream size changes).
            if let Some(aspect) = player.display_aspect()
                && self.video_aspect.is_none_or(|current| (current - aspect).abs() > 0.001)
            {
                self.video_aspect = Some(aspect);
                changed = true;
            }
            if self.decoder.is_none()
                && let Some(decoder) = player.video_decoder()
            {
                eprintln!("video decoder: {decoder}");
                self.decoder = Some(decoder);
                changed = true;
            }
        }
        drop(slot);
        if changed {
            cx.notify();
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        let ctrl_only = modifiers.control && !modifiers.alt && !modifiers.shift;
        if ctrl_only && event.keystroke.key == "l" {
            self.address.update(cx, |input, cx| input.focus_and_select_all(window, cx));
            return;
        }
        if ctrl_only && event.keystroke.key == "," {
            if window.is_fullscreen() {
                window.toggle_fullscreen();
            }
            self.open_settings(window, cx);
            return;
        }
        if self.address.focus_handle(cx).is_focused(window) {
            if event.keystroke.key == "escape" {
                window.focus(&self.focus, cx);
            }
            return;
        }
        if !matches!(self.route, Route::Watch(_)) {
            return;
        }
        if self.menu.is_some() && event.keystroke.key == "escape" {
            self.menu = None;
            cx.notify();
            return;
        }
        match event.keystroke.key.as_str() {
            "space" | "k" => self.toggle_pause(cx),
            "f" => window.toggle_fullscreen(),
            "escape" if window.is_fullscreen() => window.toggle_fullscreen(),
            "left" | "j" => self.seek_relative(if event.keystroke.key == "j" { -10 } else { -5 }),
            "right" | "l" => self.seek_relative(if event.keystroke.key == "l" { 10 } else { 5 }),
            _ => {}
        }
    }
}

/// Player frame size: as wide as `column_w` at the video's aspect ratio (16:9 until it's known),
/// but never taller than leaves room for the controls and title; tall videos get narrower instead.
fn fit_player(aspect: Option<f32>, column_w: f32, viewport_h: f32) -> (f32, f32) {
    let aspect = aspect.unwrap_or(16.0 / 9.0).clamp(0.2, 5.0);
    let max_h = (viewport_h - PLAYER_RESERVED_H).max(180.0);
    let (w, h) = if column_w / aspect > max_h { (max_h * aspect, max_h) } else { (column_w, column_w / aspect) };
    (w.min(column_w).floor(), h.floor())
}

#[cfg(test)]
mod tests {
    use super::fit_player;

    #[test]
    fn player_takes_the_video_aspect_ratio() {
        // Spring is 2048x858: full column width, shorter than 16:9.
        assert_eq!(fit_player(Some(2048.0 / 858.0), 712.0, 898.0), (712.0, 298.0));
        // 16:9 at full width.
        assert_eq!(fit_player(Some(16.0 / 9.0), 712.0, 898.0), (712.0, 400.0));
        // Unknown yet: 16:9 placeholder.
        assert_eq!(fit_player(None, 712.0, 898.0), (712.0, 400.0));
        // Vertical 9:16 video: capped height, narrower frame.
        assert_eq!(fit_player(Some(9.0 / 16.0), 712.0, 898.0), (336.0, 598.0));
        // Tiny window: never wider than the column, never shorter than the floor.
        let (w, h) = fit_player(Some(9.0 / 16.0), 300.0, 400.0);
        assert!(w <= 300.0 && h == 180.0);
    }
}

fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0xFF, 0xD8, ..] => Some(ImageFormat::Jpeg),
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::Webp),
        _ => None,
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        self.address_focused = self.address.focus_handle(cx).is_focused(window);
        self.viewport_h = f32::from(viewport.height);
        if matches!(self.route, Route::Watch(_)) && window.is_fullscreen() {
            return div()
                .id("root")
                .track_focus(&self.focus)
                .on_key_down(cx.listener(Self::on_key))
                .size_full()
                .font_family("Outfit")
                .text_color(rgb(TEXT))
                .bg(rgb(0x000000))
                .child(self.watch_fullscreen(f32::from(viewport.width), f32::from(viewport.height), cx));
        }
        let body = match &self.route {
            Route::Home | Route::Settings => {
                let collapsed = f32::from(viewport.width) < NARROW_W;
                let on_settings = self.route == Route::Settings;
                div()
                    .size_full()
                    .flex()
                    .flex_row()
                    .gap(px(PAD))
                    .child(sidebar(collapsed, if on_settings { "Settings" } else { "Home" }, cx))
                    .map(|row| {
                        if on_settings {
                            row.child(self.settings_page(cx))
                        } else {
                            row.child(self.home(f32::from(viewport.width), collapsed, cx))
                        }
                    })
                    .into_any_element()
            }
            Route::Watch(_) => self.watch(f32::from(viewport.width), f32::from(viewport.height), cx),
        };

        div()
            .id("root")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .p(px(PAD))
            .font_family("Outfit")
            .text_color(rgb(TEXT))
            .bg(linear_gradient(
                160.0,
                linear_color_stop(rgb(BG_TOP), 0.0),
                linear_color_stop(rgb(BG_BOTTOM), 1.0),
            ))
            .child(body)
    }
}

impl AppView {
    fn home(&self, viewport_w: f32, sidebar_collapsed: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar_w = if sidebar_collapsed { RAIL_W } else { SIDEBAR_W } + PAD;
        let main_w = viewport_w - 2.0 * PAD - sidebar_w;
        let columns = if main_w > 1300.0 { 5.0 } else if main_w > 900.0 { 4.0 } else if main_w > 560.0 { 3.0 } else { 2.0 };
        let gap = 12.0;
        let card_w = ((main_w - 8.0 - gap * (columns - 1.0)) / columns).floor();

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.top_bar(false, cx))
            .child(
                div()
                    .id("home-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(section_heading("Blender Open Movies", "PLAY ALL"))
                            .child(
                                div().flex().flex_wrap().gap(px(gap)).children(
                                    VIDEOS.iter().enumerate().map(|(ix, video)| video_card(ix, video, card_w, cx)),
                                ),
                            ),
                    ),
            )
    }

    fn player_size(&self, column_w: f32, viewport_h: f32) -> (f32, f32) {
        fit_player(self.video_aspect, column_w, viewport_h)
    }

    fn watch(&self, viewport_w: f32, viewport_h: f32, cx: &mut Context<Self>) -> AnyElement {
        if viewport_w < NARROW_W {
            return self.watch_narrow(viewport_w, viewport_h, cx);
        }
        // The player column takes half the window width.
        let video_w = ((viewport_w - 2.0 * PAD) * 0.5).floor();
        let (frame_w, frame_h) = self.player_size(video_w, viewport_h);

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.top_bar(true, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .gap_6()
                    // Left: player + details
                    .child(
                        div()
                            .w(px(video_w))
                            .flex_none()
                            .h_full()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(div().flex().justify_center().child(self.video_surface(frame_w, frame_h, cx)))
                            .child(self.controls(false, cx))
                            .when_some(self.playback_error(), |column, error| column.child(error))
                            .child(
                                div()
                                    .id("details")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(self.details(cx)),
                            ),
                    )
                    // Right: recommendations
                    .child(
                        div()
                            .id("recs")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .child(self.up_next(cx)),
                    ),
            )
            .into_any_element()
    }

    /// Narrow windows (like YouTube's own layout): full-width player, then details and up next
    /// in one scrolling column.
    fn watch_narrow(&self, viewport_w: f32, viewport_h: f32, cx: &mut Context<Self>) -> AnyElement {
        let (frame_w, frame_h) = self.player_size((viewport_w - 2.0 * PAD).floor(), viewport_h);
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .child(self.top_bar(true, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(div().flex().justify_center().child(self.video_surface(frame_w, frame_h, cx)))
                    .child(self.controls(false, cx))
                    .when_some(self.playback_error(), |column, error| column.child(error))
                    .child(
                        div()
                            .id("details")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(div().flex().flex_col().gap_4().child(self.details(cx)).child(self.up_next(cx))),
                    ),
            )
            .into_any_element()
    }

    fn up_next(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let related: &[RelatedVideo] = match &self.page {
            Page::Loaded(page) => &page.related,
            _ => &[],
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .px_2()
                    .pb_2()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(FAINT))
                    .child("UP NEXT"),
            )
            .when(matches!(self.page, Page::Loading), |list| {
                list.child(div().px_2().text_sm().text_color(rgb(FAINT)).child("Loading…"))
            })
            .children(related.iter().enumerate().map(|(ix, video)| self.recommendation(ix, video, cx)))
    }

    /// Fullscreen watch page: video fills the screen above a slim control bar. (Controls can't
    /// overlay the video yet: the waylandsink subsurface is stacked above the gpui surface.)
    fn watch_fullscreen(&self, w: f32, h: f32, cx: &mut Context<Self>) -> impl IntoElement {
        const BAR_H: f32 = 52.0;
        // The video layer covers anything drawn over it, so an open menu gets its own space.
        let menu_room = self.menu.map_or(0.0, |_| (self.menu_height() + 16.0).min(h * 0.6));
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.video_surface(w, (h - BAR_H - menu_room).max(0.0), cx))
            .when(menu_room > 0.0, |column| column.child(div().h(px(menu_room)).flex_none()))
            .child(
                div()
                    .h(px(BAR_H))
                    .flex_none()
                    .px_4()
                    .flex()
                    .items_center()
                    .bg(rgb(0x0b0f1a))
                    .child(div().flex_1().child(self.controls(true, cx)))
                    .when_some(self.playback_error(), |bar, error| bar.child(error)),
            )
    }

    /// Placeholder box whose on-screen bounds drive the waylandsink subsurface.
    /// Double-click toggles fullscreen (the subsurface has an empty input region, so
    /// pointer events reach this element).
    fn video_surface(&self, w: f32, h: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let slot = self.slot.clone();
        let http = self.youtube.http();
        let label: Option<SharedString> = match &self.status {
            Status::Resolving => Some("Resolving stream…".into()),
            Status::Starting => Some("Buffering…".into()),
            Status::Error(err) => Some(format!("Playback error: {err}").into()),
            _ => None,
        };

        div()
            .id("video")
            .relative()
            .w(px(w))
            .h(px(h))
            .flex_none()
            .bg(rgb(0x000000))
            .on_click(cx.listener(|_, event: &gpui::ClickEvent, window, _| {
                if event.click_count() == 2 {
                    window.toggle_fullscreen();
                }
            }))
            .flex()
            .items_center()
            .justify_center()
            .when_some(label, |surface, label| {
                surface.child(div().px_6().text_sm().text_color(rgb(MUTED)).child(label))
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        let rect = Rect {
                            x: f32::from(bounds.origin.x).round() as i32,
                            y: f32::from(bounds.origin.y).round() as i32,
                            w: f32::from(bounds.size.width).round() as i32,
                            h: f32::from(bounds.size.height).round() as i32,
                        };
                        let mut slot = slot.borrow_mut();
                        if let Some(player) = slot.player.as_mut() {
                            player.set_rect(rect);
                        } else if let Some(urls) = slot.pending.take() {
                            let fallback_pending = slot.fallback_pending;
                            let start = slot.start.take();
                            let result = wayland_handles(window).and_then(|(display, surface)| {
                                Player::new(&urls, start, fallback_pending, &http, display, surface, rect)
                            });
                            match result {
                                Ok(player) => slot.player = Some(player),
                                Err(err) => slot.error = Some(format!("{err:#}")),
                            }
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
    }

    fn menu_items(&self, page: MenuPage) -> Vec<MenuItem> {
        let catalog = self.catalog.as_deref();
        let video = catalog.zip(self.video_itag).and_then(|(c, itag)| c.video_by_itag(itag));
        let audio = catalog.zip(self.audio_itag).and_then(|(c, itag)| c.audio_by_itag(itag));
        let item = |label: String, detail: Option<String>, selected: bool, action: MenuAction| MenuItem {
            label,
            detail,
            selected,
            action,
        };
        match page {
            MenuPage::Main => {
                let mut items = Vec::new();
                if let (Some(catalog), Some(video), Some(audio)) = (catalog, video, audio) {
                    let quality = if self.quality_auto { format!("Auto ({})", video.label()) } else { video.label() };
                    items.push(item("Quality".into(), Some(quality), false, MenuAction::Open(MenuPage::Quality)));
                    items.push(item(
                        "Video format".into(),
                        Some(video.codec.into()),
                        false,
                        MenuAction::Open(MenuPage::Format),
                    ));
                    if catalog.tracks().len() > 1 {
                        let track = audio.track.as_ref().map_or("Default".to_string(), |track| track.name.clone());
                        items.push(item("Audio track".into(), Some(track), false, MenuAction::Open(MenuPage::Track)));
                    }
                    items.push(item(
                        "Audio quality".into(),
                        Some(audio.label()),
                        false,
                        MenuAction::Open(MenuPage::Audio),
                    ));
                }
                items.push(item(
                    "Playback speed".into(),
                    Some(speed_label(self.rate)),
                    false,
                    MenuAction::Open(MenuPage::Speed),
                ));
                items
            }
            MenuPage::Quality => {
                let Some(catalog) = catalog else { return Vec::new() };
                let current = video.map(|video| (video.resolution(), video.fps));
                let mut items = vec![item(
                    "Auto".into(),
                    Some(format!("up to {}p", self.settings.max_height)),
                    self.quality_auto,
                    MenuAction::Quality(None),
                )];
                items.extend(catalog.qualities().into_iter().map(|(resolution, fps)| {
                    let label = if fps > 30 { format!("{resolution}p{fps}") } else { format!("{resolution}p") };
                    let badge = match resolution {
                        2160.. => Some("4K".to_string()),
                        1440.. => Some("QHD".to_string()),
                        720.. => Some("HD".to_string()),
                        _ => None,
                    };
                    item(
                        label,
                        badge,
                        !self.quality_auto && current == Some((resolution, fps)),
                        MenuAction::Quality(Some((resolution, fps))),
                    )
                }));
                items
            }
            MenuPage::Format => {
                let (Some(catalog), Some(video)) = (catalog, video) else { return Vec::new() };
                catalog
                    .video_variants(video.resolution(), video.fps)
                    .into_iter()
                    .map(|variant| {
                        item(
                            variant.codec.into(),
                            Some(format!("{:.1} Mbps", variant.bitrate as f64 / 1e6)),
                            variant.itag == video.itag,
                            MenuAction::Video(variant.itag),
                        )
                    })
                    .collect()
            }
            MenuPage::Track => {
                let Some(catalog) = catalog else { return Vec::new() };
                let current = audio.and_then(|audio| audio.track.as_ref()).map(|track| track.id.clone());
                catalog
                    .tracks()
                    .into_iter()
                    .map(|track| {
                        item(
                            track.name.clone(),
                            track.is_default.then(|| "default".to_string()),
                            current.as_ref() == Some(&track.id),
                            MenuAction::Track(track.id),
                        )
                    })
                    .collect()
            }
            MenuPage::Audio => {
                let Some(catalog) = catalog else { return Vec::new() };
                let track = audio.and_then(|audio| audio.track.as_ref()).map(|track| track.id.as_str());
                catalog
                    .audio_in_track(track)
                    .into_iter()
                    .map(|option| {
                        item(option.label(), None, Some(option.itag) == self.audio_itag, MenuAction::Audio(option.itag))
                    })
                    .collect()
            }
            MenuPage::Speed => SPEEDS
                .iter()
                .map(|rate| item(speed_label(*rate), None, (rate - self.rate).abs() < 1e-9, MenuAction::Speed(*rate)))
                .collect(),
        }
    }

    fn menu_height(&self) -> f32 {
        let Some(page) = self.menu else { return 0.0 };
        let header = if page == MenuPage::Main { 0.0 } else { MENU_HEADER_H };
        header + self.menu_items(page).len() as f32 * MENU_ROW_H + 16.0
    }

    /// YouTube-style settings popover, attached to the gear button. In a window it opens
    /// downward (the video layer would hide anything drawn over the video); in fullscreen the
    /// video makes room for it above the control bar.
    fn settings_menu(&self, fullscreen: bool, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let page = self.menu?;
        let gear = self.gear_bounds.get()?;
        let items = self.menu_items(page);
        let max_list_h = if fullscreen {
            (self.viewport_h * 0.6 - MENU_HEADER_H).max(MENU_ROW_H * 3.0)
        } else {
            let below = self.viewport_h - f32::from(gear.bottom()) - 24.0;
            (below - MENU_HEADER_H - 16.0).max(MENU_ROW_H * 3.0)
        };
        let title = match page {
            MenuPage::Main => None,
            MenuPage::Quality => Some("Quality"),
            MenuPage::Format => Some("Video format"),
            MenuPage::Track => Some("Audio track"),
            MenuPage::Audio => Some("Audio quality"),
            MenuPage::Speed => Some("Playback speed"),
        };
        let gear_bounds = self.gear_bounds.clone();

        let rows: Vec<_> = items
            .into_iter()
            .enumerate()
            .map(|(ix, entry)| {
                let submenu = matches!(entry.action, MenuAction::Open(_));
                let action = entry.action;
                div()
                    .id(("menu-item", ix))
                    .h(px(MENU_ROW_H))
                    .flex_none()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(HOVER)))
                    .on_click(cx.listener(move |this, _, _, cx| this.menu_action(action.clone(), cx)))
                    .when(page != MenuPage::Main, |row| {
                        row.child(
                            div()
                                .w(px(14.0))
                                .flex_none()
                                .text_sm()
                                .text_color(rgb(ACCENT))
                                .child(if entry.selected { "✓" } else { "" }),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(rgb(TEXT))
                            .when(entry.selected, |label| label.font_weight(FontWeight::SEMIBOLD))
                            .truncate()
                            .child(entry.label),
                    )
                    .when_some(entry.detail, |row, detail| {
                        row.child(div().flex_none().text_xs().text_color(rgb(MUTED)).child(detail))
                    })
                    .when(submenu, |row| row.child(div().flex_none().text_sm().text_color(rgb(FAINT)).child("›")))
            })
            .collect();

        let menu = div()
            .id("player-settings-menu")
            .w(px(MENU_W))
            .py_2()
            .rounded_xl()
            .bg(rgba(0x141a2cf7))
            .border_1()
            .border_color(rgb(TRACK))
            .shadow_lg()
            .occlude()
            .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                // Clicks on the gear toggle the menu themselves.
                if gear_bounds.get().is_some_and(|bounds| bounds.contains(&event.position)) {
                    return;
                }
                this.menu = None;
                cx.notify();
            }))
            .when_some(title, |menu, title| {
                menu.child(
                    div()
                        .id("menu-back")
                        .h(px(MENU_HEADER_H - 8.0))
                        .mb_2()
                        .px_4()
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .border_b_1()
                        .border_color(rgb(TRACK))
                        .hover(|style| style.bg(rgb(HOVER)))
                        .on_click(cx.listener(|this, _, _, cx| this.menu_action(MenuAction::Open(MenuPage::Main), cx)))
                        .child(div().text_color(rgb(MUTED)).child("‹"))
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(title)),
                )
            })
            .child(
                div()
                    .id("menu-rows")
                    .max_h(px(max_list_h))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(rows),
            );

        let (corner, anchor, offset) = if fullscreen {
            (gpui::Anchor::TopRight, gpui::Anchor::BottomRight, point(px(0.0), px(-8.0)))
        } else {
            (gpui::Anchor::BottomRight, gpui::Anchor::TopRight, point(px(0.0), px(8.0)))
        };
        Some(deferred(anchored().attach(gear, corner).anchor(anchor).offset(offset).child(menu)).with_priority(1))
    }

    fn playback_error(&self) -> Option<impl IntoElement + use<>> {
        let Status::Error(err) = &self.status else { return None };
        Some(
            div()
                .text_xs()
                .text_color(rgb(ACCENT))
                .line_clamp(2)
                .text_ellipsis()
                .child(format!("Playback error: {err}")),
        )
    }

    fn controls(&self, fullscreen: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let paused = matches!(self.status, Status::Paused | Status::Ended);
        let position = self.position.unwrap_or(0);
        let duration = self.duration.unwrap_or(0);
        let progress = if duration > 0 { (position as f32 / duration as f32).clamp(0.0, 1.0) } else { 0.0 };
        let seek_bounds = self.seek_bounds.clone();
        let seek_bounds_paint = self.seek_bounds.clone();

        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .id("play-pause")
                    .size(px(34.0))
                    .flex_none()
                    .rounded_full()
                    .bg(rgb(PANEL))
                    .hover(|style| style.bg(rgb(HOVER)))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx)))
                    .child(if paused {
                        play_icon(12.0, rgb(TEXT).into()).into_any_element()
                    } else {
                        pause_icon().into_any_element()
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .flex_none()
                    .child(format!("{} / {}", fmt_time(position), fmt_time(duration))),
            )
            .child(
                // Seek bar: generous hit area, thin track.
                div()
                    .id("seek")
                    .flex_1()
                    .py_2()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, _| {
                            if let (Some(bounds), Some(duration)) = (seek_bounds.get(), this.duration) {
                                let fraction =
                                    ((event.position.x - bounds.origin.x) / bounds.size.width).clamp(0.0, 1.0);
                                if let Some(player) = this.slot.borrow().player.as_ref() {
                                    player.seek_seconds((fraction * duration as f32) as u64);
                                }
                            }
                        }),
                    )
                    .child(
                        div()
                            .relative()
                            .h(px(5.0))
                            .rounded_full()
                            .bg(rgb(TRACK))
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .h_full()
                                    .w(gpui::relative(progress))
                                    .rounded_full()
                                    .bg(rgb(ACCENT)),
                            )
                            .child(
                                canvas(move |bounds, _, _| seek_bounds_paint.set(Some(bounds)), |_, _, _, _| {})
                                    .absolute()
                                    .size_full(),
                            ),
                    ),
            )
            .child({
                let gear_bounds = self.gear_bounds.clone();
                let open = self.menu.is_some();
                div()
                    .id("player-settings")
                    .relative()
                    .size(px(34.0))
                    .flex_none()
                    .rounded_full()
                    .when(open, |button| button.bg(rgb(HOVER)))
                    .hover(|style| style.bg(rgb(HOVER)))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu = if this.menu.is_some() { None } else { Some(MenuPage::Main) };
                        cx.notify();
                    }))
                    .child(icon(Icon::Settings, 17.0, rgb(if open { TEXT } else { MUTED }).into()))
                    .child(
                        canvas(move |bounds, _, _| gear_bounds.set(Some(bounds)), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .when_some(self.settings_menu(fullscreen, cx), |button, menu| button.child(menu))
            })
            .child(
                div()
                    .id("fullscreen")
                    .size(px(34.0))
                    .flex_none()
                    .rounded_full()
                    .hover(|style| style.bg(rgb(HOVER)))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_click(|_, window, _| window.toggle_fullscreen())
                    .child(fullscreen_icon(fullscreen)),
            )
    }

    fn details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let page = match &self.page {
            Page::Loaded(page) => page.clone(),
            Page::Loading => {
                return div().text_sm().text_color(rgb(FAINT)).child("Loading video details…").into_any_element();
            }
            Page::Failed(err) => {
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
        let subscribed = self.subscribed;
        let tech: Option<SharedString> = match (&self.decoder, &self.stream_info) {
            (Some(decoder), Some(info)) => Some(format!("decoder: {decoder} · {info}").into()),
            (None, Some(info)) => Some(info.clone().into()),
            _ => None,
        };
        let avatar_image = page.channel_avatar_url.as_ref().and_then(|url| self.images.get(url)).cloned();
        let stats = [Some(page.views.clone()), page.date.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");

        div()
            .flex()
            .flex_col()
            .gap_4()
            .pb_4()
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).line_height(px(28.0)).child(page.title.clone()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(match avatar_image {
                        Some(image) => img(image).size(px(40.0)).flex_none().rounded_full().into_any_element(),
                        None => avatar(&page.channel, 40.0).into_any_element(),
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(page.channel.clone()))
                            .when_some(page.subscribers.clone(), |column, subscribers| {
                                column.child(div().text_xs().text_color(rgb(MUTED)).child(subscribers))
                            }),
                    )
                    .child(
                        div()
                            .id("subscribe")
                            .ml_3()
                            .px_4()
                            .py_1p5()
                            .rounded_lg()
                            .cursor_pointer()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .map(|button| {
                                if subscribed {
                                    button.bg(rgb(0x7d87a3)).text_color(rgb(0x1b2236)).child("SUBSCRIBED")
                                } else {
                                    button.bg(rgb(ACCENT)).text_color(rgb(0xffffff)).child("SUBSCRIBE")
                                }
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.subscribed = !this.subscribed;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(pill(match &page.likes {
                        Some(likes) => format!("Like · {likes}"),
                        None => "Like".to_string(),
                    }))
                    .child(pill("Share".to_string()))
                    .child(pill("Save".to_string())),
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
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .line_height(px(21.0))
                            .child(page.description.clone()),
                    ),
            )
            .when_some(tech, |column, tech| column.child(div().text_xs().text_color(rgb(FAINT)).child(tech)))
            .into_any_element()
    }

    fn recommendation(&self, ix: usize, video: &RelatedVideo, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id: SharedString = video.id.clone().into();
        let image = video.thumbnail_url.as_ref().and_then(|url| self.images.get(url)).cloned();
        div()
            .id(("rec", ix))
            .flex()
            .gap_3()
            .p_2()
            .rounded_xl()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(HOVER)))
            .on_click(cx.listener(move |this, _, window, cx| this.open_video(id.clone(), window, cx)))
            .child(thumbnail(image.map(ImageSource::from), video.duration.clone(), px(176.0), px(99.0)))
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
                    .when(!video.channel.is_empty(), |column| {
                        column.child(div().text_xs().text_color(rgb(MUTED)).child(video.channel.clone()))
                    })
                    .when(!video.meta.is_empty(), |column| {
                        column.child(div().text_xs().text_color(rgb(FAINT)).child(video.meta.clone()))
                    }),
            )
    }

    fn settings_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let section = |title: &'static str| {
            div().flex().flex_col().gap_2().child(
                div().px_1().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(FAINT)).child(title),
            )
        };
        let row = |label: &'static str, description: &'static str| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_4()
                .rounded_xl()
                .bg(rgb(PANEL))
                .px_5()
                .py_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .max_w(px(460.0))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(label))
                        .child(div().text_sm().text_color(rgb(MUTED)).line_height(px(20.0)).child(description)),
                )
        };

        let resolution = div().flex().gap_1().p_1().rounded_full().bg(rgb(BG_BOTTOM)).children(
            RESOLUTIONS.map(|height| {
                let selected = self.settings.max_height == height;
                div()
                    .id(("resolution", height as usize))
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .cursor_pointer()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .map(|chip| {
                        if selected {
                            chip.bg(rgb(ACCENT)).text_color(rgb(0xffffff))
                        } else {
                            chip.text_color(rgb(MUTED)).hover(|style| style.bg(rgb(HOVER)).text_color(rgb(TEXT)))
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_settings(|settings| settings.max_height = height, cx)
                    }))
                    .child(format!("{height}p"))
            }),
        );

        let seek_fallback = self.settings.seek_fallback;
        let toggle = div()
            .id("seek-fallback")
            .w(px(44.0))
            .h(px(24.0))
            .flex_none()
            .rounded_full()
            .cursor_pointer()
            .bg(rgb(if seek_fallback { ACCENT } else { TRACK }))
            .p(px(3.0))
            .flex()
            .when(seek_fallback, |track| track.justify_end())
            .child(div().size(px(18.0)).rounded_full().bg(rgb(0xffffff)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_settings(|settings| settings.seek_fallback = !settings.seek_fallback, cx)
            }));

        let shortcuts: [(&str, &str); 7] = [
            ("Ctrl L", "Focus the address bar"),
            ("Ctrl ,", "Open settings"),
            ("Space / K", "Play or pause"),
            ("J / L", "Back / forward 10 s"),
            ("← / →", "Back / forward 5 s"),
            ("F", "Toggle fullscreen (or double-click the video)"),
            ("Esc", "Leave fullscreen"),
        ];
        let shortcut_list = div().flex().flex_col().gap_2().children(shortcuts.map(|(keys, action)| {
            div()
                .flex()
                .items_center()
                .gap_4()
                .child(
                    div()
                        .w(px(96.0))
                        .flex_none()
                        .child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .bg(rgb(BG_BOTTOM))
                                .border_1()
                                .border_color(rgb(TRACK))
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(TEXT))
                                .child(keys),
                        ),
                )
                .child(div().text_sm().text_color(rgb(MUTED)).child(action))
        }));

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.top_bar(false, cx))
            .child(
                div().id("settings-scroll").flex_1().min_h_0().overflow_y_scroll().child(
                    div()
                        .max_w(px(760.0))
                        .px_2()
                        .pb_6()
                        .flex()
                        .flex_col()
                        .gap_6()
                        .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child("Settings"))
                        .child(section("PLAYBACK").child(
                            row(
                                "Maximum resolution",
                                "The highest quality moth will play. Lower uses less bandwidth and power. Applies to the next video you open.",
                            )
                            .child(resolution),
                        ))
                        .child(section("SEEKING").child(
                            row(
                                "Seekable stream URLs in the background",
                                "YouTube's iOS stream URLs only serve a little ahead of playback, so moth asks yt-dlp for web URLs to the same files. Costs a few seconds of CPU per video; without it, seeking far ahead can fail.",
                            )
                            .child(toggle),
                        ))
                        .child(section("KEYBOARD SHORTCUTS").child(
                            div().rounded_xl().bg(rgb(PANEL)).px_5().py_4().child(shortcut_list),
                        ))
                        .child(section("ABOUT").child(
                            div()
                                .rounded_xl()
                                .bg(rgb(PANEL))
                                .px_5()
                                .py_4()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(concat!("moth ", env!("CARGO_PKG_VERSION"))),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(MUTED))
                                        .child(format!("Settings are saved to {}", Settings::path().display())),
                                ),
                        ))
                        .when_some(self.settings_error.clone(), |column, error| {
                            column.child(div().text_sm().text_color(rgb(ACCENT)).child(error))
                        }),
                ),
            )
    }

    fn top_bar(&self, back: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let on_back = back.then(|| cx.listener(|this: &mut AppView, _: &gpui::ClickEvent, window, cx| this.go_home(window, cx)));
        let address_focused = self.address_focused;
        div()
            .flex()
            .items_center()
            .gap_4()
            .h(px(44.0))
            .flex_none()
            .when_some(on_back, |bar, on_back| {
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
                        .on_click(on_back),
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
                    .border_color(rgb(if address_focused { 0x4a5679 } else { PANEL }))
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
            .child(div().flex_1())
            .child(
                div()
                    .px_4()
                    .py_2()
                    .rounded_lg()
                    .bg(rgb(ACCENT))
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(0xffffff))
                    .child("CREATE"),
            )
            .child(avatar("Sortedcord", 34.0))
    }
}

// ── Components ───────────────────────────────────────────────────────────────

const NAV: [(Icon, &str, &str); 3] = [
    (Icon::Home, "Home", "Home"),
    (Icon::Trending, "Trending", "Trending"),
    (Icon::Subscriptions, "Subscriptions", "Subs"),
];
const LIBRARY: [(Icon, &str, &str); 3] = [
    (Icon::History, "History", "History"),
    (Icon::WatchLater, "Watch Later", "Later"),
    (Icon::Liked, "Liked Videos", "Liked"),
];
const CHANNELS: [&str; 2] = ["Blender", "Blender Studio"];

/// Full sidebar on wide windows; on narrow ones it collapses to an icon rail (like YouTube's
/// mini guide) instead of disappearing.
fn sidebar(collapsed: bool, active: &'static str, cx: &mut Context<AppView>) -> impl IntoElement + use<> {
    let mut on_home = Some(cx.listener(|this, _: &gpui::ClickEvent, window, cx| this.go_home(window, cx)));
    let mut on_settings = Some(cx.listener(|this, _: &gpui::ClickEvent, window, cx| this.open_settings(window, cx)));
    let nav = move |(kind, label, short): (Icon, &'static str, &'static str)| {
        let is_active = label == active;
        let color = rgb(if is_active { TEXT } else { MUTED });
        let item = div()
            .id(label)
            .rounded_lg()
            .cursor_pointer()
            .text_color(color)
            .hover(|style| style.bg(rgb(HOVER)).text_color(rgb(TEXT)))
            .when(is_active, |item| item.bg(rgb(HOVER)).font_weight(FontWeight::SEMIBOLD));
        if collapsed {
            item.flex()
                .flex_col()
                .items_center()
                .gap_1()
                .py_2p5()
                .text_size(px(10.5))
                .child(icon(kind, 20.0, rgb(if is_active { ACCENT } else { MUTED }).into()))
                .child(short)
        } else {
            item.flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .text_sm()
                .child(icon(kind, 18.0, rgb(if is_active { ACCENT } else { MUTED }).into()))
                .child(label)
        }
    };
    let label = |text: &'static str| {
        div().px_3().pt_4().pb_1().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(FAINT)).child(text)
    };
    let divider = || div().mx_3().my_2().h(px(1.0)).bg(rgb(TRACK));
    let channel = move |name: &'static str| {
        let item = div()
            .id(name)
            .flex()
            .items_center()
            .rounded_lg()
            .cursor_pointer()
            .text_sm()
            .text_color(rgb(MUTED))
            .hover(|style| style.bg(rgb(HOVER)).text_color(rgb(TEXT)));
        if collapsed {
            item.justify_center().py_1p5().child(avatar(name, 26.0))
        } else {
            item.gap_3().px_3().py_1p5().child(avatar(name, 22.0)).child(name)
        }
    };
    let logo_mark = div()
        .w(px(30.0))
        .h(px(21.0))
        .flex_none()
        .rounded(px(6.0))
        .bg(rgb(ACCENT))
        .flex()
        .items_center()
        .justify_center()
        .child(play_icon(9.0, rgb(0xffffff).into()));

    div()
        .w(px(if collapsed { RAIL_W } else { SIDEBAR_W }))
        .flex_none()
        .h_full()
        .rounded(px(20.0))
        .bg(rgb(PANEL))
        .when(collapsed, |rail| rail.px_1p5().py_4())
        .when(!collapsed, |panel| panel.p_4())
        .flex()
        .flex_col()
        .gap_0p5()
        .child(if collapsed {
            div().flex().justify_center().pb_4().pt_1().child(logo_mark)
        } else {
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .pb_5()
                .pt_1()
                .child(logo_mark)
                .child(div().font_weight(FontWeight::SEMIBOLD).child("moth"))
        })
        .children(NAV.map(|entry| {
            let item = nav(entry);
            match (entry.1, on_home.take()) {
                ("Home", Some(on_home)) => item.on_click(on_home),
                _ => item,
            }
        }))
        .map(|column| if collapsed { column.child(divider()) } else { column.child(label("LIBRARY")) })
        .children(LIBRARY.map(nav))
        .map(|column| if collapsed { column.child(divider()) } else { column.child(label("CHANNELS")) })
        .children(CHANNELS.map(channel))
        .child(div().flex_1())
        .when_some(on_settings.take(), |column, on_settings| {
            column.child(nav((Icon::Settings, "Settings", "Settings")).on_click(on_settings))
        })
}

fn section_heading(title: &'static str, action: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_5()
        .px_2()
        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(title))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(MUTED))
                .child(play_icon(8.0, rgb(MUTED).into()))
                .child(action),
        )
}

fn video_card(ix: usize, video: &'static data::Video, card_w: f32, cx: &mut Context<AppView>) -> impl IntoElement + use<> {
    let thumb_h = (card_w - 16.0) * 9.0 / 16.0;
    div()
        .id(("card", ix))
        .w(px(card_w))
        .p_2()
        .rounded_xl()
        .cursor_pointer()
        .hover(|style| style.bg(rgb(HOVER)))
        .flex()
        .flex_col()
        .gap_2()
        .on_click(cx.listener(move |this, _, window, cx| this.open_video(video.id.into(), window, cx)))
        .child(thumbnail(
            Some(ImageSource::from(video.thumbnail())),
            Some(video.duration.to_string()),
            px(card_w - 16.0),
            px(thumb_h),
        ))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .line_height(px(19.0))
                .line_clamp(2)
                .text_ellipsis()
                .child(video.title),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_xs().text_color(rgb(MUTED)).child(video.channel))
                .child(div().text_xs().text_color(rgb(FAINT)).child(format!("{} · {}", video.views, video.age))),
        )
}

fn thumbnail(source: Option<ImageSource>, duration: Option<String>, w: Pixels, h: Pixels) -> impl IntoElement {
    div()
        .relative()
        .flex_none()
        .w(w)
        .h(h)
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(0x0b0f1a))
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

fn avatar(name: &str, diameter: f32) -> impl IntoElement {
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
        .text_color(rgb(0xffffff))
        .child(initial)
}

fn pill(text: String) -> impl IntoElement {
    div()
        .px_3()
        .py_1p5()
        .rounded_full()
        .bg(rgb(PANEL))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(MUTED))
        .child(text)
}

#[derive(Clone, Copy)]
enum Icon {
    Home,
    Trending,
    Subscriptions,
    History,
    WatchLater,
    Liked,
    Settings,
}

/// Line icons drawn on a 24×24 grid.
fn icon(kind: Icon, size: f32, color: Hsla) -> impl IntoElement {
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
                Icon::Trending => {
                    path.add_polygon(&[at(3.0, 17.0), at(9.0, 11.0), at(13.0, 15.0), at(21.0, 7.0)], false);
                    path.add_polygon(&[at(15.0, 7.0), at(21.0, 7.0), at(21.0, 13.0)], false);
                }
                Icon::Subscriptions => {
                    path.add_polygon(&[at(7.0, 3.5), at(17.0, 3.5)], false);
                    path.add_polygon(&[at(5.0, 6.5), at(19.0, 6.5)], false);
                    path.add_polygon(&[at(3.0, 9.5), at(21.0, 9.5), at(21.0, 20.5), at(3.0, 20.5)], true);
                    path.add_polygon(&[at(10.5, 12.5), at(15.0, 15.0), at(10.5, 17.5)], true);
                }
                Icon::History => {
                    // Counter-clockwise arrow around a clock face.
                    path.move_to(at(4.5, 12.0));
                    path.arc_to(radius(7.5), px(0.0), true, true, at(6.7, 17.3));
                    path.add_polygon(&[at(2.3, 9.8), at(4.5, 12.0), at(6.9, 10.0)], false);
                    path.add_polygon(&[at(12.0, 8.0), at(12.0, 12.0), at(14.8, 13.8)], false);
                }
                Icon::WatchLater => {
                    path.move_to(at(12.0, 3.5));
                    path.arc_to(radius(8.5), px(0.0), false, true, at(12.0, 20.5));
                    path.arc_to(radius(8.5), px(0.0), false, true, at(12.0, 3.5));
                    path.add_polygon(&[at(12.0, 7.5), at(12.0, 12.0), at(15.0, 14.0)], false);
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
                Icon::Liked => {
                    path.move_to(at(12.0, 20.0));
                    path.cubic_bezier_to(at(3.0, 8.8), at(12.0, 20.0), at(3.0, 14.5));
                    path.cubic_bezier_to(at(7.8, 4.0), at(3.0, 6.0), at(5.2, 4.0));
                    path.cubic_bezier_to(at(12.0, 6.4), at(9.6, 4.0), at(11.1, 5.0));
                    path.cubic_bezier_to(at(16.2, 4.0), at(12.9, 5.0), at(14.4, 4.0));
                    path.cubic_bezier_to(at(21.0, 8.8), at(18.8, 4.0), at(21.0, 6.0));
                    path.cubic_bezier_to(at(12.0, 20.0), at(21.0, 14.5), at(12.0, 20.0));
                    path.close();
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

fn play_icon(size: f32, color: Hsla) -> impl IntoElement {
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
fn fullscreen_icon(exit: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let (x, y, s) = (bounds.origin.x, bounds.origin.y, bounds.size.width);
            let k = s * 0.36;
            let mut path = PathBuilder::stroke(px(1.8));
            // (corner, horizontal direction into the icon, vertical direction into the icon)
            for (cx, cy, dx, dy) in [(x, y, 1.0, 1.0), (x + s, y, -1.0, 1.0), (x, y + s, 1.0, -1.0), (x + s, y + s, -1.0, -1.0)] {
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

fn pause_icon() -> impl IntoElement {
    let bar = || div().w(px(3.5)).h(px(12.0)).rounded_sm().bg(rgb(TEXT));
    div().flex().gap(px(3.0)).child(bar()).child(bar())
}

fn fmt_time(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

fn wayland_handles(window: &Window) -> anyhow::Result<(*mut std::ffi::c_void, *mut std::ffi::c_void)> {
    let RawWindowHandle::Wayland(w) = HasWindowHandle::window_handle(window)?.as_raw() else {
        anyhow::bail!("not a Wayland window");
    };
    let RawDisplayHandle::Wayland(d) = HasDisplayHandle::display_handle(window)?.as_raw() else {
        anyhow::bail!("not a Wayland display");
    };
    Ok((d.display.as_ptr(), w.surface.as_ptr()))
}
