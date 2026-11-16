//! Animated home layout: the grid's target geometry is computed here and every card (plus the
//! sidebar width) follows its target on a spring, so resizes and column-count changes glide
//! instead of jumping.
//!
//! Each frame is stepped from `render`. While anything is still moving the view requests another
//! animation frame; once everything has settled no extra frames are scheduled.

use std::time::Instant;

use gpui::{ScrollHandle, SpringConfig, SpringState};

/// Slightly under-damped (ζ ≈ 0.9): settles in about a third of a second with no visible overshoot.
const SPRING: SpringConfig = SpringConfig::new(320.0, 32.0, 1.0);
/// Settled once within a quarter pixel (and the matching velocity).
const EPSILON: f32 = 0.25;
/// Longest step we take, so a stalled frame doesn't teleport everything.
const MAX_DT: f32 = 1.0 / 30.0;
/// Shortest step: much smaller steps vanish in f32 rounding and the springs would never settle.
const MIN_DT: f32 = 0.001;
/// The first step after idling (we don't know the frame interval yet).
const FIRST_DT: f32 = 1.0 / 60.0;
const SLOW_MOTION: f32 = 0.2;

pub const SIDEBAR_W: f32 = 220.0;
pub const RAIL_W: f32 = 76.0;
/// Below this window width the sidebar collapses to the rail.
pub const COLLAPSE_W: f32 = 1000.0;
pub const PAD: f32 = 16.0;
/// Room kept free on the right for the scrollbar.
const SCROLLBAR_W: f32 = 8.0;

pub const GAP: f32 = 12.0;
/// Columns are added whenever every card can still be at least this wide.
const MIN_CARD_W: f32 = 220.0;
const MAX_COLUMNS: usize = 8;
/// Title and channel lines below the thumbnail.
pub const CARD_TEXT_H: f32 = 44.0;
/// Cards this far outside the scroll viewport are still rendered.
const CULL_MARGIN: f32 = 200.0;

#[derive(Clone, Copy, Default)]
pub struct Spring {
    state: SpringState,
    target: f32,
}

impl Spring {
    pub fn value(&self) -> f32 {
        self.state.position
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn snap(&mut self, target: f32) {
        self.target = target;
        self.state = SpringState { position: target, velocity: 0.0 };
    }

    /// Advances toward `target` (which may change every frame without losing velocity).
    /// Returns whether the spring is still moving.
    pub fn step(&mut self, target: f32, dt: f32) -> bool {
        self.target = target;
        if !SPRING.is_settled(self.state, target, EPSILON) {
            self.state = SPRING.step(self.state, target, dt);
        }
        if SPRING.is_settled(self.state, target, EPSILON) {
            self.snap(target);
            false
        } else {
            true
        }
    }
}

/// Target grid geometry for a given content width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridSpec {
    pub columns: usize,
    pub card_w: f32,
}

impl GridSpec {
    pub fn for_width(main_w: f32) -> Self {
        let usable = (main_w - SCROLLBAR_W).max(0.0);
        let fit = ((usable + GAP) / (MIN_CARD_W + GAP)).floor() as usize;
        let columns = fit.clamp(1, MAX_COLUMNS);
        let card_w = ((usable - GAP * (columns as f32 - 1.0)) / columns as f32).max(1.0);
        Self { columns, card_w }
    }

    pub fn slot(&self, ix: usize) -> (f32, f32) {
        let (row, col) = (ix / self.columns, ix % self.columns);
        (col as f32 * (self.card_w + GAP), row as f32 * (card_h(self.card_w) + GAP))
    }
}

pub fn card_h(card_w: f32) -> f32 {
    thumb_h(card_w) + CARD_TEXT_H
}

pub fn thumb_h(card_w: f32) -> f32 {
    card_w * 9.0 / 16.0
}

/// Width left for the grid next to a sidebar of `sidebar_w`.
pub fn main_width(viewport_w: f32, sidebar_w: f32) -> f32 {
    viewport_w - 2.0 * PAD - sidebar_w - PAD
}

#[derive(Clone, Copy, Default)]
pub struct CardMotion {
    pub x: Spring,
    pub y: Spring,
    pub w: Spring,
}

impl CardMotion {
    pub fn is_moving(&self) -> bool {
        self.x.value() != self.x.target() || self.y.value() != self.y.target() || self.w.value() != self.w.target()
    }
}

pub struct HomeLayout {
    pub sidebar_w: Spring,
    pub cards: Vec<CardMotion>,
    pub spec: GridSpec,
    pub scroll: ScrollHandle,
    last_tick: Option<Instant>,
    initialized: bool,
    /// Debug: the step used for the last frame, and how many springs were still moving.
    pub dt: f32,
    pub moving: usize,
    /// Debug: draw target outlines and highlight moving cards.
    pub show_targets: bool,
    /// Debug: run every animation at a fifth of its speed.
    pub slow_motion: bool,
}

