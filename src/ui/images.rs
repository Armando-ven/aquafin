//! Inline cover/poster rendering via `ratatui-image`.
//!
//! A [`Picker`] is created once at startup (detecting the terminal's graphics
//! protocol — kitty / sixel / iTerm2 — or falling back to unicode half-blocks).
//! Primary images are fetched and disk-cached off the UI thread; once decoded
//! they become a `StatefulProtocol` the detail pane and now-playing bar draw.
//!
//! Everything degrades gracefully: with no graphics support (or in a non-tty
//! environment) the picker is `None` and image areas simply stay empty.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

use image::DynamicImage;
use ratatui::layout::Rect;
use ratatui::Frame;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::StatefulImage;
use tokio::runtime::Handle;

use crate::api::JellyfinClient;
use crate::config::ImageProtocol;

/// Width we request from the server; covers/posters are small, and the protocol
/// resizes to the cell area on render anyway.
const REQUEST_MAX_WIDTH: u32 = 500;

enum Entry {
    Loading,
    Ready(Box<StatefulProtocol>),
    Failed,
}

pub struct Images {
    rt: Handle,
    client: JellyfinClient,
    /// `None` when the terminal has no graphics support — image areas stay blank.
    picker: Option<Picker>,
    cache: HashMap<String, Entry>,
    tx: Sender<(String, Option<DynamicImage>)>,
    rx: Receiver<(String, Option<DynamicImage>)>,
}

impl Images {
    pub fn new(rt: Handle, client: JellyfinClient, preference: ImageProtocol) -> Self {
        let picker = build_picker(preference);
        let (tx, rx) = mpsc::channel();
        Self {
            rt,
            client,
            picker,
            cache: HashMap::new(),
            tx,
            rx,
        }
    }

    pub fn is_available(&self) -> bool {
        self.picker.is_some()
    }

    /// Ensure the primary image for `item_id` is being fetched (no-op if already
    /// requested, ready, or failed). Cheap to call every frame.
    pub fn request(&mut self, item_id: &str) {
        if item_id.is_empty() || self.picker.is_none() || self.cache.contains_key(item_id) {
            return;
        }
        self.cache.insert(item_id.to_string(), Entry::Loading);
        let client = self.client.clone();
        let tx = self.tx.clone();
        let id = item_id.to_string();
        self.rt.spawn(async move {
            let image = load_image(&client, &id).await;
            let _ = tx.send((id, image));
        });
    }

    /// Promote any finished downloads into renderable protocols.
    pub fn tick(&mut self) {
        while let Ok((id, image)) = self.rx.try_recv() {
            let entry = match (image, &self.picker) {
                (Some(image), Some(picker)) => {
                    Entry::Ready(Box::new(picker.new_resize_protocol(image)))
                }
                _ => Entry::Failed,
            };
            self.cache.insert(id, entry);
        }
    }

    /// Draw the cover for `item_id` into `area`. Returns `true` if an image was
    /// drawn, so callers can lay out text in the remaining space.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, item_id: &str) -> bool {
        if area.width == 0 || area.height == 0 {
            return false;
        }
        match self.cache.get_mut(item_id) {
            Some(Entry::Ready(protocol)) => {
                frame.render_stateful_widget(StatefulImage::default(), area, protocol.as_mut());
                true
            }
            _ => false,
        }
    }
}

/// Create a picker honoring the user's protocol preference. `Auto` uses the
/// terminal-detected protocol; the others force a specific one.
fn build_picker(preference: ImageProtocol) -> Option<Picker> {
    let mut picker = match Picker::from_query_stdio() {
        Ok(picker) => picker,
        Err(e) => {
            tracing::info!(error = %e, "no terminal graphics support; covers disabled");
            return None;
        }
    };
    match preference {
        ImageProtocol::Auto => {}
        ImageProtocol::Kitty => picker.set_protocol_type(ProtocolType::Kitty),
        ImageProtocol::Sixel => picker.set_protocol_type(ProtocolType::Sixel),
        ImageProtocol::Ascii => picker.set_protocol_type(ProtocolType::Halfblocks),
    }
    Some(picker)
}

/// Load the primary image for `item_id`: from the on-disk cache if present,
/// otherwise downloaded from the server and cached. Decoding runs off the async
/// worker via `spawn_blocking`.
async fn load_image(client: &JellyfinClient, item_id: &str) -> Option<DynamicImage> {
    let path = cache_path(item_id);

    let bytes = match &path {
        Some(path) if tokio::fs::try_exists(path).await.unwrap_or(false) => {
            tokio::fs::read(path).await.ok()?
        }
        _ => {
            let response = client.primary_image(item_id, Some(REQUEST_MAX_WIDTH)).await.ok()?;
            if let Some(path) = &path {
                if let Some(parent) = path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                let _ = tokio::fs::write(path, &response.bytes).await;
            }
            response.bytes
        }
    };

    tokio::task::spawn_blocking(move || image::load_from_memory(&bytes).ok())
        .await
        .ok()
        .flatten()
}

/// `$XDG_CACHE_HOME/aquafin/images/<itemId>` — the raw downloaded image bytes.
fn cache_path(item_id: &str) -> Option<PathBuf> {
    let safe: String = item_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if safe.is_empty() {
        return None;
    }
    Some(crate::paths::cache_dir().ok()?.join("images").join(safe))
}
