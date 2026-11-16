//! Choosing streams from a [`StreamCatalog`] according to the user's preferences.

use std::cmp::Reverse;

use super::media::{AudioCodec, AudioStream, AudioTrack, Selection, StreamCatalog, StreamId, VideoCodec, VideoStream};

/// What "automatic" quality means. Built from the settings.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamPreferences {
    /// Highest resolution class to pick automatically (e.g. 720 for 720p).
    pub max_height: u32,
    /// Most preferred first; codecs not listed come last.
    pub video_codecs: Vec<VideoCodec>,
    pub audio_codec: AudioCodec,
}

impl Default for StreamPreferences {
    fn default() -> Self {
        // VP9 and AV1 are hardware-decodable on current GPUs; H.264 is the last resort.
        Self { max_height: 720, video_codecs: VideoCodec::ALL.to_vec(), audio_codec: AudioCodec::Opus }
    }
}

impl StreamPreferences {
    fn codec_rank(&self, codec: VideoCodec) -> usize {
        self.video_codecs.iter().position(|preferred| *preferred == codec).unwrap_or(self.video_codecs.len())
    }
}

impl StreamCatalog {
    /// The automatic pick: best video and audio (in the default track).
    pub fn auto_selection(&self, preferences: &StreamPreferences) -> Option<Selection> {
        Some(Selection { video: self.auto_video(preferences, None)?.id, audio: self.auto_audio(preferences, None)?.id })
    }

    /// Best stream at or below the preferred height, preferring `keep_codec` (if given) and then
    /// the preferred codec order; if nothing is that small, the smallest stream.
    pub fn auto_video(&self, preferences: &StreamPreferences, keep_codec: Option<VideoCodec>) -> Option<&VideoStream> {
        let rank = |stream: &VideoStream| (Some(stream.codec) != keep_codec, preferences.codec_rank(stream.codec));
        let fitting = self.video.iter().filter(|stream| stream.resolution() <= preferences.max_height);
        let best = fitting.clone().map(|stream| (stream.resolution(), stream.fps)).max();
        match best {
            Some(best) => {
                fitting.filter(|stream| (stream.resolution(), stream.fps) == best).min_by_key(|stream| rank(stream))
            }
            None => self.video.iter().min_by_key(|stream| (stream.resolution(), rank(stream))),
        }
    }

    /// Video streams with the given resolution and frame rate, preferred codec first.
    pub fn video_variants(&self, resolution: u32, fps: u8, preferences: &StreamPreferences) -> Vec<&VideoStream> {
        let mut variants: Vec<_> =
            self.video.iter().filter(|stream| stream.resolution() == resolution && stream.fps == fps).collect();
        variants.sort_by_key(|stream| preferences.codec_rank(stream.codec));
        variants
    }

    /// Distinct (resolution, fps) pairs, highest first.
    pub fn qualities(&self) -> Vec<(u32, u8)> {
        let mut qualities: Vec<_> = self.video.iter().map(|stream| (stream.resolution(), stream.fps)).collect();
        qualities.sort_unstable_by(|a, b| b.cmp(a));
        qualities.dedup();
        qualities
    }

    /// Audio tracks, the default one first.
    pub fn tracks(&self) -> Vec<AudioTrack> {
        let mut tracks: Vec<AudioTrack> = Vec::new();
        for track in self.audio.iter().filter_map(|stream| stream.track.clone()) {
            if !tracks.iter().any(|known| known.id == track.id) {
                tracks.push(track);
            }
        }
        tracks.sort_by_key(|track| !track.is_default);
        tracks
    }

    /// Audio streams in a track (the default track if `track_id` is `None`), best first.
    pub fn audio_in_track(&self, track_id: Option<&str>, preferences: &StreamPreferences) -> Vec<&AudioStream> {
        let default_id = self.tracks().into_iter().find(|track| track.is_default).map(|track| track.id);
        let wanted = track_id.map(str::to_string).or(default_id);
        let mut streams: Vec<_> = self
            .audio
            .iter()
            .filter(|stream| wanted.is_none() || stream.track.as_ref().map(|track| &track.id) == wanted.as_ref())
            .collect();
        streams.sort_by_key(|stream| (stream.codec != preferences.audio_codec, Reverse(stream.bitrate)));
        streams
    }

    /// Best audio in a track (the default track if `None`).
    pub fn auto_audio(&self, preferences: &StreamPreferences, track_id: Option<&str>) -> Option<&AudioStream> {
        self.audio_in_track(track_id, preferences).into_iter().next()
    }

    pub fn video_stream(&self, id: StreamId) -> Option<&VideoStream> {
        self.video.iter().find(|stream| stream.id == id)
    }

    pub fn audio_stream(&self, id: StreamId) -> Option<&AudioStream> {
        self.audio.iter().find(|stream| stream.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: u64, quality: &str, fps: u8, codec: VideoCodec) -> VideoStream {
        let height = quality.trim_end_matches('p').parse().unwrap();
        VideoStream {
            id: StreamId(id),
            location: Default::default(),
            quality: quality.into(),
            width: height * 16 / 9,
            height,
            fps,
            codec,
            bitrate: None,
            size: None,
        }
    }

    fn audio(id: u64, codec: AudioCodec, bitrate: u32) -> AudioStream {
        AudioStream {
            id: StreamId(id),
            location: Default::default(),
            codec,
            bitrate: Some(bitrate),
            size: None,
            track: None,
        }
    }

    fn catalog() -> StreamCatalog {
        StreamCatalog {
            video: vec![
                video(1, "1080p", 60, VideoCodec::Vp9),
                video(2, "720p", 60, VideoCodec::Av1),
                video(3, "720p", 60, VideoCodec::Vp9),
                video(4, "720p", 30, VideoCodec::H264),
                video(5, "360p", 30, VideoCodec::H264),
            ],
            audio: vec![audio(10, AudioCodec::Aac, 128_000), audio(11, AudioCodec::Opus, 130_000)],
            resolver: "test",
            resolver_detail: String::new(),
            has_alternates: false,
        }
    }

    #[test]
    fn auto_pick_follows_preferences() {
        let catalog = catalog();
        let mut preferences = StreamPreferences::default();
        assert_eq!(catalog.auto_selection(&preferences), Some(Selection { video: StreamId(3), audio: StreamId(11) }));

        preferences.video_codecs = vec![VideoCodec::Av1];
        preferences.audio_codec = AudioCodec::Aac;
        assert_eq!(catalog.auto_selection(&preferences), Some(Selection { video: StreamId(2), audio: StreamId(10) }));

        // Keeping the current codec wins over the preference order.
        assert_eq!(catalog.auto_video(&preferences, Some(VideoCodec::Vp9)).map(|v| v.id), Some(StreamId(3)));

        // Nothing small enough: the smallest stream.
        preferences.max_height = 240;
        assert_eq!(catalog.auto_video(&preferences, None).map(|v| v.id), Some(StreamId(5)));
    }

    #[test]
    fn qualities_are_distinct_and_descending() {
        assert_eq!(catalog().qualities(), vec![(1080, 60), (720, 60), (720, 30), (360, 30)]);
    }
}
