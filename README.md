# aquafin

A Jellyfin client for the terminal. Rust. Linux only.

- **Browse** your libraries with a three-pane layout (libraries / list / detail), drill into series → seasons → episodes and artists → albums → tracks.
- **Watch video** in external `mpv` (launched from the TUI; progress reported back to the server).
- **Listen to audio** in-app via PipeWire/ALSA — direct-streamed when possible, transcoded by the server otherwise.
- **Covers** for items and the now-playing bar via `ratatui-image` (kitty/sixel/halfblocks).
- **Themeable.** Five built-in themes, plus your own from `~/.config/aquafin/themes/`.

## Requirements

- Linux with PipeWire or ALSA.
- A C toolchain (for cpal/audio), `pkg-config`, and `mpv` (for video playback).
- Rust 1.80+ (stable).
- Optional, but supported terminals for inline images: `kitty`, `ghostty`, `foot`.

## Install

### `just install` (recommended)

```sh
just install
```

Builds in release mode and copies the binary to `$XDG_BIN_HOME` (falling back to `~/.local/bin`). Make sure that directory is on your `$PATH`. To remove it later:

```sh
just uninstall
```

### `cargo install`

```sh
cargo install --path .
```

Places `aquafin` in `~/.cargo/bin`.

### Manual

```sh
cargo build --release
install -Dm755 target/release/aquafin ~/.local/bin/aquafin
```

## First run

The first launch (or `aquafin --setup`) opens a wizard that asks for your Jellyfin server URL and signs you in via password or Quick Connect. The token is stored in the OS keyring when available, otherwise in a `0600` file under `$XDG_DATA_HOME/aquafin/`.

## Keybindings

Defaults — all rebindable in [`config.example.toml`](config.example.toml).

| Action          | Key                              |
|-----------------|----------------------------------|
| Move selection  | `↑` / `↓` / `Home` / `End`       |
| Focus panes     | `←` / `→`                        |
| Open / play     | `Enter` (folder drills in; leaf plays) |
| Back / up       | `Backspace` (or `←` from a drilled list) |
| Multi-select    | `Space`                          |
| Pause / resume  | `p`                              |
| Stop            | `s`                              |
| Volume          | `+` / `-`                        |
| Theme picker    | `t`                              |
| Help            | `F1`                             |
| Quit            | `q`                              |

Video plays in external `mpv`; close the mpv window to return.

## Configuration

Config lives at `$XDG_CONFIG_HOME/aquafin/config.toml` (usually `~/.config/aquafin/config.toml`). Every field is optional, and deleting a section never breaks aquafin — see the fully-documented [`config.example.toml`](config.example.toml) for the schema.

Tunable areas: server URL, theme, image protocol (`auto`/`kitty`/`sixel`/`ascii`), keymap overrides, default audio volume, log level and rotation.

## Themes

aquafin ships five themes: `default` (an original aqua-accented dark), and `catppuccin-mocha`, `catppuccin-macchiato`, `catppuccin-frappe`, `catppuccin-latte`. Select one with `ui.theme = "<name>"` in `config.toml`, or press `t` in the app to switch at runtime (session-only).

To write your own, copy [`themes/example.toml`](themes/example.toml) (which documents every palette key and overridable component) into `~/.config/aquafin/themes/<name>.toml`. To import from anywhere:

```sh
aquafin --import-theme path/to/my-theme.toml
```

## Logs

aquafin writes a rolling daily log to `$XDG_STATE_HOME/aquafin/` (usually `~/.local/state/aquafin/`). Verbosity is controlled by `log.level` in `config.toml` or the `--log-level` CLI flag (`error`/`warn`/`info`/`debug`/`trace`). On a panic, the terminal is restored and a crash log is written alongside.

## License

GNU GPL v3.0 — see [LICENSE](LICENSE).
