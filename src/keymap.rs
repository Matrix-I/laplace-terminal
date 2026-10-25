//! Keyboard shortcuts and key-to-byte encoding, as pure functions of the key,
//! the modifiers and the target platform. Both platform tables are always
//! compiled so the macOS/Linux shortcut matrix in CLAUDE.md is unit-tested on
//! any host.

use winit::keyboard::{Key, NamedKey};

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
    HistorySearch,
    Quit,
    Minimize,
    OpenConfig,
    ReloadConfig,
    NewTab,
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
        "t" | "n" if terminal_mod => Action::NewTab,
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
            ("n", Action::NewTab),
            ("w", Action::CloseTab),
            ("c", Action::Copy),
            ("v", Action::Paste),
            ("q", Action::Quit),
            ("m", Action::Minimize),
            ("k", Action::ClearScreen),
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
}
