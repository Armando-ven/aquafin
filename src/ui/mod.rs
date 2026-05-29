//! Terminal user interface: the browser app loop, layout, panes, the F1
//! cheatsheet, the error modal, and the first-launch setup wizard.

pub mod app;
pub mod browse;
pub mod cheatsheet;
pub mod details;
pub mod error_modal;
pub mod images;
pub mod keymap;
pub mod layout;
pub mod now_playing;
pub mod panes;
pub mod playback;
pub mod theme_picker;
pub mod wizard;

use anyhow::Result;

use crate::api;

/// Entry point: set up the terminal, run the setup wizard when needed, load the
/// user's libraries, then run the main browser UI. The terminal is always
/// restored on the way out.
pub fn run(setup: bool) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let mut terminal = app::init_terminal()?;
    // ALSA / cpal and friends write to stderr from their C internals; inside the
    // alternate screen those writes corrupt the TUI. Send stderr to a log file
    // while the UI is running, then put it back.
    crate::error::silence_stderr();
    let result = orchestrate(&mut terminal, &runtime, setup);
    app::restore_terminal(&mut terminal)?;
    crate::error::restore_stderr();
    result
}

fn orchestrate(terminal: &mut app::Tui, runtime: &tokio::runtime::Runtime, setup: bool) -> Result<()> {
    let config_exists = crate::config::Config::exists();
    if needs_wizard(setup, config_exists) {
        let overwriting = setup && config_exists;
        if wizard::run(terminal, runtime, overwriting)? == wizard::Outcome::Cancelled {
            return Ok(());
        }
    }

    // Load config (best-effort) for the keymap and UI preferences.
    let config = crate::config::Config::load()
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "config.toml could not be parsed; using defaults");
            None
        })
        .unwrap_or_default();
    let (keys, warnings) = keymap::Keymap::from_config(&config.keymap);
    for warning in &warnings {
        tracing::warn!("{warning}");
    }

    // A client (and therefore playback) needs saved credentials; without them we
    // still run the UI so the user sees an actionable error.
    let mut startup_error: Option<String> = None;
    let client = match build_client() {
        Ok(client) => Some(client),
        Err(e) => {
            tracing::error!(error = %e, "no usable credentials");
            startup_error =
                Some("Not signed in. Run `aquafin --setup` to connect to your server.".to_string());
            None
        }
    };

    let libraries = match &client {
        Some(client) => load_libraries(runtime, client).unwrap_or_else(|e| {
            tracing::error!(error = %e, "failed to load library data");
            startup_error.get_or_insert(format!("Couldn't load your libraries:\n{e}"));
            Vec::new()
        }),
        None => Vec::new(),
    };

    // Apply the configured theme (fall back to default if it can't load).
    let theme = match crate::theme::load(&config.ui.theme) {
        Ok(theme) => theme,
        Err(e) => {
            tracing::warn!(theme = %config.ui.theme, error = %e, "couldn't load theme; using default");
            crate::theme::Theme::default()
        }
    };

    let mut app = app::App::with_libraries(libraries)
        .with_keymap(keys)
        .with_theme(theme)
        .with_available_themes(crate::theme::available_names())
        .with_audio_prefs(config.audio.repeat_mode.into(), config.audio.shuffle)
        .with_section_memory(config.ui.section_memory.clone())
        .with_last_library(config.ui.last_library_id.clone())
        .with_search_history(config.ui.search_history.clone());
    if let Some(error) = startup_error {
        app.show_error(error);
    } else if !warnings.is_empty() {
        app.show_error(format!(
            "Config issues (defaults used where needed):\n{}",
            warnings.join("\n")
        ));
    }

    match client {
        Some(client) => {
            let audio = crate::audio::AudioEngine::new(config.audio.volume);
            let mut browser = browse::Browser::new(runtime.handle().clone(), client.clone());
            let mut images =
                images::Images::new(runtime.handle().clone(), client.clone(), config.ui.image_protocol);
            let mut detail_fetcher = details::Details::new(runtime.handle().clone(), client.clone());
            let mut player = playback::Playback::new(
                runtime.handle().clone(),
                client,
                audio,
                config.audio.seek_seconds,
            );
            let result = app::run_browser(
                terminal,
                &mut app,
                Some(&mut player),
                Some(&mut browser),
                Some(&mut images),
                Some(&mut detail_fetcher),
            );
            // Tell the server we've stopped before the runtime goes away.
            player.shutdown(runtime);
            result
        }
        None => app::run_browser(terminal, &mut app, None, None, None, None),
    }
}

/// Build a Jellyfin client from saved credentials, erroring if none are stored.
fn build_client() -> Result<api::JellyfinClient> {
    let creds = api::auth::load_credentials()?
        .ok_or_else(|| anyhow::anyhow!("no saved credentials — run `aquafin --setup`"))?;
    Ok(api::JellyfinClient::from_credentials(&creds)?)
}

/// Load the user's libraries + items. Blocks (one-time startup load); the main
/// loop stays input-driven afterward.
fn load_libraries(
    runtime: &tokio::runtime::Runtime,
    client: &api::JellyfinClient,
) -> Result<Vec<app::Library>> {
    let client = client.clone();
    runtime.block_on(async move {
        let views = client.user_views().await?;
        let mut libraries = Vec::with_capacity(views.len());
        for view in views {
            let result = client
                .items(&api::models::ItemsQuery {
                    parent_id: Some(view.id.clone()),
                    limit: Some(200),
                    fields: vec!["Overview".to_string()],
                    ..Default::default()
                })
                .await?;
            libraries.push(app::Library {
                id: view.id,
                name: view.name.unwrap_or_else(|| "(library)".to_string()),
                collection_type: view.collection_type,
                items: result.items.into_iter().map(item_from_dto).collect(),
            });
        }
        Ok::<_, anyhow::Error>(libraries)
    })
}



/// Map a Jellyfin item DTO to the UI's [`app::Item`]. Shared by the startup
/// library load and the on-demand folder browser.
pub(crate) fn item_from_dto(dto: api::models::BaseItemDto) -> app::Item {
    let primary_image_tag = dto
        .image_tags
        .as_ref()
        .and_then(|tags| tags.get("Primary").cloned());
    let is_favorite = dto
        .user_data
        .as_ref()
        .and_then(|u| u.is_favorite)
        .unwrap_or(false);
    app::Item {
        is_folder: dto.is_folder.unwrap_or(false),
        id: dto.id,
        name: dto.name.unwrap_or_else(|| "(untitled)".to_string()),
        overview: dto.overview,
        production_year: dto.production_year,
        run_time_ticks: dto.run_time_ticks,
        kind: dto.type_,
        primary_image_tag,
        is_favorite,
    }
}

/// The wizard runs on an explicit `--setup`, or when no config exists yet.
fn needs_wizard(setup: bool, config_exists: bool) -> bool {
    setup || !config_exists
}

#[cfg(test)]
mod tests {
    use super::needs_wizard;

    #[test]
    fn wizard_runs_on_setup_or_missing_config() {
        assert!(needs_wizard(false, false)); // fresh user
        assert!(needs_wizard(true, true)); // --setup over existing config
        assert!(needs_wizard(true, false)); // --setup, no config
        assert!(!needs_wizard(false, true)); // configured, normal launch
    }
}
