# Project: aquafin — Jellyfin TUI client. Rust. Linux only.

## Core libraries
- TUI: `ratatui` + `crossterm`
- HTTP: `reqwest` (async) + `tokio`
- Errors: `anyhow` (app) + `thiserror` (library boundaries)
- Logging: `tracing` + `tracing-subscriber` + rolling file appender
- Config: `serde` + `toml`
- CLI: `clap`
- XDG paths: `directories` or `xdg` crate
- Video: external `mpv` (spawned subprocess, controlled via IPC socket)
- Audio: `rodio` (output) + `symphonia` (decoding). Enable Cargo features `mp3`, `aac`, `isomp4`, `flac`, `opus`, `ogg` — OR `--features symphonia/all-codecs,symphonia/all-formats` for full coverage.
- Images: `ratatui-image` (kitty graphics protocol + sixel + ASCII fallback)

## Target terminals
kitty, ghostty, foot (primary). ASCII block fallback for everything else.

## XDG (strict)
- Config: `$XDG_CONFIG_HOME/aquafin/config.toml`
- Themes: `$XDG_CONFIG_HOME/aquafin/themes/`
- Cache: `$XDG_CACHE_HOME/aquafin/` (images, thumbnails)
- Data: `$XDG_DATA_HOME/aquafin/` (token, history, resume positions)
- State: `$XDG_STATE_HOME/aquafin/` (logs, last session)

## Files
All config + theme files in TOML.

## Global quality bar
- Async I/O. UI thread never blocks on network.
- No panics in normal use. Errors propagate via `Result`.
- Panic hook restores terminal cleanly before exit.
- Structured logging at all levels.
- MIT or Apache-2.0 license.

## Theme schema (referenced by Step 10)
- **palette:** named colors — `base`, `surface`, `overlay`, `text`, `subtext`, `accent`, `success`, `warn`, `error`, `border`, `selection`
- **components:** each UI element references palette names + text style flags (`bold`, `italic`, `underline`, `dim`, `reversed`)
- Themeable components: list item, selected list item, focused/unfocused border, status bar, header, search input, modal/overlay, scrollbar, progress bar, hint text, cheatsheet, now-playing

## Keymap (referenced by Step 3)
- Default = yazi manager keymap. Reference: https://yazi-rs.github.io/docs/configuration/keymap/
- If docs unclear, read `~/.config/yazi/keymap.toml` on dev system for ground truth (fallback `/etc/yazi/keymap.toml`).
- Manager-style nav: `hjkl`, `gg`/`G`, `l`/Enter into item, `h`/Backspace back, `/` search, `q` quit, space select, etc.