//! The player debug panel (F8). Sits below the controls: the video subsurface is stacked above
//! the window, so nothing can be drawn over the picture.

use std::time::Duration;

use gpui::{div, prelude::*, px, rgb};

use crate::backend::player::stats::{AlternateState, NetStats, QueueLevel, SinkStats};
use crate::ui::playback::PlaybackSession;
use crate::ui::theme::{FAINT, TRACK};

pub fn render(session: &PlaybackSession) -> Option<impl IntoElement + use<>> {
    let stats = session.stats()?;
    let secs = |time: Option<Duration>| time.map_or("—".to_string(), |time| format!("{:.3}s", time.as_secs_f64()));
    let kib = |bytes: u64| format!("{:.0} KiB", bytes as f64 / 1024.0);
    // GStreamer resets these counters on every pause/resume.
    let none = || "none (audio only)".to_string();
    let sink = |s: SinkStats| format!("{} rendered · {} dropped (since last play)", s.rendered, s.dropped);
    let queue = |q: QueueLevel| {
        format!("{} buf · {} · {:.0} ms", q.buffers, kib(u64::from(q.bytes)), q.time.as_secs_f64() * 1e3)
    };
    let net = |n: &NetStats| {
        let alternate = match n.alternate {
            AlternateState::None => "none",
            AlternateState::Resolving => "resolving",
            AlternateState::Ready => "ready",
            AlternateState::Switched => "switched",
        };
        format!(
            "{} · {} · {} req · {} 403 · last {:.1} MiB/s in {} ms · {} queued · alternate {alternate}",
            n.host,
            kib(n.bytes),
            n.requests,
            n.forbidden,
            n.last_rate / (1024.0 * 1024.0),
            n.last_request.as_millis(),
            kib(n.queued),
        )
    };
    let jump = session
        .last_jump()
        .map_or("none".to_string(), |(seconds, at)| format!("{seconds:+.2}s, {:.0}s ago", at.elapsed().as_secs_f64()));
    let rows: Vec<(&str, String)> = vec![
        ("state", format!("{} · {}", stats.state, session.decoder().unwrap_or("decoder —"))),
        (
            "clock",
            format!(
                "{} · running {} · position {} · rate {}×",
                stats.clock,
                secs(stats.running),
                secs(stats.position),
                stats.rate
            ),
        ),
        ("stream", session.stream_summary().unwrap_or_default()),
        ("video caps", stats.video_caps.clone().unwrap_or_default()),
        ("audio caps", stats.audio_caps.clone().unwrap_or_default()),
        ("video sink", stats.video_sink.map_or_else(none, sink)),
        ("audio sink", sink(stats.audio_sink)),
        ("video queue", stats.video_queue.map_or_else(none, queue)),
        ("audio queue", queue(stats.audio_queue)),
        (
            "a/v offset",
            stats.av_offset_ms.map_or("—".to_string(), |ms| format!("{ms:+.1} ms (last video − last audio pts)")),
        ),
        ("video net", stats.video_net.as_ref().map_or_else(none, net)),
        ("video", if stats.video_suspended { "suspended: window hidden" } else { "running" }.to_string()),
        ("audio net", net(&stats.audio_net)),
        ("last jump", jump),
    ];
    Some(
        div()
            .flex_none()
            .p_2()
            .border_1()
            .border_color(rgb(TRACK))
            .font_family("monospace")
            .text_size(px(11.0))
            .line_height(px(15.0))
            .flex()
            .flex_col()
            .children(rows.into_iter().map(|(label, value)| {
                div()
                    .flex()
                    .gap_2()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(div().w(px(90.0)).flex_none().text_color(rgb(FAINT)).child(label))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().child(value))
            })),
    )
}
