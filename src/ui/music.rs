//! Music playback: a queue of tracks played audio-only (or with their video, on request) by the
//! playback session, with next/previous, shuffle, repeat and ratings. Radio-style queues load
//! more tracks as they near the end.

use std::time::Duration;

use gpui::{Context, Task};
use rand::seq::SliceRandom as _;

use super::playback::{PlaybackSession, Status};
use crate::backend::music::{PlayTarget, QueuePage, Rating, Track};

/// Within this many tracks of the end, a radio queue fetches more.
const PREFETCH_WITHIN: usize = 3;
/// "Previous" this far into a track restarts it instead.
const RESTART_AFTER: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Repeat {
    #[default]
    Off,
    /// Start over after the last track.
    All,
    /// Play the current track again and again.
    One,
}

impl Repeat {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

pub struct MusicQueue {
    pub source: &'static str,
    pub tracks: Vec<Track>,
    /// Play order: indices into `tracks` (shuffled, or in order).
    order: Vec<usize>,
    /// The current position in `order`.
    at: usize,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// The next page of a radio queue.
    more: Option<String>,
    /// The listener's rating of the current track.
    pub rating: Rating,
    /// Play the tracks' videos rather than audio alone (Song/Video set to Video).
    pub show_video: bool,
    /// Whether the expanded player, the only place the video shows, is open.
    visible: bool,
    _more: Option<Task<()>>,
    _rating: Option<Task<()>>,
}

impl MusicQueue {
    fn new(source: &'static str, page: QueuePage) -> Self {
        Self {
            source,
            order: (0..page.tracks.len()).collect(),
            tracks: page.tracks,
            at: 0,
            shuffle: false,
            repeat: Repeat::Off,
            more: page.more,
            rating: Rating::None,
            show_video: false,
            visible: false,
            _more: None,
            _rating: None,
        }
    }

    pub fn current(&self) -> Option<&Track> {
        self.tracks.get(*self.order.get(self.at)?)
    }

    /// The rest of the queue in play order, with each track's place in it.
    pub fn upcoming(&self) -> impl Iterator<Item = (usize, &Track)> {
        self.order.iter().enumerate().skip(self.at + 1).map(|(place, &ix)| (place, &self.tracks[ix]))
    }

    pub fn has_next(&self) -> bool {
        self.at + 1 < self.order.len() || self.more.is_some() || self.repeat != Repeat::Off
    }

    pub fn has_previous(&self) -> bool {
        self.at > 0
    }

    fn append(&mut self, page: QueuePage) {
        let start = self.tracks.len();
        let new: Vec<Track> = page
            .tracks
            .into_iter()
            .filter(|track| !self.tracks.iter().any(|known| known.video == track.video))
            .collect();
        self.tracks.extend(new);
        let mut added: Vec<usize> = (start..self.tracks.len()).collect();
        if self.shuffle {
            added.shuffle(&mut rand::rng());
        }
        self.order.extend(added);
        self.more = page.more;
    }
}

impl PlaybackSession {
    pub fn music(&self) -> Option<&MusicQueue> {
        self.music.as_ref()
    }

    /// Start the queue `target` leads to (a track's radio, a playlist, an album).
    pub fn play_music(&mut self, target: PlayTarget, cx: &mut Context<Self>) {
        self.close(cx);
        self.status = Status::Resolving;
        let source = target.source;
        let queue = self.backend.music_queue(target);
        self._music = Some(cx.spawn(async move |this, cx| {
            let result = queue.await;
            this.update(cx, |this, cx| match result {
                Ok(page) => {
                    this.music = Some(MusicQueue::new(source, page));
                    this.play_queued(0, cx);
                }
                Err(err) => this.status = Status::Error(format!("{err:#}")),
            })
            .ok();
        }));
        cx.notify();
    }

    /// Play the track at `place` in the queue's play order.
    pub fn play_queued(&mut self, place: usize, cx: &mut Context<Self>) {
        let Some(queue) = self.music.as_mut() else { return };
        let Some(track) = queue.order.get(place).map(|&ix| queue.tracks[ix].clone()) else { return };
        queue.at = place;
        queue.rating = Rating::None;
        let audio_only = !(queue.show_video && queue.visible);
        let rating = self.backend.rating(&track.video);
        queue._rating = Some(cx.spawn(async move |this, cx| {
            let Ok(rating) = rating.await else { return };
            this.update(cx, |this, cx| {
                if let Some(queue) = this.music.as_mut()
                    && queue.current().is_some_and(|current| current.video == track.video)
                {
                    queue.rating = rating;
                    cx.notify();
                }
            })
            .ok();
        }));
        let video = self.music.as_ref().and_then(MusicQueue::current).map(|track| track.video.clone());
        if let Some(video) = video {
            self.load(video, None, audio_only, cx);
        }
        self.fetch_more_if_needed(cx);
    }

