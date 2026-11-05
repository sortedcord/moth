mod data;
mod player;

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, Bounds, Context, FocusHandle, FontWeight, Hsla, KeyDownEvent, MouseButton, MouseDownEvent,
    ObjectFit, PathBuilder, Pixels, SharedString, Task, Window, WindowBounds, WindowDecorations,
    WindowOptions, canvas, div, img, linear_color_stop, linear_gradient, point, prelude::*, px,
    rgb, rgba, size,
};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

use data::{VIDEOS, Video};
use player::{Player, PlayerEvent, Rect, StreamUrls};

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
const PAD: f32 = 16.0;

fn main() {
    gstreamer::init().expect("failed to initialize GStreamer");

    let app = gpui::Application::with_platform(gpui_linux::linux_platform(gpui::WindowingModes::WAYLAND));
    app.run(|cx: &mut App| {
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-Regular.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-Medium.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-SemiBold.ttf").as_slice()),
                Cow::Borrowed(include_bytes!("../assets/fonts/Outfit-Bold.ttf").as_slice()),
            ])
            .expect("failed to load fonts");

        let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("moth".into()),
                window_decorations: Some(WindowDecorations::Server),
                window_min_size: Some(size(px(960.0), px(600.0))),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let mut view = AppView::new(window, cx);
                    // Dev aid: `MOTH_OPEN=<index>` opens that video on startup.
                    if let Some(ix) = std::env::var("MOTH_OPEN").ok().and_then(|v| v.parse::<usize>().ok()) {
                        view.open_video(ix.min(VIDEOS.len() - 1), cx);
                    }
                    view
                })
            },
        )
        .expect("failed to open window");
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}

// ── State ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Route {
    Home,
    Watch(usize),
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
    player: Option<Player>,
    error: Option<String>,
}

struct AppView {
    route: Route,
    focus: FocusHandle,
    slot: Rc<RefCell<PlayerSlot>>,
    generation: u64,
    status: Status,
    position: Option<u64>,
    duration: Option<u64>,
    stream_info: Option<String>,
    decoder: Option<String>,
    subscribed: bool,
    seek_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _poll: Task<()>,
}

