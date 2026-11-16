//! The playback session: what's playing, with which streams, and the player itself.
//!
//! One session lives for the whole app; views render it and call its methods, and it notifies
//! when anything they show changes. Async work (resolving streams, alternate URLs, player events)
//! runs in tasks owned by the session, so replacing or dropping one cancels it: there's no stale
//! result to filter out.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gpui::{App, Context, Task};

use super::BackendHandle;
use crate::backend::Backend;
use crate::backend::media::{AudioStream, Selection, StreamCatalog, StreamId, VideoCodec, VideoRef, VideoStream};
use crate::backend::player::{
    Player, PlayerEvent, PlayerInput, PlayerStats, Rect, Start, VideoFormat, VideoOverlay, WaylandTarget,
};
use crate::desktop::Seek;
use crate::desktop::status::{PlaybackStatus, StreamInfo};
use crate::settings::Settings;

/// Position, duration and stats refresh while a player exists.
const TICK: Duration = Duration::from_millis(250);
/// How long the window has to stay out of sight before video is suspended: switching workspaces
/// past it, or a moment behind another window, shouldn't cost a catch-up.
const SUSPEND_AFTER: Duration = Duration::from_secs(1);
/// Playback moving this much more or less than the clock says counts as a jump.
const JUMP_THRESHOLD_SECONDS: f64 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Resolving,
    Buffering,
    Playing,
    Paused,
    Ended,
    Error(String),
}

impl Status {
    pub fn report(&self) -> (PlaybackStatus, Option<String>) {
        match self {
            Self::Idle => (PlaybackStatus::Idle, None),
            Self::Resolving => (PlaybackStatus::Resolving, None),
            Self::Buffering => (PlaybackStatus::Buffering, None),
            Self::Playing => (PlaybackStatus::Playing, None),
            Self::Paused => (PlaybackStatus::Paused, None),
            Self::Ended => (PlaybackStatus::Ended, None),
            Self::Error(err) => (PlaybackStatus::Error, Some(err.clone())),
        }
    }
}

/// Where the video is on screen. The video element reports its bounds here as it paints, which
/// moves the player's subsurface in the same frame.
#[derive(Default)]
pub struct VideoAnchor {
    rect: Cell<Option<Rect>>,
    overlay: RefCell<Option<VideoOverlay>>,
}

impl VideoAnchor {
    pub fn place(&self, rect: Rect) {
        if self.rect.replace(Some(rect)) != Some(rect)
            && let Some(overlay) = self.overlay.borrow().as_ref()
        {
            overlay.set_rect(rect);
        }
    }

    fn attach(&self, overlay: VideoOverlay) {
        *self.overlay.borrow_mut() = Some(overlay);
    }

    fn detach(&self) {
        self.overlay.borrow_mut().take();
        self.rect.set(None);
    }

    fn rect(&self) -> Rect {
        self.rect.get().unwrap_or(Rect::HIDDEN)
    }
}

/// Notices playback jumping (position moving unlike the clock): logged, and shown in the debug
/// panel. Seeks show up here too.
#[derive(Default)]
struct JumpDetector {
    previous: Option<(Instant, Duration)>,
    last: Option<(f64, Instant)>,
}

impl JumpDetector {
    fn sample(&mut self, playing: bool, position: Option<Duration>, rate: f64) {
        let now = Instant::now();
        if playing && let (Some(position), Some((then, previous))) = (position, self.previous) {
            let expected = (now - then).as_secs_f64() * rate;
            let actual = position.as_secs_f64() - previous.as_secs_f64();
            if (actual - expected).abs() > JUMP_THRESHOLD_SECONDS {
                eprintln!("moth: playback jumped {:+.2}s (expected {expected:+.2}s)", actual - expected);
                self.last = Some((actual - expected, now));
            }
        }
        self.previous = if playing { position.map(|position| (now, position)) } else { None };
    }
}

