//! A channel's page: banner, who it is, a Subscribe button, and its videos (latest or most
//! popular first) with pagination.

use gpui::{Context, Entity, EventEmitter, FontWeight, ImageSource, SharedString, Task, div, img, prelude::*, px, rgb};

use super::BackendHandle;
use super::components::{avatar, chip, chip_row, thumbnail};
use super::format;
use super::images::ImageCache;
use super::theme::{ACCENT, DEEP, FAINT, HOVER, MUTED, PANEL, TEXT};
use crate::backend::accounts::Role;
use crate::backend::media::{
    ChannelDetails, ChannelMore, ChannelOrder, ChannelRef, ChannelVideos, VideoRef, VideoSummary,
};

pub enum ChannelEvent {
    OpenVideo(VideoRef),
    GoBack,
}

enum Status {
    Loading,
    Loaded,
    Failed(SharedString),
}

/// Width of a video card, with its padding.
const CARD_W: f32 = 272.0;
const THUMB_W: f32 = CARD_W - 16.0;

pub struct ChannelPage {
    channel: ChannelRef,
    status: Status,
    details: Option<ChannelDetails>,
    videos: Vec<VideoSummary>,
    more: Option<ChannelMore>,
    order: ChannelOrder,
    /// Loading the videos in another order.
    sorting: bool,
    loading_more: bool,
    more_error: Option<SharedString>,
    /// Whether the account follows the channel, if known.
    subscribed: Option<bool>,
    subscribing: bool,
    subscribe_error: Option<SharedString>,
    description_open: bool,
    images: Entity<ImageCache>,
    _load: Option<Task<()>>,
    _videos: Option<Task<()>>,
    _subscribe: Option<Task<()>>,
}

impl EventEmitter<ChannelEvent> for ChannelPage {}

