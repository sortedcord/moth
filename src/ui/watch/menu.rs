//! The player settings popover (quality, format, audio track and quality, speed).

use crate::backend::media::{StreamId, quality_label};
use crate::settings::{SPEEDS, Settings};
use crate::ui::actions::{APP, Cancel};
use crate::ui::format;
use crate::ui::playback::PlaybackSession;
use crate::ui::theme::{ACCENT, FAINT, FONT, HOVER, MUTED, TEXT, TRACK};
use gpui::popup::{PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupOptions};
use gpui::{
    App, Bounds, Context, Entity, FocusHandle, FontWeight, Pixels, Size, Window, WindowBackgroundAppearance,
    WindowBounds, WindowDecorations, WindowHandle, WindowKind, WindowOptions, div, point, prelude::*, px, rgb, rgba,
    size,
};

const MENU_W: f32 = 300.0;
const ROW_H: f32 = 38.0;
const HEADER_H: f32 = 44.0;
const GUTTERS: f32 = 18.0; // Vertical padding and border.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuPage {
    Main,
    Quality,
    Format,
    Track,
    Audio,
    Speed,
}

impl MenuPage {
    fn title(self) -> Option<&'static str> {
        match self {
            Self::Main => None,
            Self::Quality => Some("Quality"),
            Self::Format => Some("Video format"),
            Self::Track => Some("Audio track"),
            Self::Audio => Some("Audio quality"),
            Self::Speed => Some("Playback speed"),
        }
    }
}

#[derive(Clone, Debug)]
enum MenuAction {
    Open(MenuPage),
    /// `None` = automatic.
    Quality(Option<(u32, u8)>),
    Video(StreamId),
    Audio(StreamId),
    Track(String),
    Speed(f64),
}

impl MenuAction {
    fn apply(self, session: &mut PlaybackSession, cx: &mut Context<PlaybackSession>) {
        match self {
            Self::Open(_) => {}
            Self::Quality(quality) => session.choose_quality(quality, cx),
            Self::Video(id) => session.choose_video(id, cx),
            Self::Audio(id) => session.choose_audio(id, cx),
            Self::Track(id) => session.choose_track(&id, cx),
            Self::Speed(rate) => session.set_rate(rate, cx),
        }
    }
}

struct MenuItem {
    label: String,
    detail: Option<String>,
    selected: bool,
    action: MenuAction,
}

fn item(label: impl Into<String>, detail: Option<String>, selected: bool, action: MenuAction) -> MenuItem {
    MenuItem { label: label.into(), detail, selected, action }
}

fn items(page: MenuPage, session: &PlaybackSession, settings: &Settings) -> Vec<MenuItem> {
    let preferences = settings.stream_preferences();
    let catalog = session.catalog();
    let video = session.video_stream();
    let audio = session.audio_stream();
    match page {
        MenuPage::Main => {
            let mut items = Vec::new();
            if let (Some(catalog), Some(video), Some(audio)) = (catalog, video, audio) {
                let quality = if session.quality_auto() { format!("Auto ({})", video.label()) } else { video.label() };
                items.push(item("Quality", Some(quality), false, MenuAction::Open(MenuPage::Quality)));
                items.push(item("Video format", Some(video.format_label()), false, MenuAction::Open(MenuPage::Format)));
                if catalog.tracks().len() > 1 {
                    let track = audio.track.as_ref().map_or("Default".to_string(), |track| track.name.clone());
                    items.push(item("Audio track", Some(track), false, MenuAction::Open(MenuPage::Track)));
                }
                items.push(item("Audio quality", Some(audio.label()), false, MenuAction::Open(MenuPage::Audio)));
            }
            items.push(item(
                "Playback speed",
                Some(format::speed(session.rate())),
                false,
                MenuAction::Open(MenuPage::Speed),
            ));
            items
        }
        MenuPage::Quality => {
            let Some(catalog) = catalog else { return Vec::new() };
            let current = video.map(|video| (video.resolution(), video.fps));
            let auto = session.quality_auto();
            let mut items =
                vec![item("Auto", Some(format!("up to {}p", settings.max_height)), auto, MenuAction::Quality(None))];
            items.extend(catalog.qualities().into_iter().map(|(resolution, fps)| {
                let badge = match resolution {
                    2160.. => Some("4K".to_string()),
                    1440.. => Some("QHD".to_string()),
                    720.. => Some("HD".to_string()),
                    _ => None,
                };
                let selected = !auto && current == Some((resolution, fps));
                item(quality_label(resolution, fps), badge, selected, MenuAction::Quality(Some((resolution, fps))))
            }));
            items
        }
        MenuPage::Format => {
            let (Some(catalog), Some(video)) = (catalog, video) else { return Vec::new() };
            catalog
                .video_variants(video.resolution(), video.fps, &preferences)
                .into_iter()
                .map(|variant| {
                    let bitrate = variant.bitrate.map(|bitrate| format!("{:.1} Mbps", f64::from(bitrate) / 1e6));
                    item(variant.format_label(), bitrate, variant.id == video.id, MenuAction::Video(variant.id))
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
                    let selected = current.as_ref() == Some(&track.id);
                    item(
                        track.name.clone(),
                        track.is_default.then(|| "default".into()),
                        selected,
                        MenuAction::Track(track.id),
                    )
                })
                .collect()
        }
        MenuPage::Audio => {
            let Some(catalog) = catalog else { return Vec::new() };
            let track = audio.and_then(|audio| audio.track.as_ref()).map(|track| track.id.as_str());
            catalog
                .audio_in_track(track, &preferences)
                .into_iter()
                .map(|stream| {
                    item(stream.label(), None, audio.map(|a| a.id) == Some(stream.id), MenuAction::Audio(stream.id))
                })
                .collect()
        }
        MenuPage::Speed => SPEEDS
            .iter()
            .map(|&rate| item(format::speed(rate), None, (rate - session.rate()).abs() < 1e-9, MenuAction::Speed(rate)))
            .collect(),
    }
}

