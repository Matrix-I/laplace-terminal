//! Compact 12-byte cells, a zero-allocation ring buffer with scrollback, and the
//! `vte::Perform` implementation that turns the PTY byte stream into grid state.
//!
//! Memory layout: the whole scrollback + screen lives in one flat `Vec<Cell>`
//! (`cols * (SCROLLBACK + rows)`), allocated with `alloc_zeroed` so the OS only
//! commits pages once history actually reaches them. Scrolling the full screen
//! is O(1): bump `top` and clear one row in place. Nothing is allocated while
//! printing or scrolling; allocation only happens on window resize.

use std::io::Write;
use vte::{Params, Perform};

/// Number of scrollback lines kept above the visible screen.
pub const SCROLLBACK: usize = 5000;

/// Default colors, stored as `0xRRGGBB00`. Cells store `0` to mean "default",
/// which keeps a freshly zeroed buffer valid and lets the shader apply the theme.
pub const DEFAULT_FG: u32 = 0xD8DE_E900;
pub const DEFAULT_BG: u32 = 0x1E22_2A00;

/// `fg` low byte: 0xFF = explicit color, 0x00 = default foreground.
pub const FG_SET: u32 = 0xFF;
/// `bg` low byte bit: explicit background color (otherwise default bg).
pub const BG_SET: u32 = 0x80;
/// `bg` low byte bit: draw an underline.
pub const BG_UNDERLINE: u32 = 0x01;

const ATTR_BOLD: u8 = 1 << 0;
const ATTR_DIM: u8 = 1 << 1;
const ATTR_ITALIC: u8 = 1 << 2;
const ATTR_UNDERLINE: u8 = 1 << 3;
const ATTR_INVERSE: u8 = 1 << 4;
const ATTR_HIDDEN: u8 = 1 << 5;

/// One terminal cell: exactly 12 bytes.
#[repr(C)]
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Cell {
    /// Unicode scalar value (0 = empty).
    pub codepoint: u32,
    /// Packed `0xRRGGBBAA`; AA = 0xFF if explicit, 0 for default fg.
    pub fg: u32,
    /// Packed `0xRRGGBB` + style flags in the low byte (`BG_SET`, `BG_UNDERLINE`).
    pub bg: u32,
}

const _: () = assert!(std::mem::size_of::<Cell>() == 12);

#[derive(Copy, Clone, Default)]
struct Pen {
    fg: u32, // 0 = default, else rgb<<8 | FG_SET
    bg: u32, // 0 = default, else rgb<<8 | BG_SET
    attrs: u8,
}

#[derive(Copy, Clone, Default)]
struct Cursor {
    row: usize,
    col: usize,
    pen: Pen,
    line_drawing: bool,
}

pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    /// Ring capacity in rows (`SCROLLBACK + rows`).
    cap: usize,
    /// Main screen + scrollback ring.
    cells: Vec<Cell>,
    /// Physical ring row of visible row 0.
    top: usize,
    /// Number of valid scrollback rows above `top`.
    history: usize,
    /// How many lines the user has scrolled back into history (0 = live).
    pub display_offset: usize,

    /// Alternate screen (vim, less, htop...), no scrollback.
    alt: Vec<Cell>,
    pub alt_active: bool,

    cur: Cursor,
    saved: Cursor,
    saved_main: Cursor,
    wrap_pending: bool,
    scroll_top: usize,
    scroll_bot: usize,

    pub cursor_visible: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub sync_output: bool,
    autowrap: bool,

    /// Per visible row dirty flags, plus a "everything changed" shortcut.
    pub dirty: Vec<bool>,
    pub all_dirty: bool,

    /// Bytes the terminal must send back to the shell (DSR, DA replies).
    pub response: Vec<u8>,
}

