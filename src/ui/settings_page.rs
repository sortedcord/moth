//! The settings page. Every option edits the [`Settings`] global, which is saved immediately.

use gpui::{Context, FontWeight, SharedString, div, prelude::*, px, rgb};

use super::AppView;
use super::actions::SHORTCUTS;
use super::components::{chip, chip_row, toggle};
use super::format;
use super::theme::{ACCENT, BG_BOTTOM, FAINT, MUTED, PANEL, TEXT, TRACK};
use crate::backend::media::{AudioCodec, VideoCodec};
use crate::settings::{Animations, RESOLUTIONS, SEEK_STEPS, SPEEDS, Settings};

fn section(title: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().px_1().text_xs().font_weight(FontWeight::BOLD).text_color(rgb(FAINT)).child(title))
}

fn row(label: &'static str, description: impl Into<SharedString>, control: impl IntoElement) -> impl IntoElement {
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
                .child(div().text_sm().text_color(rgb(MUTED)).line_height(px(20.0)).child(description.into())),
        )
        .child(control)
}

/// A segmented control: one chip per option, the current one highlighted.
fn choice<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: impl IntoIterator<Item = T>,
    current: T,
    label: impl Fn(T) -> String,
    set: fn(&mut Settings, T),
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    chip_row().children(options.into_iter().enumerate().map(|(ix, option)| {
        chip((id, ix), label(option), option == current)
            .on_click(cx.listener(move |this, _, _, cx| this.update_settings(|settings| set(settings, option), cx)))
    }))
}

fn switch(id: &'static str, on: bool, flip: fn(&mut Settings), cx: &mut Context<AppView>) -> impl IntoElement {
    toggle(id, on).on_click(cx.listener(move |this, _, _, cx| this.update_settings(flip, cx)))
}

pub fn render(settings: &Settings, error: Option<SharedString>, cx: &mut Context<AppView>) -> impl IntoElement {
    let seconds = |step: u32| format!("{step} s");
    let playback = section("PLAYBACK")
        .child(row(
            "Maximum resolution",
            "The highest quality picked automatically. Lower uses less bandwidth and power. Applies to the next video you open.",
            choice("resolution", RESOLUTIONS, settings.max_height, |h| format!("{h}p"), |s, h| s.max_height = h, cx),
        ))
        .child(row(
            "Preferred video codec",
            "Used when a quality is available in several codecs. VP9 and AV1 are hardware-decoded on most recent GPUs.",
            choice("video-codec", VideoCodec::ALL, settings.preferred_video_codec(), |c| c.to_string(), Settings::prefer_video_codec, cx),
        ))
        .child(row(
            "Preferred audio codec",
            "Opus is what YouTube plays by default; AAC is offered for compatibility.",
            choice("audio-codec", AudioCodec::ALL, settings.audio_codec, |c| c.to_string(), |s, c| s.audio_codec = c, cx),
        ))
        .child(row(
            "Default speed",
            "The speed new videos start at.",
            choice("default-rate", SPEEDS, settings.default_rate, format::speed, |s, rate| s.default_rate = rate, cx),
        ))
        .child(row(
            "Keep speed between videos",
            "Carry the current speed over to the next video instead of starting at the default speed.",
            switch("remember-rate", settings.remember_rate, |s| s.remember_rate = !s.remember_rate, cx),
        ));

    let seeking = section("SEEKING")
        .child(row(
            "Arrow keys",
            "How far ← and → seek.",
            choice("seek-step", SEEK_STEPS, settings.seek_step, seconds, |s, step| s.seek_step = step, cx),
        ))
        .child(row(
            "J and L",
            "How far J and L seek.",
            choice("seek-step-long", SEEK_STEPS, settings.seek_step_long, seconds, |s, step| s.seek_step_long = step, cx),
        ))
        .child(row(
            "Seekable stream URLs in the background",
            "YouTube's iOS stream URLs only serve a little ahead of playback, so moth asks yt-dlp for web URLs to the same files. Costs a few seconds of CPU per video; without it, seeking far ahead can fail.",
            switch("seek-fallback", settings.seek_fallback, |s| s.seek_fallback = !s.seek_fallback, cx),
        ));

    let interface = section("INTERFACE")
        .child(row(
            "Animations",
            "Animate layout changes. System follows your desktop's reduce-motion setting.",
            choice(
                "animations",
                [Animations::System, Animations::On, Animations::Off],
                settings.animations,
                |mode| match mode {
                    Animations::System => "System".into(),
                    Animations::On => "On".into(),
                    Animations::Off => "Off".into(),
                },
                |s, mode| s.animations = mode,
                cx,
            ),
        ))
        .child(row(
            "Hide Shorts",
            "Leave Shorts out of the up-next list.",
            switch("hide-shorts", settings.hide_shorts, |s| s.hide_shorts = !s.hide_shorts, cx),
        ))
        .child(row(
            "Player debug panel",
            "Show pipeline, decoder and network details under the video (F8 toggles it while watching).",
            switch("player-stats", settings.player_stats, |s| s.player_stats = !s.player_stats, cx),
        ))
        .child(row(
            "Stop video while hidden",
            "While the window is out of sight, don't download or decode video; audio keeps playing, \
             and the picture catches up when you come back.",
            switch(
                "suspend-hidden-video",
                settings.suspend_hidden_video,
                |s| s.suspend_hidden_video = !s.suspend_hidden_video,
                cx,
            ),
        ));

    let shortcut_list = div().flex().flex_col().gap_2().children(SHORTCUTS.iter().filter_map(|shortcut| {
        let label = shortcut.label?;
        Some(
            div()
                .flex()
                .items_center()
                .gap_4()
                .child(
                    div().w(px(96.0)).flex_none().child(
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
                            .child(label),
                    ),
                )
                .child(div().text_sm().text_color(rgb(MUTED)).child((shortcut.describe)(settings))),
        )
    }));

    div().id("settings-scroll").flex_1().min_h_0().overflow_y_scroll().child(
        div()
            .max_w(px(760.0))
            .px_2()
            .pb_6()
            .flex()
            .flex_col()
            .gap_6()
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child("Settings"))
            .child(playback)
            .child(seeking)
            .child(interface)
            .child(
                section("KEYBOARD SHORTCUTS")
                    .child(div().rounded_xl().bg(rgb(PANEL)).px_5().py_4().child(shortcut_list)),
            )
            .child(
                section("ABOUT").child(
                    div()
                        .rounded_xl()
                        .bg(rgb(PANEL))
                        .px_5()
                        .py_4()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div().font_weight(FontWeight::SEMIBOLD).child(concat!("moth ", env!("CARGO_PKG_VERSION"))),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .child(format!("Settings are saved to {}", Settings::path().display())),
                        ),
                ),
            )
            .when_some(error, |column, error| column.child(div().text_sm().text_color(rgb(ACCENT)).child(error))),
    )
}
