//! Keyboard shortcuts and key-to-byte encoding, as pure functions of the key,
//! the modifiers and the target platform. Both platform tables are always
//! compiled so the macOS/Linux shortcut matrix in CLAUDE.md is unit-tested on
//! any host.

use winit::keyboard::{Key, KeyCode, KeyLocation, NamedKey};

use crate::split::Direction;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
}

/// The platform this binary was built for.
pub const PLATFORM: Platform = if cfg!(target_os = "macos") { Platform::MacOs } else { Platform::Linux };

/// Modifier keys held with a key press (`sup` = Cmd on macOS, Super on Linux).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub sup: bool,
}

impl Mods {
    /// Exactly the given modifiers are held (`ctrl`, `alt`, `shift`, `sup`).
    fn only(self, ctrl: bool, alt: bool, shift: bool, sup: bool) -> bool {
        self == Mods { ctrl, alt, shift, sup }
    }
}

/// An application shortcut. Some actions only apply in a given state (e.g. more
/// than one tab); the caller reports whether it handled the action, and unhandled
/// keys fall through to the shell.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Action {
    ClearScreen,
    SelectAll,
    Find,
    HistorySearch,
    Quit,
    Minimize,
    OpenConfig,
    ReloadConfig,
    NewTab,
    NewWindow,
    CloseTab,
    SplitRight,
    SplitDown,
    EqualizeSplits,
    ToggleSplitZoom,
    Copy,
    Paste,
    PrevTab,
    NextTab,
    /// Select tab `0..=8`; index 8 means the last tab.
    SelectTab(usize),
    PrevSplit,
    NextSplit,
    /// Move focus to the split in a direction (cycling tabs at the left/right edge).
    Navigate(Direction),
    ResizeSplit(f32),
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ScrollPageUp,
    ScrollPageDown,
    /// Scroll to the previous (-1) or next (+1) shell prompt (OSC 133).
    JumpToPrompt(isize),
    /// Select (and copy) the output of the last command (OSC 133).
    SelectLastOutput,
    /// macOS Secure Keyboard Entry on / off.
    ToggleSecureInput,
    /// Terminal inspector overlay on / off.
    ToggleInspector,
    /// Check for updates (Ghostty `check_for_updates`).
    CheckUpdate,
    /// Show / hide the Quick Terminal (bound globally, `keybind = global:...`).
    ToggleQuickTerminal,
}

/// Resolve a key press to a shortcut. `alt_screen` is true while a full-screen
/// application runs (so plain `Ctrl+R` reaches vim as redo).
pub fn resolve(platform: Platform, key: &Key, m: Mods, alt_screen: bool) -> Option<Action> {
    match key {
        Key::Character(ch) => resolve_char(platform, &ch.to_lowercase(), m, alt_screen),
        Key::Named(named) => resolve_named(platform, *named, m),
        _ => None,
    }
}

fn resolve_char(platform: Platform, ch: &str, m: Mods, alt_screen: bool) -> Option<Action> {
    let mac = platform == Platform::MacOs;
    // Terminal actions: Cmd+<key> on macOS; Ctrl+Shift+<key> or Super+<key> on Linux,
    // because bare Ctrl+<letter> belongs to the shell.
    let terminal_mod = if mac {
        m.sup && !m.ctrl && !m.alt
    } else {
        m.only(true, false, true, false) || m.only(false, false, false, true)
    };
    // UI actions on Linux may use bare Ctrl (no shell collision for `,` `=` `-` `0`).
    let ui_mod = if mac {
        m.sup && !m.ctrl && !m.alt
    } else {
        (m.ctrl && !m.alt && !m.sup) || (m.sup && !m.ctrl && !m.alt)
    };

    let action = match ch {
        "k" if terminal_mod => Action::ClearScreen,
        // Bare Ctrl+O is operate-and-get-next in bash.
        "o" if mac && m.only(false, false, true, true) => Action::SelectLastOutput,
        "o" if !mac && (m.only(true, false, true, false) || m.only(false, false, true, true)) => Action::SelectLastOutput,
        // Bare Ctrl+A is readline beginning-of-line, so Linux uses Ctrl+Shift+A / Super+A.
        "a" if terminal_mod => Action::SelectAll,
        "i" if mac && m.only(false, true, false, true) => Action::ToggleInspector,
        "i" if !mac && (m.only(true, false, true, false) || m.only(false, true, false, true)) => Action::ToggleInspector,
        // Bare Ctrl+F is readline forward-char, so Linux uses Ctrl+Shift+F / Super+F.
        "f" if terminal_mod => Action::Find,
        "r" if (m.only(true, false, false, false) && !alt_screen)
            || m.only(false, false, false, true)
            || (!mac && m.only(true, false, true, false)) =>
        {
            Action::HistorySearch
        }
        "q" if terminal_mod => Action::Quit,
        "m" if terminal_mod || (!mac && m.sup && !m.ctrl && !m.alt) => Action::Minimize,
        "," | "<" if ui_mod && (ch == "," || m.shift) => {
            if m.shift {
                Action::ReloadConfig
            } else {
                Action::OpenConfig
            }
        }
        "t" if terminal_mod => Action::NewTab,
        "n" if terminal_mod => Action::NewWindow,
        "d" if mac && m.only(false, false, false, true) => Action::SplitRight,
        "d" if mac && m.only(false, false, true, true) => Action::SplitDown,
        "d" if !mac && terminal_mod => Action::SplitRight,
        "d" if !mac && (m.only(false, false, true, true) || m.only(true, true, false, false)) => Action::SplitDown,
        "e" if !mac && m.only(true, false, true, false) => Action::SplitDown,
        "=" | "+" if (mac && m.sup && (m.alt ^ m.ctrl)) || (!mac && m.alt && (m.ctrl ^ m.sup)) => Action::EqualizeSplits,
        "c" if terminal_mod => Action::Copy,
        "v" if terminal_mod => Action::Paste,
        "w" if terminal_mod => Action::CloseTab,
        "[" | "{" if m.shift && !m.alt && (if mac { m.sup && !m.ctrl } else { m.ctrl || m.sup }) => Action::PrevTab,
        "]" | "}" if m.shift && !m.alt && (if mac { m.sup && !m.ctrl } else { m.ctrl || m.sup }) => Action::NextTab,
        "[" if split_cycle_mod(mac, m) => Action::PrevSplit,
        "]" if split_cycle_mod(mac, m) => Action::NextSplit,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" if tab_number_mod(mac, m) => {
            Action::SelectTab((ch.as_bytes()[0] - b'1') as usize)
        }
        "=" | "+" if ui_mod => Action::ZoomIn,
        "-" | "_" if ui_mod => Action::ZoomOut,
        "0" | ")" if ui_mod => Action::ZoomReset,
        _ => return None,
    };
    Some(action)
}

fn split_cycle_mod(mac: bool, m: Mods) -> bool {
    if mac {
        m.only(false, false, false, true)
    } else {
        !m.shift && ((m.ctrl && m.alt) || m.sup)
    }
}

fn tab_number_mod(mac: bool, m: Mods) -> bool {
    if mac {
        m.only(false, false, false, true)
    } else {
        m.only(true, false, false, false) || m.only(false, true, false, false) || m.only(false, false, false, true)
    }
}

fn resolve_named(platform: Platform, key: NamedKey, m: Mods) -> Option<Action> {
    let mac = platform == Platform::MacOs;
    let arrow = match key {
        NamedKey::ArrowLeft => Some(Direction::Left),
        NamedKey::ArrowRight => Some(Direction::Right),
        NamedKey::ArrowUp => Some(Direction::Up),
        NamedKey::ArrowDown => Some(Direction::Down),
        _ => None,
    };
    if let Some(dir) = arrow {
        let resize = m.only(true, false, false, true) || (!mac && m.only(true, true, true, false));
        if resize {
            let step = if matches!(dir, Direction::Left | Direction::Up) { -0.05 } else { 0.05 };
            return Some(Action::ResizeSplit(step));
        }
        let navigate = if mac { m.sup && !m.ctrl && !m.shift } else { !m.shift && ((m.ctrl && m.alt) || (m.sup && !m.ctrl)) };
        if navigate {
            return Some(Action::Navigate(dir));
        }
        let jump = if mac { m.only(false, false, true, true) } else { m.only(true, false, true, false) || m.only(false, false, true, true) };
        if jump && matches!(dir, Direction::Up | Direction::Down) {
            return Some(Action::JumpToPrompt(if dir == Direction::Up { -1 } else { 1 }));
        }
        return None;
    }
    match key {
        NamedKey::Enter => {
            let zoom = m.only(false, false, true, true) || (!mac && m.only(true, false, true, false));
            zoom.then_some(Action::ToggleSplitZoom)
        }
        NamedKey::Tab if m.ctrl && !m.alt && !m.sup => Some(if m.shift { Action::PrevTab } else { Action::NextTab }),
        NamedKey::PageUp if m.ctrl && !m.shift && !m.alt => Some(Action::PrevTab),
        NamedKey::PageDown if m.ctrl && !m.shift && !m.alt => Some(Action::NextTab),
        NamedKey::PageUp if m.shift && !m.sup => Some(Action::ScrollPageUp),
        NamedKey::PageDown if m.shift && !m.sup => Some(Action::ScrollPageDown),
        _ => None,
    }
}

