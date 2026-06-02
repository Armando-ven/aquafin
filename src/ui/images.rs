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
use ratatui::style::Color;
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
    /// Dominant vibrant color extracted from each cover, used to tint the UI
    /// to match the now-playing track. Populated alongside `cache`.
    colors: HashMap<String, Color>,
    /// Item ids whose color request is in flight, so we keep extracting even
    /// when the terminal can't render the image itself (kept separate from
    /// `cache` because `cache` only exists when graphics are available).
    color_pending: std::collections::HashSet<String>,
    tx: Sender<(String, Option<DynamicImage>, Option<Color>)>,
    rx: Receiver<(String, Option<DynamicImage>, Option<Color>)>,
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
            colors: HashMap::new(),
            color_pending: std::collections::HashSet::new(),
            tx,
            rx,
        }
    }

    pub fn is_available(&self) -> bool {
        self.picker.is_some()
    }

    /// Ensure the primary image for `item_id` is being fetched (no-op if already
    /// requested, ready, or failed). Cheap to call every frame. Even with no
    /// terminal graphics support the fetch still runs so the dominant-color
    /// extraction (used to tint the UI by the current cover) can populate.
    pub fn request(&mut self, item_id: &str) {
        if item_id.is_empty() {
            return;
        }
        let needs_render = self.picker.is_some() && !self.cache.contains_key(item_id);
        let needs_color =
            !self.colors.contains_key(item_id) && !self.color_pending.contains(item_id);
        if !needs_render && !needs_color {
            return;
        }
        if needs_render {
            self.cache.insert(item_id.to_string(), Entry::Loading);
        }
        if needs_color {
            self.color_pending.insert(item_id.to_string());
        }
        let client = self.client.clone();
        let tx = self.tx.clone();
        let id = item_id.to_string();
        self.rt.spawn(async move {
            let image = load_image(&client, &id).await;
            let color = image.as_ref().and_then(dominant_color);
            let _ = tx.send((id, image, color));
        });
    }

    /// Promote any finished downloads into renderable protocols (and stash the
    /// extracted dominant color for the now-playing tint).
    pub fn tick(&mut self) {
        while let Ok((id, image, color)) = self.rx.try_recv() {
            self.color_pending.remove(&id);
            if let Some(color) = color {
                self.colors.insert(id.clone(), color);
            }
            if self.picker.is_some() {
                let entry = match (image, &self.picker) {
                    (Some(image), Some(picker)) => {
                        Entry::Ready(Box::new(picker.new_resize_protocol(image)))
                    }
                    _ => Entry::Failed,
                };
                self.cache.insert(id, entry);
            }
        }
    }

    /// Dominant vibrant color extracted from `item_id`'s cover, if it has
    /// landed yet.
    pub fn color_for(&self, item_id: &str) -> Option<Color> {
        self.colors.get(item_id).copied()
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

/// Extract a vibrant dominant color from `image`, suitable for use as a UI
/// accent. The image is downsampled, near-grayscale / near-black / near-white
/// pixels are dropped, and the remainder is binned by hue (12 bins). The
/// heaviest bin's saturation-weighted average is then boosted so a muted
/// cover still yields a usable accent.
fn dominant_color(image: &DynamicImage) -> Option<Color> {
    let small = image.thumbnail(80, 80).to_rgb8();
    if small.width() == 0 || small.height() == 0 {
        return None;
    }
    const BINS: usize = 12;
    let mut weights = [0.0f32; BINS];
    let mut sum_r = [0.0f32; BINS];
    let mut sum_g = [0.0f32; BINS];
    let mut sum_b = [0.0f32; BINS];
    for px in small.pixels() {
        let [r, g, b] = px.0;
        let (h, s, v) = rgb_to_hsv(r, g, b);
        // Skip washed-out and pitch-dark pixels so the bin winner is genuinely
        // a chromatic dominant rather than the background brightness average.
        if s < 0.30 || v < 0.20 || v > 0.95 {
            continue;
        }
        let weight = s * v;
        let bin = ((h / (360.0 / BINS as f32)) as usize) % BINS;
        weights[bin] += weight;
        sum_r[bin] += r as f32 * weight;
        sum_g[bin] += g as f32 * weight;
        sum_b[bin] += b as f32 * weight;
    }
    let (best, &w) = weights
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))?;
    if w <= 0.0 {
        return None;
    }
    let r = (sum_r[best] / w).round().clamp(0.0, 255.0) as u8;
    let g = (sum_g[best] / w).round().clamp(0.0, 255.0) as u8;
    let b = (sum_b[best] / w).round().clamp(0.0, 255.0) as u8;
    // Boost low-saturation / low-value picks so the resulting accent reads as
    // an accent rather than a muddy mid-tone.
    let (h, s, v) = rgb_to_hsv(r, g, b);
    let (r, g, b) = hsv_to_rgb(h, s.max(0.65), v.max(0.70));
    Some(Color::Rgb(r, g, b))
}

fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let rf = r as f32 / 255.0;
    let gf = g as f32 / 255.0;
    let bf = b as f32 / 255.0;
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;
    let h = if delta == 0.0 {
        0.0
    } else if max == rf {
        60.0 * (((gf - bf) / delta).rem_euclid(6.0))
    } else if max == gf {
        60.0 * ((bf - rf) / delta + 2.0)
    } else {
        60.0 * ((rf - gf) / delta + 4.0)
    };
    let s = if max == 0.0 { 0.0 } else { delta / max };
    (h, s, max)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let h6 = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (h6.rem_euclid(2.0) - 1.0).abs());
    let (rf, gf, bf) = match h6 as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (
        ((rf + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((gf + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((bf + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
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
