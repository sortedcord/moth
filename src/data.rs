//! Hardcoded home-screen cards (mock). Watch pages load real data from YouTube.

use std::path::PathBuf;

pub struct Video {
    pub id: &'static str,
    pub title: &'static str,
    pub channel: &'static str,
    pub views: &'static str,
    pub age: &'static str,
    pub duration: &'static str,
}

impl Video {
    pub fn thumbnail(&self) -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/thumbs")).join(format!("{}.jpg", self.id))
    }
}

pub const VIDEOS: &[Video] = &[
    Video {
        id: "aqz-KE-bpKQ",
        title: "Big Buck Bunny 60fps 4K - Official Blender Foundation Short Film",
        channel: "Blender",
        views: "23M views",
        age: "11 years ago",
        duration: "10:35",
    },
    Video {
        id: "WhWc3b3KhnY",
        title: "Spring - Blender Open Movie",
        channel: "Blender Studio",
        views: "10M views",
        age: "7 years ago",
        duration: "7:44",
    },
    Video {
        id: "eRsGyueVLvQ",
        title: "Sintel - Open Movie by Blender Foundation",
        channel: "Blender",
        views: "5.8M views",
        age: "16 years ago",
        duration: "14:48",
    },
    Video {
        id: "mN0zPOpADL4",
        title: "Agent 327: Operation Barbershop",
        channel: "Blender Studio",
        views: "9.4M views",
        age: "9 years ago",
        duration: "3:52",
    },
    Video {
        id: "R6MlUcmOul8",
        title: "Tears of Steel - Blender VFX Open Movie",
        channel: "Blender",
        views: "2.6M views",
        age: "14 years ago",
        duration: "12:14",
    },
    Video {
        id: "UXqq0ZvbOnk",
        title: "CHARGE - Blender Open Movie",
        channel: "Blender Studio",
        views: "2.5M views",
        age: "3 years ago",
        duration: "4:23",
    },
    Video {
        id: "Y-rmzh0PI3c",
        title: "Cosmos Laundromat - First Cycle. Official Blender Foundation release.",
        channel: "Blender",
        views: "1.3M views",
        age: "11 years ago",
        duration: "12:11",
    },
];
