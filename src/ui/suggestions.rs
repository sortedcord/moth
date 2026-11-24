//! Autocomplete suggestions popup anchored below the top bar.
//!
//! Uses a non-grabbing Wayland `AnchoredPopup` so it renders above the watch page's video
//! subsurface while the parent window keeps keyboard focus for typing.

use gpui::popup::{PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupOptions};
use gpui::{
    AnyWindowHandle, App, Bounds, Context, Entity, FontWeight, ImageSource, MouseButton, Pixels, ScrollHandle, Size,
    WeakEntity, Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowHandle, WindowKind,
    WindowOptions, div, point, prelude::*, px, rgb, rgba, size,
};

use super::AppView;
use super::components::{round_thumbnail, thumbnail};
use super::icons::{Icon, icon};
use super::images::ImageCache;
use super::theme::{FONT, HOVER, MUTED, TEXT, TRACK};
use crate::backend::music::{MusicItem, MusicKind};

const QUERY_ROW_H: f32 = 38.0;
const MUSIC_ROW_H: f32 = 52.0;
const GUTTERS: f32 = 16.0;
const MAX_H: f32 = 420.0;

#[derive(Clone)]
pub enum Suggestion {
    Query(String),
    Music(MusicItem),
}

pub struct SuggestionsPopup {
    parent: WeakEntity<AppView>,
    parent_window: AnyWindowHandle,
    images: Entity<ImageCache>,
    items: Vec<Suggestion>,
    selected: Option<usize>,
    scroll: ScrollHandle,
    width: Pixels,
    max_height: Pixels,
    requested_size: Size<Pixels>,
}

fn popup_size(width: Pixels, items: &[Suggestion], max_height: Pixels) -> Size<Pixels> {
    let content_h: f32 = items
        .iter()
        .map(|item| match item {
            Suggestion::Query(_) => QUERY_ROW_H,
            Suggestion::Music(_) => MUSIC_ROW_H,
        })
        .sum();
    size(width, px(content_h + GUTTERS).min(max_height))
}

impl SuggestionsPopup {
    pub fn open(
        parent: WeakEntity<AppView>,
        images: Entity<ImageCache>,
        anchor: Bounds<Pixels>,
        items: Vec<Suggestion>,
        parent_window: &Window,
        cx: &mut App,
    ) -> anyhow::Result<WindowHandle<Self>> {
        let parent_handle = parent_window.window_handle();
        let width = anchor.size.width.max(px(320.0));
        let max_height = (parent_window.viewport_size().height * 0.55).min(px(MAX_H)).floor();
        let requested_size = popup_size(width, &items, max_height);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.0), px(0.0)), requested_size))),
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Server),
                kind: WindowKind::AnchoredPopup(PopupOptions {
                    parent: parent_handle,
                    anchor_rect: anchor,
                    anchor: PopupAnchor::BottomLeft,
                    gravity: PopupGravity::BottomRight,
                    constraint_adjustment: PopupConstraintAdjustment::SLIDE_X | PopupConstraintAdjustment::RESIZE_Y,
                    offset: point(px(0.0), px(6.0)),
                    grab: false,
                }),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    cx.observe(&images, |_, _, cx| cx.notify()).detach();
                    Self {
                        parent,
                        parent_window: parent_handle,
                        images,
                        items,
                        selected: None,
                        scroll: ScrollHandle::new(),
                        width,
                        max_height,
                        requested_size,
                    }
                })
            },
        )
    }

    pub fn set_items(&mut self, items: Vec<Suggestion>, window: &mut Window, cx: &mut Context<Self>) {
        self.items = items;
        self.selected = None;
        let size = popup_size(self.width, &self.items, self.max_height);
        if size != self.requested_size {
            self.requested_size = size;
            window.resize(size);
        }
        cx.notify();
    }

    pub fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.items.is_empty() {
            self.selected = None;
            return;
        }
        let len = self.items.len();
        self.selected = match (self.selected, delta) {
            (None, d) if d > 0 => Some(0),
            (None, d) if d < 0 => Some(len - 1),
            (Some(0), d) if d < 0 => None,
            (Some(ix), d) if d > 0 && ix + 1 >= len => None,
            (Some(ix), d) => Some((ix as isize + d).clamp(0, len as isize - 1) as usize),
            (current, _) => current,
        };
        if let Some(ix) = self.selected {
            self.scroll.scroll_to_item(ix);
        }
        cx.notify();
    }

    pub fn selected_item(&self) -> Option<Suggestion> {
        self.selected.and_then(|ix| self.items.get(ix).cloned())
    }

    fn pick(&mut self, suggestion: Suggestion, window: &mut Window, cx: &mut Context<Self>) {
        let parent = self.parent.clone();
        let parent_window = self.parent_window;
        window.remove_window();
        cx.defer(move |cx| {
            cx.update_window(parent_window, |_, window, cx| {
                parent.update(cx, |this, cx| this.pick_suggestion(suggestion, window, cx)).ok();
            })
            .ok();
        });
    }
}

impl Render for SuggestionsPopup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.items.iter().enumerate().map(|(ix, item)| {
            let selected = self.selected == Some(ix);
            let picked = item.clone();
            let row = div()
                .id(("suggestion", ix))
                .flex_none()
                .px_4()
                .flex()
                .items_center()
                .gap_3()
                .cursor_pointer()
                .when(selected, |row| row.bg(rgb(HOVER)))
                .hover(|style| style.bg(rgb(HOVER)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| this.pick(picked.clone(), window, cx)),
                );
            match item {
                Suggestion::Query(term) => row
                    .h(px(QUERY_ROW_H))
                    .child(icon(Icon::Search, 15.0, rgb(MUTED).into()))
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(term.clone()))
                    .into_any_element(),
                Suggestion::Music(music) => {
                    let image = music
                        .thumbnail_url
                        .as_deref()
                        .and_then(|url| self.images.read(cx).get(url))
                        .map(ImageSource::from);
                    let thumb = if music.kind == MusicKind::Artist {
                        round_thumbnail(image, px(36.0)).into_any_element()
                    } else {
                        thumbnail(image, None, px(36.0), px(36.0)).into_any_element()
                    };
                    row.h(px(MUSIC_ROW_H))
                        .child(thumb)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child(music.title.clone()),
                                )
                                .when(!music.subtitle.is_empty(), |col| {
                                    col.child(
                                        div().text_xs().text_color(rgb(MUTED)).truncate().child(music.subtitle.clone()),
                                    )
                                }),
                        )
                        .into_any_element()
                }
            }
        });

        div()
            .id("suggestions-popup")
            .size_full()
            .flex()
            .flex_col()
            .font_family(FONT)
            .text_color(rgb(TEXT))
            .py_2()
            .rounded_xl()
            .bg(rgba(0x141a2cfa))
            .border_1()
            .border_color(rgb(TRACK))
            .overflow_hidden()
            .child(
                div()
                    .id("suggestions-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .children(rows),
            )
    }
}
