//! The music page: the music home's shelves. Card shelves (albums, playlists, mixes, artists)
//! and song lists both scroll sideways, as in YouTube Music. Clicking anything plays it.

use gpui::{Context, Entity, FontWeight, ImageSource, SharedString, div, prelude::*, px, rgb};

use super::AppView;
use super::components::{round_thumbnail, thumbnail};
use super::images::ImageCache;
use super::theme::{HOVER, MUTED, TEXT};
use crate::backend::music::{MusicItem, MusicKind, MusicShelf, ShelfLayout};

const CARD_W: f32 = 176.0;
const ROW_W: f32 = 360.0;
const ROW_THUMB: f32 = 48.0;
/// Song rows per column, as YouTube Music shows them.
const ROWS_PER_COLUMN: usize = 4;

pub enum MusicHome {
    Loading,
    Loaded(Vec<MusicShelf>),
    Failed(SharedString),
}

pub fn render(home: &MusicHome, images: &Entity<ImageCache>, cx: &mut Context<AppView>) -> impl IntoElement {
    let content = match home {
        MusicHome::Loading => notice("Loading music…").into_any_element(),
        MusicHome::Failed(err) => notice(format!("Couldn't load music: {err}")).into_any_element(),
        MusicHome::Loaded(shelves) => {
            let mut rendered = Vec::with_capacity(shelves.len());
            for (ix, shelf) in shelves.iter().enumerate() {
                rendered.push(render_shelf(ix, shelf, images, cx).into_any_element());
            }
            div().flex().flex_col().gap_8().pb_6().children(rendered).into_any_element()
        }
    };
    div().id("music-scroll").flex_1().min_h_0().overflow_y_scroll().child(content)
}

fn notice(text: impl Into<SharedString>) -> impl IntoElement {
    div().text_sm().text_color(rgb(MUTED)).child(text.into())
}

fn image(item: &MusicItem, images: &Entity<ImageCache>, cx: &mut Context<AppView>) -> Option<ImageSource> {
    item.thumbnail_url.as_deref().and_then(|url| images.read(cx).get(url)).map(ImageSource::from)
}

fn render_shelf(
    ix: usize,
    shelf: &MusicShelf,
    images: &Entity<ImageCache>,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let mut children = Vec::new();
    match shelf.layout {
        ShelfLayout::Cards => {
            for (item_ix, item) in shelf.items.iter().enumerate() {
                let image = image(item, images, cx);
                children.push(card((ix, item_ix), item, image, cx).into_any_element());
            }
        }
        ShelfLayout::List => {
            for (column, items) in shelf.items.chunks(ROWS_PER_COLUMN).enumerate() {
                let mut rows = Vec::new();
                for (row_ix, item) in items.iter().enumerate() {
                    let image = image(item, images, cx);
                    rows.push(list_row((ix, column * ROWS_PER_COLUMN + row_ix), item, image, cx).into_any_element());
                }
                children
                    .push(div().flex().flex_col().gap_1().w(px(ROW_W)).flex_none().children(rows).into_any_element());
            }
        }
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(div().text_xl().font_weight(FontWeight::BOLD).text_color(rgb(TEXT)).child(shelf.title.clone()))
        .child(div().id(("music-shelf", ix)).overflow_x_scroll().flex().gap_4().pb_2().children(children))
}

fn line(text: impl Into<SharedString>) -> gpui::Div {
    div().whitespace_nowrap().overflow_hidden().text_ellipsis().child(text.into())
}

fn card(
    (shelf, ix): (usize, usize),
    item: &MusicItem,
    image: Option<ImageSource>,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let target = item.play.clone();
    let round = item.kind == MusicKind::Artist;
    div()
        .id(("music-card", shelf * 1000 + ix))
        .w(px(CARD_W))
        .flex_none()
        .flex()
        .flex_col()
        .gap_1()
        .p_1()
        .rounded_lg()
        .when(target.is_some(), |card| card.cursor_pointer().hover(|style| style.bg(rgb(HOVER))))
        .on_click(cx.listener(move |this, _, window, cx| {
            if let Some(target) = target.clone() {
                this.play_music(target, window, cx);
            }
        }))
        .child(if round {
            round_thumbnail(image, px(CARD_W - 8.0)).into_any_element()
        } else {
            thumbnail(image, None, px(CARD_W - 8.0), px(CARD_W - 8.0)).into_any_element()
        })
        .child(line(item.title.clone()).pt_1().text_sm().font_weight(FontWeight::SEMIBOLD))
        .child(line(item.subtitle.clone()).text_xs().text_color(rgb(MUTED)))
}

fn list_row(
    (shelf, ix): (usize, usize),
    item: &MusicItem,
    image: Option<ImageSource>,
    cx: &mut Context<AppView>,
) -> impl IntoElement {
    let target = item.play.clone();
    div()
        .id(("music-row", shelf * 1000 + ix))
        .flex()
        .items_center()
        .gap_3()
        .p_1()
        .rounded_lg()
        .when(target.is_some(), |row| row.cursor_pointer().hover(|style| style.bg(rgb(HOVER))))
        .on_click(cx.listener(move |this, _, window, cx| {
            if let Some(target) = target.clone() {
                this.play_music(target, window, cx);
            }
        }))
        .child(thumbnail(image, None, px(ROW_THUMB), px(ROW_THUMB)))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(line(item.title.clone()).text_sm().font_weight(FontWeight::SEMIBOLD))
                .child(line(item.subtitle.clone()).text_xs().text_color(rgb(MUTED))),
        )
}
