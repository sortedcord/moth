//! The home page's sample videos (Blender Studio's open movies) until moth has a real feed.

use std::time::Duration;

use super::media::{VideoRef, VideoSummary};

/// (id, title, channel, views, age, duration in seconds)
const SAMPLES: [(&str, &str, &str, u64, &str, u64); 7] = [
    (
        "aqz-KE-bpKQ",
        "Big Buck Bunny 60fps 4K - Official Blender Foundation Short Film",
        "Blender",
        23_000_000,
        "11 years ago",
        635,
    ),
    ("WhWc3b3KhnY", "Spring - Blender Open Movie", "Blender Studio", 10_000_000, "7 years ago", 464),
    ("eRsGyueVLvQ", "Sintel - Open Movie by Blender Foundation", "Blender", 5_800_000, "16 years ago", 888),
    ("mN0zPOpADL4", "Agent 327: Operation Barbershop", "Blender Studio", 9_400_000, "9 years ago", 232),
    ("R6MlUcmOul8", "Tears of Steel - Blender VFX Open Movie", "Blender", 2_600_000, "14 years ago", 734),
    ("UXqq0ZvbOnk", "CHARGE - Blender Open Movie", "Blender Studio", 2_500_000, "3 years ago", 263),
    (
        "Y-rmzh0PI3c",
        "Cosmos Laundromat - First Cycle. Official Blender Foundation release.",
        "Blender",
        1_300_000,
        "11 years ago",
        731,
    ),
];

pub fn home_feed() -> Vec<VideoSummary> {
    SAMPLES
        .iter()
        .map(|&(id, title, channel, views, age, seconds)| VideoSummary {
            video: VideoRef { source: super::youtube::ID, id: id.to_string() },
            title: title.to_string(),
            channel: Some(channel.to_string()),
            views: Some(views),
            age: Some(age.to_string()),
            duration: Some(Duration::from_secs(seconds)),
            thumbnail_url: None,
            is_short: false,
        })
        .collect()
}