impl AppView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        // Lightweight UI tick for bus messages and the position readout.
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        });

        Self {
            route: Route::Home,
            focus,
            slot: Rc::default(),
            generation: 0,
            status: Status::Idle,
            position: None,
            duration: None,
            stream_info: None,
            decoder: None,
            subscribed: false,
            seek_bounds: Rc::default(),
            _poll: poll,
        }
    }

    fn open_video(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.stop();
        self.route = Route::Watch(ix);
        self.status = Status::Resolving;
        self.generation += 1;
        let generation = self.generation;
        let id = VIDEOS[ix].id.to_string();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { player::resolve_streams(&id) })
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return; // user navigated away meanwhile
                }
                match result {
                    Ok(urls) => {
                        this.stream_info = Some(urls.info.clone());
                        this.slot.borrow_mut().pending = Some(urls);
                        this.status = Status::Starting;
                    }
                    Err(err) => this.status = Status::Error(format!("{err:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn go_home(&mut self, cx: &mut Context<Self>) {
        self.stop();
        self.route = Route::Home;
        self.status = Status::Idle;
        cx.notify();
    }

    fn stop(&mut self) {
        self.generation += 1;
        *self.slot.borrow_mut() = PlayerSlot::default();
        self.position = None;
        self.duration = None;
        self.stream_info = None;
        self.decoder = None;
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        let paused = match self.slot.borrow_mut().player.as_mut() {
            Some(p) => {
                p.toggle_pause();
                p.is_paused()
            }
            None => return,
        };
        self.status = if paused { Status::Paused } else { Status::Playing };
        cx.notify();
    }

    fn seek_relative(&mut self, delta: i64) {
        if let (Some(p), Some(pos)) = (self.slot.borrow().player.as_ref(), self.position) {
            let target = (pos as i64 + delta).max(0) as u64;
            p.seek_seconds(self.duration.map_or(target, |d| target.min(d)));
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let slot = self.slot.borrow();
        let mut changed = false;

        if let Some(err) = slot.error.as_ref() {
            if self.status != Status::Error(err.clone()) {
                self.status = Status::Error(err.clone());
                changed = true;
            }
        }

        if let Some(p) = slot.player.as_ref() {
            for event in p.drain_events() {
                self.status = match event {
                    PlayerEvent::Error(e) => Status::Error(e),
                    PlayerEvent::Eos => Status::Ended,
                };
                changed = true;
            }
            if self.status == Status::Starting && p.is_playing() {
                self.status = Status::Playing;
                changed = true;
            }
            let (pos, dur) = p.position_duration();
            if (pos, dur) != (self.position, self.duration) {
                (self.position, self.duration) = (pos, dur);
                changed = true;
            }
            if self.decoder.is_none() {
                if let Some(dec) = p.video_decoder() {
                    eprintln!("video decoder: {dec}");
                    self.decoder = Some(dec);
                    changed = true;
                }
            }
        }
        drop(slot);
        if changed {
            cx.notify();
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Watch(_)) {
            return;
        }
        match ev.keystroke.key.as_str() {
            "space" | "k" => self.toggle_pause(cx),
            "escape" => self.go_home(cx),
            "left" | "j" => self.seek_relative(if ev.keystroke.key == "j" { -10 } else { -5 }),
            "right" | "l" => self.seek_relative(if ev.keystroke.key == "l" { 10 } else { 5 }),
            _ => {}
        }
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let body = match self.route {
            Route::Home => div()
                .size_full()
                .flex()
                .flex_row()
                .gap(px(PAD))
                .child(sidebar())
                .child(self.home(f32::from(viewport.width), cx))
                .into_any_element(),
            Route::Watch(ix) => self.watch(ix, f32::from(viewport.width), cx).into_any_element(),
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
    fn home(&self, viewport_w: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let main_w = viewport_w - 2.0 * PAD - SIDEBAR_W - PAD;
        let columns = if main_w > 1300.0 { 5.0 } else if main_w > 900.0 { 4.0 } else { 3.0 };
        let gap = 12.0;
        let card_w = ((main_w - 8.0 - gap * (columns - 1.0)) / columns).floor();

        div()
            .flex_1()
            .h_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_5()
            .child(top_bar(false, cx))
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
                                    VIDEOS.iter().enumerate().map(|(ix, v)| video_card(ix, v, card_w, cx)),
                                ),
                            ),
                    ),
            )
    }

    fn watch(&self, ix: usize, viewport_w: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let video = &VIDEOS[ix];
        // The player takes half the window width → roughly a quarter of the window area.
        let video_w = ((viewport_w - 2.0 * PAD) * 0.5).floor();
        let video_h = (video_w * 9.0 / 16.0).floor();

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .child(top_bar(true, cx))
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
                            .child(self.video_surface(video_w, video_h))
                            .child(self.controls(cx))
                            .child(
                                div()
                                    .id("details")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(self.details(video, cx)),
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
                            .children(
                                VIDEOS
                                    .iter()
                                    .enumerate()
                                    .filter(|(i, _)| *i != ix)
                                    .map(|(i, v)| recommendation(i, v, cx)),
                            ),
                    ),
            )
    }

    /// Placeholder box whose on-screen bounds drive the waylandsink subsurface.
    fn video_surface(&self, w: f32, h: f32) -> impl IntoElement {
        let slot = self.slot.clone();
        let label: Option<SharedString> = match &self.status {
            Status::Resolving => Some("Resolving stream…".into()),
            Status::Starting => Some("Buffering…".into()),
            Status::Error(e) => Some(format!("Playback error: {e}").into()),
            _ => None,
        };

        div()
            .relative()
            .w(px(w))
            .h(px(h))
            .flex_none()
            .rounded_md()
            .bg(rgb(0x000000))
            .flex()
            .items_center()
            .justify_center()
            .when_some(label, |d, label| {
                d.child(div().px_6().text_sm().text_color(rgb(MUTED)).child(label))
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
                            let result = wayland_handles(window)
                                .and_then(|(display, surface)| Player::new(&urls, display, surface, rect));
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

    fn controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paused = matches!(self.status, Status::Paused | Status::Ended);
        let pos = self.position.unwrap_or(0);
        let dur = self.duration.unwrap_or(0);
        let frac = if dur > 0 { (pos as f32 / dur as f32).clamp(0.0, 1.0) } else { 0.0 };
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
                    .hover(|s| s.bg(rgb(HOVER)))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx)))
                    .child(if paused { play_icon(12.0, rgb(TEXT).into()).into_any_element() } else { pause_icon().into_any_element() }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .flex_none()
                    .child(format!("{} / {}", fmt_time(pos), fmt_time(dur))),
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
                        cx.listener(move |this, ev: &MouseDownEvent, _, _| {
                            if let (Some(b), Some(dur)) = (seek_bounds.get(), this.duration) {
                                let f = ((ev.position.x - b.origin.x) / b.size.width).clamp(0.0, 1.0);
                                if let Some(p) = this.slot.borrow().player.as_ref() {
                                    p.seek_seconds((f * dur as f32) as u64);
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
                            .child(div().absolute().left_0().top_0().h_full().w(gpui::relative(frac)).rounded_full().bg(rgb(ACCENT)))
                            .child(
                                canvas(move |b, _, _| seek_bounds_paint.set(Some(b)), |_, _, _, _| {})
                                    .absolute()
                                    .size_full(),
                            ),
                    ),
            )
    }

    fn details(&self, v: &Video, cx: &mut Context<Self>) -> impl IntoElement {
        let subscribed = self.subscribed;
        let tech: Option<SharedString> = match (&self.decoder, &self.stream_info) {
            (Some(d), Some(i)) => Some(format!("decoder: {d} · {i}").into()),
            (None, Some(i)) => Some(i.clone().into()),
            _ => None,
        };

        div()
            .flex()
            .flex_col()
            .gap_4()
            .pb_4()
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).line_height(px(28.0)).child(v.title))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(avatar(v.channel, 40.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(v.channel))
                            .child(div().text_xs().text_color(rgb(MUTED)).child(v.subscribers)),
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
                            .map(|d| {
                                if subscribed {
                                    d.bg(rgb(0x7d87a3)).text_color(rgb(0x1b2236)).child("SUBSCRIBED")
                                } else {
                                    d.bg(rgb(ACCENT)).text_color(rgb(0xffffff)).child("SUBSCRIBE")
                                }
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.subscribed = !this.subscribed;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(pill(format!("Like · {}", v.likes)))
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
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{} · {}", v.views_full, v.date)),
                    )
                    .child(div().text_sm().text_color(rgb(MUTED)).line_height(px(21.0)).child(v.description)),
            )
            .when_some(tech, |d, tech| d.child(div().text_xs().text_color(rgb(FAINT)).child(tech)))
    }
}

