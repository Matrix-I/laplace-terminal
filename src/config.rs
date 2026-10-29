//! Ghostty-style configuration: a plain `key = value` text file at
//! `~/.litty/config`.
//!
//! - `#` starts a comment line, blank lines are ignored.
//! - Values may optionally be wrapped in double quotes.
//! - An empty value (`key =`) resets the option to its default.
//! - Repeatable keys (`font-family`) append; an empty value clears the list.
//!
//! `Cmd+,` (macOS) / `Ctrl+,` (Linux) creates the file from a documented
//! template if missing and opens it in the default text editor.
//! `Cmd+Shift+,` / `Ctrl+Shift+,` reloads it; the file is also reloaded
//! automatically when the window regains focus after the file changed.

use std::path::PathBuf;
use std::time::SystemTime;

pub use crate::update::{AutoUpdateChannel, AutoUpdateMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Bar,
    Underline,
    BlockHollow,
}

impl CursorStyle {
    /// Value passed to the shader (`u.cursor_style`).
    pub fn shader_id(self) -> u32 {
        match self {
            CursorStyle::Block => 0,
            CursorStyle::Bar => 1,
            CursorStyle::Underline => 2,
            CursorStyle::BlockHollow => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    Srgb,
    DisplayP3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellIntegration {
    Detect,
    None,
    Zsh,
    Bash,
    Fish,
    Nushell,
    Elvish,
}

/// What a BEL does (`bell-features`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BellFeatures {
    /// Play the system alert sound.
    pub system: bool,
    /// Bounce the Dock icon / request attention while unfocused.
    pub attention: bool,
    /// Mark the tab title with 🔔 until it is seen.
    pub title: bool,
    /// Play `bell-audio-path`.
    pub audio: bool,
    /// Outline the window until it is focused or a key is pressed.
    pub border: bool,
}

/// `confirm-close-surface`: when to ask before closing a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmClose {
    Never,
    /// Only while a program other than the shell is running.
    Running,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellFeatures {
    pub cursor: bool,
    pub sudo: bool,
    pub title: bool,
}

pub const DEFAULT_PALETTE: [u32; 16] = [
    0x121316, 0xF38BA8, 0xA6E3A1, 0xF5A97F, 0x00BFE3, 0xCBA6F7, 0x89DCEB, 0xD9E0EE,
    0x585B70, 0xF38BA8, 0xA6E3A1, 0xF9E2AF, 0x74C7EC, 0xF5C2E7, 0x94E2D5, 0xFFFFFF,
];

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Default background color, packed `0xRRGGBB00`.
    pub background: u32,
    /// Default foreground color, packed `0xRRGGBB00`.
    pub foreground: u32,
    /// Active theme name, if set.
    pub theme: Option<String>,
    /// ANSI 16-color palette (`0xRRGGBB`).
    pub palette: [u32; 16],
    /// Font families in priority order; the first is primary, the rest are fallbacks.
    pub font_family: Vec<String>,
    /// Font size in points.
    pub font_size: f32,
    /// Initial window width in grid columns (requires `window-height`).
    pub window_width: Option<u32>,
    /// Initial window height in grid rows (requires `window-width`).
    pub window_height: Option<u32>,
    /// Opacity of the default background, `0.0..=1.0`.
    pub background_opacity: f32,
    /// Background blur radius in points (macOS only, 0 = disabled).
    pub background_blur: u32,
    pub window_colorspace: ColorSpace,
    pub mouse_hide_while_typing: bool,
    /// Scroll multiplier for precision (trackpad) devices.
    pub mouse_scroll_precision: f64,
    /// Scroll multiplier for discrete (wheel) devices.
    pub mouse_scroll_discrete: f64,
    pub cursor_style: CursorStyle,
    /// `None` = blink by default, but programs (DECSCUSR) may change it.
    pub cursor_style_blink: Option<bool>,
    pub shell_integration: ShellIntegration,
    pub shell_integration_features: ShellFeatures,
    /// Horizontal / vertical window padding in points.
    pub window_padding_x: f32,
    pub window_padding_y: f32,
    /// Spread leftover pixels evenly around the grid.
    pub window_padding_balance: bool,
    /// Cursor color `0xRRGGBB00` (OSC 12 still overrides it per terminal).
    pub cursor_color: Option<u32>,
    /// Selection colors `0xRRGGBB00`.
    pub selection_background: Option<u32>,
    pub selection_foreground: Option<u32>,
    /// Copy a mouse selection to the clipboard as soon as it is made.
    pub copy_on_select: bool,
    /// Fixed window title instead of the running program's.
    pub title: Option<String>,
    pub bell_features: BellFeatures,
    /// Sound file for `bell-features = audio`, and its volume `0..=1`.
    pub bell_audio_path: Option<PathBuf>,
    pub bell_audio_volume: f32,
    pub confirm_close_surface: ConfirmClose,
    /// macOS: new tabs are native window tabs (NSWindow tabbing) instead of
    /// Litty's own tab bar.
    pub macos_native_tabs: bool,
    /// `custom-shader` files, applied in order (Shadertoy-style GLSL).
    pub custom_shaders: Vec<PathBuf>,
    /// `custom-shader-animation`: `None` = never, `Some(false)` = while
    /// focused (`true`), `Some(true)` = `always`.
    pub custom_shader_animation: Option<bool>,
    /// `keybind = global:<trigger>=toggle_quick_terminal` (macOS).
    pub quick_terminal_hotkey: Option<crate::keymap::Trigger>,
    /// Hide the Quick Terminal when it loses focus.
    pub quick_terminal_autohide: bool,
    /// `scrollback-limit` in bytes (Ghostty semantics); `None` = 5000 lines.
    pub scrollback_limit: Option<usize>,
    /// `grapheme-width-method = unicode` (true, default) starts terminals with
    /// DEC mode 2027 on; `legacy` gives every codepoint its own width.
    pub grapheme_unicode: bool,
    /// Opacity of unfocused splits, `0.15..=1.0`, faded toward `unfocused_split_fill`.
    pub unfocused_split_opacity: f32,
    /// Color unfocused splits fade toward (`0xRRGGBB00`); `None` = the background.
    pub unfocused_split_fill: Option<u32>,
    /// Minimum WCAG contrast ratio between text and its background, `1..=21`.
    pub minimum_contrast: f32,
    /// `adjust-cell-width` / `adjust-cell-height` and `font-thicken`.
    pub adjust_cell_width: crate::atlas::MetricAdjust,
    pub adjust_cell_height: crate::atlas::MetricAdjust,
    pub font_thicken: bool,
    /// `font-feature` (`-calt`, `ss01`, ...); ligatures are on unless disabled.
    pub font_features: Vec<String>,
    /// `keybind = trigger=action` entries in file order (later ones win).
    pub keybinds: Vec<(crate::keymap::Trigger, crate::keymap::Binding)>,
    /// Auto update checking mode (`check`, `download`, `off`).
    pub auto_update: AutoUpdateMode,
    /// Channel for auto updates (`stable`, `tip`).
    pub auto_update_channel: AutoUpdateChannel,
}

pub const DEFAULT_FONT_PT: f32 = 14.0;

impl Default for Config {
    fn default() -> Self {
        let font_size = std::env::var("LITTY_FONT_SIZE")
            .or_else(|_| std::env::var("LAPLACE_FONT_SIZE"))
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(DEFAULT_FONT_PT)
            .clamp(6.0, 72.0);
        Config {
            background: 0x0000_0000,
            foreground: 0xE6E6_E600,
            theme: None,
            palette: DEFAULT_PALETTE,
            font_family: Vec::new(),
            font_size,
            window_width: None,
            window_height: None,
            background_opacity: 1.0,
            background_blur: 0,
            window_colorspace: ColorSpace::Srgb,
            mouse_hide_while_typing: false,
            mouse_scroll_precision: 1.0,
            mouse_scroll_discrete: 3.0,
            cursor_style: CursorStyle::Block,
            cursor_style_blink: None,
            shell_integration: ShellIntegration::Detect,
            shell_integration_features: ShellFeatures { cursor: true, sudo: false, title: true },
            window_padding_x: 4.0,
            window_padding_y: 4.0,
            window_padding_balance: false,
            keybinds: Vec::new(),
            cursor_color: None,
            selection_background: None,
            selection_foreground: None,
            copy_on_select: true,
            title: None,
            bell_features: BellFeatures { system: false, attention: true, title: true, audio: false, border: false },
            bell_audio_path: None,
            bell_audio_volume: 0.5,
            scrollback_limit: None,
            grapheme_unicode: true,
            confirm_close_surface: ConfirmClose::Running,
            macos_native_tabs: false,
            custom_shaders: Vec::new(),
            custom_shader_animation: Some(false),
            quick_terminal_hotkey: None,
            quick_terminal_autohide: true,
            unfocused_split_opacity: 0.7,
            unfocused_split_fill: None,
            minimum_contrast: 1.0,
            adjust_cell_width: crate::atlas::MetricAdjust::None,
            adjust_cell_height: crate::atlas::MetricAdjust::None,
            font_thicken: false,
            font_features: Vec::new(),
            auto_update: AutoUpdateMode::Check,
            auto_update_channel: AutoUpdateChannel::Stable,
        }
    }
}

/// Path for Litty config: `~/.litty/config`.
pub fn litty_config_path() -> PathBuf {
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty()).unwrap_or_else(|| "/".into());
    PathBuf::from(home).join(".litty").join("config")
}