pub struct PlaybackSession {
    backend: Arc<Backend>,
    target: Result<WaylandTarget, String>,
    anchor: Rc<VideoAnchor>,
    video: Option<VideoRef>,
    status: Status,
    catalog: Option<Arc<StreamCatalog>>,
    selection: Option<Selection>,
    quality_auto: bool,
    rate: f64,
    player: Option<Player>,
    position: Option<Duration>,
    duration: Option<Duration>,
    decoder: Option<String>,
    format: Option<VideoFormat>,
    /// Width / height; the player frame takes this shape.
    aspect: Option<f32>,
    collect_stats: bool,
    stats: Option<PlayerStats>,
    jumps: JumpDetector,
    /// When the window went out of sight, if it is.
    hidden_since: Option<Instant>,
    video_suspended: bool,
    _resolve: Option<Task<()>>,
    _alternates: Option<Task<()>>,
    _events: Option<Task<()>>,
    _tick: Option<Task<()>>,
}

impl PlaybackSession {
    pub fn new(target: Result<WaylandTarget, String>, cx: &mut Context<Self>) -> Self {
        let settings = cx.global::<Settings>();
        Self {
            backend: cx.global::<BackendHandle>().0.clone(),
            target,
            anchor: Rc::default(),
            video: None,
            status: Status::Idle,
            catalog: None,
            selection: None,
            quality_auto: true,
            rate: settings.default_rate,
            player: None,
            position: None,
            duration: None,
            decoder: None,
            format: None,
            aspect: None,
            collect_stats: false,
            stats: None,
            jumps: JumpDetector::default(),
            hidden_since: None,
            video_suspended: false,
            _resolve: None,
            _alternates: None,
            _events: None,
            _tick: None,
        }
    }

    // ── Reading ──────────────────────────────────────────────────────────────

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn anchor(&self) -> Rc<VideoAnchor> {
        self.anchor.clone()
    }

    pub fn position(&self) -> Option<Duration> {
        self.position
    }

    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn aspect(&self) -> Option<f32> {
        self.aspect
    }

    pub fn decoder(&self) -> Option<&str> {
        self.decoder.as_deref()
    }

    pub fn quality_auto(&self) -> bool {
        self.quality_auto
    }

    pub fn catalog(&self) -> Option<&StreamCatalog> {
        self.catalog.as_deref()
    }

    pub fn video_stream(&self) -> Option<&VideoStream> {
        self.catalog()?.video_stream(self.selection?.video)
    }

    pub fn audio_stream(&self) -> Option<&AudioStream> {
        self.catalog()?.audio_stream(self.selection?.audio)
    }

    pub fn stats(&self) -> Option<&PlayerStats> {
        self.stats.as_ref()
    }

    /// Most recent playback jump: (seconds beyond the expected advance, when).
    pub fn last_jump(&self) -> Option<(f64, Instant)> {
        self.jumps.last
    }

    pub fn using_alternates(&self) -> bool {
        self.player.as_ref().is_some_and(Player::using_alternates)
    }

    /// e.g. "VP9 1920x1080@24 · Opus · 135 kbps · via RustyPipe (ios client)".
    pub fn stream_summary(&self) -> Option<String> {
        let catalog = self.catalog()?;
        let (video, audio) = (self.video_stream()?, self.audio_stream()?);
        let mut summary = format!(
            "{} {}x{}@{} · {} · via {}",
            video.codec,
            video.width,
            video.height,
            video.fps,
            audio.label(),
            catalog.resolver_detail
        );
        if self.using_alternates() {
            summary.push_str(" · switched to alternate URLs for seeking");
        }
        Some(summary)
    }

    /// The `stream` part of `moth status`.
    pub fn stream_info(&self) -> Option<StreamInfo> {
        let catalog = self.catalog()?;
        let video = self.video_stream();
        let width = self.format.map(|format| format.width).or(video.map(|video| video.width));
        let height = self.format.map(|format| format.height).or(video.map(|video| video.height));
        Some(StreamInfo {
            width,
            height,
            fps: self.format.and_then(|format| format.fps).or(video.map(|video| f64::from(video.fps))),
            quality: video.map(VideoStream::label).or_else(|| Some(format!("{}p", width?.min(height?)))),
            quality_auto: self.quality_auto,
            video_codec: video.map(|video| video.codec.to_string()),
            audio_codec: self.audio_stream().map(|audio| audio.codec.to_string()),
            decoder: self.decoder.clone(),
            resolver: catalog.resolver,
            fallback_urls: self.using_alternates(),
            video_suspended: self.video_suspended,
        })
    }

