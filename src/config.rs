//! Ghostty-style configuration: a plain `key = value` text file at
//! `~/.laplace/config`.
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellFeatures {
    pub cursor: bool,
    pub sudo: bool,
    pub title: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Default background color, packed `0xRRGGBB00`.
    pub background: u32,
    /// Default foreground color, packed `0xRRGGBB00`.
    pub foreground: u32,
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
}

pub const DEFAULT_FONT_PT: f32 = 14.0;

impl Default for Config {
    fn default() -> Self {
        let font_size = std::env::var("LAPLACE_FONT_SIZE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(DEFAULT_FONT_PT)
            .clamp(6.0, 72.0);
        Config {
            background: 0x1213_1600,
            foreground: 0xE6E6_E600,
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
        }
    }
}

/// Path to the config file: `~/.laplace/config` (it may not exist yet).
pub fn config_path() -> PathBuf {
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty()).unwrap_or_else(|| "/".into());
    PathBuf::from(home).join(".laplace").join("config")
}

pub fn config_mtime() -> Option<SystemTime> {
    std::fs::metadata(config_path()).and_then(|m| m.modified()).ok()
}

impl Config {
    /// Load the config file. A missing file yields defaults; malformed lines
    /// are reported on stderr and skipped (like Ghostty, a bad line never
    /// prevents the terminal from starting).
    pub fn load() -> Config {
        let path = config_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
            Err(e) => {
                eprintln!("laplace: cannot read config {}: {e}", path.display());
                return Config::default();
            }
        };
        let (cfg, errors) = Config::parse(&text);
        for err in errors {
            eprintln!("laplace: config {}: {err}", path.display());
        }
        cfg
    }

    /// Parse config text, returning the config and a list of human-readable errors.
    pub fn parse(text: &str) -> (Config, Vec<String>) {
        let mut cfg = Config::default();
        let mut errors = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                errors.push(format!("line {}: expected `key = value`, got `{line}`", i + 1));
                continue;
            };
            let key = key.trim();
            let mut value = value.trim();
            if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                value = &value[1..value.len() - 1];
            }
            if let Err(e) = cfg.set(key, value) {
                errors.push(format!("line {}: {key}: {e}", i + 1));
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
                    v => return Err(format!("expected detect, none, zsh or bash, got `{v}`")),
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
            _ => return Err("unknown option".into()),
        }
        Ok(())
    }
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
                eprintln!("laplace: cannot create {}: {e}", dir.display());
                return;
            }
        }
        let body = TEMPLATE.replace("{path}", &path.display().to_string());
        if let Err(e) = std::fs::write(&path, body) {
            eprintln!("laplace: cannot create {}: {e}", path.display());
            return;
        }
    }

    #[cfg(target_os = "macos")]
    let res = std::process::Command::new("/usr/bin/open").arg("-t").arg(&path).spawn();
    #[cfg(not(target_os = "macos"))]
    let res = std::process::Command::new("xdg-open").arg(&path).spawn();
    if let Err(e) = res {
        eprintln!("laplace: failed to open {}: {e}", path.display());
    }
}

const TEMPLATE: &str = r#"# This is the configuration file for Laplace.
#
# This template file has been automatically created at the following
# path since Laplace couldn't find any existing config file on your system:
#
#   {path}
#
# The template does not set any options, since Laplace ships with sensible
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

# Font size in points. Cmd+0 / Ctrl+Shift+0 resets zoom to this size.
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
    fn config_path_ends_with_laplace_config() {
        let p = config_path();
        assert!(p.ends_with(".laplace/config") || p.ends_with(".laplace\\config"));
    }
}