/// Primary path for Litty config.
pub fn config_path() -> PathBuf {
    litty_config_path()
}

/// Active path for Litty config: `~/.litty/config`.
pub fn active_config_path() -> PathBuf {
    litty_config_path()
}

pub fn config_mtime() -> Option<SystemTime> {
    std::fs::metadata(active_config_path()).and_then(|m| m.modified()).ok()
}

impl Config {
    /// Load the config file. A missing file yields defaults; malformed lines
    /// are reported on stderr and skipped (like Ghostty, a bad line never
    /// prevents the terminal from starting).
    pub fn load() -> Config {
        let path = active_config_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
            Err(e) => {
                eprintln!("litty: cannot read config {}: {e}", path.display());
                return Config::default();
            }
        };
        let (cfg, errors) = Config::parse(&text);
        for err in errors {
            eprintln!("litty: config {}: {err}", path.display());
        }
        cfg
    }

    /// Parse config text, returning the config and a list of human-readable errors.
    pub fn parse(text: &str) -> (Config, Vec<String>) {
        let mut cfg = Config::default();
        let mut errors = Vec::new();
        let mut entries = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            match parse_line(raw) {
                Some(Ok(entry)) => entries.push((i + 1, entry)),
                Some(Err(line)) => errors.push(format!("line {}: expected `key = value`, got `{line}`", i + 1)),
                None => {}
            }
        }
        // As in Ghostty, the theme is applied first, so every other setting in
        // the file wins over it wherever it appears.
        if let Some(&(n, (_, value))) = entries.iter().rev().find(|(_, (k, _))| *k == "theme") {
            let name = theme_variant(value, crate::grid::color_scheme_is_dark());
            if !value.is_empty() && !cfg.apply_theme(name) {
                errors.push(format!("line {n}: theme: unknown theme `{value}`"));
            }
        }
        for (n, (key, value)) in entries {
            if let Err(e) = cfg.set(key, value) {
                errors.push(format!("line {n}: {key}: {e}"));
            }
        }
        if cfg.window_width.is_some() != cfg.window_height.is_some() {
            errors.push("window-width and window-height must both be set; ignoring".into());
            cfg.window_width = None;
            cfg.window_height = None;
        }
        (cfg, errors)
    }

    fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let d = Config::default();
        let empty = value.is_empty();
        match key {
            "background" => self.background = if empty { d.background } else { parse_color(value)? },
            "foreground" => self.foreground = if empty { d.foreground } else { parse_color(value)? },
            "font-family" => {
                if empty {
                    self.font_family.clear();
                } else {
                    self.font_family.push(value.to_string());
                }
            }
            "font-size" => {
                self.font_size = if empty {
                    d.font_size
                } else {
                    let v: f32 = value.parse().map_err(|_| format!("invalid number `{value}`"))?;
                    if !(1.0..=255.0).contains(&v) {
                        return Err(format!("out of range `{value}`"));
                    }
                    v
                }
            }
            "window-width" => {
                self.window_width = if empty { None } else { Some(parse_uint(value)?.max(10)) }
            }
            "window-height" => {
                self.window_height = if empty { None } else { Some(parse_uint(value)?.max(4)) }
            }
            "background-opacity" => {
                self.background_opacity = if empty {
                    d.background_opacity
                } else {
                    let v: f32 = value.parse().map_err(|_| format!("invalid number `{value}`"))?;
                    v.clamp(0.0, 1.0)
                }
            }
            "background-blur" | "background-blur-radius" => {
                self.background_blur = if empty {
                    d.background_blur
                } else {
                    match value {
                        "true" => 20,
                        "false" => 0,
                        v => parse_uint(v)?.min(255),
                    }
                }
            }
            "window-colorspace" => {
                self.window_colorspace = match value {
                    "" | "srgb" => ColorSpace::Srgb,
                    "display-p3" => ColorSpace::DisplayP3,
                    v => return Err(format!("expected `srgb` or `display-p3`, got `{v}`")),
                }
            }
            "mouse-hide-while-typing" => {
                self.mouse_hide_while_typing = if empty { d.mouse_hide_while_typing } else { parse_bool(value)? }
            }
            "mouse-scroll-multiplier" => {
                if empty {
                    self.mouse_scroll_precision = d.mouse_scroll_precision;
                    self.mouse_scroll_discrete = d.mouse_scroll_discrete;
                } else if value.contains(':') {
                    // Ghostty 1.2 syntax: `precision:1,discrete:3`
                    for part in value.split(',') {
                        let (k, v) = part
                            .split_once(':')
                            .ok_or_else(|| format!("invalid entry `{part}`"))?;
                        let n = parse_multiplier(v.trim())?;
                        match k.trim() {
                            "precision" => self.mouse_scroll_precision = n,
                            "discrete" => self.mouse_scroll_discrete = n,
                            other => return Err(format!("unknown device `{other}`")),
                        }
                    }
                } else {
                    let n = parse_multiplier(value)?;
                    self.mouse_scroll_precision = n * d.mouse_scroll_precision;
                    self.mouse_scroll_discrete = n * d.mouse_scroll_discrete;
                }
            }
            "cursor-style" => {
                self.cursor_style = match value {
                    "" | "block" => CursorStyle::Block,
                    "bar" => CursorStyle::Bar,
                    "underline" => CursorStyle::Underline,
                    "block_hollow" => CursorStyle::BlockHollow,
                    v => return Err(format!("expected block, bar, underline or block_hollow, got `{v}`")),
                }
            }
            "cursor-style-blink" => {
                self.cursor_style_blink = if empty { None } else { Some(parse_bool(value)?) }
            }
            "shell-integration" => {
                self.shell_integration = match value {
                    "" | "detect" => ShellIntegration::Detect,
                    "none" => ShellIntegration::None,
                    "zsh" => ShellIntegration::Zsh,
                    "bash" => ShellIntegration::Bash,
                    "fish" => ShellIntegration::Fish,
                    "nushell" | "nu" => ShellIntegration::Nushell,
                    "elvish" => ShellIntegration::Elvish,
                    v => return Err(format!("expected detect, none, zsh, bash, fish, nushell or elvish, got `{v}`")),
                }
            }
            "shell-integration-features" => {
                if empty {
                    self.shell_integration_features = d.shell_integration_features;
                    return Ok(());
                }
                let f = &mut self.shell_integration_features;
                match value {
                    "true" => *f = ShellFeatures { cursor: true, sudo: true, title: true },
                    "false" => *f = ShellFeatures { cursor: false, sudo: false, title: false },
                    _ => {
                        for item in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                            let (on, name) = match item.strip_prefix("no-") {
                                Some(n) => (false, n),
                                None => (true, item),
                            };
                            match name {
                                "cursor" => f.cursor = on,
                                "sudo" => f.sudo = on,
                                "title" => f.title = on,
                                other => return Err(format!("unknown feature `{other}`")),
                            }
                        }
                    }
                }
            }
            // Applied before the rest of the file (see `parse`).
            "theme" => self.theme = (!empty).then(|| value.to_string()),
            "palette" => {
                if empty {
                    self.palette = DEFAULT_PALETTE;
                } else {
                    let (idx_str, col_str) = value
                        .split_once('=')
                        .ok_or_else(|| format!("expected `N=color`, got `{value}`"))?;
                    let idx: usize = idx_str
                        .trim()
                        .parse()
                        .map_err(|_| format!("invalid palette index `{idx_str}`"))?;
                    if idx >= 16 {
                        return Err(format!("palette index `{idx}` out of range (0..15)"));
                    }
                    let col = parse_color(col_str.trim())? >> 8;
                    self.palette[idx] = col;
                }
            }
            "window-padding-x" => self.window_padding_x = if empty { d.window_padding_x } else { parse_padding(value)? },
            "window-padding-y" => self.window_padding_y = if empty { d.window_padding_y } else { parse_padding(value)? },
            "cursor-color" => self.cursor_color = if empty { None } else { Some(parse_color(value)?) },
            "selection-background" => self.selection_background = if empty { None } else { Some(parse_color(value)?) },
            "selection-foreground" => self.selection_foreground = if empty { None } else { Some(parse_color(value)?) },
            // Ghostty also accepts `clipboard` (copy to the system clipboard, as Litty does).
            "copy-on-select" => {
                self.copy_on_select = match value {
                    "" | "true" | "clipboard" => true,
                    "false" => false,
                    v => return Err(format!("expected true, false or clipboard, got `{v}`")),
                }
            }
            "title" => self.title = (!empty).then(|| value.to_string()),
            "adjust-cell-width" => {
                self.adjust_cell_width = if empty { d.adjust_cell_width } else { crate::atlas::MetricAdjust::parse(value)? }
            }
            "adjust-cell-height" => {
                self.adjust_cell_height = if empty { d.adjust_cell_height } else { crate::atlas::MetricAdjust::parse(value)? }
            }
            "font-feature" => {
                if empty {
                    self.font_features.clear();
                }
                for f in value.split(',').map(str::trim).filter(|f| !f.is_empty()) {
                    crate::atlas::parse_font_feature(f)?;
                    self.font_features.push(f.to_string());
                }
            }
            "font-thicken" => self.font_thicken = if empty { d.font_thicken } else { parse_bool(value)? },
            "quick-terminal-autohide" => {
                self.quick_terminal_autohide = if empty { d.quick_terminal_autohide } else { parse_bool(value)? }
            }
            "custom-shader" => {
                if empty {
                    self.custom_shaders.clear();
                } else {
                    self.custom_shaders.push(expand_home(value));
                }
            }
            "custom-shader-animation" => {
                self.custom_shader_animation = match value {
                    "" | "true" => Some(false),
                    "false" => None,
                    "always" => Some(true),
                    v => return Err(format!("expected true, false or always, got `{v}`")),
                }
            }
            "macos-native-tabs" => self.macos_native_tabs = if empty { d.macos_native_tabs } else { parse_bool(value)? },
            "confirm-close-surface" => {
                self.confirm_close_surface = match value {
                    "" | "true" => ConfirmClose::Running,
                    "false" => ConfirmClose::Never,
                    "always" => ConfirmClose::Always,
                    v => return Err(format!("expected true, false or always, got `{v}`")),
                }
            }
            "grapheme-width-method" => {
                self.grapheme_unicode = match value {
                    "" | "unicode" => true,
                    "legacy" | "wcswidth" => false,
                    _ => return Err(format!("expected `unicode` or `legacy`, got `{value}`")),
                }
            }
            "scrollback-limit" => self.scrollback_limit = if empty { None } else { Some(value.parse::<usize>().map_err(|_| format!("invalid integer `{value}`"))?) },
            "unfocused-split-opacity" => {
                self.unfocused_split_opacity = if empty { d.unfocused_split_opacity } else { parse_f32(value)?.clamp(0.15, 1.0) }
            }
            "unfocused-split-fill" => self.unfocused_split_fill = if empty { None } else { Some(parse_color(value)?) },
            "minimum-contrast" => {
                self.minimum_contrast = if empty { d.minimum_contrast } else { parse_f32(value)?.clamp(1.0, 21.0) }
            }
            "bell-features" => {
                if empty {
                    self.bell_features = d.bell_features;
                    return Ok(());
                }
                let f = &mut self.bell_features;
                for item in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let (on, name) = match item.strip_prefix("no-") {
                        Some(n) => (false, n),
                        None => (true, item),
                    };
                    match name {
                        "system" => f.system = on,
                        "attention" => f.attention = on,
                        "title" => f.title = on,
                        "audio" => f.audio = on,
                        "border" => f.border = on,
                        other => return Err(format!("unknown bell feature `{other}`")),
                    }
                }
            }
            "bell-audio-path" => self.bell_audio_path = (!empty).then(|| expand_home(value)),
            "bell-audio-volume" => {
                self.bell_audio_volume = if empty { d.bell_audio_volume } else { parse_f32(value)?.clamp(0.0, 1.0) }
            }
            "keybind" => {
                if empty || value == "clear" {
                    self.keybinds.clear();
                } else if let Some(rest) = value.strip_prefix("global:") {
                    // System-wide hotkeys exist for the Quick Terminal only.
                    match parse_keybind(rest)? {
                        (t, crate::keymap::Binding::Action(crate::keymap::Action::ToggleQuickTerminal)) => {
                            self.quick_terminal_hotkey = Some(t)
                        }
                        _ => return Err("only toggle_quick_terminal can be bound globally".into()),
                    }
                } else {
                    self.keybinds.push(parse_keybind(value)?);
                }
            }
            "window-padding-balance" => {
                self.window_padding_balance = if empty { d.window_padding_balance } else { parse_bool(value)? }
            }
            "auto-update" => {
                self.auto_update = if empty {
                    d.auto_update
                } else {
                    match value {
                        "check" => AutoUpdateMode::Check,
                        "download" => AutoUpdateMode::Download,
                        "off" | "false" => AutoUpdateMode::Off,
                        "true" => AutoUpdateMode::Check,
                        _ => return Err(format!("expected 'check', 'download', or 'off', got '{value}'")),
                    }
                };
            }
            "auto-update-channel" => {
                self.auto_update_channel = if empty {
                    d.auto_update_channel
                } else {
                    match value {
                        "stable" => AutoUpdateChannel::Stable,
                        "tip" => AutoUpdateChannel::Tip,
                        _ => return Err(format!("expected 'stable' or 'tip', got '{value}'")),
                    }
                };
            }
            "link-url"
            | "adjust-cursor-thickness"
            | "window-inherit-working-directory"
            | "alpha-blending" => {
                // Ghostty-compatible configuration keys accepted cleanly.
            }
            _ => return Err("unknown option".into()),
        }
        Ok(())
    }
}