impl Grid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let cap = SCROLLBACK + rows;
        Grid {
            cols,
            rows,
            cap,
            cells: bytemuck::zeroed_vec(cols * cap),
            top: 0,
            history: 0,
            display_offset: 0,
            alt: bytemuck::zeroed_vec(cols * rows),
            alt_active: false,
            cur: Cursor::default(),
            saved: Cursor::default(),
            saved_main: Cursor::default(),
            wrap_pending: false,
            scroll_top: 0,
            scroll_bot: rows - 1,
            cursor_visible: true,
            app_cursor: false,
            bracketed_paste: false,
            sync_output: false,
            autowrap: true,
            dirty: vec![true; rows],
            all_dirty: true,
            response: Vec::with_capacity(256),
        }
    }

    // ------------------------------------------------------------------
    // Row addressing
    // ------------------------------------------------------------------

    #[inline]
    fn row_start(&self, r: usize) -> usize {
        if self.alt_active {
            r * self.cols
        } else {
            ((self.top + r) % self.cap) * self.cols
        }
    }

    #[inline]
    fn buf_mut(&mut self) -> &mut [Cell] {
        if self.alt_active {
            &mut self.alt
        } else {
            &mut self.cells
        }
    }

    #[inline]
    fn row_mut(&mut self, r: usize) -> &mut [Cell] {
        let s = self.row_start(r);
        let c = self.cols;
        &mut self.buf_mut()[s..s + c]
    }

    /// Row `r` of what should be on screen, accounting for scrollback viewing.
    #[inline]
    pub fn display_row(&self, r: usize) -> &[Cell] {
        if self.alt_active {
            let s = r * self.cols;
            &self.alt[s..s + self.cols]
        } else {
            let p = (self.top + self.cap - self.display_offset + r) % self.cap;
            let s = p * self.cols;
            &self.cells[s..s + self.cols]
        }
    }

    /// Cursor position in screen coordinates, if it should be drawn.
    pub fn cursor_pos(&self) -> Option<(usize, usize)> {
        if !self.cursor_visible {
            return None;
        }
        let off = if self.alt_active { 0 } else { self.display_offset };
        let r = self.cur.row + off;
        (r < self.rows).then(|| (self.cur.col.min(self.cols - 1), r))
    }

    /// Scroll the viewport through history (positive = towards older lines).
    pub fn scroll_display(&mut self, delta: isize) {
        if self.alt_active {
            return;
        }
        let new = (self.display_offset as isize + delta).clamp(0, self.history as isize) as usize;
        if new != self.display_offset {
            self.display_offset = new;
            self.all_dirty = true;
        }
    }

    pub fn reset_display(&mut self) {
        if self.display_offset != 0 {
            self.display_offset = 0;
            self.all_dirty = true;
        }
    }

    // ------------------------------------------------------------------
    // Primitive edits (all in place, no allocation)
    // ------------------------------------------------------------------

    #[inline]
    fn mark(&mut self, r: usize) {
        if let Some(d) = self.dirty.get_mut(r) {
            *d = true;
        }
    }

    #[inline]
    fn blank(&self) -> Cell {
        Cell { codepoint: 0, fg: 0, bg: self.cur.pen.bg }
    }

    fn clear_cols(&mut self, r: usize, c0: usize, c1: usize) {
        let b = self.blank();
        let c1 = c1.min(self.cols);
        if c0 < c1 {
            self.row_mut(r)[c0..c1].fill(b);
        }
        self.mark(r);
    }

    fn clear_rows(&mut self, r0: usize, r1: usize) {
        for r in r0..r1.min(self.rows) {
            self.clear_cols(r, 0, self.cols);
        }
    }

    fn copy_row(&mut self, src: usize, dst: usize) {
        let s = self.row_start(src);
        let d = self.row_start(dst);
        let c = self.cols;
        self.buf_mut().copy_within(s..s + c, d);
        self.mark(dst);
    }

    /// Scroll rows `t..=b` up by `n`. When the region is the whole main screen
    /// and `to_history` is set, this is an O(1) ring rotation into scrollback.
    fn scroll_up_in(&mut self, t: usize, b: usize, n: usize, to_history: bool) {
        if t > b || b >= self.rows {
            return;
        }
        let n = n.min(b - t + 1);
        if n == 0 {
            return;
        }
        if to_history && !self.alt_active && t == 0 && b == self.rows - 1 {
            for _ in 0..n {
                self.top = (self.top + 1) % self.cap;
                if self.history < self.cap - self.rows {
                    self.history += 1;
                }
                self.clear_cols(self.rows - 1, 0, self.cols);
            }
            if self.display_offset > 0 {
                // Keep the user's scrolled-back view anchored on the same lines.
                self.display_offset = (self.display_offset + n).min(self.history);
            }
            self.all_dirty = true;
        } else {
            for r in t..(b + 1 - n) {
                self.copy_row(r + n, r);
            }
            self.clear_rows(b + 1 - n, b + 1);
        }
    }

    fn scroll_down_in(&mut self, t: usize, b: usize, n: usize) {
        if t > b || b >= self.rows {
            return;
        }
        let n = n.min(b - t + 1);
        for r in (t + n..=b).rev() {
            self.copy_row(r - n, r);
        }
        self.clear_rows(t, t + n);
    }

    fn linefeed(&mut self) {
        if self.cur.row == self.scroll_bot {
            self.scroll_up_in(self.scroll_top, self.scroll_bot, 1, true);
        } else if self.cur.row + 1 < self.rows {
            self.cur.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cur.row == self.scroll_top {
            self.scroll_down_in(self.scroll_top, self.scroll_bot, 1);
        } else if self.cur.row > 0 {
            self.cur.row -= 1;
        }
    }

    /// Resolve the current pen into packed (fg, bg) cell colors.
    fn colors(&self) -> (u32, u32) {
        let p = self.cur.pen;
        let (mut fg, mut bg) = (p.fg, p.bg);
        if p.attrs & ATTR_INVERSE != 0 {
            let f = if fg == 0 { DEFAULT_FG } else { fg & !0xFF };
            let b = if bg == 0 { DEFAULT_BG } else { bg & !0xFF };
            fg = b | FG_SET;
            bg = f | BG_SET;
        }
        if p.attrs & ATTR_DIM != 0 {
            let rgb = if fg == 0 { DEFAULT_FG } else { fg & !0xFF };
            fg = ((rgb >> 1) & 0x7F7F_7F00) | FG_SET;
        }
        if p.attrs & ATTR_HIDDEN != 0 {
            fg = if bg == 0 { DEFAULT_BG | FG_SET } else { (bg & !0xFF) | FG_SET };
        }
        if p.attrs & ATTR_UNDERLINE != 0 {
            bg |= BG_UNDERLINE;
        }
        (fg, bg)
    }

    fn clamp_cursor(&mut self) {
        self.cur.row = self.cur.row.min(self.rows - 1);
        self.cur.col = self.cur.col.min(self.cols - 1);
    }

    fn set_alt(&mut self, on: bool, save_cursor: bool) {
        if on == self.alt_active {
            return;
        }
        if on {
            if save_cursor {
                self.saved_main = self.cur;
            }
            self.alt_active = true;
            self.cur.row = 0;
            self.cur.col = 0;
            let b = Cell { codepoint: 0, fg: 0, bg: 0 };
            self.alt.fill(b);
            self.display_offset = 0;
        } else {
            self.alt_active = false;
            if save_cursor {
                self.cur = self.saved_main;
                self.clamp_cursor();
            }
        }
        self.wrap_pending = false;
        self.all_dirty = true;
    }

    fn full_reset(&mut self) {
        self.set_alt(false, false);
        self.cur = Cursor::default();
        self.saved = Cursor::default();
        self.scroll_top = 0;
        self.scroll_bot = self.rows - 1;
        self.wrap_pending = false;
        self.cursor_visible = true;
        self.app_cursor = false;
        self.bracketed_paste = false;
        self.autowrap = true;
        self.clear_rows(0, self.rows);
        self.all_dirty = true;
    }

    // ------------------------------------------------------------------
    // Resize (the only place the grid allocates after startup)
    // ------------------------------------------------------------------

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let new_cap = SCROLLBACK + rows;
        let mut cells = bytemuck::zeroed_vec::<Cell>(cols * new_cap);

        // Logical line 0 = oldest history line; the screen starts at `history`.
        let main_row = if self.alt_active { self.saved_main.row } else { self.cur.row };
        let total = self.history + self.rows;
        let mut new_top = self.history;
        if rows < self.rows {
            // Shrinking: push lines into history only as needed to keep the cursor visible.
            new_top += (main_row + 1).saturating_sub(rows);
        } else {
            // Growing: pull lines back out of history.
            new_top = new_top.saturating_sub(rows - self.rows);
        }
        let new_hist = new_top.min(new_cap - rows);
        let start = new_top - new_hist;
        let n_lines = total.saturating_sub(start).min(new_hist + rows);
        let w = cols.min(self.cols);
        for i in 0..n_lines {
            let op = (self.top + self.cap - self.history + start + i) % self.cap;
            let src = op * self.cols;
            cells[i * cols..i * cols + w].copy_from_slice(&self.cells[src..src + w]);
        }
        let new_main_row =
            ((main_row + self.history) as isize - new_top as isize).clamp(0, rows as isize - 1) as usize;

        let mut alt = bytemuck::zeroed_vec::<Cell>(cols * rows);
        for r in 0..rows.min(self.rows) {
            alt[r * cols..r * cols + w].copy_from_slice(&self.alt[r * self.cols..r * self.cols + w]);
        }

        self.cells = cells;
        self.alt = alt;
        self.cap = new_cap;
        self.top = new_hist;
        self.history = new_hist;
        self.display_offset = 0;
        if self.alt_active {
            self.saved_main.row = new_main_row;
        } else {
            self.cur.row = new_main_row;
        }
        self.cols = cols;
        self.rows = rows;
        self.clamp_cursor();
        for c in [&mut self.saved, &mut self.saved_main] {
            c.row = c.row.min(rows - 1);
            c.col = c.col.min(cols - 1);
        }
        self.scroll_top = 0;
        self.scroll_bot = rows - 1;
        self.wrap_pending = false;
        self.dirty = vec![true; rows];
        self.all_dirty = true;
    }

    // ------------------------------------------------------------------
    // SGR
    // ------------------------------------------------------------------

    fn sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.cur.pen = Pen::default();
            return;
        }
        let mut it = params.iter();
        while let Some(sp) = it.next() {
            let pen = &mut self.cur.pen;
            match sp[0] {
                0 => *pen = Pen::default(),
                1 => pen.attrs |= ATTR_BOLD,
                2 => pen.attrs |= ATTR_DIM,
                3 => pen.attrs |= ATTR_ITALIC,
                4 => pen.attrs |= ATTR_UNDERLINE,
                7 => pen.attrs |= ATTR_INVERSE,
                8 => pen.attrs |= ATTR_HIDDEN,
                21 | 22 => pen.attrs &= !(ATTR_BOLD | ATTR_DIM),
                23 => pen.attrs &= !ATTR_ITALIC,
                24 => pen.attrs &= !ATTR_UNDERLINE,
                27 => pen.attrs &= !ATTR_INVERSE,
                28 => pen.attrs &= !ATTR_HIDDEN,
                n @ 30..=37 => pen.fg = palette((n - 30) as u8) | FG_SET,
                38 => {
                    if let Some(c) = ext_color(sp, &mut it) {
                        self.cur.pen.fg = c | FG_SET;
                    }
                }
                39 => pen.fg = 0,
                n @ 40..=47 => pen.bg = palette((n - 40) as u8) | BG_SET,
                48 => {
                    if let Some(c) = ext_color(sp, &mut it) {
                        self.cur.pen.bg = c | BG_SET;
                    }
                }
                49 => pen.bg = 0,
                n @ 90..=97 => pen.fg = palette((n - 90 + 8) as u8) | FG_SET,
                n @ 100..=107 => pen.bg = palette((n - 100 + 8) as u8) | BG_SET,
                _ => {}
            }
        }
    }
}

