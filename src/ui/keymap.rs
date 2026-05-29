//! Config-driven keymap. Built-in defaults are arrow-key navigation (the user's
//! chosen default); `[keymap]` entries in `config.toml` override per action.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A single dispatched action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Up,
    Down,
    Left,
    Right,
    Top,
    Bottom,
    /// Go up a folder level (or to the sidebar at a library root).
    Back,
    /// Play the focused item: video opens in mpv, audio plays in-app.
    Play,
    /// Pause/resume in-app audio (or mpv video).
    PlayPause,
    /// Stop in-app audio.
    Stop,
    VolumeUp,
    VolumeDown,
    /// Toggle favorite on the focused item.
    Favorite,
    /// Skip forward by the configured number of seconds.
    SeekForward,
    /// Skip backward by the configured number of seconds.
    SeekBackward,
    /// Advance to the next queued track.
    QueueNext,
    /// Go back to the previous queued track.
    QueuePrev,
    /// Toggle shuffle on the active queue.
    QueueShuffle,
    /// Cycle repeat mode: None → All → One → None.
    QueueRepeat,
    /// Open the theme picker.
    Themes,
    Help,
    Cancel,
}

struct ActionSpec {
    action: Action,
    name: &'static str,
    default_keys: &'static str,
    desc: &'static str,
    group: &'static str,
}

/// The single source of truth for actions: drives defaults, config lookup, the
/// cheatsheet, and `config.example.toml`.
const ACTIONS: &[ActionSpec] = &[
    ActionSpec { action: Action::Up, name: "up", default_keys: "up", desc: "Move up", group: "Navigation" },
    ActionSpec { action: Action::Down, name: "down", default_keys: "down", desc: "Move down", group: "Navigation" },
    ActionSpec { action: Action::Left, name: "left", default_keys: "left", desc: "Focus previous pane", group: "Navigation" },
    ActionSpec { action: Action::Right, name: "right", default_keys: "right", desc: "Focus next pane", group: "Navigation" },
    ActionSpec { action: Action::Top, name: "top", default_keys: "home", desc: "Jump to top", group: "Navigation" },
    ActionSpec { action: Action::Bottom, name: "bottom", default_keys: "end", desc: "Jump to bottom", group: "Navigation" },
    ActionSpec { action: Action::Back, name: "back", default_keys: "backspace", desc: "Back / up a folder", group: "Navigation" },
    ActionSpec { action: Action::Play, name: "play", default_keys: "enter", desc: "Play (video → mpv, audio → in-app)", group: "Playback" },
    ActionSpec { action: Action::PlayPause, name: "play_pause", default_keys: "space", desc: "Pause / resume", group: "Playback" },
    ActionSpec { action: Action::Stop, name: "stop", default_keys: "s", desc: "Stop audio", group: "Playback" },
    ActionSpec { action: Action::SeekForward, name: "seek_forward", default_keys: ">", desc: "Skip forward", group: "Playback" },
    ActionSpec { action: Action::SeekBackward, name: "seek_backward", default_keys: "<", desc: "Skip backward", group: "Playback" },
    ActionSpec { action: Action::QueueNext, name: "queue_next", default_keys: "n", desc: "Next track", group: "Queue" },
    ActionSpec { action: Action::QueuePrev, name: "queue_prev", default_keys: "p", desc: "Previous track", group: "Queue" },
    ActionSpec { action: Action::QueueShuffle, name: "queue_shuffle", default_keys: "z", desc: "Toggle shuffle", group: "Queue" },
    ActionSpec { action: Action::QueueRepeat, name: "queue_repeat", default_keys: "r", desc: "Cycle repeat mode", group: "Queue" },
    ActionSpec { action: Action::VolumeUp, name: "volume_up", default_keys: "+", desc: "Volume up", group: "Playback" },
    ActionSpec { action: Action::VolumeDown, name: "volume_down", default_keys: "-", desc: "Volume down", group: "Playback" },
    ActionSpec { action: Action::Favorite, name: "favorite", default_keys: "f", desc: "Toggle favorite", group: "Library" },
    ActionSpec { action: Action::Themes, name: "themes", default_keys: "t", desc: "Pick a theme", group: "General" },
    ActionSpec { action: Action::Help, name: "help", default_keys: "f1", desc: "Toggle help", group: "General" },
    ActionSpec { action: Action::Cancel, name: "cancel", default_keys: "esc", desc: "Close overlay / cancel", group: "General" },
    ActionSpec { action: Action::Quit, name: "quit", default_keys: "q", desc: "Quit", group: "General" },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct KeyChord {
    code: KeyCode,
    mods: KeyModifiers,
}

#[derive(Debug)]
struct Entry {
    action: Action,
    chords: Vec<KeyChord>,
    display: String,
}

/// The active key bindings.
#[derive(Debug)]
pub struct Keymap {
    entries: Vec<Entry>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::from_config(&BTreeMap::new()).0
    }
}

