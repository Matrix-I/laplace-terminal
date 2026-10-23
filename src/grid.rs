//! Compact 12-byte cells, a zero-allocation ring buffer with scrollback, and the
//! `vte::Perform` implementation that turns the PTY byte stream into grid state.
//!
//! Memory layout: the whole scrollback + screen lives in one flat `Vec<Cell>`
//! (`cols * (SCROLLBACK + rows)`), allocated with `alloc_zeroed` so the OS only
//! commits pages once history actually reaches them. Scrolling the full screen
//! is O(1): bump `top` and clear one row in place. Nothing is allocated while
//! printing or scrolling; allocation only happens on window resize.

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
}