/// `key = value` of one config line (quotes around the value dropped), `None`
/// for blank lines and comments, `Err(line)` when there is no `=`.
fn parse_line(raw: &str) -> Option<Result<(&str, &str), &str>> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let Some((key, value)) = line.split_once('=') else { return Some(Err(line)) };
    let mut value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value = &value[1..value.len() - 1];
    }
    Some(Ok((key.trim(), value)))
}

/// Themes from iTerm2-Color-Schemes, `[Name]` sections in Ghostty syntax.
const BUNDLED_THEMES: &str = include_str!("../assets/themes.txt");

/// Names of the bundled themes, in order.
pub fn bundled_theme_names() -> impl Iterator<Item = &'static str> {
    BUNDLED_THEMES.lines().filter_map(|l| l.strip_prefix('[')?.strip_suffix(']'))
}

/// Body of the bundled theme `name` (case-insensitive).
fn bundled_theme(name: &str) -> Option<&'static str> {
    let mut rest = BUNDLED_THEMES;
    while let Some(start) = rest.find("\n[") {
        rest = &rest[start + 2..];
        let (header, body) = rest.split_once('\n')?;
        let title = header.strip_suffix(']')?;
        if title.eq_ignore_ascii_case(name) {
            return Some(body.split("\n[").next().unwrap_or(body));
        }
    }
    None
}

