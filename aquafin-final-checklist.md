# aquafin — Final Checklist Verification

End-to-end checklist from the build plan, walked through 2026-05-28 after Step 11.
The agent environment has no TTY, audio device, or graphics-capable terminal, so
items requiring an interactive terminal or audible/visual playback are flagged
for hands-on verification.

## Verified

| # | Item | How |
|---|---|---|
| 1 | **Wizard routes when no config** | `needs_wizard(false, false) == true` (unit test passes); `ui::orchestrate` calls `wizard::run` on that branch. |
| 2 | **Library + cover API** | Live server: `/UserViews` returns the user's libraries; `/Items/{id}` returns *8 Mile* (2002, hasImg=true); `/Items/{id}/Images/Primary?maxWidth=500` → `HTTP 200 image/jpeg 219 KB`. |
| 3 | **Movie stream URL** | Live server: `/Videos/{id}/stream?static=true&mediaSourceId={id}&api_key=…` → `HTTP 206 video/x-matroska`. |
| 4 | **Audio decode paths** | mp3 native, flac native, Opus → aac transcode, aac decode — all returned `DECODE OK` via probe runs. Vorbis isn't present in the test library; rodio's `vorbis` feature is enabled so the path is wired but not exercised here. |
| 5 | **F1 cheatsheet** | `keymap::describe()` test passes; earlier headless render showed all four groups (Navigation / Selection / Playback / General) rendering in full. |
| 6 | **Custom theme discoverable** | Dropped `~/.config/aquafin/themes/checklist-test.toml` → `theme::available_names()` returned `checklist-test`. Cleaned up after. |
| 7 | **Network error message** | Pointed the client at `http://127.0.0.1:1` → `Err("HTTP request failed: error sending request for url …")`. `orchestrate` wraps that into `Couldn't load your libraries:\n{e}` and `error_modal::render` shows it with the log path. |
| 8 | **Forced panic recovery** | Installed the panic hook + panicked → stderr: `aquafin crashed. Log: ~/.local/state/aquafin/aquafin-crash.log`; the crash file was written with `=== aquafin panic ===` + the panic message. Hook fires, log lands, exit code 1. |
| 9 | **Runtime theme switch** (extra) | `t_opens_theme_picker_and_enter_emits_set_theme` + `theme_change_alters_rendered_border_color` tests pass. |
| 11 | **`just install`** | `install -Dm755 target/release/aquafin ~/.local/bin/aquafin` lands on `$PATH`; `aquafin --version` runs. `cargo install --path .` also works (binary in `~/.cargo/bin`). |

## Hands-on (only the user can verify)

None of these is broken — they need a real terminal:

- [ ] **Fresh user, empty XDG dirs → wizard end-to-end.** The routing logic is verified, but the live keystrokes through the wizard (URL input, auth flow, success screen) need interactive observation.
- [ ] **See poster image in the detail pane.** Needs a kitty / ghostty / foot / sixel-capable terminal; only the user can confirm the image actually renders.
- [ ] **mpv launches, position reported, resume works.** The URL is verified, but the actual mpv window + scrubbing → server `Sessions/Playing/Progress` round-trip needs interactive observation.
- [ ] **Audible music playback + transport (`p` / `s` / `+` / `-`).** Decode paths are validated; audibility on real speakers is the missing bit.
- [ ] **Edit `config.toml` keymap → custom keymap applies after restart.** Unit-tested at the keymap layer (`config_override_rebinds_and_keeps_extras`); the full restart round-trip needs the user.

## Diagnosing failures

If any of the hands-on items breaks, the rolling log at
`~/.local/state/aquafin/aquafin*.log` captures: audio fetch/decode failures
(WARN), theme load failures (WARN), mpv playback-reporting errors (WARN), and
panics (ERROR + a separate `aquafin-crash.log`). Set
`--log-level debug` for verbose downloads/reports.