/// Parse `38;5;n`, `38;2;r;g;b` and their colon sub-parameter forms.
fn ext_color<'a>(sp: &[u16], it: &mut impl Iterator<Item = &'a [u16]>) -> Option<u32> {
    if sp.len() > 1 {
        match sp[1] {
            5 => sp.get(2).map(|&n| palette(n as u8)),
            2 => {
                let o = if sp.len() >= 6 { 3 } else { 2 };
                Some(rgb(*sp.get(o)?, *sp.get(o + 1)?, *sp.get(o + 2)?))
            }
            _ => None,
        }
    } else {
        match it.next()?[0] {
            5 => Some(palette(it.next()?[0] as u8)),
            2 => {
                let r = it.next()?[0];
                let g = it.next()?[0];
                let b = it.next()?[0];
                Some(rgb(r, g, b))
            }
            _ => None,
        }
    }
}

#[inline]
fn rgb(r: u16, g: u16, b: u16) -> u32 {
    ((r.min(255) as u32) << 24) | ((g.min(255) as u32) << 16) | ((b.min(255) as u32) << 8)
}

/// xterm 256-color palette, returned as `0xRRGGBB00`.
fn palette(i: u8) -> u32 {
    const BASE: [u32; 16] = [
        0x3B4252, 0xBF616A, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x88C0D0, 0xE5E9F0, //
        0x4C566A, 0xD08770, 0xB5D19C, 0xF0D399, 0x8FBCBB, 0xC895BF, 0x93CCDC, 0xECEFF4,
    ];
    match i {
        0..=15 => BASE[i as usize] << 8,
        16..=231 => {
            let i = i - 16;
            let lv = |v: u8| if v == 0 { 0u16 } else { 55 + 40 * v as u16 };
            rgb(lv(i / 36), lv((i / 6) % 6), lv(i % 6))
        }
        _ => {
            let v = 8 + 10 * (i - 232) as u16;
            rgb(v, v, v)
        }
    }
}

