# Aquafin — Build Plan

Jellyfin TUI client. Rust. Linux only.

This document is split into a **Shared Context** block and **11 Steps**. Feed the agent **Shared Context + one Step at a time**. Verify the step's "Done when" before moving on.

---

## How to use this doc

1. Start a fresh agent session (Claude Code, Cursor, etc.) in an empty project directory.
2. Paste the **Shared Context** section.
3. Paste **Step 1**.
4. Let the agent work. Check the **Done when** criteria. Fix or iterate.
5. Commit. Start a new session (or `/clear`). Paste **Shared Context** + **Step 2**. Repeat.

Each step is self-contained when paired with Shared Context. Steps must run in order — later steps depend on earlier ones.

---

## Shared Context

> Paste this before every step.

**Project:** `aquafin` — Jellyfin TUI client. Rust. Linux only.

**Core libraries:**
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

**Target terminals:** kitty, ghostty, foot (primary). ASCII block fallback for everything else.

**XDG (strict):**
- Config: `$XDG_CONFIG_HOME/aquafin/config.toml`
- Themes: `$XDG_CONFIG_HOME/aquafin/themes/`
- Cache: `$XDG_CACHE_HOME/aquafin/` (images, thumbnails)
- Data: `$XDG_DATA_HOME/aquafin/` (token, history, resume positions)
- State: `$XDG_STATE_HOME/aquafin/` (logs, last session)

All config + theme files in TOML.

**Global quality bar:**
- Async I/O. UI thread never blocks on network.
- No panics in normal use. Errors propagate via `Result`.
- Panic hook restores terminal cleanly before exit.
- Structured logging at all levels.
- MIT or Apache-2.0 license.

**Theme schema (referenced by Step 10):**
- **palette:** named colors — `base`, `surface`, `overlay`, `text`, `subtext`, `accent`, `success`, `warn`, `error`, `border`, `selection`
- **components:** each UI element references palette names + text style flags (`bold`, `italic`, `underline`, `dim`, `reversed`)
- Themeable components: list item, selected list item, focused/unfocused border, status bar, header, search input, modal/overlay, scrollbar, progress bar, hint text, cheatsheet, now-playing

**Keymap (referenced by Step 3):**
- Default = yazi manager keymap. Reference: https://yazi-rs.github.io/docs/configuration/keymap/
- If docs unclear, read `~/.config/yazi/keymap.toml` on dev system for ground truth (fallback `/etc/yazi/keymap.toml`).
- Manager-style nav: `hjkl`, `gg`/`G`, `l`/Enter into item, `h`/Backspace back, `/` search, `q` quit, space select, etc.

---

## Step 1: Project Scaffold

**Goal:** Bootable Cargo project with module skeleton, dependencies, basic CLI entry.

**Tasks:**
- `cargo init aquafin --bin`. Edition 2021 or later.
- Add all dependencies from Shared Context to `Cargo.toml`.
- Module skeleton: `main.rs`, `cli.rs`, `config.rs`, `api/mod.rs`, `ui/mod.rs`, `audio.rs`, `video.rs`, `theme.rs`, `error.rs`, `paths.rs`.
- Implement `paths.rs`: helpers returning config/cache/data/state/themes/bin dirs, respecting `XDG_*` env vars with fallbacks.
- Implement `cli.rs` with clap: `--setup`, `--log-level`, `--version`, `--help`.
- `main.rs` initializes logging, parses CLI, prints "aquafin started", exits cleanly.
- `LICENSE` file (MIT or Apache-2.0).
- `.gitignore` for Rust + IDE files.
- Minimal `README.md` stub.

**Done when:**
- `cargo build` succeeds with no warnings.
- `cargo run -- --help` prints CLI usage.
- `cargo run -- --version` prints version.
- `cargo clippy` clean.

---

## Step 2: Auth + Jellyfin API Client

**Goal:** Authentication (password + Quick Connect) and a read-only API client.