impl HomeLayout {
    pub fn new(card_count: usize) -> Self {
        Self {
            sidebar_w: Spring::default(),
            cards: vec![CardMotion::default(); card_count],
            spec: GridSpec::for_width(0.0),
            scroll: ScrollHandle::new(),
            last_tick: None,
            initialized: false,
            dt: 0.0,
            moving: 0,
            show_targets: false,
            slow_motion: false,
        }
    }

    /// Steps every spring toward the layout for this frame. Returns whether another frame is needed.
    pub fn step(&mut self, viewport_w: f32, reduce_motion: bool) -> bool {
        self.step_at(Instant::now(), viewport_w, reduce_motion)
    }

    fn step_at(&mut self, now: Instant, viewport_w: f32, reduce_motion: bool) -> bool {
        let dt = self.last_tick.map_or(FIRST_DT, |last| (now - last).as_secs_f32().clamp(MIN_DT, MAX_DT));
        let dt = if self.slow_motion { dt * SLOW_MOTION } else { dt };
        let snap = !self.initialized || reduce_motion;
        self.initialized = true;
        let mut moving = 0;

        let sidebar_target = if viewport_w < COLLAPSE_W { RAIL_W } else { SIDEBAR_W };
        if snap {
            self.sidebar_w.snap(sidebar_target);
        } else if self.sidebar_w.step(sidebar_target, dt) {
            moving += 1;
        }

        // The grid follows the sidebar as it animates, so the content width is continuous too.
        self.spec = GridSpec::for_width(main_width(viewport_w, self.sidebar_w.value()));
        for (ix, card) in self.cards.iter_mut().enumerate() {
            let (x, y) = self.spec.slot(ix);
            if snap {
                card.x.snap(x);
                card.y.snap(y);
                card.w.snap(self.spec.card_w);
            } else {
                // Non-short-circuiting: every spring must advance.
                let busy = card.x.step(x, dt) | card.y.step(y, dt) | card.w.step(self.spec.card_w, dt);
                moving += busy as usize;
            }
        }

        self.dt = dt;
        self.moving = moving;
        // Idle: the next movement starts fresh instead of seeing the whole idle gap as one step.
        self.last_tick = (moving > 0).then_some(now);
        moving > 0
    }

    /// Height of the scrollable content: covers both where cards are and where they're heading.
    pub fn content_height(&self) -> f32 {
        let target =
            self.cards.len().checked_sub(1).map_or(0.0, |last| self.spec.slot(last).1 + card_h(self.spec.card_w));
        let current = self.cards.iter().map(|card| card.y.value() + card_h(card.w.value())).fold(0.0, f32::max);
        target.max(current)
    }

    /// Indices of cards that intersect the scroll viewport (with margin), now or at their target.
    pub fn visible(&self, fallback_h: f32) -> impl Iterator<Item = usize> + '_ {
        let bounds = self.scroll.bounds();
        let view_h = if bounds.size.height > gpui::px(0.0) { f32::from(bounds.size.height) } else { fallback_h };
        let top = -f32::from(self.scroll.offset().y) - CULL_MARGIN;
        let bottom = top + view_h + 2.0 * CULL_MARGIN;
        self.cards.iter().enumerate().filter_map(move |(ix, card)| {
            let spans = |y: f32, w: f32| y < bottom && y + card_h(w) > top;
            (spans(card.y.value(), card.w.value()) || spans(card.y.target(), card.w.target())).then_some(ix)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_grow_with_width() {
        assert_eq!(GridSpec::for_width(100.0).columns, 1);
        assert_eq!(GridSpec::for_width(MIN_CARD_W * 2.0 + GAP + SCROLLBAR_W).columns, 2);
        assert_eq!(GridSpec::for_width(10_000.0).columns, MAX_COLUMNS);
    }

    #[test]
    fn cards_fill_the_row() {
        let spec = GridSpec::for_width(1000.0);
        let (x, _) = spec.slot(spec.columns - 1);
        assert!((x + spec.card_w - (1000.0 - SCROLLBAR_W)).abs() < 0.01);
    }

    #[test]
    fn spring_settles_on_target() {
        let mut spring = Spring::default();
        let mut frames = 0;
        while spring.step(100.0, 1.0 / 60.0) {
            frames += 1;
            assert!(frames < 120, "spring didn't settle within two seconds");
        }
        assert_eq!(spring.value(), 100.0);
    }

    #[test]
    fn layout_snaps_on_first_frame_then_animates() {
        let mut layout = HomeLayout::new(20);
        let mut now = Instant::now();
        assert!(!layout.step_at(now, 1440.0, false));
        let mut frames = 0;
        while layout.step_at(now, 900.0, false) {
            now += std::time::Duration::from_millis(16);
            frames += 1;
            assert!(frames < 120, "layout didn't settle within two seconds");
        }
        assert!(frames > 5, "crossing the collapse width should animate");
        assert_eq!(layout.sidebar_w.value(), RAIL_W);
    }
}