// ----------------------------------------------------------------------
// User key bindings (`keybind = trigger=action`, Ghostty syntax)
// ----------------------------------------------------------------------

/// The key part of a binding trigger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TriggerKey {
    Char(char),
    Named(NamedKey),
}

/// `cmd+shift+t`: exact modifiers plus a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub mods: Mods,
    pub key: TriggerKey,
}

/// What a user binding does.
#[derive(Clone, Debug, PartialEq)]
pub enum Binding {
    Action(Action),
    /// Bytes written to the shell (`text:`, `csi:`, `esc:`).
    Text(Vec<u8>),
    /// Remove a built-in shortcut so the key reaches the shell.
    Unbind,
    /// Swallow the key.
    Ignore,
}

/// Parse a trigger such as `cmd+shift+t`, `ctrl+alt+left` or `super+comma`.
/// macOS virtual key code (`kVK_*`) of a trigger key, for global hotkeys.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_virtual_keycode(key: &TriggerKey) -> Option<u32> {
    use NamedKey as N;
    Some(match key {
        TriggerKey::Char('`') => 0x32,
        TriggerKey::Char(c) => {
            // Indexed by key code; `\0` marks codes without a character.
            const KEYS: &str = "asdfhgzxcv\0bqweryt123465=97-80]ou[ip\0lj'k;\\,/nm.";
            let c = c.to_ascii_lowercase();
            KEYS.chars().position(|k| k == c && k != '\0')? as u32
        }
        TriggerKey::Named(n) => match n {
            N::Enter => 0x24,
            N::Tab => 0x30,
            N::Space => 0x31,
            N::Backspace => 0x33,
            N::Escape => 0x35,
            N::Delete => 0x75,
            N::Home => 0x73,
            N::End => 0x77,
            N::PageUp => 0x74,
            N::PageDown => 0x79,
            N::ArrowLeft => 0x7B,
            N::ArrowRight => 0x7C,
            N::ArrowDown => 0x7D,
            N::ArrowUp => 0x7E,
            N::F1 => 0x7A,
            N::F2 => 0x78,
            N::F3 => 0x63,
            N::F4 => 0x76,
            N::F5 => 0x60,
            N::F6 => 0x61,
            N::F7 => 0x62,
            N::F8 => 0x64,
            N::F9 => 0x65,
            N::F10 => 0x6D,
            N::F11 => 0x67,
            N::F12 => 0x6F,
            _ => return None,
        },
    })
}

/// X11 keysym of a trigger key, for global hotkeys on Linux.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn x11_keysym(key: &TriggerKey) -> Option<u32> {
    use NamedKey as N;
    Some(match key {
        // Latin-1 keysyms are the characters themselves (lowercase for letters).
        TriggerKey::Char(c) if (' '..='~').contains(c) => c.to_ascii_lowercase() as u32,
        TriggerKey::Char(_) => return None,
        TriggerKey::Named(n) => match n {
            N::Space => 0x20,
            N::Backspace => 0xFF08,
            N::Tab => 0xFF09,
            N::Enter => 0xFF0D,
            N::Escape => 0xFF1B,
            N::Home => 0xFF50,
            N::ArrowLeft => 0xFF51,
            N::ArrowUp => 0xFF52,
            N::ArrowRight => 0xFF53,
            N::ArrowDown => 0xFF54,
            N::PageUp => 0xFF55,
            N::PageDown => 0xFF56,
            N::End => 0xFF57,
            N::Insert => 0xFF63,
            N::Delete => 0xFFFF,
            N::F1 => 0xFFBE,
            N::F2 => 0xFFBF,
            N::F3 => 0xFFC0,
            N::F4 => 0xFFC1,
            N::F5 => 0xFFC2,
            N::F6 => 0xFFC3,
            N::F7 => 0xFFC4,
            N::F8 => 0xFFC5,
            N::F9 => 0xFFC6,
            N::F10 => 0xFFC7,
            N::F11 => 0xFFC8,
            N::F12 => 0xFFC9,
            _ => return None,
        },
    })
}

pub fn parse_trigger(s: &str) -> Result<Trigger, String> {
    let mut mods = Mods::default();
    let mut key = None;
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    // A trailing `+` means the plus key itself (`ctrl++`).
    let (parts, plus) = match parts.as_slice() {
        [rest @ .., "", ""] => (rest.to_vec(), true),
        _ => (parts, false),
    };
    for (i, part) in parts.iter().enumerate() {
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" | "opt" | "option" => mods.alt = true,
            "super" | "cmd" | "command" => mods.sup = true,
            _ if i + 1 == parts.len() && !plus => key = Some(parse_trigger_key(&lower)?),
            other => return Err(format!("unknown modifier `{other}`")),
        }
    }
    let key = if plus { Some(TriggerKey::Char('+')) } else { key };
    Ok(Trigger { mods, key: key.ok_or_else(|| format!("no key in `{s}`"))? })
}

fn parse_trigger_key(k: &str) -> Result<TriggerKey, String> {
    use NamedKey as N;
    let named = match k {
        "enter" | "return" => N::Enter,
        "tab" => N::Tab,
        "escape" | "esc" => N::Escape,
        "space" => N::Space,
        "backspace" => N::Backspace,
        "delete" => N::Delete,
        "insert" => N::Insert,
        "up" | "arrow_up" => N::ArrowUp,
        "down" | "arrow_down" => N::ArrowDown,
        "left" | "arrow_left" => N::ArrowLeft,
        "right" | "arrow_right" => N::ArrowRight,
        "home" => N::Home,
        "end" => N::End,
        "page_up" => N::PageUp,
        "page_down" => N::PageDown,
        "f1" => N::F1,
        "f2" => N::F2,
        "f3" => N::F3,
        "f4" => N::F4,
        "f5" => N::F5,
        "f6" => N::F6,
        "f7" => N::F7,
        "f8" => N::F8,
        "f9" => N::F9,
        "f10" => N::F10,
        "f11" => N::F11,
        "f12" => N::F12,
        _ => {
            let c = match k {
                "comma" => ',',
                "period" => '.',
                "slash" => '/',
                "semicolon" => ';',
                "quote" | "apostrophe" => '\'',
                "backquote" | "grave" | "grave_accent" => '`',
                "minus" => '-',
                "equal" => '=',
                "plus" => '+',
                "bracket_left" | "left_bracket" => '[',
                "bracket_right" | "right_bracket" => ']',
                "backslash" => '\\',
                _ => single_char(k).ok_or_else(|| format!("unknown key `{k}`"))?,
            };
            return Ok(TriggerKey::Char(c));
        }
    };
    Ok(TriggerKey::Named(named))
}

/// Parse a binding action: Ghostty action names (`new_tab`, `goto_tab:3`,
/// `text:\x1b[A`, `unbind`, ...).
pub fn parse_binding(s: &str) -> Result<Binding, String> {
    let (name, arg) = match s.split_once(':') {
        Some((n, a)) => (n.trim(), Some(a)),
        None => (s.trim(), None),
    };
    let num = |a: Option<&str>| a.and_then(|a| a.trim().parse::<i32>().ok());
    let action = match name {
        "unbind" => return Ok(Binding::Unbind),
        "ignore" => return Ok(Binding::Ignore),
        "text" => return Ok(Binding::Text(unescape(arg.unwrap_or_default())?)),
        "csi" => return Ok(Binding::Text([b"\x1b[".as_slice(), arg.unwrap_or_default().as_bytes()].concat())),
        "esc" => return Ok(Binding::Text([b"\x1b".as_slice(), arg.unwrap_or_default().as_bytes()].concat())),
        "new_tab" => Action::NewTab,
        "new_window" => Action::NewWindow,
        "inspector" => Action::ToggleInspector,
        "toggle_quick_terminal" => Action::ToggleQuickTerminal,
        "close_surface" | "close_tab" => Action::CloseTab,
        "quit" => Action::Quit,
        "copy_to_clipboard" => Action::Copy,
        "paste_from_clipboard" => Action::Paste,
        "select_all" => Action::SelectAll,
        "clear_screen" => Action::ClearScreen,
        "start_search" | "search" => Action::Find,
        "history_search" => Action::HistorySearch,
        "open_config" => Action::OpenConfig,
        "reload_config" => Action::ReloadConfig,
        "toggle_minimize" | "minimize" => Action::Minimize,
        "previous_tab" => Action::PrevTab,
        "next_tab" => Action::NextTab,
        "last_tab" => Action::SelectTab(8),
        "goto_tab" => match num(arg) {
            Some(n @ 1..=9) => Action::SelectTab(n as usize - 1),
            _ => return Err(format!("goto_tab needs 1..9, got `{}`", arg.unwrap_or_default())),
        },
        "new_split" => match arg.map(str::trim) {
            Some("right") | None => Action::SplitRight,
            Some("down") => Action::SplitDown,
            Some(d) => return Err(format!("unsupported split direction `{d}`")),
        },
        "goto_split" => match arg.map(str::trim) {
            Some("previous") => Action::PrevSplit,
            Some("next") => Action::NextSplit,
            Some("left") => Action::Navigate(Direction::Left),
            Some("right") => Action::Navigate(Direction::Right),
            Some("up" | "top") => Action::Navigate(Direction::Up),
            Some("down" | "bottom") => Action::Navigate(Direction::Down),
            d => return Err(format!("unknown split target `{}`", d.unwrap_or_default())),
        },
        "resize_split" => match arg.and_then(|a| a.split(',').next()).map(str::trim) {
            Some("left" | "up") => Action::ResizeSplit(-0.05),
            Some("right" | "down") => Action::ResizeSplit(0.05),
            d => return Err(format!("unknown resize direction `{}`", d.unwrap_or_default())),
        },
        "equalize_splits" => Action::EqualizeSplits,
        "toggle_split_zoom" => Action::ToggleSplitZoom,
        "increase_font_size" => Action::ZoomIn,
        "decrease_font_size" => Action::ZoomOut,
        "reset_font_size" => Action::ZoomReset,
        "scroll_page_up" => Action::ScrollPageUp,
        "scroll_page_down" => Action::ScrollPageDown,
        "jump_to_prompt" => Action::JumpToPrompt(if num(arg).unwrap_or(-1) < 0 { -1 } else { 1 }),
        "select_last_output" | "select_command_output" => Action::SelectLastOutput,
        "toggle_secure_input" => Action::ToggleSecureInput,
        "check_for_updates" | "check_update" => Action::CheckUpdate,
        other => return Err(format!("unknown action `{other}`")),
    };
    Ok(Binding::Action(action))
}

