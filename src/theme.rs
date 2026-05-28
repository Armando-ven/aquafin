//! Theme system: a named **palette** of colors plus per-**component** styles.
//!
//! A theme TOML defines a `[palette]` (named colors) and optionally overrides
//! `[components]` (each referencing a palette name — or a literal color — plus
//! style flags). Any component a theme omits falls back to the built-in default
//! mapping, so a theme can be as small as a palette. Five themes are compiled
//! into the binary; users can drop more into `$XDG_CONFIG_HOME/aquafin/themes/`.

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{Context, Result};
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

/// Themes compiled into the binary: `(name, toml)`. The first is the default.
const BUILTIN: &[(&str, &str)] = &[
    ("default", include_str!("../themes/default.toml")),
    ("catppuccin-mocha", include_str!("../themes/catppuccin-mocha.toml")),
    ("catppuccin-macchiato", include_str!("../themes/catppuccin-macchiato.toml")),
    ("catppuccin-frappe", include_str!("../themes/catppuccin-frappe.toml")),
    ("catppuccin-latte", include_str!("../themes/catppuccin-latte.toml")),
];

pub const DEFAULT_THEME: &str = "default";

// --- TOML schema ------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawTheme {
    name: Option<String>,
    palette: BTreeMap<String, String>,
    components: BTreeMap<String, RawComponent>,
}

/// A component's styling as written in TOML: `fg`/`bg` are palette names (or
/// literal colors like `"#ff0000"`), plus boolean style flags.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawComponent {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    italic: bool,
    underline: bool,
    dim: bool,
    reversed: bool,
}

impl RawComponent {
    fn fg(key: &str) -> Self {
        Self { fg: Some(key.into()), ..Default::default() }
    }
    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    fn dim(mut self) -> Self {
        self.dim = true;
        self
    }
    fn bg(mut self, key: &str) -> Self {
        self.bg = Some(key.into());
        self
    }
}

// --- Resolved theme ---------------------------------------------------------

/// A fully-resolved theme: each component name maps to a ready-to-use [`Style`].
#[derive(Debug, Clone)]
pub struct Theme {
    name: String,
    components: BTreeMap<String, Style>,
}

impl Default for Theme {
    fn default() -> Self {
        load(DEFAULT_THEME).unwrap_or_else(|_| Self::from_palette_only(fallback_palette()))
    }
}

macro_rules! component_accessors {
    ($($method:ident => $key:literal),+ $(,)?) => {
        $(pub fn $method(&self) -> Style { self.style($key) })+
    };
}

impl Theme {
    pub fn name(&self) -> &str {
        &self.name
    }

    fn style(&self, key: &str) -> Style {
        self.components.get(key).copied().unwrap_or_default()
    }

    component_accessors! {
        list_item => "list_item",
        focused_border => "focused_border",
        unfocused_border => "unfocused_border",
        status_bar => "status_bar",
        hint => "hint",
        header => "header",
        muted => "muted",
        modal => "modal",
        modal_border => "modal_border",
        cheatsheet_group => "cheatsheet_group",
        cheatsheet_key => "cheatsheet_key",
        now_playing_title => "now_playing_title",
        now_playing_subtitle => "now_playing_subtitle",
        now_playing_meta => "now_playing_meta",
        progress_bar => "progress_bar",
        progress_track => "progress_track",
        error_text => "error_text",
        error_border => "error_border",
        folder_marker => "folder_marker",
        scrollbar => "scrollbar",
    }

    /// Selected-row style: the strong `selected_item` when the pane is focused,
    /// the calmer `selected_item_blur` otherwise.
    pub fn selected_item(&self, focused: bool) -> Style {
        self.style(if focused { "selected_item" } else { "selected_item_blur" })
    }

    /// Pane border style, by focus.
    pub fn border(&self, focused: bool) -> Style {
        if focused {
            self.focused_border()
        } else {
            self.unfocused_border()
        }
    }

    fn from_palette_only(palette: BTreeMap<String, Color>) -> Self {
        Self::resolve("default".to_string(), &palette, &BTreeMap::new())
    }

    /// Resolve a palette + component overrides into styles, filling any omitted
    /// component from [`default_components`].
    fn resolve(
        name: String,
        palette: &BTreeMap<String, Color>,
        overrides: &BTreeMap<String, RawComponent>,
    ) -> Self {
        let mut components = BTreeMap::new();
        for (key, default) in default_components() {
            let raw = overrides.get(key).cloned().unwrap_or(default);
            components.insert(key.to_string(), style_from(&raw, palette));
        }
        Self { name, components }
    }
}