impl Config {
    /// Apply theme `name`: a file in a themes directory, a bundled theme, or a
    /// built-in alias. Unsupported keys in a theme (e.g. `cursor-text`) are skipped.
    fn apply_theme(&mut self, name: &str) -> bool {
        let text = theme_file_text(name).map(std::borrow::Cow::Owned).or_else(|| bundled_theme(name).map(Into::into));
        if let Some(text) = text {
            for (key, value) in text.lines().filter_map(parse_line).filter_map(Result::ok) {
                let _ = self.set(key, value);
            }
            return true;
        }
        if let Some(t) = lookup_theme(name) {
            self.background = t.background;
            self.foreground = t.foreground;
            self.palette = t.palette;
            return true;
        }
        false
    }
}

pub struct ThemeDef {
    pub background: u32,
    pub foreground: u32,
    pub palette: [u32; 16],
}

/// Pick the variant of `theme = light:X,dark:Y` (either order, either part
/// optional) that matches the system appearance; a plain name is returned as is.
pub fn theme_variant(name: &str, dark: bool) -> &str {
    let mut light = None;
    let mut dark_name = None;
    for part in name.split(',').map(str::trim) {
        if let Some(l) = part.strip_prefix("light:") {
            light = Some(l.trim());
        } else if let Some(d) = part.strip_prefix("dark:") {
            dark_name = Some(d.trim());
        }
    }
    match (light, dark_name) {
        (None, None) => name.trim(),
        (l, d) if dark => d.or(l).unwrap_or_default(),
        (l, d) => l.or(d).unwrap_or_default(),
    }
}