/// Zig-style escapes in `text:` bindings: `\x1b`, `\n`, `\r`, `\t`, `\e`, `\\`, `\u{..}`.
fn unescape(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('e') => out.push(0x1b),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                out.push(u8::from_str_radix(&hex, 16).map_err(|_| format!("bad escape `\\x{hex}`"))?);
            }
            Some('u') => {
                let rest: String = chars.by_ref().take_while(|&c| c != '}').collect();
                let cp = rest.strip_prefix('{').and_then(|h| u32::from_str_radix(h, 16).ok()).and_then(char::from_u32);
                let c = cp.ok_or_else(|| format!("bad escape `\\u{rest}}}`"))?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            other => return Err(format!("unknown escape `\\{}`", other.unwrap_or(' '))),
        }
    }
    Ok(out)
}

/// Whether a key event matches a trigger. Character keys compare the
/// unshifted key, so `cmd+shift+[` matches whatever `[` produces with Shift.
pub fn trigger_matches(t: &Trigger, unshifted: &Key, m: Mods) -> bool {
    if t.mods != m {
        return false;
    }
    match (&t.key, unshifted) {
        (TriggerKey::Named(n), Key::Named(k)) => n == k,
        (TriggerKey::Char(c), Key::Character(s)) => single_char(s).is_some_and(|k| k.to_lowercase().eq(c.to_lowercase())),
        (TriggerKey::Char(' '), Key::Named(NamedKey::Space)) => true,
        _ => false,
    }
}

/// Shift, Ctrl, Alt/Option or Cmd/Super pressed on its own.
pub fn is_modifier_key(key: &Key) -> bool {
    matches!(key, Key::Named(NamedKey::Shift | NamedKey::Control | NamedKey::Alt | NamedKey::Super | NamedKey::Meta))
}

/// Bytes the shell expects for a key press that is not a shortcut. `text` is the
/// layout-translated text winit produced for the key.
pub fn encode(platform: Platform, key: &Key, text: Option<&str>, m: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    let modifier_param = 1 + m.shift as u8 + if m.alt { 2 } else { 0 } + if m.ctrl { 4 } else { 0 };
    let bytes: Vec<u8> = match key {
        Key::Named(NamedKey::Enter) => if m.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() },
        Key::Named(NamedKey::Backspace) => {
            if m.alt {
                b"\x1b\x7f".to_vec()
            } else if m.ctrl {
                b"\x08".to_vec()
            } else {
                b"\x7f".to_vec()
            }
        }
        Key::Named(NamedKey::Tab) => if m.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() },
        Key::Named(NamedKey::Escape) => b"\x1b".to_vec(),
        Key::Named(NamedKey::Space) => {
            if m.ctrl {
                b"\x00".to_vec()
            } else if m.alt {
                b"\x1b ".to_vec()
            } else {
                b" ".to_vec()
            }
        }
        Key::Named(k @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)) if is_word_jump(platform, m) => {
            if *k == NamedKey::ArrowLeft { b"\x1bb".to_vec() } else { b"\x1bf".to_vec() }
        }
        Key::Named(k @ (NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::ArrowRight | NamedKey::ArrowLeft | NamedKey::Home | NamedKey::End)) => {
            let code = match k {
                NamedKey::ArrowUp => 'A',
                NamedKey::ArrowDown => 'B',
                NamedKey::ArrowRight => 'C',
                NamedKey::ArrowLeft => 'D',
                NamedKey::Home => 'H',
                _ => 'F',
            };
            if modifier_param > 1 {
                format!("\x1b[1;{modifier_param}{code}").into_bytes()
            } else {
                format!("\x1b{}{code}", if app_cursor { 'O' } else { '[' }).into_bytes()
            }
        }
        Key::Named(k @ (NamedKey::F1 | NamedKey::F2 | NamedKey::F3 | NamedKey::F4)) if modifier_param == 1 => {
            let code = match k {
                NamedKey::F1 => 'P',
                NamedKey::F2 => 'Q',
                NamedKey::F3 => 'R',
                _ => 'S',
            };
            format!("\x1bO{code}").into_bytes()
        }
        Key::Named(k) => {
            let num = match k {
                NamedKey::Insert => 2,
                NamedKey::Delete => 3,
                NamedKey::PageUp => 5,
                NamedKey::PageDown => 6,
                NamedKey::F1 => 11,
                NamedKey::F2 => 12,
                NamedKey::F3 => 13,
                NamedKey::F4 => 14,
                NamedKey::F5 => 15,
                NamedKey::F6 => 17,
                NamedKey::F7 => 18,
                NamedKey::F8 => 19,
                NamedKey::F9 => 20,
                NamedKey::F10 => 21,
                NamedKey::F11 => 23,
                NamedKey::F12 => 24,
                _ => return None,
            };
            if modifier_param > 1 {
                format!("\x1b[{num};{modifier_param}~").into_bytes()
            } else {
                format!("\x1b[{num}~").into_bytes()
            }
        }
        Key::Character(ch) if m.ctrl => {
            let c = match ch.as_bytes().first()? {
                b @ b'a'..=b'z' => b - b'a' + 1,
                b @ b'A'..=b'Z' => b - b'A' + 1,
                b'@' | b'2' | b' ' => 0,
                b'[' | b'3' => 27,
                b'\\' | b'4' => 28,
                b']' | b'5' => 29,
                b'^' | b'6' => 30,
                b'_' | b'7' | b'/' => 31,
                b'?' | b'8' => 127,
                _ => return None,
            };
            if m.alt { vec![0x1b, c] } else { vec![c] }
        }
        _ => {
            let txt = text.filter(|t| !t.is_empty())?;
            if m.alt {
                let mut v = Vec::with_capacity(txt.len() + 1);
                v.push(0x1b);
                v.extend_from_slice(txt.as_bytes());
                v
            } else {
                txt.as_bytes().to_vec()
            }
        }
    };
    Some(bytes)
}

/// Encode a key for xterm's `modifyOtherKeys` (`level` 1 or 2): modified keys the
/// legacy encoding would lose become `CSI 27 ; mods ; code ~`. Level 1 keeps the
/// well-known legacy bytes (`Ctrl+letter`, `Alt+key`, `Shift+Tab`); level 2 encodes
/// every modified key except shifted text.
pub fn encode_modify_other_keys(
    platform: Platform,
    key: &Key,
    text: Option<&str>,
    m: Mods,
    level: u8,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    let legacy = || encode(platform, key, text, m, app_cursor);
    let code = match key {
        Key::Character(s) => single_char(s).map(|c| c as u32),
        Key::Named(NamedKey::Enter) => Some(13),
        Key::Named(NamedKey::Tab) => Some(9),
        Key::Named(NamedKey::Backspace) => Some(127),
        Key::Named(NamedKey::Escape) => Some(27),
        Key::Named(NamedKey::Space) => Some(32),
        _ => None,
    };
    let Some(code) = code else { return legacy() };
    if !m.ctrl && !m.alt && (!m.shift || matches!(key, Key::Character(_) | Key::Named(NamedKey::Space))) {
        return legacy();
    }
    if level < 2 {
        let c = char::from_u32(code).unwrap_or('\0');
        let ctrl_char = m.ctrl && !m.alt && !m.shift && (c.is_ascii_alphabetic() || "@[\\]^_ ".contains(c));
        let meta = m.alt && !m.ctrl && !m.shift;
        let back_tab = code == 9 && m == Mods { shift: true, ..Mods::default() };
        if ctrl_char || meta || back_tab {
            return legacy();
        }
    }
    let mods = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    Some(format!("\x1b[27;{mods};{code}~").into_bytes())
}