impl Keymap {
    /// Build the keymap from `[keymap]` overrides, returning any warnings about
    /// unknown actions or unparseable key strings (the action keeps its default).
    pub fn from_config(overrides: &BTreeMap<String, String>) -> (Self, Vec<String>) {
        let mut entries = Vec::with_capacity(ACTIONS.len());
        let mut warnings = Vec::new();

        for spec in ACTIONS {
            let configured = overrides.get(spec.name).map(String::as_str);
            let source = configured.unwrap_or(spec.default_keys);

            let mut chords = Vec::new();
            for token in source.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                match parse_chord(token) {
                    Some(chord) => chords.push(chord),
                    None => warnings.push(format!(
                        "keymap: ignoring unknown key \"{token}\" for action \"{}\"",
                        spec.name
                    )),
                }
            }

            if chords.is_empty() {
                if configured.is_some() {
                    warnings.push(format!(
                        "keymap: action \"{}\" had no valid keys; using default \"{}\"",
                        spec.name, spec.default_keys
                    ));
                }
                chords = spec
                    .default_keys
                    .split(',')
                    .filter_map(|t| parse_chord(t.trim()))
                    .collect();
            }

            let display = chords
                .iter()
                .map(chord_display)
                .collect::<Vec<_>>()
                .join(" / ");
            entries.push(Entry { action: spec.action, chords, display });
        }

        for name in overrides.keys() {
            if !ACTIONS.iter().any(|spec| spec.name == name) {
                warnings.push(format!("keymap: unknown action \"{name}\""));
            }
        }

        (Self { entries }, warnings)
    }

    pub fn action_for(&self, key: KeyEvent) -> Option<Action> {
        let chord = normalize(key.code, key.modifiers);
        self.entries
            .iter()
            .find(|entry| entry.chords.contains(&chord))
            .map(|entry| entry.action)
    }

    /// Active bindings grouped by context, for the F1 cheatsheet.
    pub fn describe(&self) -> Vec<DescribedGroup> {
        let mut groups: Vec<DescribedGroup> = Vec::new();
        for spec in ACTIONS {
            let keys = self
                .entries
                .iter()
                .find(|entry| entry.action == spec.action)
                .map(|entry| entry.display.clone())
                .unwrap_or_default();
            let binding = DescribedBinding { keys, desc: spec.desc };
            match groups.iter_mut().find(|group| group.title == spec.group) {
                Some(group) => group.bindings.push(binding),
                None => groups.push(DescribedGroup {
                    title: spec.group,
                    bindings: vec![binding],
                }),
            }
        }
        groups
    }
}

pub struct DescribedBinding {
    pub keys: String,
    pub desc: &'static str,
}

pub struct DescribedGroup {
    pub title: &'static str,
    pub bindings: Vec<DescribedBinding>,
}

/// SHIFT is dropped for character keys (the character already encodes case), so
/// `"G"` in config matches the `Char('G')` + SHIFT event crossterm reports.
fn normalize(code: KeyCode, mods: KeyModifiers) -> KeyChord {
    let mods = if matches!(code, KeyCode::Char(_)) {
        mods & !KeyModifiers::SHIFT
    } else {
        mods
    };
    KeyChord { code, mods }
}

/// Parse a key string like `down`, `j`, `ctrl+d`, `G`, `f1`, `space`.
fn parse_chord(spec: &str) -> Option<KeyChord> {
    let spec = spec.trim();
    let chars: Vec<char> = spec.chars().collect();
    if chars.len() == 1 {
        return Some(normalize(KeyCode::Char(chars[0]), KeyModifiers::NONE));
    }

    let mut parts: Vec<&str> = spec.split('+').collect();
    let key = parts.pop()?.trim();
    let mut mods = KeyModifiers::NONE;
    for modifier in parts {
        match modifier.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "alt" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            _ => return None,
        }
    }
    Some(normalize(parse_keycode(key)?, mods))
}

fn parse_keycode(name: &str) -> Option<KeyCode> {
    let lower = name.to_ascii_lowercase();
    let code = match lower.as_str() {
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "tab" => KeyCode::Tab,
        "backspace" | "bksp" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        other => {
            if let Some(num) = other.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                if (1..=12).contains(&num) {
                    return Some(KeyCode::F(num));
                }
            }
            let mut chars = other.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) => Some(KeyCode::Char(c)),
                _ => None,
            };
        }
    };
    Some(code)
}