/// Whether a theme value depends on the system appearance.
pub fn theme_is_conditional(name: &str) -> bool {
    name.contains("light:") || name.contains("dark:")
}

pub fn lookup_theme(name: &str) -> Option<ThemeDef> {
    let norm = name.trim().to_ascii_lowercase().replace(['-', '_', ' '], "");
    match norm.as_str() {
        "horizon" | "horizondark" => Some(ThemeDef {
            background: 0x1C1E_2600,
            foreground: 0xD5D8_DA00,
            palette: [
                0x000000, 0xE95678, 0x29D398, 0xFAB795,
                0x26BBD9, 0xEE64AC, 0x59E1E3, 0xE5E5E5,
                0x666666, 0xEC6A88, 0x3FDAA4, 0xFBC3A7,
                0x3FC4DE, 0xF075B5, 0x6BE4E6, 0xE5E5E5,
            ],
        }),
        "horizonbright" | "horizonlight" => Some(ThemeDef {
            background: 0xFDF0_ED00,
            foreground: 0x1616_1D00,
            palette: [
                0xFDF0ED, 0xE45649, 0x50A14F, 0xC18401,
                0x4078F2, 0xA626A4, 0x0184BC, 0x16161D,
                0x9D9D9F, 0xE45649, 0x50A14F, 0xC18401,
                0x4078F2, 0xA626A4, 0x0184BC, 0x383A42,
            ],
        }),
        "catppuccinmocha" | "catppuccin" => Some(ThemeDef {
            background: 0x1E1E_2E00,
            foreground: 0xCDD6_F400,
            palette: [
                0x45475A, 0xF38BA8, 0xA6E3A1, 0xF9E2AF,
                0x89B4FA, 0xF5C2E7, 0x94E2D5, 0xBAC2DE,
                0x585B70, 0xF38BA8, 0xA6E3A1, 0xF9E2AF,
                0x89B4FA, 0xF5C2E7, 0x94E2D5, 0xA6ADC8,
            ],
        }),
        "tokyonight" | "tokyonightstorm" => Some(ThemeDef {
            background: 0x1A1B_2600,
            foreground: 0xC0CA_F500,
            palette: [
                0x15161E, 0xF7768E, 0x9ECE6A, 0xE0AF68,
                0x7AA2F7, 0xBB9AF7, 0x7DCFFF, 0xA9B1D6,
                0x414868, 0xF7768E, 0x9ECE6A, 0xE0AF68,
                0x7AA2F7, 0xBB9AF7, 0x7DCFFF, 0xC0CAF5,
            ],
        }),
        "dracula" => Some(ThemeDef {
            background: 0x282A_3600,
            foreground: 0xF8F8_F200,
            palette: [
                0x21222C, 0xFF5555, 0x50FA7B, 0xF1FA8C,
                0xBD93F9, 0xFF79C6, 0x8BE9FD, 0xF8F8F2,
                0x6272A4, 0xFF6E6E, 0x69FF94, 0xFFFFA5,
                0xD6ACFF, 0xFF92DF, 0xA4FFFF, 0xFFFFFF,
            ],
        }),
        "nord" => Some(ThemeDef {
            background: 0x2E34_4000,
            foreground: 0xD8DE_E900,
            palette: [
                0x3B4252, 0xBF616A, 0xA3BE8C, 0xEBCB8B,
                0x81A1C1, 0xB48EAD, 0x88C0D0, 0xE5E9F0,
                0x4C566A, 0xBF616A, 0xA3BE8C, 0xEBCB8B,
                0x81A1C1, 0xB48EAD, 0x8FBCBB, 0xECEFF4,
            ],
        }),
        "gruvboxdark" | "gruvbox" => Some(ThemeDef {
            background: 0x2828_2800,
            foreground: 0xEBDB_B200,
            palette: [
                0x282828, 0xCC241D, 0x98971A, 0xD79921,
                0x458588, 0xB16286, 0x689D6A, 0xA89984,
                0x928374, 0xFB4934, 0xB8BB26, 0xFABD2F,
                0x83A598, 0xD3869B, 0x8EC07C, 0xEBDBB2,
            ],
        }),
        "solarizeddark" | "solarized" => Some(ThemeDef {
            background: 0x002B_3600,
            foreground: 0x8394_9600,
            palette: [
                0x073642, 0xDC322F, 0x859900, 0xB58900,
                0x268BD2, 0xD33682, 0x2AA198, 0xEEE8D5,
                0x002B36, 0xCB4B16, 0x586E75, 0x657B83,
                0x839496, 0x6C71C4, 0x93A1A1, 0xFDF6E3,
            ],
        }),
        _ => None,
    }
}

/// A theme file from the user's or Ghostty's themes directories.
fn theme_file_text(name: &str) -> Option<String> {
    // A theme name is a file name, never a path.
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return None;
    }
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty()).unwrap_or_else(|| "/".into());
    let candidates = [
        PathBuf::from(&home).join(".litty").join("themes").join(name),
        PathBuf::from(&home).join(".laplace").join("themes").join(name),
        PathBuf::from(&home).join(".config").join("ghostty").join("themes").join(name),
        PathBuf::from(&home).join("Applications/Ghostty.app/Contents/Resources/ghostty/themes").join(name),
        PathBuf::from("/Applications/Ghostty.app/Contents/Resources/ghostty/themes").join(name),
    ];
    candidates.iter().find_map(|p| std::fs::read_to_string(p).ok())
}

