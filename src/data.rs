//! Hardcoded mock data for the proof of concept.

use std::path::PathBuf;

pub struct Video {
    pub id: &'static str,
    pub title: &'static str,
    pub channel: &'static str,
    pub subscribers: &'static str,
    pub views: &'static str,
    pub views_full: &'static str,
    pub age: &'static str,
    pub date: &'static str,
    pub duration: &'static str,
    pub likes: &'static str,
    pub description: &'static str,
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
        subscribers: "2.1M subscribers",
        views: "23M views",
        views_full: "23,507,910 views",
        age: "11 years ago",
        date: "Nov 10, 2014",
        duration: "10:35",
        likes: "312K",
        description: "Big Buck Bunny tells the story of a giant rabbit with a heart bigger than himself. \
When one sunny day three rodents rudely harass him, something snaps and the rabbit ain't no bunny anymore. \
In the typical cartoon tradition he prepares the nasty rodents a comical revenge.\n\n\
This is the remastered 4K 60fps edition of the Blender Foundation's second open movie, \
made entirely with free and open source software.",
    },
    Video {
        id: "WhWc3b3KhnY",
        title: "Spring - Blender Open Movie",
        channel: "Blender Studio",
        subscribers: "534K subscribers",
        views: "10M views",
        views_full: "10,967,424 views",
        age: "7 years ago",
        date: "Apr 4, 2019",
        duration: "7:44",
        likes: "401K",
        description: "Spring is the story of a shepherd girl and her dog, who face ancient spirits \
in order to continue the cycle of life. This poetic and visually stunning short film was written and \
directed by Andy Goralczyk, inspired by his childhood in the mountains of Germany.\n\n\
Produced by Blender Studio and funded by the Blender Cloud subscribers.",
    },
    Video {
        id: "eRsGyueVLvQ",
        title: "Sintel - Open Movie by Blender Foundation",
        channel: "Blender",
        subscribers: "2.1M subscribers",
        views: "5.8M views",
        views_full: "5,855,283 views",
        age: "16 years ago",
        date: "Sep 30, 2010",
        duration: "14:48",
        likes: "98K",
        description: "Sintel is an independently produced short film, initiated by the Blender Foundation \
as a means to further improve and validate the free and open source 3D creation suite Blender.\n\n\
A lonely young woman, Sintel, helps and befriends a dragon, whom she calls Scales. But when he is \
kidnapped by an adult dragon, Sintel decides to embark on a dangerous quest to find her lost friend.",
    },
    Video {
        id: "mN0zPOpADL4",
        title: "Agent 327: Operation Barbershop",
        channel: "Blender Studio",
        subscribers: "534K subscribers",
        views: "9.4M views",
        views_full: "9,477,278 views",
        age: "9 years ago",
        date: "May 15, 2017",
        duration: "3:52",
        likes: "220K",
        description: "Agent 327 is investigating a clue that leads him to a shady barbershop in Amsterdam. \
Little does he know that he is being tailed by mercenary Boris Kloris.\n\n\
A 3-minute teaser for a feature film based on the classic Dutch comic series by Martin Lodewijk.",
    },
    Video {
        id: "R6MlUcmOul8",
        title: "Tears of Steel - Blender VFX Open Movie",
        channel: "Blender",
        subscribers: "2.1M subscribers",
        views: "2.6M views",
        views_full: "2,687,319 views",
        age: "14 years ago",
        date: "Sep 26, 2012",
        duration: "12:14",
        likes: "41K",
        description: "Tears of Steel was realized with crowd-funding by users of the open source 3D creation \
tool Blender. The target was to improve and test a complete open and free pipeline for visual effects \
in film — and to make a compelling sci-fi film in Amsterdam, the Netherlands.",
    },
    Video {
        id: "UXqq0ZvbOnk",
        title: "CHARGE - Blender Open Movie",
        channel: "Blender Studio",
        subscribers: "534K subscribers",
        views: "2.5M views",
        views_full: "2,563,703 views",
        age: "3 years ago",
        date: "Dec 15, 2022",
        duration: "4:23",
        likes: "118K",
        description: "In a dystopian future where energy is scarce, an old destitute man breaks into a \
heavily guarded energy facility — and finds himself in a fight against a deadly robot.\n\n\
CHARGE was made by Blender Studio as a showcase for the Eevee real-time render engine.",
    },
    Video {
        id: "Y-rmzh0PI3c",
        title: "Cosmos Laundromat - First Cycle. Official Blender Foundation release.",
        channel: "Blender",
        subscribers: "2.1M subscribers",
        views: "1.3M views",
        views_full: "1,304,046 views",
        age: "11 years ago",
        date: "Aug 10, 2015",
        duration: "12:11",
        likes: "34K",
        description: "On a desolate island, a suicidal sheep called Franck meets his fate in a quirky \
salesman, who offers him the gift of a lifetime. Little does he know that he can only handle so much \
lifetime...\n\nCosmos Laundromat is the pilot of the Blender Institute's first open feature film project.",
    },
];
