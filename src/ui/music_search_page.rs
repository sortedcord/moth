//! YouTube Music search results: shelves and rows with category filters and pagination.

use gpui::{Context, Entity, EventEmitter, FontWeight, ImageSource, SharedString, Task, div, prelude::*, px, rgb};

use super::BackendHandle;
use super::components::{chip, chip_row, pill, round_thumbnail, thumbnail};
use super::images::ImageCache;
use super::theme::{ACCENT, FAINT, HOVER, MUTED, PANEL, TEXT};
use crate::backend::music::{
    MusicItem, MusicKind, MusicSearchFilter, MusicSearchMore, MusicSearchPage as MusicSearchResult, MusicSearchShelf,
    PlayTarget,
};

pub enum MusicSearchEvent {
    Play(PlayTarget),
    GoMusic,
}

enum Status {
    Loading,
    Loaded,
    Failed(SharedString),
}

pub struct MusicSearchPage {
    query: String,
    filter: MusicSearchFilter,
    corrected_query: Option<String>,
    shelves: Vec<MusicSearchShelf>,
    more: Option<MusicSearchMore>,
    status: Status,
    loading_more: bool,
    more_error: Option<SharedString>,
    images: Entity<ImageCache>,
    _task: Option<Task<()>>,
}

impl EventEmitter<MusicSearchEvent> for MusicSearchPage {}

impl MusicSearchPage {
    pub fn new(query: String, filter: MusicSearchFilter, images: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        cx.observe(&images, |_, _, cx| cx.notify()).detach();
        let mut page = Self {
            query,
            filter,
            corrected_query: None,
            shelves: Vec::new(),
            more: None,
            status: Status::Loading,
            loading_more: false,
            more_error: None,
            images,
            _task: None,
        };
        page.fetch(cx);
        page
    }

    fn set_filter(&mut self, filter: MusicSearchFilter, cx: &mut Context<Self>) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.shelves.clear();
        self.more = None;
        self.loading_more = false;
        self.more_error = None;
        self.status = Status::Loading;
        self.fetch(cx);
        cx.notify();
    }

    fn fetch(&mut self, cx: &mut Context<Self>) {
        let search = cx.global::<BackendHandle>().0.music_search(self.query.clone(), self.filter);
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = search.await;
            this.update(cx, |this, cx| this.loaded(result, cx)).ok();
        }));
    }

    fn loaded(&mut self, result: anyhow::Result<MusicSearchResult>, cx: &mut Context<Self>) {
        self._task = None;
        match result {
            Ok(page) => {
                self.corrected_query = page.corrected_query;
                self.more = page.more;
                self.load_images(&page.shelves, cx);
                self.shelves = page.shelves;
                self.status = Status::Loaded;
            }
            Err(err) => {
                self.status = Status::Failed(format!("{err:#}").into());
            }
        }
        cx.notify();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.loading_more {
            return;
        }
        let Some(more) = self.more.clone() else { return };
        self.loading_more = true;
        self.more_error = None;
        let fetch = cx.global::<BackendHandle>().0.music_search_more(more);
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |this, cx| {
                this._task = None;
                this.loading_more = false;
                match result {
                    Ok(page) => {
                        this.more = page.more;
                        this.load_images(&page.shelves, cx);
                        for shelf in page.shelves {
                            if let Some(last) = this.shelves.last_mut() {
                                last.items.extend(shelf.items);
                            } else {
                                this.shelves.push(shelf);
                            }
                        }
                    }
                    Err(err) => {
                        this.more_error = Some(format!("{err:#}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn load_images(&self, shelves: &[MusicSearchShelf], cx: &mut Context<Self>) {
        let urls: Vec<String> =
            shelves.iter().flat_map(|shelf| &shelf.items).filter_map(|item| item.thumbnail_url.clone()).collect();
        self.images.update(cx, |images, cx| urls.into_iter().for_each(|url| images.load(url, cx)));
    }
}

impl Render for MusicSearchPage {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .child(
                div()
                    .id("music-search-back")
                    .px_3()
                    .py_1p5()
                    .rounded_full()
                    .bg(rgb(PANEL))
                    .hover(|style| style.bg(rgb(HOVER)))
                    .cursor_pointer()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .child("← Music")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(MusicSearchEvent::GoMusic))),
            )
            .child(div().text_sm().text_color(rgb(MUTED)).child(format!("Music results for “{}”", self.query)))
            .when_some(self.corrected_query.clone(), |row, corrected| {
                row.child(div().text_xs().text_color(rgb(FAINT)).child(format!("Showing results for “{corrected}”")))
            });

        let filters = div().flex_none().flex().child(chip_row().children(
            MusicSearchFilter::ALL.into_iter().enumerate().map(|(ix, filter)| {
                chip(("music-filter", ix), filter.label(), filter == self.filter)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx)))
            }),
        ));

        let body = match &self.status {
            Status::Loading => div().text_sm().text_color(rgb(MUTED)).child("Searching music…").into_any_element(),
            Status::Failed(err) => {
                div().text_sm().text_color(rgb(ACCENT)).child(format!("Search failed: {err}")).into_any_element()
            }
            Status::Loaded if self.shelves.is_empty() => {
                div().text_sm().text_color(rgb(MUTED)).child("No music results found.").into_any_element()
            }
            Status::Loaded => {
                let show_kind = self.filter == MusicSearchFilter::All;
                let shelves: Vec<_> = self
                    .shelves
                    .iter()
                    .enumerate()
                    .map(|(ix, shelf)| render_shelf(ix, shelf, show_kind, &self.images, cx))
                    .collect();
                let footer = (self.more.is_some() || self.loading_more || self.more_error.is_some()).then(|| {
                    div()
                        .pt_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .when(self.loading_more, |footer| {
                            footer.child(div().text_sm().text_color(rgb(MUTED)).child("Loading more…"))
                        })
                        .when(!self.loading_more && self.more.is_some(), |footer| {
                            footer.child(
                                chip("music-search-load-more", "Load more", false)
                                    .bg(rgb(PANEL))
                                    .text_color(rgb(TEXT))
                                    .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                            )
                        })
                        .when_some(self.more_error.clone(), |footer, err| {
                            footer.child(div().text_xs().text_color(rgb(ACCENT)).child(err))
                        })
                });
                div()
                    .max_w(px(960.0))
                    .flex()
                    .flex_col()
                    .gap_6()
                    .pb_6()
                    .children(shelves)
                    .children(footer)
                    .into_any_element()
            }
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_4()
            .child(header)
            .child(filters)
            .child(div().id("music-search-scroll").flex_1().min_h_0().overflow_y_scroll().child(body))
    }
}

