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

#[inline]
fn write_u32(buf: &mut [u8], mut n: u32) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut temp = [0u8; 10];
    let mut i = 0;
    while n > 0 {
        temp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    for j in 0..i {
        buf[j] = temp[i - 1 - j];
    }
    i
}

/// Encode a mouse event at 0-based cell `(row, col)`. Returns `None` when the
/// legacy encoding cannot represent the position (beyond column/row 223).
/// Writes directly into a fixed 32-byte stack buffer without heap allocations.
pub fn encode(button: Button, action: Action, mods: Mods, row: usize, col: usize, sgr: bool) -> Option<([u8; 32], usize)> {
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

    let mut buf = [0u8; 32];
    if sgr {
        buf[0] = 0x1b;
        buf[1] = b'[';
        buf[2] = b'<';
        let mut pos = 3;
        pos += write_u32(&mut buf[pos..], code);
        buf[pos] = b';';
        pos += 1;
        pos += write_u32(&mut buf[pos..], (col + 1) as u32);
        buf[pos] = b';';
        pos += 1;
        pos += write_u32(&mut buf[pos..], (row + 1) as u32);
        buf[pos] = if action == Action::Release { b'm' } else { b'M' };
        pos += 1;
        return Some((buf, pos));
    }

    // Legacy encoding reports every release as button 3.
    if action == Action::Release {
        code = (code & !3) | 3;
    }
    let (cx, cy) = (col + 1 + 32, row + 1 + 32);
    if cx > 255 || cy > 255 {
        return None;
    }
    buf[0] = 0x1b;
    buf[1] = b'[';
    buf[2] = b'M';
    buf[3] = (code + 32) as u8;
    buf[4] = cx as u8;
    buf[5] = cy as u8;
    Some((buf, 6))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_press_release_and_modifiers() {
        let none = Mods::default();
        let (buf, len) = encode(Button::Left, Action::Press, none, 4, 9, true).unwrap();
        assert_eq!(&buf[..len], b"\x1b[<0;10;5M");
        let (buf, len) = encode(Button::Left, Action::Release, none, 4, 9, true).unwrap();
        assert_eq!(&buf[..len], b"\x1b[<0;10;5m");
        let ctrl = Mods { ctrl: true, ..Mods::default() };
        let (buf, len) = encode(Button::Right, Action::Press, ctrl, 0, 0, true).unwrap();
        assert_eq!(&buf[..len], b"\x1b[<18;1;1M");
        let (buf, len) = encode(Button::WheelDown, Action::Press, none, 1, 1, true).unwrap();
        assert_eq!(&buf[..len], b"\x1b[<65;2;2M");
        let (buf, len) = encode(Button::Left, Action::Motion, none, 0, 2, true).unwrap();
        assert_eq!(&buf[..len], b"\x1b[<32;3;1M");
        let (buf, len) = encode(Button::None, Action::Motion, none, 0, 0, true).unwrap();
        assert_eq!(&buf[..len], b"\x1b[<35;1;1M");
    }

    #[test]
    fn legacy_encoding_and_limits() {
        let none = Mods::default();
        let (buf, len) = encode(Button::Left, Action::Press, none, 0, 0, false).unwrap();
        assert_eq!(&buf[..len], b"\x1b[M\x20\x21\x21");
        let (buf, len) = encode(Button::Middle, Action::Release, none, 2, 3, false).unwrap();
        assert_eq!(&buf[..len], b"\x1b[M\x23\x24\x23");
        assert!(encode(Button::Left, Action::Press, none, 0, 300, false).is_none());
        assert!(encode(Button::Left, Action::Press, none, 0, 300, true).is_some());
    }
}
