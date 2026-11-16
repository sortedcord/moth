//! Colors and shared sizes.

pub const BG_TOP: u32 = 0x1d2540;
pub const BG_BOTTOM: u32 = 0x131929;
pub const PANEL: u32 = 0x212a43;
pub const HOVER: u32 = 0x29334f;
pub const TRACK: u32 = 0x343e5c;
pub const TEXT: u32 = 0xe8ebf4;
pub const MUTED: u32 = 0x9aa3bb;
pub const FAINT: u32 = 0x6b7490;
pub const ACCENT: u32 = 0xe94b5a;
pub const ON_ACCENT: u32 = 0xffffff;
/// Behind the video and in the fullscreen control bar.
pub const VIDEO_BG: u32 = 0x000000;
pub const DEEP: u32 = 0x0b0f1a;
pub const FOCUS_RING: u32 = 0x4a5679;

pub const FONT: &str = "Outfit";
pub const FONT_FILES: [&[u8]; 4] = [
    include_bytes!("../../assets/fonts/Outfit-Regular.ttf"),
    include_bytes!("../../assets/fonts/Outfit-Medium.ttf"),
    include_bytes!("../../assets/fonts/Outfit-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Outfit-Bold.ttf"),
];

/// Outer padding of the window content.
pub const PAD: f32 = 16.0;
