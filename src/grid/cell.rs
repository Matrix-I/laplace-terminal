//! Terminal Cell and associated attribute flags and cursors.
//!
//! Compact 12-byte cells packed with Unicode scalar and style attributes.

/// `fg` low byte bit: explicit foreground color (otherwise default fg).
pub const FG_SET: u32 = 0x80;
/// `fg` low byte bit: bold glyph.
pub const FG_BOLD: u32 = 0x01;
/// `fg` low byte bit: italic glyph.
pub const FG_ITALIC: u32 = 0x02;
/// `fg` low byte bits 2..=6: low 5 bits of the cell's extra-attribute index.
pub const FG_EXTRA: u32 = 0x7C;
/// `bg` low byte bit: explicit background color (otherwise default bg).
pub const BG_SET: u32 = 0x80;
/// `bg` low byte bits 0..=2: underline style (`UNDERLINE_*`, 0 = none).
pub const BG_UNDERLINE_MASK: u32 = 0x07;
/// `bg` low byte bit: single underline (`UNDERLINE_SINGLE` in the style bits).
pub const BG_UNDERLINE: u32 = UNDERLINE_SINGLE as u32;
/// `bg` low byte bit: strikethrough.
pub const BG_STRIKE: u32 = 0x08;
/// `bg` low byte bits 4..=6: high 3 bits of the cell's extra-attribute index.
pub const BG_EXTRA: u32 = 0x70;

/// Underline styles (SGR `4:x`), stored in `BG_UNDERLINE_MASK`:
/// 1 single, 2 double, 3 curly, 4 dotted, 5 dashed.
pub const UNDERLINE_SINGLE: u8 = 1;
pub const UNDERLINE_DOUBLE: u8 = 2;
pub const UNDERLINE_DASHED: u8 = 5;

pub const ATTR_BOLD: u8 = 1 << 0;
pub const ATTR_DIM: u8 = 1 << 1;
pub const ATTR_ITALIC: u8 = 1 << 2;
pub const ATTR_STRIKE: u8 = 1 << 3;
pub const ATTR_INVERSE: u8 = 1 << 4;
pub const ATTR_HIDDEN: u8 = 1 << 5;

/// Kitty unicode placeholder: a cell showing part of a virtual image placement.
pub const PLACEHOLDER: char = '\u{10EEEE}';
/// A placeholder cell's `codepoint`: `PH_FLAG | count << 27 | msb << 18 | row << 9 | col`,
/// outside the Unicode range. `count` is how many of the row / column / image id
/// high byte diacritics followed it (each an index into `PLACEHOLDER_DIACRITICS`).
pub const PH_FLAG: u32 = 1 << 31;

/// A multi-codepoint grapheme (emoji ZWJ sequence, flag, skin tone, a base
/// with marks that have no precomposed form): `codepoint` is
/// `CLUSTER_FLAG | CLUSTER_WIDE? | index into Grid::clusters`.
pub const CLUSTER_FLAG: u32 = 1 << 30;
pub const CLUSTER_WIDE: u32 = 1 << 29;
pub const CLUSTER_ID: u32 = CLUSTER_WIDE - 1;
/// Distinct clusters one grid keeps; past that, marks are dropped as before.
pub const MAX_CLUSTERS: usize = 1 << 16;
/// Longest cluster kept (UTF-8 bytes); longer sequences stop growing.
pub const MAX_CLUSTER_BYTES: usize = 64;

/// Upper bound on an OSC 8 URI; longer links are ignored.
pub const MAX_LINK_LEN: usize = 2048;

/// One terminal cell: exactly 12 bytes.
#[repr(C)]
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Cell {
    /// Unicode scalar value (0 = empty).
    pub codepoint: u32,
    /// Packed `0xRRGGBB` + flags in the low byte (`FG_SET`, `FG_BOLD`, `FG_ITALIC`,
    /// low bits of the extra index).
    pub fg: u32,
    /// Packed `0xRRGGBB` + style flags in the low byte (`BG_SET`, underline style,
    /// `BG_STRIKE`, high bits of the extra index).
    pub bg: u32,
}

const _: () = assert!(std::mem::size_of::<Cell>() == 12);

impl Cell {
    /// Index into `Grid::extras` (0 = no hyperlink and default underline color).
    #[inline]
    pub fn extra(&self) -> u8 {
        (((self.fg & FG_EXTRA) >> 2) | ((self.bg & BG_EXTRA) << 1)) as u8
    }
}

/// Pack an extra-attribute index into the spare low bits of `fg` / `bg`.
#[inline]
pub fn with_extra(fg: u32, bg: u32, extra: u8) -> (u32, u32) {
    let e = extra as u32;
    ((fg & !FG_EXTRA) | ((e & 0x1F) << 2), (bg & !BG_EXTRA) | ((e >> 5) << 4))
}

/// Attributes too large for a `Cell`, shared by every cell that uses them: an
/// OSC 8 hyperlink and an SGR 58 underline color. Cells refer to them by index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellExtra {
    pub link: Option<OscLink>,
    /// `0xRRGGBB00` | 1, or 0 for "same as the text color".
    pub underline_color: u32,
}

/// An OSC 8 hyperlink: `ESC ] 8 ; id=... ; URI ST`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OscLink {
    pub id: Box<str>,
    pub uri: Box<str>,
}

#[derive(Copy, Clone, Default)]
pub struct Pen {
    pub fg: u32, // 0 = default, else rgb<<8 | FG_SET
    pub bg: u32, // 0 = default, else rgb<<8 | BG_SET
    pub attrs: u8,
    /// Underline style (`UNDERLINE_*`).
    pub underline: u8,
    /// Index into `Grid::extras` (hyperlink + underline color), 0 = none.
    pub extra: u8,
}

#[derive(Copy, Clone, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
    pub pen: Pen,
    pub line_drawing: bool,
    pub origin_mode: bool,
}

/// Kitty keyboard protocol flags for one screen: the active flags plus the
/// entries saved below them by `CSI > flags u` (at most 8, oldest dropped first).
#[derive(Copy, Clone, Debug, Default)]
pub struct KittyKeyboard {
    pub flags: u8,
    pub saved: [u8; 8],
    pub len: usize,
}

impl KittyKeyboard {
    /// The five progressive-enhancement bits the protocol defines.
    pub const MASK: u8 = 0b1_1111;

    pub fn push(&mut self, flags: u8) {
        if self.len == self.saved.len() {
            self.saved.copy_within(1.., 0);
            self.len -= 1;
        }
        self.saved[self.len] = self.flags;
        self.len += 1;
        self.flags = flags & Self::MASK;
    }

    pub fn pop(&mut self, n: usize) {
        for _ in 0..n {
            if self.len == 0 {
                self.flags = 0;
                return;
            }
            self.len -= 1;
            self.flags = self.saved[self.len];
        }
    }

    /// `CSI = flags ; mode u`: 1 replaces, 2 sets bits, 3 clears bits.
    pub fn set(&mut self, flags: u8, mode: usize) {
        let flags = flags & Self::MASK;
        match mode {
            1 => self.flags = flags,
            2 => self.flags |= flags,
            3 => self.flags &= !flags,
            _ => {}
        }
    }
}

/// Mouse reporting requested by the application (DECSET 9 / 1000 / 1002 / 1003).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum MouseTracking {
    #[default]
    Off,
    /// Button presses and releases only.
    Click,
    /// Presses, releases, and motion while a button is held.
    Drag,
    /// All motion, with or without a button held.
    Motion,
}