fn render_shelf(
    ix: usize,
    shelf: &MusicSearchShelf,
    show_kind: bool,
    images: &Entity<ImageCache>,
    cx: &mut Context<MusicSearchPage>,
) -> impl IntoElement + use<> {
    let rows: Vec<_> =
        shelf.items.iter().enumerate().map(|(item_ix, item)| row((ix, item_ix), item, show_kind, images, cx)).collect();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_1()
                .child(div().text_lg().font_weight(FontWeight::BOLD).text_color(rgb(TEXT)).child(shelf.title.clone()))
                .when_some(shelf.filter, |header, filter| {
                    header.child(
                        chip(("shelf-more", ix), "Show all", false)
                            .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx))),
                    )
                }),
        )
        .child(div().rounded_xl().bg(rgb(PANEL)).p_2().flex().flex_col().gap_0p5().children(rows))
}

fn kind_label(kind: MusicKind) -> &'static str {
    match kind {
        MusicKind::Song => "Song",
        MusicKind::Video => "Video",
        MusicKind::Album => "Album",
        MusicKind::Playlist => "Playlist",
        MusicKind::Artist => "Artist",
    }
}

fn row(
    (shelf, ix): (usize, usize),
    item: &MusicItem,
    show_kind: bool,
    images: &Entity<ImageCache>,
    cx: &mut Context<MusicSearchPage>,
) -> impl IntoElement + use<> {
    let target = item.play.clone();
    let image = item.thumbnail_url.as_deref().and_then(|url| images.read(cx).get(url)).map(ImageSource::from);
    let thumb = if item.kind == MusicKind::Artist {
        round_thumbnail(image, px(48.0)).into_any_element()
    } else {
        thumbnail(image, None, px(48.0), px(48.0)).into_any_element()
    };
    div()
        .id(("music-search-row", shelf * 1000 + ix))
        .flex()
        .items_center()
        .gap_3()
        .p_2()
        .rounded_lg()
        .when(target.is_some(), |row| row.cursor_pointer().hover(|style| style.bg(rgb(HOVER))))
        .on_click(cx.listener(move |_, _, _, cx| {
            if let Some(target) = target.clone() {
                cx.emit(MusicSearchEvent::Play(target));
            }
        }))
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
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(item.title.clone()),
                )
                .when(!item.subtitle.is_empty(), |col| {
                    col.child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(item.subtitle.clone()),
                    )
                }),
        )
        .when(show_kind, |row| row.child(pill(kind_label(item.kind).to_string())))
}