fn chord_display(chord: &KeyChord) -> String {
    let mut out = String::new();
    if chord.mods.contains(KeyModifiers::CONTROL) {
        out.push_str("Ctrl+");
    }
    if chord.mods.contains(KeyModifiers::ALT) {
        out.push_str("Alt+");
    }
    out.push_str(&keycode_display(chord.code));
    out
}

fn keycode_display(code: KeyCode) -> String {
    match code {
        KeyCode::Up => "↑".to_string(),
        KeyCode::Down => "↓".to_string(),
        KeyCode::Left => "←".to_string(),
        KeyCode::Right => "→".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PgUp".to_string(),
        KeyCode::PageDown => "PgDn".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::Backspace => "Bksp".to_string(),
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn defaults_are_arrow_keys() {
        let km = Keymap::default();
        assert_eq!(km.action_for(ev(KeyCode::Down, KeyModifiers::NONE)), Some(Action::Down));
        assert_eq!(km.action_for(ev(KeyCode::Up, KeyModifiers::NONE)), Some(Action::Up));
        assert_eq!(km.action_for(ev(KeyCode::Home, KeyModifiers::NONE)), Some(Action::Top));
        assert_eq!(km.action_for(ev(KeyCode::Char('q'), KeyModifiers::NONE)), Some(Action::Quit));
        // hjkl is NOT bound by default.
        assert_eq!(km.action_for(ev(KeyCode::Char('j'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn playback_actions_have_defaults() {
        let km = Keymap::default();
        assert_eq!(km.action_for(ev(KeyCode::Enter, KeyModifiers::NONE)), Some(Action::Play));
        assert_eq!(km.action_for(ev(KeyCode::Char(' '), KeyModifiers::NONE)), Some(Action::PlayPause));
        assert_eq!(km.action_for(ev(KeyCode::Char('s'), KeyModifiers::NONE)), Some(Action::Stop));
        assert_eq!(km.action_for(ev(KeyCode::Char('+'), KeyModifiers::NONE)), Some(Action::VolumeUp));
        assert_eq!(km.action_for(ev(KeyCode::Char('-'), KeyModifiers::NONE)), Some(Action::VolumeDown));
        assert_eq!(km.action_for(ev(KeyCode::Char('>'), KeyModifiers::NONE)), Some(Action::SeekForward));
        assert_eq!(km.action_for(ev(KeyCode::Char('<'), KeyModifiers::NONE)), Some(Action::SeekBackward));
        assert_eq!(km.action_for(ev(KeyCode::Char('f'), KeyModifiers::NONE)), Some(Action::Favorite));
    }

    #[test]
    fn config_override_rebinds_and_keeps_extras() {
        let mut overrides = BTreeMap::new();
        overrides.insert("down".to_string(), "j, down".to_string());
        let (km, warnings) = Keymap::from_config(&overrides);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(km.action_for(ev(KeyCode::Char('j'), KeyModifiers::NONE)), Some(Action::Down));
        assert_eq!(km.action_for(ev(KeyCode::Down, KeyModifiers::NONE)), Some(Action::Down));
    }

    #[test]
    fn invalid_key_warns_and_falls_back_to_default() {
        let mut overrides = BTreeMap::new();
        overrides.insert("up".to_string(), "definitely-not-a-key".to_string());
        let (km, warnings) = Keymap::from_config(&overrides);
        assert!(!warnings.is_empty());
        // Falls back to the default (Up arrow).
        assert_eq!(km.action_for(ev(KeyCode::Up, KeyModifiers::NONE)), Some(Action::Up));
    }

    #[test]
    fn unknown_action_warns() {
        let mut overrides = BTreeMap::new();
        overrides.insert("fly".to_string(), "x".to_string());
        let (_, warnings) = Keymap::from_config(&overrides);
        assert!(warnings.iter().any(|w| w.contains("fly")));
    }

    #[test]
    fn ctrl_modifier_and_uppercase_char() {
        assert_eq!(
            parse_chord("ctrl+d"),
            Some(KeyChord { code: KeyCode::Char('d'), mods: KeyModifiers::CONTROL })
        );
        // SHIFT is normalized away for chars; "G" matches the shift+g event.
        let chord = parse_chord("G").unwrap();
        assert_eq!(chord, normalize(KeyCode::Char('G'), KeyModifiers::SHIFT));
    }

    #[test]
    fn describe_groups_actions() {
        let groups = Keymap::default().describe();
        assert!(groups.iter().any(|g| g.title == "Navigation"));
        let nav = groups.iter().find(|g| g.title == "Navigation").unwrap();
        assert!(nav.bindings.iter().any(|b| b.keys.contains('↑') || b.keys.contains("Up")));
    }
}