/// Display width of a character: 0 (combining), 1, or 2 (CJK / emoji).
fn char_width(c: char) -> usize {
    let c = c as u32;
    match c {
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
        | 0x200B..=0x200F | 0xFE00..=0xFE0F => 0,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6 | 0x1F300..=0x1F64F | 0x1F900..=0x1F9FF | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// DEC Special Graphics (ESC ( 0) used by tmux, mc, etc. for box drawing.
fn dec_line_drawing(c: char) -> char {
    match c {
        '`' => '◆', 'a' => '▒', 'f' => '°', 'g' => '±', 'j' => '┘', 'k' => '┐', 'l' => '┌',
        'm' => '└', 'n' => '┼', 'o' => '⎺', 'p' => '⎻', 'q' => '─', 'r' => '⎼', 's' => '⎽',
        't' => '├', 'u' => '┤', 'v' => '┴', 'w' => '┬', 'x' => '│', 'y' => '≤', 'z' => '≥',
        '{' => 'π', '|' => '≠', '}' => '£', '~' => '·',
        _ => c,
    }
}

impl Perform for Grid {
    fn print(&mut self, c: char) {
        let c = if self.cur.line_drawing { dec_line_drawing(c) } else { c };
        let mut w = char_width(c);
        if w == 0 {
            return;
        }
        if w == 2 && self.cols < 2 {
            w = 1;
        }
        if self.wrap_pending {
            self.wrap_pending = false;
            if self.autowrap {
                self.cur.col = 0;
                self.linefeed();
            }
        }
        if w == 2 && self.cur.col + 1 >= self.cols {
            if !self.autowrap {
                return;
            }
            self.clear_cols(self.cur.row, self.cur.col, self.cols);
            self.cur.col = 0;
            self.linefeed();
        }
        let (fg, bg) = self.colors();
        let (row, col) = (self.cur.row, self.cur.col);
        {
            let r = self.row_mut(row);
            r[col] = Cell { codepoint: c as u32, fg, bg };
            if w == 2 {
                r[col + 1] = Cell { codepoint: 0, fg, bg };
            }
        }
        self.mark(row);
        let next = col + w;
        if next >= self.cols {
            self.cur.col = self.cols - 1;
            self.wrap_pending = self.autowrap;
        } else {
            self.cur.col = next;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => {
                self.cur.col = self.cur.col.saturating_sub(1);
                self.wrap_pending = false;
            }
            0x09 => {
                self.cur.col = ((self.cur.col / 8 + 1) * 8).min(self.cols - 1);
            }
            0x0A..=0x0C => {
                self.linefeed();
                self.wrap_pending = false;
            }
            0x0D => {
                self.cur.col = 0;
                self.wrap_pending = false;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        if action == 'm' {
            if intermediates.is_empty() {
                self.sgr(params);
            }
            return;
        }

        // Flatten the first value of each param into a fixed stack array.
        let mut p = [0u16; 16];
        let mut n = 0;
        for sp in params.iter() {
            if n < p.len() {
                p[n] = sp[0];
                n += 1;
            }
        }
        let raw = |i: usize| if i < n { p[i] as usize } else { 0 };
        let arg = |i: usize, d: usize| match raw(i) {
            0 => d,
            v => v,
        };
        let private = intermediates.first() == Some(&b'?');
        let (rows, cols) = (self.rows, self.cols);
        self.wrap_pending = false;

        match action {
            'A' => self.cur.row = self.cur.row.saturating_sub(arg(0, 1)),
            'B' | 'e' => self.cur.row = (self.cur.row + arg(0, 1)).min(rows - 1),
            'C' | 'a' => self.cur.col = (self.cur.col + arg(0, 1)).min(cols - 1),
            'D' => self.cur.col = self.cur.col.saturating_sub(arg(0, 1)),
            'E' => {
                self.cur.row = (self.cur.row + arg(0, 1)).min(rows - 1);
                self.cur.col = 0;
            }
            'F' => {
                self.cur.row = self.cur.row.saturating_sub(arg(0, 1));
                self.cur.col = 0;
            }
            'G' | '`' => self.cur.col = (arg(0, 1) - 1).min(cols - 1),
            'd' => self.cur.row = (arg(0, 1) - 1).min(rows - 1),
            'H' | 'f' => {
                self.cur.row = (arg(0, 1) - 1).min(rows - 1);
                self.cur.col = (arg(1, 1) - 1).min(cols - 1);
            }
            'J' => {
                let (r, c) = (self.cur.row, self.cur.col);
                match raw(0) {
                    0 => {
                        self.clear_cols(r, c, cols);
                        self.clear_rows(r + 1, rows);
                    }
                    1 => {
                        self.clear_rows(0, r);
                        self.clear_cols(r, 0, c + 1);
                    }
                    2 => self.clear_rows(0, rows),
                    3 => {
                        self.history = 0;
                        self.display_offset = 0;
                        self.all_dirty = true;
                    }
                    _ => {}
                }
            }
            'K' => {
                let (r, c) = (self.cur.row, self.cur.col);
                match raw(0) {
                    0 => self.clear_cols(r, c, cols),
                    1 => self.clear_cols(r, 0, c + 1),
                    2 => self.clear_cols(r, 0, cols),
                    _ => {}
                }
            }
            'L' => {
                if (self.scroll_top..=self.scroll_bot).contains(&self.cur.row) {
                    self.scroll_down_in(self.cur.row, self.scroll_bot, arg(0, 1));
                    self.cur.col = 0;
                }
            }
            'M' => {
                if (self.scroll_top..=self.scroll_bot).contains(&self.cur.row) {
                    self.scroll_up_in(self.cur.row, self.scroll_bot, arg(0, 1), false);
                    self.cur.col = 0;
                }
            }
            '@' => {
                let (r, c) = (self.cur.row, self.cur.col);
                let k = arg(0, 1).min(cols - c);
                let b = self.blank();
                let row = self.row_mut(r);
                row.copy_within(c..cols - k, c + k);
                row[c..c + k].fill(b);
                self.mark(r);
            }
            'P' => {
                let (r, c) = (self.cur.row, self.cur.col);
                let k = arg(0, 1).min(cols - c);
                let b = self.blank();
                let row = self.row_mut(r);
                row.copy_within(c + k..cols, c);
                row[cols - k..].fill(b);
                self.mark(r);
            }
            'X' => {
                let (r, c) = (self.cur.row, self.cur.col);
                self.clear_cols(r, c, c + arg(0, 1));
            }
            'S' if !private => self.scroll_up_in(self.scroll_top, self.scroll_bot, arg(0, 1), true),
            'T' if !private => self.scroll_down_in(self.scroll_top, self.scroll_bot, arg(0, 1)),
            'r' if !private => {
                let t = arg(0, 1) - 1;
                let b = arg(1, rows).min(rows) - 1;
                if t < b {
                    self.scroll_top = t;
                    self.scroll_bot = b;
                    self.cur.row = 0;
                    self.cur.col = 0;
                }
            }
            's' if !private => self.saved = self.cur,
            'u' if !private => {
                self.cur = self.saved;
                self.clamp_cursor();
            }
            'h' | 'l' if private => {
                let on = action == 'h';
                for &mode in &p[..n] {
                    match mode {
                        1 => self.app_cursor = on,
                        7 => self.autowrap = on,
                        25 => {
                            if self.cursor_visible != on {
                                self.cursor_visible = on;
                                self.mark(self.cur.row);
                            }
                        }
                        47 | 1047 => self.set_alt(on, false),
                        1049 => self.set_alt(on, true),
                        2004 => self.bracketed_paste = on,
                        2026 => self.sync_output = on,
                        _ => {}
                    }
                }
            }
            'n' => match raw(0) {
                5 => self.response.extend_from_slice(b"\x1b[0n"),
                6 => {
                    if private {
                        let _ = write!(self.response, "\x1b[?{};{}R", self.cur.row + 1, self.cur.col + 1);
                    } else {
                        let _ = write!(self.response, "\x1b[{};{}R", self.cur.row + 1, self.cur.col + 1);
                    }
                }
                _ => {}
            },
            'p' if private && intermediates.last() == Some(&b'$') => {
                // DECRQM (Request Mode): reply with 2 (reset) or 1 (set) so shells never hang waiting.
                let mode = raw(0);
                let state = match mode {
                    1 => if self.app_cursor { 1 } else { 2 },
                    7 => if self.autowrap { 1 } else { 2 },
                    25 => if self.cursor_visible { 1 } else { 2 },
                    1049 => if self.alt_active { 1 } else { 2 },
                    2004 => if self.bracketed_paste { 1 } else { 2 },
                    2026 => if self.sync_output { 1 } else { 2 },
                    _ => 0,
                };
                let _ = write!(self.response, "\x1b[?{mode};{state}$y");
            }
            'c' if intermediates.is_empty() => self.response.extend_from_slice(b"\x1b[?6c"),
            'c' if intermediates == b">" => self.response.extend_from_slice(b"\x1b[>0;0;0c"),
            'c' if intermediates == b"=" => self.response.extend_from_slice(b"\x1bP!|00000000\x1b\\"),
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        if params.len() < 2 || params[1] != b"?" {
            return;
        }
        let st = if bell_terminated { "\x07" } else { "\x1b\\" };
        match params[0] {
            b"10" => {
                let r = (DEFAULT_FG >> 24) & 0xFF;
                let g = (DEFAULT_FG >> 16) & 0xFF;
                let b = (DEFAULT_FG >> 8) & 0xFF;
                let _ = write!(self.response, "\x1b]10;rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{st}");
            }
            b"11" => {
                let r = (DEFAULT_BG >> 24) & 0xFF;
                let g = (DEFAULT_BG >> 16) & 0xFF;
                let b = (DEFAULT_BG >> 8) & 0xFF;
                let _ = write!(self.response, "\x1b]11;rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{st}");
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates, byte) {
            ([], b'7') => self.saved = self.cur,
            ([], b'8') => {
                self.cur = self.saved;
                self.clamp_cursor();
            }
            ([], b'D') => self.linefeed(),
            ([], b'E') => {
                self.cur.col = 0;
                self.linefeed();
            }
            ([], b'M') => self.reverse_index(),
            ([], b'c') => self.full_reset(),
            ([b'('], b'0') => self.cur.line_drawing = true,
            ([b'('], b'B') => self.cur.line_drawing = false,
            _ => {}
        }
        self.wrap_pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(g: &mut Grid, s: &str) {
        let mut p = vte::Parser::new();
        for b in s.bytes() {
            p.advance(g, b);
        }
    }

    fn line(g: &Grid, r: usize) -> String {
        g.display_row(r)
            .iter()
            .map(|c| char::from_u32(c.codepoint).filter(|&c| c != '\0').unwrap_or(' '))
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn print_and_wrap() {
        let mut g = Grid::new(5, 3);
        feed(&mut g, "hello world");
        assert_eq!(line(&g, 0), "hello");
        assert_eq!(line(&g, 1), " worl");
        assert_eq!(line(&g, 2), "d");
    }

    #[test]
    fn scroll_into_history() {
        let mut g = Grid::new(4, 2);
        feed(&mut g, "a\r\nb\r\nc\r\nd");
        assert_eq!(line(&g, 0), "c");
        assert_eq!(line(&g, 1), "d");
        g.scroll_display(2);
        assert_eq!(line(&g, 0), "a");
        assert_eq!(line(&g, 1), "b");
    }

    #[test]
    fn csi_cursor_and_erase() {
        let mut g = Grid::new(6, 3);
        feed(&mut g, "abcdef\x1b[2;3HX\x1b[1;1H\x1b[K");
        assert_eq!(line(&g, 0), "");
        assert_eq!(line(&g, 1), "  X");
    }

    #[test]
    fn sgr_colors() {
        let mut g = Grid::new(4, 1);
        feed(&mut g, "\x1b[31;48;2;1;2;3mx");
        let c = g.display_row(0)[0];
        assert_eq!(c.fg, palette(1) | FG_SET);
        assert_eq!(c.bg, 0x0102_0300 | BG_SET);
    }

    #[test]
    fn alt_screen_restores() {
        let mut g = Grid::new(4, 2);
        feed(&mut g, "main\x1b[?1049hALT");
        assert_eq!(line(&g, 0), "ALT");
        feed(&mut g, "\x1b[?1049l");
        assert_eq!(line(&g, 0), "main");
    }

    #[test]
    fn scroll_region() {
        let mut g = Grid::new(3, 4);
        feed(&mut g, "1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[3;1H\n");
        assert_eq!(line(&g, 0), "1");
        assert_eq!(line(&g, 1), "3");
        assert_eq!(line(&g, 2), "");
        assert_eq!(line(&g, 3), "4");
    }

    #[test]
    fn resize_keeps_cursor_line() {
        let mut g = Grid::new(4, 4);
        feed(&mut g, "a\r\nb\r\nc\r\nd");
        g.resize(4, 2);
        assert_eq!(line(&g, 0), "c");
        assert_eq!(line(&g, 1), "d");
        g.resize(4, 4);
        assert_eq!(line(&g, 0), "a");
        assert_eq!(line(&g, 3), "d");
    }

    #[test]
    fn dsr_reply() {
        let mut g = Grid::new(10, 5);
        feed(&mut g, "\x1b[3;4H\x1b[6n");
        assert_eq!(g.response, b"\x1b[3;4R");
    }
}