// ----------------------------------------------------------------------
// Kitty keyboard protocol
// https://sw.kovidgoyal.net/kitty/keyboard-protocol/
// ----------------------------------------------------------------------

/// Report `Esc`, `Alt`/`Ctrl` combinations and modified `Enter`/`Tab`/`Backspace` as `CSI u`.
pub const KITTY_DISAMBIGUATE: u8 = 1;
/// Report key repeat and release events.
pub const KITTY_REPORT_EVENTS: u8 = 2;
/// Report the shifted key and the US-layout base key alongside the key code.
pub const KITTY_REPORT_ALTERNATES: u8 = 4;
/// Report every key, including plain text and modifier keys, as an escape code.
pub const KITTY_REPORT_ALL: u8 = 8;
/// With `KITTY_REPORT_ALL`, include the text a key produces.
pub const KITTY_REPORT_TEXT: u8 = 16;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Press,
    Repeat,
    Release,
}

/// A key event in the terms the Kitty protocol reports it.
#[derive(Copy, Clone, Debug)]
pub struct KittyKey<'a> {
    /// Logical key with the layout and modifiers applied (`A` for Shift+a).
    pub key: &'a Key,
    /// Logical key ignoring modifiers (`a` for Shift+a, also with Ctrl or Option).
    pub unshifted: &'a Key,
    /// Physical key position, for the US-layout base key and left/right modifiers.
    pub physical: Option<KeyCode>,
    pub location: KeyLocation,
    pub text: Option<&'a str>,
    pub kind: KeyKind,
    /// Lock state: `KITTY_CAPS_LOCK` / `KITTY_NUM_LOCK` bits.
    pub locks: u8,
}

/// Lock modifiers reported in Kitty escapes (as modifier bits 64 / 128).
pub const KITTY_CAPS_LOCK: u8 = 1;
pub const KITTY_NUM_LOCK: u8 = 2;

/// Lock state inferred from a key press, where the platform does not report
/// it (winit on Linux): a letter whose case disagrees with Shift means Caps
/// Lock is on, a keypad digit means Num Lock is on and a keypad navigation key
/// that it is off; the lock keys themselves toggle. Other keys keep `prev`.
pub fn infer_locks(prev: u8, key: &Key, location: KeyLocation, shift: bool) -> u8 {
    let set = |locks: u8, bit: u8, on: bool| if on { locks | bit } else { locks & !bit };
    match key {
        Key::Named(NamedKey::CapsLock) => prev ^ KITTY_CAPS_LOCK,
        Key::Named(NamedKey::NumLock) => prev ^ KITTY_NUM_LOCK,
        Key::Named(
            NamedKey::ArrowUp
            | NamedKey::ArrowDown
            | NamedKey::ArrowLeft
            | NamedKey::ArrowRight
            | NamedKey::Home
            | NamedKey::End
            | NamedKey::PageUp
            | NamedKey::PageDown
            | NamedKey::Insert
            | NamedKey::Delete
            | NamedKey::Clear,
        ) if location == KeyLocation::Numpad => prev & !KITTY_NUM_LOCK,
        Key::Character(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if location == KeyLocation::Numpad && c.is_ascii_digit() => prev | KITTY_NUM_LOCK,
                // Only letters with case (not digits, punctuation or caseless scripts).
                (Some(c), None) if c.is_lowercase() || c.is_uppercase() => set(prev, KITTY_CAPS_LOCK, c.is_uppercase() != shift),
                _ => prev,
            }
        }
        _ => prev,
    }
}

/// How a key is spelled in a Kitty escape: `CSI number ... final`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum KittyCode {
    /// `CSI code ; mods u`
    U(u32),
    /// `CSI number ; mods ~`
    Tilde(u32),
    /// `CSI 1 ; mods <letter>` (arrows, Home, End, F1, F2, F4).
    Letter(u8),
}

/// Encode a key for an application that enabled the Kitty keyboard protocol
/// (`flags` != 0). Keys the active flags leave alone use the legacy encoding.
pub fn encode_kitty(platform: Platform, k: &KittyKey, mut m: Mods, flags: u8, app_cursor: bool) -> Option<Vec<u8>> {
    let report_all = flags & KITTY_REPORT_ALL != 0;
    let report_events = flags & KITTY_REPORT_EVENTS != 0;
    let kind = match k.kind {
        KeyKind::Release if !report_events => return None,
        KeyKind::Repeat if !report_events => KeyKind::Press,
        kind => kind,
    };
    let Some(code) = kitty_code(k) else {
        // Dead keys and other keys the protocol has no number for keep their legacy bytes.
        return if kind == KeyKind::Release { None } else { encode(platform, k.key, k.text, m, app_cursor) };
    };

    // A modifier key reports its own bit while held, not after release.
    if let Key::Named(named) = k.key {
        let held = kind != KeyKind::Release;
        match named {
            NamedKey::Shift => m.shift = held,
            NamedKey::Control => m.ctrl = held,
            NamedKey::Alt => m.alt = held,
            NamedKey::Super | NamedKey::Meta => m.sup = held,
            _ => {}
        }
    }
    let is_modifier = matches!(code, KittyCode::U(57358..=57360 | 57441..=57452));
    // Keypad Enter is a key of its own (`CSI 57414 u`), not the legacy `\r`.
    let legacy_editing = matches!(k.key, Key::Named(NamedKey::Enter | NamedKey::Tab | NamedKey::Backspace))
        && k.location != KeyLocation::Numpad;
    let text = k.text.filter(|t| !t.is_empty() && t.chars().all(|c| c >= ' ' && c != '\x7f'));

    if !report_all {
        if is_modifier || (kind == KeyKind::Release && legacy_editing) {
            return None;
        }
        if kind != KeyKind::Release {
            // Text typed without Ctrl/Alt/Super is sent as is, like a legacy terminal.
            if let Some(t) = text.filter(|_| !m.ctrl && !m.alt && !m.sup && !legacy_editing) {
                return Some(t.as_bytes().to_vec());
            }
            let plain = m == Mods::default();
            let functional_plain = plain && !matches!(code, KittyCode::U(_)) && kind == KeyKind::Press;
            if flags & KITTY_DISAMBIGUATE == 0 || (plain && legacy_editing) || functional_plain {
                return encode(platform, k.key, k.text, m, app_cursor);
            }
        }
    }

    // Lock states only travel with keys that are sent as escapes anyway.
    let locks = 64 * (k.locks & KITTY_CAPS_LOCK != 0) as u8 + 128 * (k.locks & KITTY_NUM_LOCK != 0) as u8;
    let mods = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8 + 8 * m.sup as u8 + locks;
    let event = match kind {
        KeyKind::Press => None,
        KeyKind::Repeat => Some(2),
        KeyKind::Release => Some(3),
    };
    let text_field = text.filter(|_| report_all && flags & KITTY_REPORT_TEXT != 0 && kind != KeyKind::Release);

    let mut out = String::with_capacity(16);
    out.push_str("\x1b[");
    let final_byte = match code {
        KittyCode::U(c) => {
            out.push_str(&c.to_string());
            if flags & KITTY_REPORT_ALTERNATES != 0 {
                push_alternates(&mut out, k, m, c);
            }
            'u'
        }
        KittyCode::Tilde(n) => {
            out.push_str(&n.to_string());
            '~'
        }
        KittyCode::Letter(l) => l as char,
    };
    if mods > 1 || event.is_some() || text_field.is_some() {
        if matches!(code, KittyCode::Letter(_)) {
            out.push('1');
        }
        out.push(';');
        // Text alone leaves the modifier field empty: `CSI 97;;97u`.
        if mods > 1 || event.is_some() {
            out.push_str(&mods.to_string());
        }
        if let Some(e) = event {
            out.push(':');
            out.push_str(&e.to_string());
        }
    }
    if let Some(t) = text_field {
        out.push(';');
        for (i, c) in t.chars().enumerate() {
            if i > 0 {
                out.push(':');
            }
            out.push_str(&(c as u32).to_string());
        }
    }
    out.push(final_byte);
    Some(out.into_bytes())
}

/// `:shifted:base` after the key code, each part only when it differs from the code.
fn push_alternates(out: &mut String, k: &KittyKey, m: Mods, code: u32) {
    let shifted = match k.key {
        Key::Character(s) if m.shift => single_char(s).map(|c| c as u32).filter(|&c| c != code),
        _ => None,
    };
    let base = k.physical.and_then(us_layout_char).map(|c| c as u32).filter(|&c| c != code);
    if shifted.is_none() && base.is_none() {
        return;
    }
    out.push(':');
    if let Some(s) = shifted {
        out.push_str(&s.to_string());
    }
    if let Some(b) = base {
        out.push(':');
        out.push_str(&b.to_string());
    }
}

fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

