//! Remote images (thumbnails, avatars), fetched once and kept for the session.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Context, Image, ImageFormat};

use super::BackendHandle;

#[derive(Default)]
pub struct ImageCache {
    images: HashMap<String, Arc<Image>>,
    /// Requested and not failed; avoids fetching the same URL twice.
    requested: std::collections::HashSet<String>,
}

impl ImageCache {
    pub fn get(&self, url: &str) -> Option<Arc<Image>> {
        self.images.get(url).cloned()
    }

    /// Start fetching `url` unless it's already loaded or loading. Notifies when it arrives.
    pub fn load(&mut self, url: String, cx: &mut Context<Self>) {
        if !self.requested.insert(url.clone()) {
            return;
        }
        let fetch = cx.global::<BackendHandle>().0.fetch_bytes(url.clone());
        cx.spawn(async move |this, cx| {
            let image = match fetch.await {
                Ok(bytes) => match image_format(&bytes) {
                    Some(format) => Arc::new(Image::from_bytes(format, bytes)),
                    None => return eprintln!("moth: unrecognized image format: {url}"),
                },
                Err(err) => return eprintln!("moth: failed to load {url}: {err:#}"),
            };
            this.update(cx, |this, cx| {
                this.images.insert(url, image);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0xFF, 0xD8, ..] => Some(ImageFormat::Jpeg),
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::Webp),
        _ => None,
    }
}