**Tasks:**
- Module `api/` with submodules: `client.rs`, `auth.rs`, `items.rs`, `playback.rs`, `models.rs`.
- Auth flows:
  - Username/password: `POST /Users/AuthenticateByName` with proper `X-Emby-Authorization` header.
  - Quick Connect: `POST /QuickConnect/Initiate` → poll `/QuickConnect/Connect?Secret=...` → finalize.
- Persist token + user id + server URL to `$XDG_DATA_HOME/aquafin/credentials.toml` with file mode `0600`.
- `JellyfinClient` struct: base URL, token, user id, `reqwest::Client`.
- Endpoints:
  - `GET /UserViews` (libraries)
  - `GET /Users/{userId}/Items` (with `parentId`, `searchTerm`, `includeItemTypes`, paging)
  - `GET /Items/{itemId}` (detail)
  - `GET /Items/{itemId}/Images/Primary` (returns bytes)
  - `GET /UserItems/Resume` (Continue Watching)
  - `GET /Search/Hints`
- Playback reporting (used in Steps 6 & 7):
  - `POST /Sessions/Playing` (start)
  - `POST /Sessions/Playing/Progress` (heartbeat)
  - `POST /Sessions/Playing/Stopped` (stop)
- All async. Errors via `thiserror` in `api::Error`.

**Done when:**
- Unit tests pass against mocked HTTP responses.
- A throwaway integration test or manual run against a real Jellyfin server can: authenticate, list views, list items in a library, fetch one item's detail. (Skip if no test server handy — note for later.)

---

## Step 3: TUI Shell + Navigation + F1 Cheatsheet

**Goal:** Running TUI with multi-pane layout, yazi-default keys, F1 overlay. No real data yet.

**Tasks:**
- `ui/` module with: `app.rs` (state + loop), `layout.rs`, `panes/` (sidebar, list, detail), `cheatsheet.rs`, `keymap.rs`.
- App loop: ratatui + crossterm, alternate screen, raw mode, proper teardown on exit.
- Layout: 2–3 vertical panes (sidebar / main list / detail) + 1 horizontal row at the bottom for status. Resize-aware.
- Focus model: which pane has focus, per-pane selection cursor.
- Keymap from Shared Context (yazi defaults, hardcoded for now — config-driven in Step 8).
- `F1` opens a cheatsheet overlay listing all currently-active bindings, grouped by context.
- Mock data: hardcoded sidebar entries ("Movies", "TV", "Music") and dummy list items.

**Done when:**
- App launches and renders three panes + status bar.
- Arrow keys moves focus and selection correctly.
- `F1` opens cheatsheet; any key closes it.
- `q` quits cleanly, terminal restored.

---

## Step 4: First-Launch Wizard + `--setup` Flag

**Goal:** Interactive TUI wizard on first run; `aquafin --setup` to re-run later.

**Tasks:**
- On startup, before loading config: check for `$XDG_CONFIG_HOME/aquafin/config.toml`. If missing → enter wizard.
- Wizard screens, all in ratatui:
  1. Welcome / explanation.
  2. Server URL input (validated: must parse as URL, must reach `/System/Info/Public`).
  3. Auth method choice: password vs Quick Connect.
  4. Run the chosen auth flow from Step 2.
  5. Success screen → write fresh `config.toml` from defaults, persist credentials, drop into main UI.
  6. Failure screen → show error + log path + retry/quit.
- `--setup` flag: re-runs wizard regardless of existing config (overwrites on completion, with confirmation prompt).

**Done when:**
- Fresh user (no XDG dirs) launches app → wizard runs → completes → lands in main UI.
- `aquafin --setup` re-enters wizard.
- Cancelling wizard mid-flow exits cleanly without partial state.

---

## Step 5: Error & Logging System

**Goal:** Robust error handling with user-visible reporting.