/// Turn one [`RawComponent`] into a ratatui [`Style`], resolving color tokens
/// against the palette (a token is a palette name, else a literal color).
fn style_from(raw: &RawComponent, palette: &BTreeMap<String, Color>) -> Style {
    let mut style = Style::new();
    if let Some(color) = raw.fg.as_deref().and_then(|t| resolve_color(t, palette)) {
        style = style.fg(color);
    }
    if let Some(color) = raw.bg.as_deref().and_then(|t| resolve_color(t, palette)) {
        style = style.bg(color);
    }
    let flags = [
        (raw.bold, Modifier::BOLD),
        (raw.italic, Modifier::ITALIC),
        (raw.underline, Modifier::UNDERLINED),
        (raw.dim, Modifier::DIM),
        (raw.reversed, Modifier::REVERSED),
    ];
    for (on, modifier) in flags {
        if on {
            style = style.add_modifier(modifier);
        }
    }
    style
}

/// A color token is a palette name first, otherwise a literal (`"#1e1e2e"`,
/// `"cyan"`, `"12"`, …) parsed by ratatui.
fn resolve_color(token: &str, palette: &BTreeMap<String, Color>) -> Option<Color> {
    palette
        .get(token)
        .copied()
        .or_else(|| Color::from_str(token).ok())
}

/// The component → (palette key + flags) mapping used when a theme omits a
/// component. The single source of truth for which palette entries drive each
/// UI element. Also the basis for `themes/example.toml`.
fn default_components() -> Vec<(&'static str, RawComponent)> {
    vec![
        ("list_item", RawComponent::fg("text")),
        ("selected_item", RawComponent::fg("base").bg("accent").bold()),
        ("selected_item_blur", RawComponent::fg("text").bg("selection")),
        ("focused_border", RawComponent::fg("accent").bold()),
        ("unfocused_border", RawComponent::fg("border")),
        ("status_bar", RawComponent::fg("subtext").bg("surface")),
        ("hint", RawComponent::fg("subtext").bg("surface")),
        ("header", RawComponent::fg("text").bold()),
        ("muted", RawComponent::fg("subtext")),
        ("modal", RawComponent::fg("text").bg("surface")),
        ("modal_border", RawComponent::fg("accent").bold()),
        ("search_input", RawComponent::fg("text")),
        ("scrollbar", RawComponent::fg("overlay")),
        ("cheatsheet_group", RawComponent::fg("accent").bold()),
        ("cheatsheet_key", RawComponent::fg("warn")),
        ("now_playing_title", RawComponent::fg("text").bold()),
        ("now_playing_subtitle", RawComponent::fg("subtext")),
        ("now_playing_meta", RawComponent::fg("subtext").dim()),
        ("progress_bar", RawComponent::fg("accent")),
        ("progress_track", RawComponent::fg("overlay")),
        ("error_text", RawComponent::fg("error").bold()),
        ("error_border", RawComponent::fg("error").bold()),
        ("success_text", RawComponent::fg("success").bold()),
        ("warn_text", RawComponent::fg("warn").bold()),
        ("folder_marker", RawComponent::fg("subtext")),
    ]
}

/// The palette keys every theme is expected to define. Themes that omit one
/// inherit it from the default palette.
#[cfg(test)]
const PALETTE_KEYS: &[&str] = &[
    "base", "surface", "overlay", "text", "subtext", "accent", "success", "warn", "error",
    "border", "selection",
];

/// The default theme's palette, hardcoded as the ultimate fallback so the app
/// always has usable colors even if every theme file fails to parse.
fn fallback_palette() -> BTreeMap<String, Color> {
    [
        ("base", Color::Rgb(0x0e, 0x14, 0x16)),
        ("surface", Color::Rgb(0x15, 0x20, 0x25)),
        ("overlay", Color::Rgb(0x23, 0x32, 0x3a)),
        ("text", Color::Rgb(0xcd, 0xd9, 0xde)),
        ("subtext", Color::Rgb(0x7e, 0x94, 0xa0)),
        ("accent", Color::Rgb(0x39, 0xbd, 0xb6)),
        ("success", Color::Rgb(0x7f, 0xd1, 0xa0)),
        ("warn", Color::Rgb(0xe6, 0xc4, 0x78)),
        ("error", Color::Rgb(0xe2, 0x74, 0x7f)),
        ("border", Color::Rgb(0x2a, 0x3a, 0x42)),
        ("selection", Color::Rgb(0x1c, 0x4f, 0x50)),
    ]
    .into_iter()
    .map(|(k, c)| (k.to_string(), c))
    .collect()
}

// --- Loading ----------------------------------------------------------------

/// Parse a theme from TOML text. Unknown palette colors are dropped (and inherit
/// from the default palette); the whole parse only fails on malformed TOML.
pub fn parse(name: &str, toml_text: &str) -> Result<Theme> {
    let raw: RawTheme = toml::from_str(toml_text).context("parsing theme TOML")?;

    let mut palette = fallback_palette();
    for (key, value) in &raw.palette {
        match Color::from_str(value) {
            Ok(color) => {
                palette.insert(key.clone(), color);
            }
            Err(_) => tracing::warn!(theme = name, key, value, "invalid theme color; ignoring"),
        }
    }

    let display_name = raw.name.clone().unwrap_or_else(|| name.to_string());
    Ok(Theme::resolve(display_name, &palette, &raw.components))
}