fn menu_size(page: MenuPage, item_count: usize, max_height: Pixels) -> Size<Pixels> {
    let header = if page == MenuPage::Main { 0.0 } else { HEADER_H };
    size(px(MENU_W), px(header + item_count as f32 * ROW_H + GUTTERS).min(max_height))
}

/// Its own Wayland surface: GPUI elements in the main window can't cover the video subsurface.
pub(super) struct SettingsPopup {
    session: Entity<PlaybackSession>,
    page: MenuPage,
    focus: FocusHandle,
    max_height: Pixels,
    requested_size: Size<Pixels>,
}

impl SettingsPopup {
    pub(super) fn open(
        session: Entity<PlaybackSession>,
        gear: Bounds<Pixels>,
        parent: &Window,
        cx: &mut App,
    ) -> anyhow::Result<WindowHandle<Self>> {
        let page = MenuPage::Main;
        let max_height = (parent.viewport_size().height * 0.6).floor();
        let requested_size = menu_size(page, items(page, session.read(cx), cx.global::<Settings>()).len(), max_height);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.0), px(0.0)), requested_size))),
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Server),
                kind: WindowKind::AnchoredPopup(PopupOptions {
                    parent: parent.window_handle(),
                    anchor_rect: gear,
                    anchor: PopupAnchor::TopRight,
                    gravity: PopupGravity::TopLeft,
                    // Shrink the scrollable list rather than flipping the menu below the video.
                    constraint_adjustment: PopupConstraintAdjustment::SLIDE_X | PopupConstraintAdjustment::RESIZE_Y,
                    offset: point(px(0.0), px(-8.0)),
                    grab: true,
                }),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    window.focus(&focus, cx);
                    cx.observe_in(&session, window, |this: &mut Self, _, window, cx| this.update_size(window, cx))
                        .detach();
                    Self { session, page, focus, max_height, requested_size }
                })
            },
        )
    }

    fn update_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = items(self.page, self.session.read(cx), cx.global::<Settings>()).len();
        let size = menu_size(self.page, count, self.max_height);
        if size != self.requested_size {
            self.requested_size = size;
            window.resize(size);
        }
        cx.notify();
    }

    fn menu_action(&mut self, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        if let MenuAction::Open(page) = action {
            self.page = page;
            self.update_size(window, cx);
        } else {
            self.session.update(cx, |session, cx| action.apply(session, cx));
            window.remove_window();
        }
    }
}

impl Render for SettingsPopup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render(self.page, items(self.page, self.session.read(cx), cx.global::<Settings>()), &self.focus, cx)
    }
}

fn render(
    page: MenuPage,
    items: Vec<MenuItem>,
    focus: &FocusHandle,
    cx: &mut Context<SettingsPopup>,
) -> impl IntoElement + use<> {
    let rows = items.into_iter().enumerate().map(|(ix, entry)| {
        let submenu = matches!(entry.action, MenuAction::Open(_));
        let action = entry.action;
        div()
            .id(("menu-item", ix))
            .h(px(ROW_H))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(HOVER)))
            .on_click(cx.listener(move |this, _, window, cx| this.menu_action(action.clone(), window, cx)))
            .when(page != MenuPage::Main, |row| {
                row.child(div().w(px(14.0)).flex_none().text_sm().text_color(rgb(ACCENT)).child(if entry.selected {
                    "✓"
                } else {
                    ""
                }))
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
    });

    div()
        .id("player-settings-menu")
        .key_context(APP)
        .track_focus(focus)
        .on_action(|_: &Cancel, window, _| window.remove_window())
        .size_full()
        .flex()
        .flex_col()
        .font_family(FONT)
        .text_color(rgb(TEXT))
        .py_2()
        .rounded_xl()
        .bg(rgba(0x141a2cf7))
        .border_1()
        .border_color(rgb(TRACK))
        .overflow_hidden()
        .when_some(page.title(), |menu, title| {
            menu.child(
                div()
                    .id("menu-back")
                    .h(px(HEADER_H - 8.0))
                    .flex_none()
                    .mb_2()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(rgb(TRACK))
                    .hover(|style| style.bg(rgb(HOVER)))
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.menu_action(MenuAction::Open(MenuPage::Main), window, cx)
                        }),
                    )
                    .child(div().text_color(rgb(MUTED)).child("‹"))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(title)),
            )
        })
        .child(
            div()
                .id(("menu-rows", page as usize))
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows),
        )
}