impl ChannelPage {
    pub fn new(channel: ChannelRef, images: Entity<ImageCache>, cx: &mut Context<Self>) -> Self {
        cx.observe(&images, |_, _, cx| cx.notify()).detach();
        let load = cx.global::<BackendHandle>().0.channel(&channel);
        let task = cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| this.loaded(result, cx)).ok();
        });
        Self {
            channel,
            status: Status::Loading,
            details: None,
            videos: Vec::new(),
            more: None,
            order: ChannelOrder::Latest,
            sorting: false,
            loading_more: false,
            more_error: None,
            subscribed: None,
            subscribing: false,
            subscribe_error: None,
            description_open: false,
            images,
            _load: Some(task),
            _videos: None,
            _subscribe: None,
        }
    }

    fn loaded(&mut self, result: anyhow::Result<ChannelDetails>, cx: &mut Context<Self>) {
        self._load = None;
        match result {
            Ok(mut details) => {
                let urls = [details.avatar_url.clone(), details.banner_url.clone()].into_iter().flatten();
                self.load_images(urls, cx);
                self.load_video_images(&details.videos.videos, cx);
                self.videos = std::mem::take(&mut details.videos.videos);
                self.more = details.videos.more.take();
                self.subscribed = details.subscribed;
                self.channel = details.channel.clone();
                self.details = Some(details);
                self.status = Status::Loaded;
            }
            Err(err) => self.status = Status::Failed(format!("{err:#}").into()),
        }
        cx.notify();
    }

    fn set_order(&mut self, order: ChannelOrder, cx: &mut Context<Self>) {
        if order == self.order && !self.sorting {
            return;
        }
        self.order = order;
        self.sorting = true;
        self.loading_more = false;
        self.more_error = None;
        let fetch = cx.global::<BackendHandle>().0.channel_videos(&self.channel, order);
        self._videos = Some(cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |this, cx| {
                this._videos = None;
                this.sorting = false;
                match result {
                    Ok(page) => this.replace_videos(page, cx),
                    Err(err) => this.more_error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn replace_videos(&mut self, page: ChannelVideos, cx: &mut Context<Self>) {
        self.load_video_images(&page.videos, cx);
        self.videos = page.videos;
        self.more = page.more;
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.loading_more || self.sorting {
            return;
        }
        let Some(more) = self.more.clone() else { return };
        self.loading_more = true;
        self.more_error = None;
        let fetch = cx.global::<BackendHandle>().0.channel_videos_more(more);
        self._videos = Some(cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |this, cx| {
                this._videos = None;
                this.loading_more = false;
                match result {
                    Ok(page) => {
                        this.more = page.more;
                        this.load_video_images(&page.videos, cx);
                        for video in page.videos {
                            if !this.videos.iter().any(|known| known.video == video.video) {
                                this.videos.push(video);
                            }
                        }
                    }
                    Err(err) => this.more_error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn toggle_subscribed(&mut self, cx: &mut Context<Self>) {
        if self.subscribing {
            return;
        }
        let target = !self.subscribed.unwrap_or(false);
        self.subscribing = true;
        self.subscribe_error = None;
        let request = cx.global::<BackendHandle>().0.subscribe(&self.channel, target);
        self._subscribe = Some(cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |this, cx| {
                this._subscribe = None;
                this.subscribing = false;
                match result {
                    Ok(()) => this.subscribed = Some(target),
                    Err(err) => this.subscribe_error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn load_images(&self, urls: impl Iterator<Item = String>, cx: &mut Context<Self>) {
        let urls: Vec<String> = urls.collect();
        self.images.update(cx, |images, cx| urls.into_iter().for_each(|url| images.load(url, cx)));
    }

    fn load_video_images(&self, videos: &[VideoSummary], cx: &mut Context<Self>) {
        self.load_images(videos.iter().filter_map(|video| video.thumbnail_url.clone()), cx);
    }

    fn image(&self, url: Option<&str>, cx: &Context<Self>) -> Option<ImageSource> {
        url.and_then(|url| self.images.read(cx).get(url)).map(ImageSource::from)
    }

    fn header(&self, details: &ChannelDetails, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let avatar_image = self.image(details.avatar_url.as_deref(), cx);
        let stats = [
            details.handle.clone(),
            details.subscribers.map(|subscribers| format!("{} subscribers", format::compact_count(subscribers))),
            details.video_count.map(|count| format!("{} videos", format::grouped_count(count))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let signed_in = cx.global::<BackendHandle>().0.account(Role::Recommendations).is_some();
        let subscribed = self.subscribed.unwrap_or(false);
        let button = div()
            .id("channel-subscribe")
            .flex_none()
            .px_5()
            .h(px(38.0))
            .rounded_full()
            .flex()
            .items_center()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .map(|button| {
                if subscribed {
                    button.bg(rgb(PANEL)).text_color(rgb(TEXT)).hover(|style| style.bg(rgb(HOVER)))
                } else {
                    button.bg(rgb(TEXT)).text_color(rgb(DEEP)).hover(|style| style.opacity(0.85))
                }
            })
            .when(self.subscribing, |button| button.opacity(0.6))
            .child(match (self.subscribing, subscribed) {
                (true, true) => "Unsubscribing…",
                (true, false) => "Subscribing…",
                (false, true) => "Subscribed",
                (false, false) => "Subscribe",
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_subscribed(cx)));

        div()
            .flex()
            .items_center()
            .gap_5()
            .child(match avatar_image {
                Some(image) => img(image).size(px(88.0)).flex_none().rounded_full().into_any_element(),
                None => avatar(&details.name, 88.0).into_any_element(),
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div().text_2xl().font_weight(FontWeight::BOLD).truncate().child(details.name.clone()),
                            )
                            .when(details.verified, |row| {
                                row.child(
                                    div()
                                        .size(px(18.0))
                                        .flex_none()
                                        .rounded_full()
                                        .bg(rgb(MUTED))
                                        .text_color(rgb(DEEP))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_xs()
                                        .font_weight(FontWeight::BOLD)
                                        .child("✓"),
                                )
                            }),
                    )
                    .when(!stats.is_empty(), |column| column.child(div().text_sm().text_color(rgb(MUTED)).child(stats)))
                    .when(!signed_in, |column| {
                        column.child(
                            div()
                                .text_xs()
                                .text_color(rgb(FAINT))
                                .child("Sign in a recommendation account in Settings to subscribe."),
                        )
                    })
                    .when_some(self.subscribe_error.clone(), |column, error| {
                        column.child(div().text_xs().text_color(rgb(ACCENT)).child(error))
                    }),
            )
            .child(button)
    }

    fn description(&self, details: &ChannelDetails, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if details.description.is_empty() {
            return None;
        }
        let open = self.description_open;
        Some(
            div()
                .id("channel-description")
                .rounded_xl()
                .bg(rgb(PANEL))
                .p_4()
                .cursor_pointer()
                .text_sm()
                .text_color(rgb(MUTED))
                .line_height(px(21.0))
                .when(!open, |panel| panel.line_clamp(2).text_ellipsis())
                .child(details.description.clone())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.description_open = !this.description_open;
                    cx.notify();
                })),
        )
    }

    fn card(&self, ix: usize, video: &VideoSummary, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let target = video.video.clone();
        let image = self.image(video.thumbnail_url.as_deref(), cx);
        let meta = [video.views.map(|views| format!("{} views", format::compact_count(views))), video.age.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        div()
            .id(("channel-video", ix))
            .w(px(CARD_W))
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .rounded_xl()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(HOVER)))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChannelEvent::OpenVideo(target.clone()))))
            .child(thumbnail(image, video.duration.map(format::clock), px(THUMB_W), px(THUMB_W * 9.0 / 16.0)))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(px(19.0))
                    .line_clamp(2)
                    .text_ellipsis()
                    .child(video.title.clone()),
            )
            .when(!meta.is_empty(), |card| card.child(div().text_xs().text_color(rgb(FAINT)).child(meta)))
    }

    fn videos_section(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let sorts = chip_row().children(ChannelOrder::ALL.map(|order| {
            chip(("channel-sort", order as usize), order.name(), order == self.order)
                .on_click(cx.listener(move |this, _, _, cx| this.set_order(order, cx)))
        }));
        let cards: Vec<_> = self.videos.iter().enumerate().map(|(ix, video)| self.card(ix, video, cx)).collect();
        let notice = if self.sorting {
            Some("Loading…")
        } else if self.videos.is_empty() {
            Some("No videos.")
        } else {
            None
        };
        let footer = (self.more.is_some() || self.loading_more || self.more_error.is_some()).then(|| {
            div()
                .pt_2()
                .flex()
                .items_center()
                .gap_3()
                .when(self.loading_more, |footer| {
                    footer.child(div().text_sm().text_color(rgb(MUTED)).child("Loading more…"))
                })
                .when(!self.loading_more && !self.sorting && self.more.is_some(), |footer| {
                    footer.child(
                        chip("channel-load-more", "Load more", false)
                            .bg(rgb(PANEL))
                            .text_color(rgb(TEXT))
                            .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                    )
                })
                .when_some(self.more_error.clone(), |footer, error| {
                    footer.child(div().text_xs().text_color(rgb(ACCENT)).child(error))
                })
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().child(sorts))
            .when_some(notice, |section, notice| section.child(div().text_sm().text_color(rgb(MUTED)).child(notice)))
            .when(!self.sorting, |section| section.child(div().flex().flex_wrap().gap_2().children(cards)))
            .children(footer)
    }
}

impl Render for ChannelPage {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let back = div().flex_none().flex().child(
            div()
                .id("channel-back")
                .px_3()
                .py_1p5()
                .rounded_full()
                .bg(rgb(PANEL))
                .hover(|style| style.bg(rgb(HOVER)))
                .cursor_pointer()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .child("← Back")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ChannelEvent::GoBack))),
        );

        let body = match (&self.status, self.details.clone()) {
            (Status::Loading, _) => div().text_sm().text_color(rgb(MUTED)).child("Loading channel…").into_any_element(),
            (Status::Failed(err), _) => div()
                .text_sm()
                .text_color(rgb(ACCENT))
                .child(format!("Couldn't load this channel: {err}"))
                .into_any_element(),
            (Status::Loaded, None) => div().into_any_element(),
            (Status::Loaded, Some(details)) => {
                let banner = self.image(details.banner_url.as_deref(), cx);
                div()
                    .max_w(px(1180.0))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .pb_6()
                    .when_some(banner, |column, banner| {
                        column.child(
                            div()
                                .w_full()
                                .h(px(150.0))
                                .rounded_xl()
                                .overflow_hidden()
                                .bg(rgb(DEEP))
                                .child(img(banner).size_full().object_fit(gpui::ObjectFit::Cover)),
                        )
                    })
                    .child(self.header(&details, cx))
                    .children(self.description(&details, cx))
                    .child(self.videos_section(cx))
                    .into_any_element()
            }
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_4()
            .child(back)
            .child(div().id("channel-scroll").flex_1().min_h_0().overflow_y_scroll().child(body))
    }
}
