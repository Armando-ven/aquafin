//! On-demand folder browsing: fetches a folder's children (seasons, episodes,
//! album tracks, …) when the user drills in.
//!
//! Like playback, the network fetch runs on the async runtime; results come back
//! to the UI thread over a channel and fill the matching loading level. The UI
//! thread never blocks.

use std::sync::mpsc::{self, Receiver, Sender};

use tokio::runtime::Handle;

use crate::api::models::ItemsQuery;
use crate::api::JellyfinClient;

use super::app::{App, Item};

/// Result of a folder children fetch, keyed by the folder's id so it fills the
/// right level even if several drills are in flight.
enum BrowseResult {
    Ready { id: String, items: Vec<Item> },
    Failed { id: String, message: String },
}

pub struct Browser {
    rt: Handle,
    client: JellyfinClient,
    tx: Sender<BrowseResult>,
    rx: Receiver<BrowseResult>,
}

impl Browser {
    pub fn new(rt: Handle, client: JellyfinClient) -> Self {
        let (tx, rx) = mpsc::channel();
        Self { rt, client, tx, rx }
    }

    /// Begin loading the children of folder `id`. The loading level was already
    /// pushed by the UI; [`Browser::tick`] fills it when the fetch returns.
    pub fn open(&mut self, id: String) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let query = ItemsQuery {
                parent_id: Some(id.clone()),
                fields: vec!["Overview".to_string()],
                // Index order keeps episodes and album tracks in sequence;
                // SortName is the sensible fallback for seasons/albums.
                sort_by: vec![
                    "ParentIndexNumber".to_string(),
                    "IndexNumber".to_string(),
                    "SortName".to_string(),
                ],
                limit: Some(500),
                ..Default::default()
            };
            let result = match client.items(&query).await {
                Ok(items) => BrowseResult::Ready {
                    id,
                    items: items.items.into_iter().map(super::item_from_dto).collect(),
                },
                Err(e) => BrowseResult::Failed {
                    id,
                    message: format!("Couldn't open folder: {e}"),
                },
            };
            let _ = tx.send(result);
        });
    }

    /// Deliver any completed fetches into the app's drill stack.
    pub fn tick(&mut self, app: &mut App) {
        while let Ok(result) = self.rx.try_recv() {
            match result {
                BrowseResult::Ready { id, items } => app.fill_level(&id, items),
                BrowseResult::Failed { id, message } => {
                    app.drop_loading_level(&id);
                    app.show_error(message);
                }
            }
        }
    }
}
