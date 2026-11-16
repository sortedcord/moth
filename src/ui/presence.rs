//! Whether the compositor is showing the window, for dropping work nobody would see.
//!
//! Wayland's way to say a window isn't shown is the `suspended` toplevel state, which gpui
//! reports as [`gpui::WindowVisibility`]. Compositors don't set it in every case: niri sets it
//! only while the screen is locked or off, not for a window on another workspace or scrolled out
//! of view. What they do in every case is hold back the frame callbacks of a surface they aren't
//! showing (niri sends them once a second instead). So, as mpv does, moth also times how long it
//! takes to be granted a frame: within a refresh or two for a window on screen, about a second
//! for one that isn't.
//!
//! This only times frames; while gpui reports the window hidden it grants none at all, so the
//! caller combines this with [`Window::observe_window_visibility`].

use std::time::{Duration, Instant};

use futures::channel::oneshot;
use gpui::{AsyncWindowContext, Context, Task, WeakEntity, Window};

/// How often to check while the window is shown. Each check draws one frame.
const INTERVAL: Duration = Duration::from_secs(1);
/// A frame granted later than this counts as a sign the window isn't shown.
const SLOW: Duration = Duration::from_millis(250);
/// How many slow frames in a row it takes to decide the window isn't shown: one can just be the
/// compositor or the GPU being busy.
const SLOW_FRAMES: u32 = 2;

/// Check whether the window is shown until the task is dropped, calling `on_change` with each
/// change (starting from shown).
pub fn watch<V: 'static>(
    window: &mut Window,
    cx: &mut Context<V>,
    on_change: impl Fn(&mut V, bool, &mut Context<V>) + 'static,
) -> Task<()> {
    cx.spawn_in(window, async move |view, cx| {
        let mut shown = true;
        let mut slow = 0;
        loop {
            // After a slow frame, and while hidden, check back to back: a check waits for the
            // compositor, which grants the frame as soon as the window is back on screen.
            if shown && slow == 0 {
                cx.background_executor().timer(INTERVAL).await;
            }
            let Some(latency) = frame_latency(&view, cx).await else { break };
            slow = if latency > SLOW { slow + 1 } else { 0 };
            let now_shown = slow == 0 || (shown && slow < SLOW_FRAMES);
            if now_shown != shown {
                shown = now_shown;
                if view.update(cx, |view, cx| on_change(view, shown, cx)).is_err() {
                    break;
                }
            }
        }
    })
}

/// Draw a frame, and time how long the compositor takes to grant the next one. `None` once the
/// window is gone.
async fn frame_latency<V: 'static>(view: &WeakEntity<V>, cx: &mut AsyncWindowContext) -> Option<Duration> {
    let (granted, latency) = oneshot::channel();
    view.update_in(cx, |_, window, cx| {
        let view = cx.entity_id();
        // The first frame comes right away; drawing it is what makes the next one wait for
        // the compositor.
        window.on_next_frame(move |window, cx| {
            cx.notify(view);
            let drawn = Instant::now();
            window.on_next_frame(move |_, _| {
                granted.send(drawn.elapsed()).ok();
            });
        });
    })
    .ok()?;
    latency.await.ok()
}
