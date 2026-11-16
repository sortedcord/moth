//! The player settings popover (quality, format, audio track and quality, speed).

use gpui::{
    Anchor, Bounds, Context, FontWeight, MouseDownEvent, Pixels, anchored, deferred, div, point, prelude::*, px, rgb,
    rgba,
};

use super::WatchPage;
use crate::backend::media::{StreamId, quality_label};
use crate::settings::{SPEEDS, Settings};
use crate::ui::format;
use crate::ui::playback::PlaybackSession;
use crate::ui::theme::{ACCENT, FAINT, HOVER, MUTED, TEXT, TRACK};

pub const MENU_W: f32 = 300.0;
pub const ROW_H: f32 = 38.0;
pub const HEADER_H: f32 = 44.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuPage {
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
pub enum MenuAction {
    Open(MenuPage),
    /// `None` = automatic.
    Quality(Option<(u32, u8)>),
    Video(StreamId),
    Audio(StreamId),
    Track(String),
    Speed(f64),
}

impl MenuAction {
    pub fn apply(self, session: &mut PlaybackSession, cx: &mut Context<PlaybackSession>) {
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

pub struct MenuItem {
    label: String,
    detail: Option<String>,
    selected: bool,
    action: MenuAction,
}

fn item(label: impl Into<String>, detail: Option<String>, selected: bool, action: MenuAction) -> MenuItem {
    MenuItem { label: label.into(), detail, selected, action }
}

pub fn items(page: MenuPage, session: &PlaybackSession, settings: &Settings) -> Vec<MenuItem> {
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
                items.push(item(
                    "Video format",
                    Some(video.codec.to_string()),
                    false,
                    MenuAction::Open(MenuPage::Format),
                ));
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
                    item(variant.codec.to_string(), bitrate, variant.id == video.id, MenuAction::Video(variant.id))
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

pub fn height(page: MenuPage, item_count: usize) -> f32 {
    let header = if page == MenuPage::Main { 0.0 } else { HEADER_H };
    header + item_count as f32 * ROW_H + 16.0
}

/// The popover, attached to the gear button. In a window it opens downward (the video layer
/// would hide anything drawn over the video); in fullscreen the video makes room for it above
/// the control bar.
pub fn render(
    page: MenuPage,
    items: Vec<MenuItem>,
    gear: Bounds<Pixels>,
    viewport_h: f32,
    fullscreen: bool,
    cx: &mut Context<WatchPage>,
) -> impl IntoElement + use<> {
    let max_list_h = if fullscreen {
        (viewport_h * 0.6 - HEADER_H).max(ROW_H * 3.0)
    } else {
        let below = viewport_h - f32::from(gear.bottom()) - 24.0;
        (below - HEADER_H - 16.0).max(ROW_H * 3.0)
    };

    let rows: Vec<_> = items
        .into_iter()
        .enumerate()
        .map(|(ix, entry)| {
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
                .on_click(cx.listener(move |this, _, _, cx| this.menu_action(action.clone(), cx)))
                .when(page != MenuPage::Main, |row| {
                    row.child(
                        div().w(px(14.0)).flex_none().text_sm().text_color(rgb(ACCENT)).child(if entry.selected {
                            "✓"
                        } else {
                            ""
                        }),
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
            if !gear.contains(&event.position) {
                this.close_menu(cx);
            }
        }))
        .when_some(page.title(), |menu, title| {
            menu.child(
                div()
                    .id("menu-back")
                    .h(px(HEADER_H - 8.0))
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
        .child(div().id("menu-rows").max_h(px(max_list_h)).overflow_y_scroll().flex().flex_col().children(rows));

    let (corner, anchor, offset) = if fullscreen {
        (Anchor::TopRight, Anchor::BottomRight, point(px(0.0), px(-8.0)))
    } else {
        (Anchor::BottomRight, Anchor::TopRight, point(px(0.0), px(8.0)))
    };
    deferred(anchored().attach(gear, corner).anchor(anchor).offset(offset).child(menu)).with_priority(1)
}