**Tasks:**
- `tracing-subscriber` with rolling file appender → `$XDG_STATE_HOME/aquafin/aquafin.log`. Keep last N files (e.g. 5).
- Log level controlled by `--log-level` CLI flag and `log_level` config field.
- Panic hook (`std::panic::set_hook`):
  - Capture panic message + backtrace.
  - Write to log.
  - Restore terminal: leave alternate screen, disable raw mode.
  - Print to stderr: `aquafin crashed. Log: <full path>`.
  - Exit non-zero.
- User-facing error modal component:
  - Renders inside TUI as overlay.
  - Shows: error summary + log path + key hints (`Enter` dismiss, `y` copy path to clipboard if available).
- Any `Result::Err` surfacing to the UI layer goes through this modal, not silent failure.

**Done when:**
- Triggering a forced panic restores terminal and writes a usable log.
- Triggering a network error (point at a dead URL) shows error modal with log path.
- Log file exists at expected path, rotates as expected.

---

## Step 6: Video Playback via External MPV

**Goal:** Stream Jellyfin video via spawned `mpv`, control via IPC, report progress to server.

**Tasks:**
- `video.rs`: spawn mpv with `--input-ipc-server=/tmp/aquafin-mpv-<pid>.sock`, plus `--force-window=yes`, `--osc=yes`, and any other sensible flags.
- Stream URL: `/Videos/{itemId}/stream?static=true&api_key=<token>` (direct play) or HLS endpoint if needed.
- IPC client: connect to the unix socket, send JSON commands (`get_property time-pos`, `cycle pause`, etc.), parse responses.
- Progress reporter: spawn a tokio task that polls `time-pos` every ~10s, posts to `/Sessions/Playing/Progress`. On mpv exit, post `/Sessions/Playing/Stopped` with final position.
- TUI status bar shows `Playing: <title>` while mpv is alive. Pressing `q` in mpv (or closing the window) returns control to the TUI.
- Handle mpv-not-installed gracefully (error modal pointing user to install mpv).

**Done when:**
- Select a video in the TUI → mpv launches → plays.
- Jellyfin server shows session as active during playback.
- Closing mpv returns to TUI; resume position is saved server-side and reflected on next visit.

---

## Step 7: In-App Audio Playback

**Goal:** Play music inside the TUI over PipeWire, no mpv. Support mp3, m4a, AAC, FLAC, Opus.

**Tasks:**
- Add `rodio` + `symphonia` with features per Shared Context.
- `audio.rs`: `AudioEngine` struct managing a playback thread.
  - Queue (Vec of track URLs + metadata).
  - Commands: `play`, `pause`, `toggle`, `next`, `prev`, `seek`, `set_volume`, `enqueue`, `clear`, `shuffle`, `repeat`.
  - Channel-based command interface so the UI thread never blocks.