/// Load a theme by name: a built-in if it matches, otherwise
/// `$XDG_CONFIG_HOME/aquafin/themes/<name>.toml`.
pub fn load(name: &str) -> Result<Theme> {
    if let Some((_, toml_text)) = BUILTIN.iter().find(|(n, _)| *n == name) {
        return parse(name, toml_text);
    }
    let path = crate::paths::themes_dir()?.join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading theme {}", path.display()))?;
    parse(name, &text)
}

/// Copy `src` into the user themes directory and validate it parses. Returns
/// the destination path so the caller can tell the user where it went.
pub fn import(src: &std::path::Path) -> Result<std::path::PathBuf> {
    let file_name = src
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("source path has no filename: {}", src.display()))?;
    if src.extension().and_then(|e| e.to_str()) != Some("toml") {
        anyhow::bail!("theme file must have a .toml extension: {}", src.display());
    }
    let text = std::fs::read_to_string(src)
        .with_context(|| format!("reading {}", src.display()))?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("imported");
    // Parse-validate first so we don't litter the themes dir with broken files.
    parse(stem, &text).with_context(|| format!("theme {} did not parse", src.display()))?;

    let dest_dir = crate::paths::themes_dir()?;
    std::fs::create_dir_all(&dest_dir)
        .with_context(|| format!("creating {}", dest_dir.display()))?;
    let dest = dest_dir.join(file_name);
    std::fs::copy(src, &dest)
        .with_context(|| format!("copying {} -> {}", src.display(), dest.display()))?;
    Ok(dest)
}

/// All selectable theme names: built-ins plus `*.toml` files in the themes dir,
/// de-duplicated and sorted (built-ins first).
pub fn available_names() -> Vec<String> {
    let mut names: Vec<String> = BUILTIN.iter().map(|(n, _)| n.to_string()).collect();
    if let Ok(dir) = crate::paths::themes_dir() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut user: Vec<String> = entries
                .filter_map(Result::ok)
                .filter_map(|e| {
                    let path = e.path();
                    (path.extension()? == "toml")
                        .then(|| path.file_stem()?.to_str().map(str::to_string))
                        .flatten()
                })
                .filter(|name| !names.contains(name))
                .collect();
            user.sort();
            names.extend(user);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtin_themes_parse() {
        for (name, _) in BUILTIN {
            let theme = load(name).unwrap_or_else(|e| panic!("{name} failed: {e}"));
            assert_eq!(theme.name(), *name);
            // A resolved theme has a style for every component.
            assert_eq!(theme.components.len(), default_components().len());
        }
    }

    #[test]
    fn builtin_themes_define_every_palette_key() {
        // Each built-in must specify all palette keys (not silently inherit), so
        // they're genuinely distinct rather than half the default.
        for (name, toml_text) in BUILTIN {
            let raw: RawTheme = toml::from_str(toml_text).unwrap();
            for key in PALETTE_KEYS {
                assert!(
                    raw.palette.contains_key(*key),
                    "{name} is missing palette key {key}"
                );
                assert!(
                    Color::from_str(&raw.palette[*key]).is_ok(),
                    "{name} has an invalid {key} color"
                );
            }
        }
    }

    #[test]
    fn component_override_wins_over_default() {
        let theme = parse(
            "t",
            r##"
            [palette]
            accent = "#010203"
            [components]
            focused_border = { fg = "#ff0000", bold = false }
        "##,
        )
        .unwrap();
        assert_eq!(theme.focused_border().fg, Some(Color::Rgb(0xff, 0, 0)));
        // Default (unoverridden) component still resolves against the palette.
        assert_eq!(theme.list_item().fg, Some(theme.style("list_item").fg.unwrap()));
    }

    #[test]
    fn palette_name_resolves_in_component() {
        let theme = parse(
            "t",
            r##"
            [palette]
            accent = "#abcdef"
        "##,
        )
        .unwrap();
        // focused_border defaults to fg = "accent".
        assert_eq!(theme.focused_border().fg, Some(Color::Rgb(0xab, 0xcd, 0xef)));
    }

    #[test]
    fn missing_palette_key_falls_back_to_default() {
        // Empty theme: every palette key inherits from the fallback palette.
        let theme = parse("t", "").unwrap();
        assert_eq!(theme.list_item().fg, Some(fallback_palette()["text"]));
    }

    #[test]
    fn available_names_lists_builtins() {
        let names = available_names();
        for (builtin, _) in BUILTIN {
            assert!(names.contains(&builtin.to_string()), "missing {builtin}");
        }
    }

    #[test]
    fn unknown_theme_name_errors() {
        assert!(load("does-not-exist-xyz").is_err());
    }
}