// ── Components ───────────────────────────────────────────────────────────────

fn sidebar() -> impl IntoElement {
    let nav = |label: &'static str, active: bool| {
        div()
            .id(label)
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_lg()
            .cursor_pointer()
            .text_sm()
            .text_color(rgb(if active { TEXT } else { MUTED }))
            .hover(|s| s.bg(rgb(HOVER)).text_color(rgb(TEXT)))
            .when(active, |d| d.bg(rgb(HOVER)).font_weight(FontWeight::SEMIBOLD))
            .child(div().size(px(6.0)).rounded_full().bg(rgb(if active { ACCENT } else { TRACK })))
            .child(label)
    };
    let label = |text: &'static str| {
        div().px_3().pt_4().pb_1().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(FAINT)).child(text)
    };
    let channel = |name: &'static str| {
        div()
            .id(name)
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_1p5()
            .rounded_lg()
            .cursor_pointer()
            .text_sm()
            .text_color(rgb(MUTED))
            .hover(|s| s.bg(rgb(HOVER)).text_color(rgb(TEXT)))
            .child(avatar(name, 22.0))
            .child(name)
    };

    div()
        .w(px(SIDEBAR_W))
        .flex_none()
        .h_full()
        .rounded(px(20.0))
        .bg(rgb(PANEL))
        .p_4()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .pb_5()
                .pt_1()
                .child(
                    div()
                        .w(px(30.0))
                        .h(px(21.0))
                        .rounded(px(6.0))
                        .bg(rgb(ACCENT))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(play_icon(9.0, rgb(0xffffff).into())),
                )
                .child(div().font_weight(FontWeight::SEMIBOLD).child("moth")),
        )
        .child(nav("Home", true))
        .child(nav("Trending", false))
        .child(nav("Subscriptions", false))
        .child(label("LIBRARY"))
        .child(nav("History", false))
        .child(nav("Watch Later", false))
        .child(nav("Liked Videos", false))
        .child(label("CHANNELS"))
        .child(channel("Blender"))
        .child(channel("Blender Studio"))
}