/// The Kitty key number of a key: the lowercase unshifted character for text
/// keys, a legacy number/letter or a Private Use Area code for functional keys.
fn kitty_code(k: &KittyKey) -> Option<KittyCode> {
    use KittyCode::{Letter, Tilde, U};
    let numpad = k.location == KeyLocation::Numpad;
    if numpad {
        let kp = match k.key {
            Key::Character(s) => match single_char(s)? {
                c @ '0'..='9' => 57399 + (c as u32 - '0' as u32),
                '.' | ',' => 57409,
                '/' => 57410,
                '*' => 57411,
                '-' => 57412,
                '+' => 57413,
                '=' => 57415,
                _ => 0,
            },
            Key::Named(n) => match n {
                NamedKey::Enter => 57414,
                NamedKey::ArrowLeft => 57417,
                NamedKey::ArrowRight => 57418,
                NamedKey::ArrowUp => 57419,
                NamedKey::ArrowDown => 57420,
                NamedKey::PageUp => 57421,
                NamedKey::PageDown => 57422,
                NamedKey::Home => 57423,
                NamedKey::End => 57424,
                NamedKey::Insert => 57425,
                NamedKey::Delete => 57426,
                NamedKey::Clear => 57427,
                _ => 0,
            },
            _ => 0,
        };
        if kp != 0 {
            return Some(U(kp));
        }
    }
    let right = k.location == KeyLocation::Right;
    let side = |left: u32| U(if right { left + 6 } else { left });
    let code = match k.key {
        Key::Character(_) => {
            let base = match k.unshifted {
                Key::Character(s) => single_char(s),
                _ => None,
            };
            let c = base.or_else(|| match k.key {
                Key::Character(s) => single_char(s),
                _ => None,
            })?;
            U(c.to_lowercase().next().unwrap_or(c) as u32)
        }
        Key::Named(n) => match n {
            NamedKey::Escape => U(27),
            NamedKey::Enter => U(13),
            NamedKey::Tab => U(9),
            NamedKey::Backspace => U(127),
            NamedKey::Space => U(32),
            NamedKey::Insert => Tilde(2),
            NamedKey::Delete => Tilde(3),
            NamedKey::PageUp => Tilde(5),
            NamedKey::PageDown => Tilde(6),
            NamedKey::ArrowUp => Letter(b'A'),
            NamedKey::ArrowDown => Letter(b'B'),
            NamedKey::ArrowRight => Letter(b'C'),
            NamedKey::ArrowLeft => Letter(b'D'),
            NamedKey::Home => Letter(b'H'),
            NamedKey::End => Letter(b'F'),
            NamedKey::F1 => Letter(b'P'),
            NamedKey::F2 => Letter(b'Q'),
            // `CSI R` would collide with a cursor position report.
            NamedKey::F3 => Tilde(13),
            NamedKey::F4 => Letter(b'S'),
            NamedKey::F5 => Tilde(15),
            NamedKey::F6 => Tilde(17),
            NamedKey::F7 => Tilde(18),
            NamedKey::F8 => Tilde(19),
            NamedKey::F9 => Tilde(20),
            NamedKey::F10 => Tilde(21),
            NamedKey::F11 => Tilde(23),
            NamedKey::F12 => Tilde(24),
            NamedKey::F13 => U(57376),
            NamedKey::F14 => U(57377),
            NamedKey::F15 => U(57378),
            NamedKey::F16 => U(57379),
            NamedKey::F17 => U(57380),
            NamedKey::F18 => U(57381),
            NamedKey::F19 => U(57382),
            NamedKey::F20 => U(57383),
            NamedKey::CapsLock => U(57358),
            NamedKey::ScrollLock => U(57359),
            NamedKey::NumLock => U(57360),
            NamedKey::PrintScreen => U(57361),
            NamedKey::Pause => U(57362),
            NamedKey::ContextMenu => U(57363),
            NamedKey::MediaPlay => U(57428),
            NamedKey::MediaPause => U(57429),
            NamedKey::MediaPlayPause => U(57430),
            NamedKey::MediaStop => U(57432),
            NamedKey::MediaFastForward => U(57433),
            NamedKey::MediaRewind => U(57434),
            NamedKey::MediaTrackNext => U(57435),
            NamedKey::MediaTrackPrevious => U(57436),
            NamedKey::MediaRecord => U(57437),
            NamedKey::AudioVolumeDown => U(57438),
            NamedKey::AudioVolumeUp => U(57439),
            NamedKey::AudioVolumeMute => U(57440),
            NamedKey::Shift => side(57441),
            NamedKey::Control => side(57442),
            NamedKey::Alt => side(57443),
            NamedKey::Super => side(57444),
            NamedKey::Hyper => side(57445),
            NamedKey::Meta => side(57446),
            _ => return None,
        },
        _ => return None,
    };
    Some(code)
}

/// The character a key produces on a US layout, for the Kitty "base layout key".
fn us_layout_char(code: KeyCode) -> Option<char> {
    let c = match code {
        KeyCode::KeyA => 'a', KeyCode::KeyB => 'b', KeyCode::KeyC => 'c', KeyCode::KeyD => 'd',
        KeyCode::KeyE => 'e', KeyCode::KeyF => 'f', KeyCode::KeyG => 'g', KeyCode::KeyH => 'h',
        KeyCode::KeyI => 'i', KeyCode::KeyJ => 'j', KeyCode::KeyK => 'k', KeyCode::KeyL => 'l',
        KeyCode::KeyM => 'm', KeyCode::KeyN => 'n', KeyCode::KeyO => 'o', KeyCode::KeyP => 'p',
        KeyCode::KeyQ => 'q', KeyCode::KeyR => 'r', KeyCode::KeyS => 's', KeyCode::KeyT => 't',
        KeyCode::KeyU => 'u', KeyCode::KeyV => 'v', KeyCode::KeyW => 'w', KeyCode::KeyX => 'x',
        KeyCode::KeyY => 'y', KeyCode::KeyZ => 'z',
        KeyCode::Digit0 => '0', KeyCode::Digit1 => '1', KeyCode::Digit2 => '2', KeyCode::Digit3 => '3',
        KeyCode::Digit4 => '4', KeyCode::Digit5 => '5', KeyCode::Digit6 => '6', KeyCode::Digit7 => '7',
        KeyCode::Digit8 => '8', KeyCode::Digit9 => '9',
        KeyCode::Backquote => '`', KeyCode::Minus => '-', KeyCode::Equal => '=',
        KeyCode::BracketLeft => '[', KeyCode::BracketRight => ']', KeyCode::Backslash => '\\',
        KeyCode::Semicolon => ';', KeyCode::Quote => '\'', KeyCode::Comma => ',',
        KeyCode::Period => '.', KeyCode::Slash => '/',
        _ => return None,
    };
    Some(c)
}