- Stream from `/Audio/{itemId}/universal?...` (or `/Audio/{itemId}/stream`), decode with symphonia, send PCM frames to rodio sink.
- Now-playing UI: shown in status row or dedicated pane. Title, artist, progress bar, time, volume.
- Default hotkeys (rebindable in Step 8): `space` play/pause, `>` next, `<` prev, `+`/`-` volume, `s` shuffle, `r` repeat.
- Report playback to server like video (Step 6's playback reporter, reused).

**Done when:**
- All five formats play without artifacts (test one file of each).
- Transport controls work. Volume changes audible.
- Server registers audio session.
- UI remains responsive while music plays.

---

## Step 8: Config + Rebindable Keys

**Goal:** All preferences driven by `config.toml`. All keys rebindable.

**Tasks:**
- `config.rs`: full schema in `serde`-derived structs.
  - `[server]` (URL, user — populated by wizard)
  - `[ui]` (theme name, image_protocol preference: `auto`/`kitty`/`sixel`/`ascii`)
  - `[keymap]` (any binding can be overridden by action name → key string)
  - `[audio]` (default volume, etc.)
  - `[log]` (level, max_files)
- Load order: built-in defaults → `config.toml` overrides → CLI flag overrides.
- Refactor Step 3's hardcoded keymap to read from config; built-in defaults match yazi.
- Missing fields fall back to defaults silently (no error).
- Invalid fields → warning logged + user-facing notice + fallback to default.
- Ship `config.example.toml` in repo with every key documented inline.

**Done when:**
- User edits `config.toml`, restarts → custom keys + theme apply.
- Deleting a section from config doesn't crash; defaults fill in.
- `config.example.toml` is readable and complete.

---

## Step 9: Inline Image Rendering

**Goal:** Posters and cover art rendered inline in the detail pane.

**Tasks:**
- Add `ratatui-image`.
- Protocol detection at startup: query terminal for kitty graphics → sixel → fall back to unicode-block ASCII.
- Respect `config.ui.image_protocol` override.
- Fetcher: download `/Items/{itemId}/Images/Primary` async, cache to `$XDG_CACHE_HOME/aquafin/images/<itemId>.<ext>`. Reuse cache on subsequent loads.
- Detail pane renders the image scaled to available cells. Aspect-ratio preserved.
- Loading state: show placeholder while fetching.

**Done when:**
- On kitty/ghostty/foot, detail pane shows proper poster images.
- On xterm or another non-graphical terminal, ASCII fallback renders without errors.
- Repeated views of the same item hit cache (verify by network watch or log line).

---

## Step 10: Theme System

**Goal:** Themeable colors + text styles. Built-in default + Catppuccin + user themes.

**Tasks:**
- `theme.rs`: schema per Shared Context (palette + components, both colors and style flags).
- Built-in themes (compiled into binary or extracted on first run):
  - `default.toml` — original, tasteful, dark-friendly. Not a clone of any existing scheme.
  - `catppuccin-mocha.toml`
  - `catppuccin-macchiato.toml`
  - `catppuccin-frappe.toml`
  - `catppuccin-latte.toml`
- User themes loaded from `$XDG_CONFIG_HOME/aquafin/themes/<name>.toml`.
- Theme selection: `config.ui.theme` field (string, matches theme name).
- Runtime theme switch: in-app command (e.g. `:theme <name>` or a key-driven picker).
- Import command: load a theme from arbitrary path, copy into themes dir.
- Ship `themes/example.toml` in repo documenting **every palette key + every overridable component + every supported style flag**.

**Done when:**
- All 5 built-in themes selectable and visually distinct.
- Editing `config.ui.theme` → restart → new theme applies.
- Runtime switch works without restart.
- Dropping a custom theme file in the themes dir makes it selectable.

---

## Step 11: Install Target

**Goal:** Easy install path for end users.

**Tasks:**
- Verify `cargo install --path .` works → binary in `~/.cargo/bin/aquafin`.
- `justfile` (or `Makefile`) with:
  - `just build` — release build.
  - `just install` — copy `target/release/aquafin` to `$XDG_BIN_HOME` (fallback `~/.local/bin`). Create dir if missing. Print final path.
  - `just uninstall` — remove the binary.
- README install section covering both paths (`cargo install` and `just install`).
- README also documents config, themes, keybindings (link to `config.example.toml` and `themes/example.toml`).
- Optionally: GitHub Actions workflow building release binaries on tag.

**Done when:**
- Fresh clone → `just install` → `aquafin` in `~/.local/bin` and on `$PATH` (assuming user's PATH includes it).
- `cargo install --path .` also works.
- README install instructions verified.

---

## Final checklist

After Step 11, run through this end-to-end:

- [ ] Fresh user, empty XDG dirs. Launch `aquafin`. Wizard runs. Auth succeeds.
- [ ] Browse libraries. Open detail pane. See poster image.
- [ ] Play a movie. mpv launches. Position reported to server. Resume works.
- [ ] Play music. All five formats. Transport controls. Volume.
- [ ] Open F1 cheatsheet. All bindings listed.
- [ ] Edit `config.toml` → custom keymap applies after restart.
- [ ] Switch theme at runtime.
- [ ] Drop a custom theme file → selectable.
- [ ] Trigger a network error → error modal shows log path.
- [ ] Force a panic → terminal restored, log written, path printed to stderr.
- [ ] `just install` → binary on `$PATH`.
