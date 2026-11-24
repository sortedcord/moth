//! YouTube video search results: a scrollable list with pagination.

use gpui::{Context, Entity, EventEmitter, FontWeight, ImageSource, SharedString, Task, div, img, prelude::*, px, rgb};

use super::BackendHandle;
use super::components::{avatar, chip, thumbnail};
use super::format;
use super::images::ImageCache;
use super::theme::{ACCENT, FAINT, HOVER, MUTED, PANEL, TEXT};
use crate::backend::media::{SearchMore, SearchPage, VideoRef, VideoSummary};
use crate::settings::Settings;

pub enum SearchEvent {
    OpenVideo(VideoRef),
    GoHome,
}

enum Status {
    Loading,
    Loaded,
    Failed(SharedString),
}

pub struct VideoSearchPage {
    query: String,
    corrected_query: Option<String>,
    videos: Vec<VideoSummary>,
    more: Option<SearchMore>,
    status: Status,
    loading_more: bool,
    more_error: Option<SharedString>,
    images: Entity<ImageCache>,
    _task: Option<Task<()>>,
}

impl EventEmitter<SearchEvent> for VideoSearchPage {}

impl VideoSearchPage {
    pub fn new(query: String, images: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        cx.observe(&images, |_, _, cx| cx.notify()).detach();
        let search = cx.global::<BackendHandle>().0.search(query.clone());
        let task = cx.spawn(async move |this, cx| {
            let result = search.await;
            this.update(cx, |this, cx| this.loaded(result, cx)).ok();
        });
        Self {
            query,
            corrected_query: None,
            videos: Vec::new(),
            more: None,
            status: Status::Loading,
            loading_more: false,
            more_error: None,
            images,
            _task: Some(task),
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    fn loaded(&mut self, result: anyhow::Result<SearchPage>, cx: &mut Context<Self>) {
        self._task = None;
        match result {
            Ok(page) => {
                self.corrected_query = page.corrected_query;
                self.more = page.more;
                self.load_images(&page.videos, cx);
                self.videos = page.videos;
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
        let fetch = cx.global::<BackendHandle>().0.search_more(more);
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |this, cx| {
                this._task = None;
                this.loading_more = false;
                match result {
                    Ok(page) => {
                        this.more = page.more;
                        this.load_images(&page.videos, cx);
                        for video in page.videos {
                            if !this.videos.iter().any(|known| known.video == video.video) {
                                this.videos.push(video);
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

    fn load_images(&self, videos: &[VideoSummary], cx: &mut Context<Self>) {
        let urls: Vec<String> = videos
            .iter()
            .flat_map(|video| [video.thumbnail_url.clone(), video.channel_avatar_url.clone()])
            .flatten()
            .collect();
        self.images.update(cx, |images, cx| urls.into_iter().for_each(|url| images.load(url, cx)));
    }
}

impl Render for VideoSearchPage {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hide_shorts = cx.global::<Settings>().hide_shorts;
        let visible: Vec<&VideoSummary> = self.videos.iter().filter(|video| !(hide_shorts && video.is_short)).collect();

        let header = div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .child(
                div()
                    .id("search-back-home")
                    .px_3()
                    .py_1p5()
                    .rounded_full()
                    .bg(rgb(PANEL))
                    .hover(|style| style.bg(rgb(HOVER)))
                    .cursor_pointer()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .child("← Home")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SearchEvent::GoHome))),
            )
            .child(div().text_sm().text_color(rgb(MUTED)).child(format!("Results for “{}”", self.query)))
            .when_some(self.corrected_query.clone(), |row, corrected| {
                row.child(div().text_xs().text_color(rgb(FAINT)).child(format!("Showing results for “{corrected}”")))
            });

        let body = match &self.status {
            Status::Loading => div().text_sm().text_color(rgb(MUTED)).child("Searching…").into_any_element(),
            Status::Failed(err) => {
                div().text_sm().text_color(rgb(ACCENT)).child(format!("Search failed: {err}")).into_any_element()
            }
            Status::Loaded if visible.is_empty() => {
                div().text_sm().text_color(rgb(MUTED)).child("No videos found.").into_any_element()
            }
            Status::Loaded => {
                let rows: Vec<_> =
                    visible.into_iter().enumerate().map(|(ix, video)| row(ix, video, &self.images, cx)).collect();
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
                                chip("search-load-more", "Load more", false)
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
                    .gap_3()
                    .pb_6()
                    .children(rows)
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
            .child(div().id("search-scroll").flex_1().min_h_0().overflow_y_scroll().child(body))
    }
}

fn row(
    ix: usize,
    video: &VideoSummary,
    images: &Entity<ImageCache>,
    cx: &mut Context<VideoSearchPage>,
) -> impl IntoElement + use<> {
    let target = video.video.clone();
    let thumb = video.thumbnail_url.as_deref().and_then(|url| images.read(cx).get(url)).map(ImageSource::from);
    let channel_avatar = video.channel_avatar_url.as_deref().and_then(|url| images.read(cx).get(url));
    let meta = [video.views.map(|views| format!("{} views", format::compact_count(views))), video.age.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");

    div()
        .id(("search-row", ix))
        .flex()
        .gap_4()
        .p_2()
        .rounded_xl()
        .cursor_pointer()
        .hover(|style| style.bg(rgb(HOVER)))
        .on_click(cx.listener(move |_, _, _, cx| cx.emit(SearchEvent::OpenVideo(target.clone()))))
        .child(thumbnail(thumb, video.duration.map(format::clock), px(280.0), px(157.5)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .line_height(px(22.0))
                        .line_clamp(2)
                        .text_ellipsis()
                        .child(video.title.clone()),
                )
                .when(!meta.is_empty(), |col| col.child(div().text_xs().text_color(rgb(MUTED)).child(meta)))
                .when_some(video.channel.clone(), |col, channel| {
                    col.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .py_0p5()
                            .child(match channel_avatar {
                                Some(image) => img(image).size(px(22.0)).flex_none().rounded_full().into_any_element(),
                                None => avatar(&channel, 22.0).into_any_element(),
                            })
                            .child(div().text_xs().text_color(rgb(MUTED)).truncate().child(channel)),
                    )
                })
                .when_some(video.description.clone(), |col, description| {
                    col.child(
                        div()
                            .text_xs()
                            .text_color(rgb(FAINT))
                            .line_height(px(18.0))
                            .line_clamp(2)
                            .text_ellipsis()
                            .child(description),
                    )
                }),
        )
}