    pub fn next(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.music.as_ref() else { return };
        if queue.at + 1 < queue.order.len() {
            let place = queue.at + 1;
            self.play_queued(place, cx);
        } else if queue.more.is_some() {
            // Waits for the next page, then plays on.
            self.status = Status::Buffering;
            self.fetch_more(true, cx);
        } else if queue.repeat != Repeat::Off {
            self.play_queued(0, cx);
        }
    }

    pub fn previous(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.music.as_ref() else { return };
        if self.position().is_some_and(|position| position > RESTART_AFTER) || queue.at == 0 {
            self.seek(crate::desktop::Seek::To(Duration::ZERO), cx);
        } else {
            let place = queue.at - 1;
            self.play_queued(place, cx);
        }
    }

    /// The current track has ended.
    pub(super) fn track_ended(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.music.as_ref() else { return };
        match queue.repeat {
            Repeat::One => {
                let place = queue.at;
                self.play_queued(place, cx);
            }
            _ if queue.has_next() => self.next(cx),
            _ => {
                self.status = Status::Ended;
                cx.notify();
            }
        }
    }

    pub fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.music.as_mut() else { return };
        let current = queue.order.get(queue.at).copied();
        queue.shuffle = !queue.shuffle;
        if queue.shuffle {
            // The current track stays where it is; the rest of the queue is shuffled after it.
            let mut rest: Vec<usize> = queue.order[queue.at + 1..].to_vec();
            rest.shuffle(&mut rand::rng());
            queue.order.truncate(queue.at + 1);
            queue.order.extend(rest);
        } else {
            queue.order = (0..queue.tracks.len()).collect();
            queue.at = current.unwrap_or(0);
        }
        cx.notify();
    }

    pub fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        if let Some(queue) = self.music.as_mut() {
            queue.repeat = queue.repeat.next();
            cx.notify();
        }
    }

    /// Like or dislike the current track (again to take it back).
    pub fn rate_track(&mut self, rating: Rating, cx: &mut Context<Self>) {
        let Some(queue) = self.music.as_mut() else { return };
        let Some(track) = queue.current().cloned() else { return };
        let rating = if queue.rating == rating { Rating::None } else { rating };
        let before = std::mem::replace(&mut queue.rating, rating);
        let rate = self.backend.rate(&track.video, rating);
        cx.spawn(async move |this, cx| {
            if let Err(err) = rate.await {
                eprintln!("moth: couldn't rate the track: {err:#}");
                this.update(cx, |this, cx| {
                    if let Some(queue) = this.music.as_mut()
                        && queue.current().is_some_and(|current| current.video == track.video)
                    {
                        queue.rating = before;
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    /// Play the tracks' videos, or their audio alone; switches the current track seamlessly.
    pub fn set_show_video(&mut self, show: bool, cx: &mut Context<Self>) {
        let Some(queue) = self.music.as_mut() else { return };
        if queue.show_video == show {
            return;
        }
        queue.show_video = show;
        let audio_only = !(show && queue.visible);
        self.set_audio_only(audio_only, cx);
    }

    /// The expanded player opened or closed: video (if chosen) only plays while it shows.
    pub fn set_music_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        let Some(queue) = self.music.as_mut() else { return };
        queue.visible = visible;
        if queue.show_video {
            self.set_audio_only(!visible, cx);
        }
    }

    fn fetch_more_if_needed(&mut self, cx: &mut Context<Self>) {
        let Some(queue) = self.music.as_ref() else { return };
        if queue.order.len() - queue.at <= PREFETCH_WITHIN && queue._more.is_none() {
            self.fetch_more(false, cx);
        }
    }

    /// Load the queue's next page; `then_play` moves on to its first track.
    fn fetch_more(&mut self, then_play: bool, cx: &mut Context<Self>) {
        let Some(queue) = self.music.as_mut() else { return };
        let Some(more) = queue.more.clone() else { return };
        let fetch = self.backend.music_queue_more(queue.source, more);
        queue._more = Some(cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |this, cx| {
                let Some(queue) = this.music.as_mut() else { return };
                queue._more = None;
                match result {
                    Ok(page) => queue.append(page),
                    Err(err) => {
                        eprintln!("moth: couldn't load more of the queue: {err:#}");
                        queue.more = None;
                    }
                }
                if then_play {
                    let place = queue.at + 1;
                    if place < queue.order.len() {
                        this.play_queued(place, cx);
                    } else {
                        this.status = Status::Ended;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }
}
