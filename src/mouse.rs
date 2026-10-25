//! Mouse reporting for applications that request it (vim, tmux, htop, less...):
//! xterm button codes in the legacy X10 byte encoding or the SGR (1006) encoding.

/// A mouse button as reported to the application.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// Motion with no button held (any-event tracking).
    None,
}

/// What happened to the button.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Press,
    Release,
    Motion,
}

/// Modifier keys held during the event.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// Encode a mouse event at 0-based cell `(row, col)`. Returns `None` when the
/// legacy encoding cannot represent the position (beyond column/row 223).
pub fn encode(button: Button, action: Action, mods: Mods, row: usize, col: usize, sgr: bool) -> Option<Vec<u8>> {
    let mut code: u32 = match button {
        Button::Left => 0,
        Button::Middle => 1,
        Button::Right => 2,
        Button::None => 3,
        Button::WheelUp => 64,
        Button::WheelDown => 65,
    };
    if action == Action::Motion {
        code += 32;
    }
    if mods.shift {
        code += 4;
    }
    if mods.alt {
        code += 8;
    }
    if mods.ctrl {
        code += 16;
    }

    if sgr {
        let suffix = if action == Action::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{suffix}", col + 1, row + 1).into_bytes());
    }

    // Legacy encoding reports every release as button 3.
    if action == Action::Release {
        code = (code & !3) | 3;
    }
    let (cx, cy) = (col + 1 + 32, row + 1 + 32);
    if cx > 255 || cy > 255 {
        return None;
    }
    Some(vec![0x1b, b'[', b'M', (code + 32) as u8, cx as u8, cy as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_press_release_and_modifiers() {
        let none = Mods::default();
        assert_eq!(encode(Button::Left, Action::Press, none, 4, 9, true).unwrap(), b"\x1b[<0;10;5M");
        assert_eq!(encode(Button::Left, Action::Release, none, 4, 9, true).unwrap(), b"\x1b[<0;10;5m");
        let ctrl = Mods { ctrl: true, ..Mods::default() };
        assert_eq!(encode(Button::Right, Action::Press, ctrl, 0, 0, true).unwrap(), b"\x1b[<18;1;1M");
        assert_eq!(encode(Button::WheelDown, Action::Press, none, 1, 1, true).unwrap(), b"\x1b[<65;2;2M");
        assert_eq!(encode(Button::Left, Action::Motion, none, 0, 2, true).unwrap(), b"\x1b[<32;3;1M");
        assert_eq!(encode(Button::None, Action::Motion, none, 0, 0, true).unwrap(), b"\x1b[<35;1;1M");
    }

    #[test]
    fn legacy_encoding_and_limits() {
        let none = Mods::default();
        assert_eq!(encode(Button::Left, Action::Press, none, 0, 0, false).unwrap(), b"\x1b[M\x20\x21\x21");
        assert_eq!(encode(Button::Middle, Action::Release, none, 2, 3, false).unwrap(), b"\x1b[M\x23\x24\x23");
        assert!(encode(Button::Left, Action::Press, none, 0, 300, false).is_none());
        assert!(encode(Button::Left, Action::Press, none, 0, 300, true).is_some());
    }
}