fn top_bar(back: bool, cx: &mut Context<AppView>) -> impl IntoElement + use<> {
    let on_back = back.then(|| cx.listener(|this: &mut AppView, _: &gpui::ClickEvent, _, cx| this.go_home(cx)));
    div()
        .flex()
        .items_center()
        .gap_4()
        .h(px(44.0))
        .flex_none()
        .when_some(on_back, |d, on_back| {
            d.child(
                div()
                    .id("back")
                    .px_4()
                    .h(px(38.0))
                    .rounded_full()
                    .bg(rgb(PANEL))
                    .hover(|s| s.bg(rgb(HOVER)))
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
                .max_w(px(460.0))
                .h(px(38.0))
                .rounded_full()
                .bg(rgb(PANEL))
                .px_4()
                .flex()
                .items_center()
                .text_sm()
                .text_color(rgb(FAINT))
                .child("Search…"),
        )
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

fn video_card(ix: usize, v: &'static Video, card_w: f32, cx: &mut Context<AppView>) -> impl IntoElement + use<> {
    let thumb_h = (card_w - 16.0) * 9.0 / 16.0;
    div()
        .id(("card", ix))
        .w(px(card_w))
        .p_2()
        .rounded_xl()
        .cursor_pointer()
        .hover(|s| s.bg(rgb(HOVER)))
        .flex()
        .flex_col()
        .gap_2()
        .on_click(cx.listener(move |this, _, _, cx| this.open_video(ix, cx)))
        .child(thumbnail(v, px(card_w - 16.0), px(thumb_h)))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .line_height(px(19.0))
                .line_clamp(2)
                .text_ellipsis()
                .child(v.title),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_xs().text_color(rgb(MUTED)).child(v.channel))
                .child(div().text_xs().text_color(rgb(FAINT)).child(format!("{} · {}", v.views, v.age))),
        )
}

fn recommendation(ix: usize, v: &'static Video, cx: &mut Context<AppView>) -> impl IntoElement + use<> {
    div()
        .id(("rec", ix))
        .flex()
        .gap_3()
        .p_2()
        .rounded_xl()
        .cursor_pointer()
        .hover(|s| s.bg(rgb(HOVER)))
        .on_click(cx.listener(move |this, _, _, cx| this.open_video(ix, cx)))
        .child(thumbnail(v, px(176.0), px(99.0)))
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
                        .child(v.title),
                )
                .child(div().text_xs().text_color(rgb(MUTED)).child(v.channel))
                .child(div().text_xs().text_color(rgb(FAINT)).child(format!("{} · {}", v.views, v.age))),
        )
}

fn thumbnail(v: &Video, w: Pixels, h: Pixels) -> impl IntoElement {
    div()
        .relative()
        .flex_none()
        .w(w)
        .h(h)
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(0x0b0f1a))
        .child(img(v.thumbnail()).size_full().rounded_lg().object_fit(ObjectFit::Cover))
        .child(
            div()
                .absolute()
                .bottom(px(6.0))
                .right(px(6.0))
                .px_1p5()
                .rounded_md()
                .bg(rgba(0x0b0f1acc))
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .child(v.duration),
        )
}

fn avatar(name: &str, d: f32) -> impl IntoElement {
    let initial: SharedString = name.chars().next().unwrap_or('?').to_uppercase().to_string().into();
    let hue = name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360;
    div()
        .size(px(d))
        .flex_none()
        .rounded_full()
        .bg(gpui::hsla(hue as f32 / 360.0, 0.55, 0.55, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(d * 0.45))
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

fn play_icon(size: f32, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, (), window, _| {
            let (x, y, w, h) = (b.origin.x, b.origin.y, b.size.width, b.size.height);
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