/// `trigger=action`. The trigger may itself end in `=` (`ctrl+==increase_font_size`),
/// so every `=` is tried as the separator.
fn parse_keybind(v: &str) -> Result<(crate::keymap::Trigger, crate::keymap::Binding), String> {
    let mut last_err = format!("expected `trigger=action`, got `{v}`");
    for (i, _) in v.match_indices('=') {
        let trigger = crate::keymap::parse_trigger(&v[..i]);
        let action = crate::keymap::parse_binding(&v[i + 1..]);
        match (trigger, action) {
            (Ok(t), Ok(a)) => return Ok((t, a)),
            (Err(e), _) | (_, Err(e)) => last_err = e,
        }
    }
    Err(last_err)
}

/// `window-padding-x = 8` or Ghostty's `left,right` form (`4,12`); Litty pads both
/// sides equally, so a pair is averaged.
fn parse_padding(v: &str) -> Result<f32, String> {
    let num = |s: &str| s.trim().parse::<f32>().map_err(|_| format!("invalid number `{s}`"));
    let pt = match v.split_once(',') {
        Some((a, b)) => (num(a)? + num(b)?) / 2.0,
        None => num(v)?,
    };
    Ok(pt.clamp(0.0, 200.0))
}

/// Expand a leading `~/` to `$HOME` (Ghostty's optional `?` prefix is dropped).
fn expand_home(v: &str) -> PathBuf {
    let v = v.strip_prefix('?').unwrap_or(v);
    match (v.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(v),
    }
}

fn parse_f32(v: &str) -> Result<f32, String> {
    v.parse::<f32>().ok().filter(|f| f.is_finite()).ok_or_else(|| format!("invalid number `{v}`"))
}

fn parse_bool(v: &str) -> Result<bool, String> {
    match v {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("expected true or false, got `{v}`")),
    }
}

fn parse_uint(v: &str) -> Result<u32, String> {
    v.parse::<u32>().map_err(|_| format!("invalid integer `{v}`"))
}

fn parse_multiplier(v: &str) -> Result<f64, String> {
    let n: f64 = v.parse().map_err(|_| format!("invalid number `{v}`"))?;
    if !(0.01..=10_000.0).contains(&n) {
        return Err(format!("out of range `{v}`"));
    }
    Ok(n)
}

/// Parse `#rrggbb`, `rrggbb`, `#rgb` or a few named colors into `0xRRGGBB00`.
pub fn parse_color(v: &str) -> Result<u32, String> {
    let named = match v.to_ascii_lowercase().as_str() {
        "black" => Some(0x000000),
        "white" => Some(0xFFFFFF),
        "red" => Some(0xFF0000),
        "green" => Some(0x00FF00),
        "blue" => Some(0x0000FF),
        _ => None,
    };
    if let Some(rgb) = named {
        return Ok(rgb << 8);
    }
    let hex = v.strip_prefix('#').unwrap_or(v);
    let rgb = match hex.len() {
        6 => u32::from_str_radix(hex, 16).map_err(|_| format!("invalid color `{v}`"))?,
        3 => {
            let n = u32::from_str_radix(hex, 16).map_err(|_| format!("invalid color `{v}`"))?;
            let (r, g, b) = ((n >> 8) & 0xF, (n >> 4) & 0xF, n & 0xF);
            (r * 17) << 16 | (g * 17) << 8 | (b * 17)
        }
        _ => return Err(format!("invalid color `{v}` (expected #rrggbb)")),
    };
    Ok(rgb << 8)
}

/// Create the config file from the template if it does not exist, then open
/// it in the system's default text editor (like Ghostty's `open_config`).
pub fn open_config_in_editor() {
    let path = config_path();
    if !path.exists() {
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("litty: cannot create {}: {e}", dir.display());
                return;
            }
        }
        let body = TEMPLATE.replace("{path}", &path.display().to_string());
        if let Err(e) = std::fs::write(&path, body) {
            eprintln!("litty: cannot create {}: {e}", path.display());
            return;
        }
    }

    #[cfg(target_os = "macos")]
    let res = crate::spawn_detached(std::process::Command::new("/usr/bin/open").arg("-t").arg(&path));
    #[cfg(not(target_os = "macos"))]
    let res = crate::spawn_detached(std::process::Command::new("xdg-open").arg(&path));
    if let Err(e) = res {
        eprintln!("litty: failed to open {}: {e}", path.display());
    }
}

const TEMPLATE: &str = r#"# This is the configuration file for Litty.
#
# This template file has been automatically created at the following
# path since Litty couldn't find any existing config file on your system:
#
#   {path}
#
# The template does not set any options, since Litty ships with sensible
# defaults for all of them. Only set the options you want to change: remove
# the leading `# ` from a line below and edit its value.
#
# Config syntax crash course
# ==========================
# # The config file consists of simple key-value pairs,
# # separated by equals signs.
# font-family = JetBrainsMono Nerd Font
# font-size = 14
#
# # Comments start with `#` and are only valid on their own line.
# # Blank lines are ignored!
#
# # An empty value resets the option to its default.
# background =
#
# Reloading: press Cmd+Shift+, (macOS) / Ctrl+Shift+, (Linux), or simply
# switch back to the terminal window after saving this file.

# ---------------------------------------------------------------------------
# Colors
# ---------------------------------------------------------------------------

# Default background and foreground color (#rrggbb).
# background = #121316
# foreground = #e6e6e6

# ---------------------------------------------------------------------------
# Font
# ---------------------------------------------------------------------------

# Font family name (or an absolute path to a .ttf/.otf/.ttc file).
# Repeat the key to add fallback fonts in priority order.
# font-family = JetBrainsMono Nerd Font
# font-family = Symbols Nerd Font Mono

# Font size in points. Cmd+0 / Ctrl+0 resets zoom to this size.
# font-size = 14

# ---------------------------------------------------------------------------
# Window
# ---------------------------------------------------------------------------

# Initial window size in terminal grid cells (columns x rows). Both must be
# set. Only applies to newly created windows (minimum 10 x 4).
# window-width = 120
# window-height = 36

# Opacity of the default background, from 0 (fully transparent) to 1.
# background-opacity = 1

# Blur the content behind a transparent window (macOS only). `true` uses a
# radius of 20; a number sets the radius. Requires background-opacity < 1.
# background-blur = false

# Color space of the window: `srgb` or `display-p3` (macOS only).
# window-colorspace = srgb

# ---------------------------------------------------------------------------
# Mouse
# ---------------------------------------------------------------------------

# Hide the mouse pointer while typing; it reappears when the mouse moves.
# mouse-hide-while-typing = false