/// Word jump: `Option+←/→` on macOS (Cmd/Ctrl arrows belong to Spaces and splits),
/// `Ctrl+←/→` or `Alt+←/→` on Linux.
fn is_word_jump(platform: Platform, m: Mods) -> bool {
    match platform {
        Platform::MacOs => m.only(false, true, false, false),
        Platform::Linux => !m.shift && !m.sup && (m.alt ^ m.ctrl),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;

    const NONE: Mods = Mods { ctrl: false, alt: false, shift: false, sup: false };
    const CMD: Mods = Mods { sup: true, ..NONE };
    const CMD_SHIFT: Mods = Mods { sup: true, shift: true, ..NONE };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };
    const CTRL_SHIFT: Mods = Mods { ctrl: true, shift: true, ..NONE };
    const CTRL_ALT: Mods = Mods { ctrl: true, alt: true, ..NONE };
    const ALT: Mods = Mods { alt: true, ..NONE };

    fn ch(c: &str) -> Key {
        Key::Character(SmolStr::new(c))
    }

    fn mac(key: &Key, m: Mods) -> Option<Action> {
        resolve(Platform::MacOs, key, m, false)
    }

    fn linux(key: &Key, m: Mods) -> Option<Action> {
        resolve(Platform::Linux, key, m, false)
    }

    #[test]
    fn terminal_actions_never_steal_bare_ctrl_letters_on_linux() {
        for (c, action) in [
            ("t", Action::NewTab),
            ("n", Action::NewWindow),
            ("w", Action::CloseTab),
            ("c", Action::Copy),
            ("v", Action::Paste),
            ("q", Action::Quit),
            ("m", Action::Minimize),
            ("k", Action::ClearScreen),
            ("f", Action::Find),
            ("a", Action::SelectAll),
        ] {
            assert_eq!(mac(&ch(c), CMD), Some(action), "Cmd+{c}");
            assert_eq!(linux(&ch(c), CTRL_SHIFT), Some(action), "Ctrl+Shift+{c}");
            assert_eq!(linux(&ch(c), CMD), Some(action), "Super+{c}");
            assert_eq!(linux(&ch(c), CTRL), None, "bare Ctrl+{c} must reach the shell");
            assert_eq!(mac(&ch(c), CTRL), None, "bare Ctrl+{c} must reach the shell");
        }
        // Shifted letters arrive uppercase on Linux.
        assert_eq!(linux(&ch("T"), CTRL_SHIFT), Some(Action::NewTab));
    }

    #[test]
    fn history_search_respects_the_alternate_screen() {
        assert_eq!(resolve(Platform::MacOs, &ch("r"), CTRL, false), Some(Action::HistorySearch));
        assert_eq!(resolve(Platform::MacOs, &ch("r"), CTRL, true), None, "vim redo");
        assert_eq!(resolve(Platform::MacOs, &ch("r"), CMD, true), Some(Action::HistorySearch));
        assert_eq!(resolve(Platform::Linux, &ch("r"), CTRL_SHIFT, true), Some(Action::HistorySearch));
        assert_eq!(resolve(Platform::MacOs, &ch("r"), CTRL_SHIFT, false), None);
    }

    #[test]
    fn splits_config_and_zoom() {
        assert_eq!(mac(&ch("d"), CMD), Some(Action::SplitRight));
        assert_eq!(mac(&ch("d"), CMD_SHIFT), Some(Action::SplitDown));
        assert_eq!(linux(&ch("d"), CTRL_SHIFT), Some(Action::SplitRight));
        assert_eq!(linux(&ch("e"), CTRL_SHIFT), Some(Action::SplitDown));
        assert_eq!(linux(&ch("d"), CMD_SHIFT), Some(Action::SplitDown));
        assert_eq!(linux(&ch("d"), CTRL_ALT), Some(Action::SplitDown));

        assert_eq!(mac(&ch(","), CMD), Some(Action::OpenConfig));
        assert_eq!(mac(&ch("<"), CMD_SHIFT), Some(Action::ReloadConfig));
        assert_eq!(linux(&ch(","), CTRL), Some(Action::OpenConfig));
        assert_eq!(linux(&ch(","), CTRL_SHIFT), Some(Action::ReloadConfig));

        assert_eq!(mac(&ch("="), CMD), Some(Action::ZoomIn));
        assert_eq!(mac(&ch("-"), CMD), Some(Action::ZoomOut));
        assert_eq!(mac(&ch("0"), CMD), Some(Action::ZoomReset));
        assert_eq!(linux(&ch("+"), CTRL_SHIFT), Some(Action::ZoomIn));
        assert_eq!(linux(&ch("_"), CTRL_SHIFT), Some(Action::ZoomOut));
        assert_eq!(mac(&ch("="), Mods { alt: true, ..CMD }), Some(Action::EqualizeSplits));
        assert_eq!(mac(&ch("="), Mods { ctrl: true, ..CMD }), Some(Action::EqualizeSplits));
        assert_eq!(linux(&ch("="), CTRL_ALT), Some(Action::EqualizeSplits));
    }

    #[test]
    fn tab_and_split_cycling() {
        assert_eq!(mac(&ch("{"), CMD_SHIFT), Some(Action::PrevTab));
        assert_eq!(mac(&ch("]"), CMD_SHIFT), Some(Action::NextTab));
        assert_eq!(mac(&ch("["), CMD), Some(Action::PrevSplit));
        assert_eq!(linux(&ch("["), CTRL_SHIFT), Some(Action::PrevTab));
        assert_eq!(linux(&ch("]"), CTRL_ALT), Some(Action::NextSplit));
        assert_eq!(mac(&ch("1"), CMD), Some(Action::SelectTab(0)));
        assert_eq!(mac(&ch("9"), CMD), Some(Action::SelectTab(8)));
        assert_eq!(linux(&ch("3"), CTRL), Some(Action::SelectTab(2)));
        assert_eq!(linux(&ch("3"), ALT), Some(Action::SelectTab(2)));
        let tab = Key::Named(NamedKey::Tab);
        assert_eq!(linux(&tab, CTRL), Some(Action::NextTab));
        assert_eq!(linux(&tab, CTRL_SHIFT), Some(Action::PrevTab));
    }

    #[test]
    fn arrows_navigate_resize_or_reach_the_shell() {
        let left = Key::Named(NamedKey::ArrowLeft);
        assert_eq!(mac(&left, CMD), Some(Action::Navigate(Direction::Left)));
        assert_eq!(mac(&left, Mods { alt: true, ..CMD }), Some(Action::Navigate(Direction::Left)));
        assert_eq!(mac(&left, Mods { ctrl: true, ..CMD }), Some(Action::ResizeSplit(-0.05)));
        assert_eq!(linux(&left, CTRL_ALT), Some(Action::Navigate(Direction::Left)));
        assert_eq!(linux(&left, Mods { shift: true, ..CTRL_ALT }), Some(Action::ResizeSplit(-0.05)));
        assert_eq!(mac(&left, ALT), None);
        assert_eq!(linux(&left, CTRL), None);
    }

    #[test]
    fn split_zoom_works_with_cmd_on_macos() {
        let enter = Key::Named(NamedKey::Enter);
        assert_eq!(mac(&enter, CMD_SHIFT), Some(Action::ToggleSplitZoom));
        assert_eq!(linux(&enter, CTRL_SHIFT), Some(Action::ToggleSplitZoom));
        assert_eq!(mac(&enter, NONE), None);
    }

    #[test]
    fn prompt_navigation_and_output_shortcuts() {
        let up = Key::Named(NamedKey::ArrowUp);
        let down = Key::Named(NamedKey::ArrowDown);
        assert_eq!(mac(&up, CMD_SHIFT), Some(Action::JumpToPrompt(-1)));
        assert_eq!(mac(&down, CMD_SHIFT), Some(Action::JumpToPrompt(1)));
        assert_eq!(linux(&up, CTRL_SHIFT), Some(Action::JumpToPrompt(-1)));
        assert_eq!(mac(&up, CMD), Some(Action::Navigate(Direction::Up)), "Cmd+Up still navigates splits");
        assert_eq!(mac(&ch("o"), CMD_SHIFT), Some(Action::SelectLastOutput));
        assert_eq!(linux(&ch("O"), CTRL_SHIFT), Some(Action::SelectLastOutput));
        assert_eq!(linux(&ch("o"), CTRL), None, "bare Ctrl+O reaches bash");
        assert_eq!(parse_binding("jump_to_prompt:-1").unwrap(), Binding::Action(Action::JumpToPrompt(-1)));
    }

    #[test]
    fn x11_keysyms() {
        let sym = |s: &str| x11_keysym(&parse_trigger(s).unwrap().key);
        assert_eq!(sym("ctrl+grave_accent"), Some(0x60));
        assert_eq!(sym("super+A"), Some(0x61));
        assert_eq!(sym("ctrl+f12"), Some(0xFFC9));
        assert_eq!(sym("alt+space"), Some(0x20));
    }

    #[test]
    fn mac_virtual_keycodes() {
        let code = |s: &str| mac_virtual_keycode(&parse_trigger(s).unwrap().key);
        assert_eq!(code("cmd+grave_accent"), Some(0x32));
        assert_eq!(code("ctrl+a"), Some(0x00));
        assert_eq!(code("ctrl+t"), Some(0x11));
        assert_eq!(code("ctrl+5"), Some(0x17));
        assert_eq!(code("ctrl+m"), Some(0x2E));
        assert_eq!(code("ctrl+comma"), Some(0x2B));
        assert_eq!(code("f12"), Some(0x6F));
        assert_eq!(code("space"), Some(0x31));
    }

    #[test]
    fn inspector_shortcut() {
        let cmd_alt = Mods { sup: true, alt: true, ..NONE };
        assert_eq!(mac(&ch("i"), cmd_alt), Some(Action::ToggleInspector));
        assert_eq!(linux(&ch("I"), CTRL_SHIFT), Some(Action::ToggleInspector));
        assert_eq!(linux(&ch("i"), cmd_alt), Some(Action::ToggleInspector));
        assert_eq!(linux(&ch("i"), CTRL), None, "bare Ctrl+I is Tab");
        assert_eq!(mac(&ch("i"), CMD), None);
    }

    #[test]
    fn parses_user_key_bindings() {
        let t = parse_trigger("cmd+shift+t").unwrap();
        assert_eq!(t, Trigger { mods: CMD_SHIFT, key: TriggerKey::Char('t') });
        assert_eq!(parse_trigger("ctrl+alt+left").unwrap().key, TriggerKey::Named(NamedKey::ArrowLeft));
        assert_eq!(parse_trigger("super+comma").unwrap().key, TriggerKey::Char(','));
        assert_eq!(parse_trigger("ctrl++").unwrap(), Trigger { mods: CTRL, key: TriggerKey::Char('+') });
        assert!(parse_trigger("hyper+x").is_err());
        assert!(parse_trigger("ctrl+shift").is_err());

        assert_eq!(parse_binding("new_tab").unwrap(), Binding::Action(Action::NewTab));
        assert_eq!(parse_binding("new_window").unwrap(), Binding::Action(Action::NewWindow));
        assert_eq!(parse_binding("inspector:toggle").unwrap(), Binding::Action(Action::ToggleInspector));
        assert_eq!(parse_binding("goto_tab:3").unwrap(), Binding::Action(Action::SelectTab(2)));
        assert_eq!(parse_binding("new_split:down").unwrap(), Binding::Action(Action::SplitDown));
        assert_eq!(parse_binding("goto_split:left").unwrap(), Binding::Action(Action::Navigate(Direction::Left)));
        assert_eq!(parse_binding("text:\\x1b[A\\n").unwrap(), Binding::Text(b"\x1b[A\n".to_vec()));
        assert_eq!(parse_binding("text:\\u{1f980}").unwrap(), Binding::Text("🦀".as_bytes().to_vec()));
        assert_eq!(parse_binding("csi:A").unwrap(), Binding::Text(b"\x1b[A".to_vec()));
        assert_eq!(parse_binding("unbind").unwrap(), Binding::Unbind);
        assert!(parse_binding("fly_to_moon").is_err());
        assert!(parse_binding("goto_tab:12").is_err());
    }

    #[test]
    fn triggers_match_unshifted_keys_with_exact_modifiers() {
        let t = parse_trigger("cmd+shift+bracket_left").unwrap();
        assert!(trigger_matches(&t, &ch("["), CMD_SHIFT));
        assert!(!trigger_matches(&t, &ch("["), CMD));
        let t = parse_trigger("ctrl+K").unwrap();
        assert!(trigger_matches(&t, &ch("k"), CTRL));
        assert!(trigger_matches(&parse_trigger("ctrl+space").unwrap(), &Key::Named(NamedKey::Space), CTRL));
        assert!(trigger_matches(&parse_trigger("alt+enter").unwrap(), &Key::Named(NamedKey::Enter), ALT));
    }

    #[test]
    fn modifier_keys_alone() {
        for k in [NamedKey::Shift, NamedKey::Control, NamedKey::Alt, NamedKey::Super, NamedKey::Meta] {
            assert!(is_modifier_key(&Key::Named(k)), "{k:?}");
        }
        assert!(!is_modifier_key(&Key::Named(NamedKey::Enter)));
        assert!(!is_modifier_key(&ch("c")));
    }

    #[test]
    fn encodes_keys_for_the_shell() {
        let enc = |p, k: &Key, t: Option<&str>, m, app| encode(p, k, t, m, app).unwrap();
        let mac = Platform::MacOs;
        let linux = Platform::Linux;
        assert_eq!(enc(mac, &Key::Named(NamedKey::ArrowUp), None, NONE, false), b"\x1b[A");
        assert_eq!(enc(mac, &Key::Named(NamedKey::ArrowUp), None, NONE, true), b"\x1bOA");
        assert_eq!(enc(mac, &Key::Named(NamedKey::ArrowUp), None, Mods { shift: true, ..NONE }, false), b"\x1b[1;2A");
        assert_eq!(enc(mac, &Key::Named(NamedKey::ArrowLeft), None, ALT, false), b"\x1bb");
        assert_eq!(enc(linux, &Key::Named(NamedKey::ArrowRight), None, CTRL, false), b"\x1bf");
        assert_eq!(enc(mac, &Key::Named(NamedKey::ArrowLeft), None, CTRL, false), b"\x1b[1;5D");
        assert_eq!(enc(mac, &Key::Named(NamedKey::Delete), None, NONE, false), b"\x1b[3~");
        assert_eq!(enc(mac, &Key::Named(NamedKey::F5), None, CTRL, false), b"\x1b[15;5~");
        assert_eq!(enc(mac, &Key::Named(NamedKey::F1), None, NONE, false), b"\x1bOP");
        assert_eq!(enc(mac, &ch("c"), Some("c"), CTRL, false), b"\x03");
        assert_eq!(enc(mac, &ch("["), Some("["), CTRL_ALT, false), b"\x1b\x1b");
        assert_eq!(enc(mac, &ch("x"), Some("x"), ALT, false), b"\x1bx");
        assert_eq!(enc(mac, &ch("ệ"), Some("ệ"), NONE, false), "ệ".as_bytes());
        assert_eq!(enc(mac, &Key::Named(NamedKey::Tab), None, Mods { shift: true, ..NONE }, false), b"\x1b[Z");
        assert!(encode(mac, &Key::Named(NamedKey::Shift), None, NONE, false).is_none());
    }

    struct K {
        key: Key,
        unshifted: Key,
        physical: Option<KeyCode>,
        location: KeyLocation,
        text: Option<&'static str>,
    }

    /// A character key: logical text, unshifted text and its physical position.
    fn kc(key: &str, unshifted: &str, physical: KeyCode) -> K {
        K { key: ch(key), unshifted: ch(unshifted), physical: Some(physical), location: KeyLocation::Standard, text: None }
    }

    fn named(n: NamedKey) -> K {
        K { key: Key::Named(n), unshifted: Key::Named(n), physical: None, location: KeyLocation::Standard, text: None }
    }

    impl K {
        fn text(mut self, t: &'static str) -> Self {
            self.text = Some(t);
            self
        }

        fn at(mut self, location: KeyLocation) -> Self {
            self.location = location;
            self
        }

        fn kitty(&self, flags: u8, m: Mods, kind: KeyKind) -> Option<String> {
            let k = KittyKey {
                key: &self.key,
                unshifted: &self.unshifted,
                physical: self.physical,
                location: self.location,
                text: self.text,
                kind,
                locks: 0,
            };
            encode_kitty(Platform::MacOs, &k, m, flags, false).map(|b| String::from_utf8(b).unwrap())
        }

        fn press(&self, flags: u8, m: Mods) -> Option<String> {
            self.kitty(flags, m, KeyKind::Press)
        }
    }

    const SHIFT: Mods = Mods { shift: true, ..NONE };
    const D: u8 = KITTY_DISAMBIGUATE;

    #[test]
    fn lock_state_is_inferred_from_key_presses() {
        use KeyLocation::{Numpad, Standard};
        let ch = |s: &str| Key::Character(s.into());
        let named = Key::Named;
        assert_eq!(infer_locks(0, &ch("A"), Standard, false), KITTY_CAPS_LOCK, "uppercase without Shift");
        assert_eq!(infer_locks(KITTY_CAPS_LOCK, &ch("a"), Standard, true), KITTY_CAPS_LOCK, "Shift cancels Caps Lock");
        assert_eq!(infer_locks(KITTY_CAPS_LOCK, &ch("A"), Standard, true), 0);
        assert_eq!(infer_locks(KITTY_CAPS_LOCK, &ch("1"), Standard, false), KITTY_CAPS_LOCK, "digits say nothing");
        assert_eq!(infer_locks(0, &named(NamedKey::CapsLock), Standard, false), KITTY_CAPS_LOCK);
        assert_eq!(infer_locks(0, &ch("7"), Numpad, false), KITTY_NUM_LOCK);
        assert_eq!(infer_locks(KITTY_NUM_LOCK, &named(NamedKey::Home), Numpad, false), 0);
        assert_eq!(infer_locks(KITTY_NUM_LOCK, &named(NamedKey::Home), Standard, false), KITTY_NUM_LOCK);
        assert_eq!(infer_locks(KITTY_NUM_LOCK, &named(NamedKey::NumLock), Numpad, false), 0);
    }

    #[test]
    fn kitty_reports_lock_modifiers_on_escaped_keys() {
        let a = ch("a");
        let k = |flags: u8, m: Mods, locks: u8| {
            let key = KittyKey {
                key: &a,
                unshifted: &a,
                physical: Some(KeyCode::KeyA),
                location: KeyLocation::Standard,
                text: Some("a"),
                kind: KeyKind::Press,
                locks,
            };
            encode_kitty(Platform::MacOs, &key, m, flags, false).map(|b| String::from_utf8(b).unwrap())
        };
        // Text typed with Caps Lock in disambiguate mode is still plain text.
        assert_eq!(k(D, NONE, KITTY_CAPS_LOCK).as_deref(), Some("a"));
        // Escaped keys carry the lock bits: 1 + 4 (ctrl) + 64 (caps) = 69.
        assert_eq!(k(D, CTRL, KITTY_CAPS_LOCK).as_deref(), Some("\x1b[97;69u"));
        assert_eq!(k(D | KITTY_REPORT_ALL, NONE, KITTY_CAPS_LOCK | KITTY_NUM_LOCK).as_deref(), Some("\x1b[97;193u"));
    }

    #[test]
    fn kitty_disambiguate_keeps_text_and_plain_editing_keys() {
        let a = kc("a", "a", KeyCode::KeyA).text("a");
        assert_eq!(a.press(D, NONE).as_deref(), Some("a"));
        assert_eq!(kc("A", "a", KeyCode::KeyA).text("A").press(D, SHIFT).as_deref(), Some("A"));
        assert_eq!(a.press(D, CTRL).as_deref(), Some("\x1b[97;5u"));
        assert_eq!(a.press(D, ALT).as_deref(), Some("\x1b[97;3u"));
        assert_eq!(kc("A", "a", KeyCode::KeyA).press(D, CTRL_SHIFT).as_deref(), Some("\x1b[97;6u"));
        assert_eq!(kc(" ", " ", KeyCode::Space).text(" ").press(D, NONE).as_deref(), Some(" "));
        assert_eq!(named(NamedKey::Space).press(D, CTRL).as_deref(), Some("\x1b[32;5u"));

        assert_eq!(named(NamedKey::Escape).press(D, NONE).as_deref(), Some("\x1b[27u"));
        assert_eq!(named(NamedKey::Enter).press(D, NONE).as_deref(), Some("\r"));
        assert_eq!(named(NamedKey::Enter).press(D, SHIFT).as_deref(), Some("\x1b[13;2u"));
        assert_eq!(named(NamedKey::Tab).press(D, NONE).as_deref(), Some("\t"));
        assert_eq!(named(NamedKey::Tab).press(D, SHIFT).as_deref(), Some("\x1b[9;2u"));
        assert_eq!(named(NamedKey::Backspace).press(D, NONE).as_deref(), Some("\x7f"));
        assert_eq!(named(NamedKey::Backspace).press(D, CTRL).as_deref(), Some("\x1b[127;5u"));
    }

    #[test]
    fn kitty_functional_keys() {
        assert_eq!(named(NamedKey::ArrowUp).press(D, NONE).as_deref(), Some("\x1b[A"));
        assert_eq!(named(NamedKey::ArrowUp).press(D, CTRL).as_deref(), Some("\x1b[1;5A"));
        // Option+Left is a real Alt+Left here, not the legacy `ESC b` word jump.
        assert_eq!(named(NamedKey::ArrowLeft).press(D, ALT).as_deref(), Some("\x1b[1;3D"));
        assert_eq!(named(NamedKey::F1).press(D, NONE).as_deref(), Some("\x1bOP"));
        assert_eq!(named(NamedKey::F1).press(D, SHIFT).as_deref(), Some("\x1b[1;2P"));
        assert_eq!(named(NamedKey::F3).press(D, CTRL).as_deref(), Some("\x1b[13;5~"));
        assert_eq!(named(NamedKey::Delete).press(D, NONE).as_deref(), Some("\x1b[3~"));
        assert_eq!(named(NamedKey::PageDown).press(D, ALT).as_deref(), Some("\x1b[6;3~"));
        assert_eq!(named(NamedKey::F13).press(D, NONE).as_deref(), Some("\x1b[57376u"));
        assert_eq!(named(NamedKey::Enter).at(KeyLocation::Numpad).press(D, NONE).as_deref(), Some("\x1b[57414u"));
        assert_eq!(kc("5", "5", KeyCode::Numpad5).text("5").at(KeyLocation::Numpad).press(D, NONE).as_deref(), Some("5"));
        assert_eq!(kc("5", "5", KeyCode::Numpad5).at(KeyLocation::Numpad).press(D, CTRL).as_deref(), Some("\x1b[57404;5u"));
        // Modifier keys alone are not reported without "report all keys".
        assert_eq!(named(NamedKey::Shift).press(D, SHIFT), None);
    }

    #[test]
    fn kitty_event_types() {
        let ev = D | KITTY_REPORT_EVENTS;
        let a = kc("a", "a", KeyCode::KeyA).text("a");
        assert_eq!(a.kitty(D, NONE, KeyKind::Release), None);
        assert_eq!(a.kitty(D, NONE, KeyKind::Repeat).as_deref(), Some("a"));
        assert_eq!(a.kitty(ev, NONE, KeyKind::Repeat).as_deref(), Some("a"));
        assert_eq!(a.kitty(ev, NONE, KeyKind::Release).as_deref(), Some("\x1b[97;1:3u"));
        assert_eq!(a.kitty(ev, CTRL, KeyKind::Repeat).as_deref(), Some("\x1b[97;5:2u"));
        assert_eq!(named(NamedKey::Enter).kitty(ev, NONE, KeyKind::Release), None);
        assert_eq!(named(NamedKey::ArrowUp).kitty(ev, NONE, KeyKind::Repeat).as_deref(), Some("\x1b[1;1:2A"));
        assert_eq!(named(NamedKey::ArrowUp).kitty(ev, NONE, KeyKind::Release).as_deref(), Some("\x1b[1;1:3A"));
        // Without event reporting a repeat is a plain press.
        assert_eq!(named(NamedKey::ArrowUp).kitty(D, NONE, KeyKind::Repeat).as_deref(), Some("\x1b[A"));
    }

    #[test]
    fn kitty_report_all_keys_and_text() {
        let all = D | KITTY_REPORT_ALL;
        let a = kc("a", "a", KeyCode::KeyA).text("a");
        assert_eq!(a.press(all, NONE).as_deref(), Some("\x1b[97u"));
        assert_eq!(kc("A", "a", KeyCode::KeyA).text("A").press(all, SHIFT).as_deref(), Some("\x1b[97;2u"));
        assert_eq!(named(NamedKey::Enter).press(all, NONE).as_deref(), Some("\x1b[13u"));
        assert_eq!(named(NamedKey::Enter).kitty(all | KITTY_REPORT_EVENTS, NONE, KeyKind::Release).as_deref(), Some("\x1b[13;1:3u"));

        let shift = named(NamedKey::Shift).at(KeyLocation::Left);
        assert_eq!(shift.press(all, SHIFT).as_deref(), Some("\x1b[57441;2u"));
        assert_eq!(shift.kitty(all | KITTY_REPORT_EVENTS, SHIFT, KeyKind::Release).as_deref(), Some("\x1b[57441;1:3u"));
        assert_eq!(named(NamedKey::Control).at(KeyLocation::Right).press(all, NONE).as_deref(), Some("\x1b[57448;5u"));

        let text = all | KITTY_REPORT_TEXT;
        assert_eq!(a.press(text, NONE).as_deref(), Some("\x1b[97;;97u"));
        assert_eq!(kc("A", "a", KeyCode::KeyA).text("A").press(text, SHIFT).as_deref(), Some("\x1b[97;2;65u"));
        assert_eq!(a.kitty(text | KITTY_REPORT_EVENTS, NONE, KeyKind::Release).as_deref(), Some("\x1b[97;1:3u"));
    }

    #[test]
    fn kitty_alternate_keys() {
        let alt = D | KITTY_REPORT_ALTERNATES;
        assert_eq!(kc("A", "a", KeyCode::KeyA).press(alt, CTRL_SHIFT).as_deref(), Some("\x1b[97:65;6u"));
        assert_eq!(kc("a", "a", KeyCode::KeyA).press(alt, CTRL).as_deref(), Some("\x1b[97;5u"));
        // A non-US layout reports its own key plus the US key at the same position.
        assert_eq!(kc("ф", "ф", KeyCode::KeyA).press(alt, CTRL).as_deref(), Some("\x1b[1092::97;5u"));
        assert_eq!(kc("Ф", "ф", KeyCode::KeyA).press(alt, CTRL_SHIFT).as_deref(), Some("\x1b[1092:1060:97;6u"));
    }

    #[test]
    fn modify_other_keys_levels() {
        let enc = |k: &Key, t: Option<&str>, m, level| {
            encode_modify_other_keys(Platform::MacOs, k, t, m, level, false).map(|b| String::from_utf8(b).unwrap())
        };
        let enter = Key::Named(NamedKey::Enter);
        let tab = Key::Named(NamedKey::Tab);
        // Level 2: every modified key that legacy would lose or blur.
        assert_eq!(enc(&ch("a"), Some("a"), CTRL, 2).as_deref(), Some("\x1b[27;5;97~"));
        assert_eq!(enc(&ch("A"), Some("A"), CTRL_SHIFT, 2).as_deref(), Some("\x1b[27;6;65~"));
        assert_eq!(enc(&enter, None, SHIFT, 2).as_deref(), Some("\x1b[27;2;13~"));
        assert_eq!(enc(&tab, None, CTRL, 2).as_deref(), Some("\x1b[27;5;9~"));
        assert_eq!(enc(&ch("x"), Some("x"), ALT, 2).as_deref(), Some("\x1b[27;3;120~"));
        // Plain and shifted text stay text; arrows keep their own modifier encoding.
        assert_eq!(enc(&ch("A"), Some("A"), SHIFT, 2).as_deref(), Some("A"));
        assert_eq!(enc(&enter, None, NONE, 2).as_deref(), Some("\r"));
        assert_eq!(enc(&Key::Named(NamedKey::ArrowUp), None, CTRL, 2).as_deref(), Some("\x1b[1;5A"));
        // Level 1 keeps the well-known legacy bytes.
        assert_eq!(enc(&ch("a"), Some("a"), CTRL, 1).as_deref(), Some("\x01"));
        assert_eq!(enc(&ch("x"), Some("x"), ALT, 1).as_deref(), Some("\x1bx"));
        assert_eq!(enc(&tab, None, SHIFT, 1).as_deref(), Some("\x1b[Z"));
        assert_eq!(enc(&ch("1"), Some("1"), CTRL, 1).as_deref(), Some("\x1b[27;5;49~"));
        assert_eq!(enc(&enter, None, CTRL, 1).as_deref(), Some("\x1b[27;5;13~"));
    }

    #[test]
    fn kitty_flags_without_disambiguate_keep_legacy_presses() {
        let ev = KITTY_REPORT_EVENTS;
        let a = kc("a", "a", KeyCode::KeyA).text("a");
        assert_eq!(a.press(ev, CTRL).as_deref(), Some("\x01"));
        assert_eq!(named(NamedKey::Escape).press(ev, NONE).as_deref(), Some("\x1b"));
        assert_eq!(a.kitty(ev, CTRL, KeyKind::Release).as_deref(), Some("\x1b[97;5:3u"));
    }
}