    // ── Controlling ──────────────────────────────────────────────────────────

    /// Start playing `video`, from `start` if given.
    pub fn open(&mut self, video: VideoRef, start: Option<Duration>, cx: &mut Context<Self>) {
        self.close(cx);
        let settings = cx.global::<Settings>();
        if !settings.remember_rate {
            self.rate = settings.default_rate;
        }
        let preferences = settings.stream_preferences();
        self.video = Some(video.clone());
        self.status = Status::Resolving;
        let streams = self.backend.streams(&video, preferences.clone());
        self._resolve = Some(cx.spawn(async move |this, cx| {
            let result = streams.await;
            this.update(cx, |this, cx| {
                let catalog = match result {
                    Ok(catalog) => Arc::new(catalog),
                    Err(err) => return this.fail(format!("{err:#}"), cx),
                };
                let Some(selection) = catalog.auto_selection(&preferences) else {
                    return this.fail("no playable streams".into(), cx);
                };
                this.catalog = Some(catalog);
                let start = (start.is_some() || this.rate != 1.0).then_some(Start {
                    position: start.unwrap_or_default(),
                    rate: this.rate,
                    paused: false,
                });
                this.play_selection(selection, start, cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// Whether the window is on screen. Video is suspended while it isn't, once it's been out of
    /// sight for a moment, and resumes as soon as it's back.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        match (shown, self.hidden_since) {
            (false, None) => self.hidden_since = Some(Instant::now()),
            (true, Some(_)) => {
                self.hidden_since = None;
                if let Some(suspended) = self.player.as_ref().map(|player| player.set_video_suspended(false))
                    && self.note_video_suspended(suspended)
                {
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// Record whether video is suspended; returns whether that changed.
    fn note_video_suspended(&mut self, suspended: bool) -> bool {
        if suspended == self.video_suspended {
            return false;
        }
        self.video_suspended = suspended;
        let position = self.position.unwrap_or_default().as_secs_f64();
        if suspended {
            eprintln!("moth: window out of sight; video suspended at {position:.1}s");
        } else {
            eprintln!("moth: video resumed at {position:.1}s");
        }
        true
    }

    /// Whether checking that the window is on screen is any use: there's video to suspend.
    pub fn wants_presence(&self, cx: &App) -> bool {
        self.player.is_some() && cx.global::<Settings>().suspend_hidden_video
    }

    /// Stop and forget the video.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self._resolve = None;
        self.stop_player();
        self.video = None;
        self.status = Status::Idle;
        self.catalog = None;
        self.selection = None;
        self.quality_auto = true;
        self.position = None;
        self.duration = None;
        self.decoder = None;
        self.format = None;
        self.aspect = None;
        self.stats = None;
        self.jumps = JumpDetector::default();
        cx.notify();
    }

    pub fn set_collect_stats(&mut self, collect: bool, cx: &mut Context<Self>) {
        self.collect_stats = collect;
        if !collect {
            self.stats = None;
        }
        cx.notify();
    }

    pub fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        match self.status {
            Status::Ended => self.restart(cx),
            Status::Paused => self.set_paused(false, cx),
            _ => self.set_paused(true, cx),
        }
    }

    pub fn play(&mut self, cx: &mut Context<Self>) {
        match self.status {
            Status::Paused => self.set_paused(false, cx),
            Status::Ended => self.restart(cx),
            _ => {}
        }
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Playing | Status::Buffering) {
            self.set_paused(true, cx);
        }
    }

    fn set_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        let Some(player) = self.player.as_mut() else { return };
        player.set_paused(paused);
        self.status = if paused { Status::Paused } else { Status::Playing };
        cx.notify();
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        let Some(player) = self.player.as_mut() else { return };
        player.seek(Duration::ZERO);
        player.set_paused(false);
        self.status = Status::Playing;
        cx.notify();
    }

    pub fn seek(&mut self, seek: Seek, cx: &mut Context<Self>) {
        let Some(player) = self.player.as_mut() else { return };
        let target = match seek {
            Seek::By(seconds) => {
                let Some(position) = self.position.or_else(|| player.position()) else { return };
                let target = Duration::from_secs_f64((position.as_secs_f64() + seconds).max(0.0));
                self.duration.map_or(target, |duration| target.min(duration))
            }
            Seek::To(target) if self.duration.is_some_and(|duration| target > duration) => return,
            Seek::To(target) => target,
        };
        player.seek(target);
        self.position = Some(target);
        cx.notify();
    }

    /// Seek to a fraction (0–1) of the video.
    pub fn seek_fraction(&mut self, fraction: f32, cx: &mut Context<Self>) {
        if let Some(duration) = self.duration {
            self.seek(Seek::To(duration.mul_f32(fraction.clamp(0.0, 1.0))), cx);
        }
    }

    pub fn set_rate(&mut self, rate: f64, cx: &mut Context<Self>) {
        self.rate = rate;
        if let Some(player) = self.player.as_mut() {
            player.set_rate(rate);
        }
        cx.notify();
    }

    /// `None` = automatic. Keeps the current codec where the quality has it.
    pub fn choose_quality(&mut self, quality: Option<(u32, u8)>, cx: &mut Context<Self>) {
        let (Some(catalog), Some(selection)) = (self.catalog.clone(), self.selection) else { return };
        let preferences = cx.global::<Settings>().stream_preferences();
        let codec: Option<VideoCodec> = self.video_stream().map(|video| video.codec);
        let video = match quality {
            None => catalog.auto_video(&preferences, codec),
            Some((resolution, fps)) => {
                let variants = catalog.video_variants(resolution, fps, &preferences);
                variants.iter().find(|video| Some(video.codec) == codec).or(variants.first()).copied()
            }
        };
        self.quality_auto = quality.is_none();
        if let Some(video) = video {
            self.switch_to(Selection { video: video.id, ..selection }, cx);
        }
        cx.notify();
    }

    pub fn choose_video(&mut self, id: StreamId, cx: &mut Context<Self>) {
        if let Some(selection) = self.selection {
            self.switch_to(Selection { video: id, ..selection }, cx);
        }
    }

    pub fn choose_audio(&mut self, id: StreamId, cx: &mut Context<Self>) {
        if let Some(selection) = self.selection {
            self.switch_to(Selection { audio: id, ..selection }, cx);
        }
    }

    /// Switch audio track, keeping the codec where the track has it.
    pub fn choose_track(&mut self, track_id: &str, cx: &mut Context<Self>) {
        let (Some(catalog), Some(selection)) = (self.catalog.clone(), self.selection) else { return };
        let preferences = cx.global::<Settings>().stream_preferences();
        let codec = self.audio_stream().map(|audio| audio.codec);
        let streams = catalog.audio_in_track(Some(track_id), &preferences);
        if let Some(audio) = streams.iter().find(|audio| Some(audio.codec) == codec).or(streams.first()) {
            self.switch_to(Selection { audio: audio.id, ..selection }, cx);
        }
    }

    /// Rebuild playback with other streams, resuming where we are.
    fn switch_to(&mut self, selection: Selection, cx: &mut Context<Self>) {
        if self.selection == Some(selection) {
            return;
        }
        let start = Start {
            position: self.position.unwrap_or_default(),
            rate: self.rate,
            paused: self.status == Status::Paused,
        };
        self.play_selection(selection, Some(start), cx);
    }

    // ── Internals ────────────────────────────────────────────────────────────

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.status = Status::Error(error);
        cx.notify();
    }

    fn stop_player(&mut self) {
        self._events = None;
        self._tick = None;
        self._alternates = None;
        self.anchor.detach();
        self.player = None;
        self.video_suspended = false;
    }

    fn play_selection(&mut self, selection: Selection, start: Option<Start>, cx: &mut Context<Self>) {
        let (Some(video), Some(catalog)) = (self.video.clone(), self.catalog.clone()) else { return };
        self.stop_player();
        self.selection = Some(selection);
        self.decoder = None;
        self.format = None;
        self.jumps.previous = None;
        if let Some(stream) = catalog.video_stream(selection.video)
            && stream.width > 0
            && stream.height > 0
        {
            self.aspect = Some(stream.width as f32 / stream.height as f32);
        }

        let alternates = cx
            .global::<Settings>()
            .seek_fallback
            .then(|| self.backend.alternate_urls(&video, &catalog, selection))
            .flatten();
        let Some(input) = PlayerInput::new(&catalog, selection, alternates.is_some()) else {
            return self.fail("the selected streams aren't in the catalog".into(), cx);
        };
        let target = match &self.target {
            Ok(target) => *target,
            Err(err) => return self.fail(err.clone(), cx),
        };
        let (player, mut events) = match Player::new(&input, start, &target, self.anchor.rect(), &self.backend.http()) {
            Ok(created) => created,
            Err(err) => return self.fail(format!("{err:#}"), cx),
        };
        self.anchor.attach(player.overlay());
        self.player = Some(player);
        self.status = Status::Buffering;

        self._events = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if this.update(cx, |this, cx| this.on_player_event(event, cx)).is_err() {
                    break;
                }
            }
        }));
        self._tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        }));
        self._alternates = alternates.map(|alternates| {
            cx.spawn(async move |this, cx| {
                let result = alternates.await;
                this.update(cx, |this, _| {
                    let Some(player) = this.player.as_ref() else { return };
                    match result {
                        Ok(urls) => player.set_alternates(urls),
                        Err(err) => {
                            eprintln!("moth: couldn't resolve alternate stream URLs: {err:#}");
                            player.alternates_failed();
                        }
                    }
                })
                .ok();
            })
        });
        cx.notify();
    }

    fn on_player_event(&mut self, event: PlayerEvent, cx: &mut Context<Self>) {
        let Some(player) = self.player.as_mut() else { return };
        player.handle(&event);
        let paused = player.is_paused();
        self.status = match event {
            PlayerEvent::Error(err) => Status::Error(err),
            PlayerEvent::Eos => Status::Ended,
            PlayerEvent::AsyncDone | PlayerEvent::SourceReady(_) => return,
            _ if self.status == Status::Ended => return,
            PlayerEvent::Playing if !paused => Status::Playing,
            PlayerEvent::Paused if paused => Status::Paused,
            PlayerEvent::Playing | PlayerEvent::Paused => return,
        };
        cx.notify();
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let Some(player) = self.player.as_ref() else { return };
        let mut changed = false;
        let mut update = |changed_now: bool| changed |= changed_now;

        let (position, duration) = (player.position(), player.duration());
        update((position, duration) != (self.position, self.duration));
        (self.position, self.duration) = (position, duration);
        self.jumps.sample(self.status == Status::Playing, position, player.rate());

        let format = player.video_format();
        update(format != self.format);
        self.format = format;
        // The decoded caps are authoritative (pixel aspect ratio, mid-stream size changes).
        if let Some(aspect) = format.and_then(|format| format.display_aspect())
            && self.aspect.is_none_or(|current| (current - aspect).abs() > 0.001)
        {
            self.aspect = Some(aspect);
            update(true);
        }
        if self.decoder.is_none()
            && let Some(decoder) = player.video_decoder()
        {
            eprintln!("moth: video decoder: {decoder}");
            self.decoder = Some(decoder);
            update(true);
        }
        let suspend = cx.global::<Settings>().suspend_hidden_video
            && self.hidden_since.is_some_and(|since| since.elapsed() >= SUSPEND_AFTER);
        let suspended = player.set_video_suspended(suspend);
        if self.collect_stats {
            self.stats = Some(player.stats());
            update(true);
        }
        update(self.note_video_suspended(suspended));
        if changed {
            cx.notify();
        }
    }
}