# Scroll speed multiplier. A single number applies to all devices, or set
# them separately for trackpads (precision) and mouse wheels (discrete).
# mouse-scroll-multiplier = precision:1,discrete:3

# ---------------------------------------------------------------------------
# Cursor
# ---------------------------------------------------------------------------

# Cursor shape: block, bar, underline or block_hollow.
# Programs (and the shell integration `cursor` feature) may change it.
# cursor-style = block

# Blink the cursor. Leave unset to blink by default while still letting
# programs change it; `true` / `false` force blinking on / off.
# cursor-style-blink =

# ---------------------------------------------------------------------------
# Shell integration
# ---------------------------------------------------------------------------

# Automatically inject shell integration: detect, none, zsh or bash.
# shell-integration = detect

# Comma-separated features; prefix with `no-` to disable one.
#   cursor - blinking bar cursor at the prompt, default cursor while running
#   sudo   - preserve TERMINFO when running sudo
#   title  - set the tab/window title to the cwd or running command
# shell-integration-features = cursor,no-sudo,title
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_title_and_copy_options() {
        let (cfg, errors) = Config::parse(
            "cursor-color = #ff0000\nselection-background = #112233\nselection-foreground = #ffffff\ncopy-on-select = false\ntitle = Work\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cfg.cursor_color, Some(0xFF00_0000));
        assert_eq!((cfg.selection_background, cfg.selection_foreground), (Some(0x1122_3300), Some(0xFFFF_FF00)));
        assert!(!cfg.copy_on_select);
        assert_eq!(cfg.title.as_deref(), Some("Work"));
        assert!(Config::default().copy_on_select);
    }

    #[test]
    fn auto_update_options() {
        let (cfg, errors) = Config::parse("auto-update = download\nauto-update-channel = tip\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cfg.auto_update, AutoUpdateMode::Download);
        assert_eq!(cfg.auto_update_channel, AutoUpdateChannel::Tip);

        let (cfg, errors) = Config::parse("auto-update = off\nauto-update-channel = stable\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cfg.auto_update, AutoUpdateMode::Off);
        assert_eq!(cfg.auto_update_channel, AutoUpdateChannel::Stable);

        assert_eq!(Config::default().auto_update, AutoUpdateMode::Check);
        assert_eq!(Config::default().auto_update_channel, AutoUpdateChannel::Stable);
    }

    #[test]
    fn scrollback_split_and_contrast_options() {
        let (cfg, errors) = Config::parse(
            "scrollback-limit = 10000000\nunfocused-split-opacity = 0.05\nunfocused-split-fill = #000000\nminimum-contrast = 3\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cfg.scrollback_limit, Some(10_000_000));
        assert_eq!(cfg.unfocused_split_opacity, 0.15, "clamped to Ghostty's minimum");
        assert_eq!(cfg.unfocused_split_fill, Some(0));
        assert_eq!(cfg.minimum_contrast, 3.0);
        assert_eq!(Config::default().confirm_close_surface, ConfirmClose::Running);
        assert!(Config::parse("macos-native-tabs = true\n").0.macos_native_tabs);
        let (cfg, errors) = Config::parse("custom-shader = ~/a.glsl\ncustom-shader = /b.glsl\ncustom-shader-animation = always\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cfg.custom_shaders.len(), 2);
        assert_eq!(cfg.custom_shader_animation, Some(true));
        assert!(Config::parse("custom-shader = /a\ncustom-shader =\n").0.custom_shaders.is_empty());
        let (cfg, errors) = Config::parse("keybind = global:cmd+grave_accent=toggle_quick_terminal\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(cfg.quick_terminal_hotkey.is_some() && cfg.keybinds.is_empty());
        assert_eq!(Config::parse("keybind = global:cmd+t=new_tab\n").1.len(), 1);
        assert_eq!(Config::parse("confirm-close-surface = always\n").0.confirm_close_surface, ConfirmClose::Always);
        assert_eq!(Config::parse("confirm-close-surface = false\n").0.confirm_close_surface, ConfirmClose::Never);
        let (cfg, errors) = Config::parse("adjust-cell-height = 20%\nadjust-cell-width = -1\nfont-thicken = true\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cfg.adjust_cell_height, crate::atlas::MetricAdjust::Percent(20.0));
        assert_eq!(cfg.adjust_cell_width, crate::atlas::MetricAdjust::Px(-1));
        assert!(cfg.font_thicken);
        let (cfg, errors) = Config::parse("font-feature = -calt\nfont-feature = ss01, liga=0\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cfg.font_features, ["-calt", "ss01", "liga=0"]);
        assert_eq!(Config::parse("font-feature = calt=abc\n").1.len(), 1);
        assert_eq!(Config::parse("minimum-contrast = nan\n").1.len(), 1);
        let d = Config::default();
        assert_eq!((d.scrollback_limit, d.unfocused_split_opacity, d.minimum_contrast), (None, 0.7, 1.0));
    }

    #[test]
    fn bell_features_option() {
        let (cfg, errors) = Config::parse("bell-features = system,no-title,border\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            cfg.bell_features,
            BellFeatures { system: true, attention: true, title: false, audio: false, border: true }
        );
        let (cfg, errors) = Config::parse("bell-features = audio\nbell-audio-path = ~/ding.wav\nbell-audio-volume = 2\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(cfg.bell_features.audio);
        assert!(cfg.bell_audio_path.is_some_and(|p| p.ends_with("ding.wav") && !p.starts_with("~")));
        assert_eq!(cfg.bell_audio_volume, 1.0);
        assert_eq!(Config::parse("bell-features = loud\n").1.len(), 1);
    }

    #[test]
    fn keybind_entries() {
        use crate::keymap::{Action, Binding, TriggerKey};
        let (cfg, errors) = Config::parse(
            "keybind = cmd+t=new_split:right\nkeybind = ctrl+==increase_font_size\nkeybind = cmd+k=text:\\x0c\nkeybind = nope=new_tab\n",
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(cfg.keybinds.len(), 3);
        assert_eq!(cfg.keybinds[0].1, Binding::Action(Action::SplitRight));
        assert_eq!(cfg.keybinds[1].0.key, TriggerKey::Char('='));
        assert_eq!(cfg.keybinds[2].1, Binding::Text(vec![0x0c]));
        let (cfg, _) = Config::parse("keybind = cmd+t=new_tab\nkeybind = clear\n");
        assert!(cfg.keybinds.is_empty());
    }

    #[test]
    fn window_padding_options() {
        let (cfg, errors) = Config::parse("window-padding-x = 10\nwindow-padding-y = 2,6\nwindow-padding-balance = true\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!((cfg.window_padding_x, cfg.window_padding_y, cfg.window_padding_balance), (10.0, 4.0, true));
        let (_, errors) = Config::parse("window-padding-x = wide\n");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn themes_apply_before_the_rest_of_the_file() {
        // The user's colors win over the theme's, even when set before `theme`.
        let (cfg, errors) = Config::parse("background = #101010\ntheme = Dracula\nselection-background = #010203\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cfg.background, 0x1010_1000);
        assert_eq!(cfg.foreground, 0xF8F8_F200, "from the theme");
        assert_eq!(cfg.palette[1], 0xFF5555);
        assert_eq!(cfg.cursor_color, Some(0xF8F8_F200), "theme cursor color");
        assert_eq!(cfg.selection_background, Some(0x0102_0300));

        // Bundled names are case-insensitive; built-in aliases still work.
        let (cfg, errors) = Config::parse("theme = catppuccin mocha\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(cfg.background, 0x1E1E_2E00);
        assert!(Config::parse("theme = tokyonight\n").1.is_empty());
        assert_eq!(Config::parse("theme = No Such Theme\n").1.len(), 1);
        assert_eq!(Config::parse("theme = ../../etc/passwd\n").1.len(), 1);
        assert!(bundled_theme_names().count() > 800);
        assert_eq!(bundled_theme_names().next(), Some("0x96f"));
    }

    #[test]
    fn theme_variants_follow_the_appearance() {
        let t = "light:Horizon Bright,dark:Dracula";
        assert_eq!(theme_variant(t, true), "Dracula");
        assert_eq!(theme_variant(t, false), "Horizon Bright");
        assert_eq!(theme_variant("dark:Nord, light:Solarized", false), "Solarized");
        assert_eq!(theme_variant("dark:Nord", false), "Nord", "a missing side falls back to the other");
        assert_eq!(theme_variant("Dracula", false), "Dracula");
        assert!(theme_is_conditional(t) && !theme_is_conditional("Dracula"));
    }

    #[test]
    fn parses_ghostty_style_config() {
        let text = r#"
# comment
background = #1e1e2e
foreground = "cdd6f4"
font-family = JetBrainsMono Nerd Font
font-family = Symbols Nerd Font Mono
font-size = 15.5
window-width = 120
window-height = 30
background-opacity = 0.85
background-blur = true
window-colorspace = display-p3
mouse-hide-while-typing = true
mouse-scroll-multiplier = precision:0.5,discrete:2
cursor-style = bar
cursor-style-blink = false
shell-integration-features = no-cursor,sudo
"#;
        let (c, errs) = Config::parse(text);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(c.background, 0x1E1E_2E00);
        assert_eq!(c.foreground, 0xCDD6_F400);
        assert_eq!(c.font_family, vec!["JetBrainsMono Nerd Font", "Symbols Nerd Font Mono"]);
        assert_eq!(c.font_size, 15.5);
        assert_eq!((c.window_width, c.window_height), (Some(120), Some(30)));
        assert_eq!(c.background_opacity, 0.85);
        assert_eq!(c.background_blur, 20);
        assert_eq!(c.window_colorspace, ColorSpace::DisplayP3);
        assert!(c.mouse_hide_while_typing);
        assert_eq!((c.mouse_scroll_precision, c.mouse_scroll_discrete), (0.5, 2.0));
        assert_eq!(c.cursor_style, CursorStyle::Bar);
        assert_eq!(c.cursor_style_blink, Some(false));
        assert_eq!(c.shell_integration_features, ShellFeatures { cursor: false, sudo: true, title: true });
    }

    #[test]
    fn reports_errors_and_resets() {
        let (c, errs) = Config::parse("bogus = 1\nfont-size = abc\nbackground = #fff\nbackground =\nwindow-width = 80\n");
        assert_eq!(errs.len(), 3, "{errs:?}");
        assert_eq!(c.background, Config::default().background);
        assert_eq!(c.window_width, None);
        assert_eq!(parse_color("#fff").unwrap(), 0xFFFF_FF00);
    }

    #[test]
    fn template_is_valid_config() {
        let (c, errs) = Config::parse(TEMPLATE);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn cursor_styles_and_single_multiplier() {
        let text = r#"
cursor-style = underline
cursor-style-blink = true
mouse-scroll-multiplier = 2.5
window-colorspace = srgb
background-blur = 30
shell-integration-features = false
"#;
        let (c, errs) = Config::parse(text);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(c.cursor_style, CursorStyle::Underline);
        assert_eq!(c.cursor_style_blink, Some(true));
        assert_eq!(c.mouse_scroll_precision, 2.5);
        assert_eq!(c.mouse_scroll_discrete, 2.5 * 3.0);
        assert_eq!(c.window_colorspace, ColorSpace::Srgb);
        assert_eq!(c.background_blur, 30);
        assert_eq!(
            c.shell_integration_features,
            ShellFeatures { cursor: false, sudo: false, title: false }
        );
    }

    #[test]
    fn config_path_ends_with_litty_config() {
        let p = config_path();
        assert!(p.ends_with(".litty/config") || p.ends_with(".litty\\config"));
    }

    #[test]
    fn parses_ghostty_horizon_and_palette() {
        let text = r#"
theme = Horizon
background = 000000
foreground = 00ff00
link-url = true
window-width = 151
window-height = 37
cursor-style-blink = false
font-family = SFMono Nerd Font
font-size = 15
shell-integration-features = no-cursor,sudo,no-title
cursor-style = block
background-opacity = 1
background-blur = false
mouse-hide-while-typing = true
mouse-scroll-multiplier = 2
window-colorspace = "display-p3"
copy-on-select = clipboard
"#;
        let (c, errs) = Config::parse(text);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(c.background, 0x0000_0000, "background must be absolutely black");
        assert_eq!(c.foreground, 0x00FF_0000, "foreground must be bright green");
        assert_eq!(c.theme, Some("Horizon".into()));
        assert_eq!(c.palette[0], 0x000000);
        assert_eq!(c.palette[1], 0xE95678);
        assert_eq!(c.window_colorspace, ColorSpace::DisplayP3);
        assert_eq!(c.font_family, vec!["SFMono Nerd Font"]);
        assert_eq!(c.font_size, 15.0);
    }
}
