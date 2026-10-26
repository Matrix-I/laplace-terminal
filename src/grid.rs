//! Compact 12-byte cells, a zero-allocation ring buffer with scrollback, and the
//! `vte::Perform` implementation that turns the PTY byte stream into grid state.
//!
//! Memory layout: the whole scrollback + screen lives in one flat `Vec<Cell>`
//! (`cols * (SCROLLBACK + rows)`), allocated with `alloc_zeroed` so the OS only
//! commits pages once history actually reaches them. Scrolling the full screen
//! is O(1): bump `top` and clear one row in place. Nothing is allocated while
//! printing or scrolling; allocation only happens on window resize.

use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use vte::{Params, Perform};

use crate::search::SearchText;

/// Number of scrollback lines kept above the visible screen.
pub const SCROLLBACK: usize = 5000;

/// Default colors, stored as `0xRRGGBB00`. Cells store `0` to mean "default",
/// which keeps a freshly zeroed buffer valid and lets the shader apply the theme.
/// They are runtime-configurable (`background` / `foreground` config options).
static DEFAULT_FG_RGB: AtomicU32 = AtomicU32::new(0xE6E6_E600);
static DEFAULT_BG_RGB: AtomicU32 = AtomicU32::new(0x0000_0000);

#[inline]
pub fn default_fg() -> u32 {
    DEFAULT_FG_RGB.load(Ordering::Relaxed)
}

#[inline]
pub fn default_bg() -> u32 {
    DEFAULT_BG_RGB.load(Ordering::Relaxed)
}

pub fn set_default_colors(fg: u32, bg: u32) {
    DEFAULT_FG_RGB.store(fg & !0xFF, Ordering::Relaxed);
    DEFAULT_BG_RGB.store(bg & !0xFF, Ordering::Relaxed);
}

/// ANSI 16-color base palette (`0xRRGGBB`).
static PALETTE_BASE: [AtomicU32; 16] = [
    AtomicU32::new(0x121316),
    AtomicU32::new(0xF38BA8),
    AtomicU32::new(0xA6E3A1),
    AtomicU32::new(0xF5A97F),
    AtomicU32::new(0x00BFE3),
    AtomicU32::new(0xCBA6F7),
    AtomicU32::new(0x89DCEB),
    AtomicU32::new(0xD9E0EE),
    AtomicU32::new(0x585B70),
    AtomicU32::new(0xF38BA8),
    AtomicU32::new(0xA6E3A1),
    AtomicU32::new(0xF9E2AF),
    AtomicU32::new(0x74C7EC),
    AtomicU32::new(0xF5C2E7),
    AtomicU32::new(0x94E2D5),
    AtomicU32::new(0xFFFFFF),
];

pub fn set_palette(pal: &[u32; 16]) {
    for (i, &col) in pal.iter().enumerate() {
        PALETTE_BASE[i].store(col, Ordering::Relaxed);
    }
}

/// `fg` low byte bit: explicit foreground color (otherwise default fg).
pub const FG_SET: u32 = 0x80;
/// `fg` low byte bit: bold glyph.
pub const FG_BOLD: u32 = 0x01;
/// `fg` low byte bit: italic glyph.
pub const FG_ITALIC: u32 = 0x02;
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
    /// Packed `0xRRGGBB` + flags in the low byte (`FG_SET`, `FG_BOLD`, `FG_ITALIC`).
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
    origin_mode: bool,
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

/// A text selection. The public API uses visible `(row, col)` coordinates; the grid
/// stores rows as absolute line numbers so the selection stays on its text while
/// output scrolls and while the viewport moves through history.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub start: (usize, usize), // (row, col)
    pub end: (usize, usize),   // (row, col)
}

/// Search matches on one visible row (see `Grid::search_hits`).
pub struct SearchHits<'a> {
    line: usize,
    /// Index of the focused match within `hits`, if it is on this row.
    current: Option<usize>,
    hits: &'a [Selection],
}

/// How a cell is highlighted by find-in-terminal.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SearchHit {
    None,
    Match,
    Current,
}

impl SearchHits<'_> {
    #[inline]
    pub fn at(&self, col: usize) -> SearchHit {
        let pos = (self.line, col);
        match self.hits.iter().position(|m| m.start <= pos && pos <= m.end) {
            None => SearchHit::None,
            Some(i) if Some(i) == self.current => SearchHit::Current,
            Some(_) => SearchHit::Match,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hyperlink {
    pub row: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub url: String,
    pub is_path: bool,
}

impl Selection {
    #[inline]
    pub fn ordered(self) -> ((usize, usize), (usize, usize)) {
        if self.start <= self.end {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }
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

    /// Per physical main-screen row: the line continues on the next row (soft wrap).
    wrapped: Vec<bool>,
    /// Horizontal tab stops, one flag per column.
    tab_stops: Vec<bool>,
    /// IRM: printing shifts the rest of the line right instead of overwriting.
    insert_mode: bool,
    /// Lines ever pushed into scrollback; anchors absolute line numbers.
    lines_pushed: usize,

    pub cursor_visible: bool,
    /// Last DECSCUSR (`CSI Ps SP q`) value: 0 = config default,
    /// 1/2 = blinking/steady block, 3/4 = underline, 5/6 = bar.
    pub cursor_shape: u8,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub sync_output: bool,
    /// When synchronized output (DECSET 2026) began; rendering is deferred meanwhile.
    pub sync_started: Option<std::time::Instant>,
    autowrap: bool,

    pub mouse_tracking: MouseTracking,
    /// SGR mouse encoding (DECSET 1006).
    pub mouse_sgr: bool,
    /// Report focus in/out (DECSET 1004).
    pub focus_events: bool,
    /// Translate the mouse wheel into arrow keys on the alternate screen (DECSET 1007).
    pub alt_scroll: bool,

    /// Window/tab title set via OSC 0 / OSC 2 (e.g. `user@host:~/dir` or running command).
    pub title: String,
    /// Set when the title or the reported working directory changes.
    pub title_changed: bool,
    /// Working directory reported by the shell via OSC 7 (`file://host/path`).
    pub reported_cwd: Option<String>,
    /// Text an application asked to place on the clipboard via OSC 52.
    pub clipboard_request: Option<String>,
    /// OSC 133;B: absolute line and column where the command input starts.
    prompt_input_mark: Option<(usize, usize)>,

    /// Active mouse text selection (rows are absolute line numbers).
    selection: Option<Selection>,
    /// Find-in-terminal matches (absolute lines, top-to-bottom, non-overlapping).
    search_matches: Vec<Selection>,
    /// Index of the focused match in `search_matches`.
    search_current: Option<usize>,

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
        let max_cap = SCROLLBACK + rows;
        // Start lean: allocate only 2x rows (minimum 64) instead of the full 5000+ scrollback ring.
        // The ring buffer doubles dynamically on demand up to max_cap when lines scroll off screen.
        let cap = (rows * 2).max(64).min(max_cap);
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
            wrapped: vec![false; cap],
            tab_stops: default_tab_stops(cols),
            insert_mode: false,
            lines_pushed: 0,
            cursor_visible: true,
            cursor_shape: 0,
            app_cursor: false,
            bracketed_paste: false,
            sync_output: false,
            sync_started: None,
            autowrap: true,
            mouse_tracking: MouseTracking::Off,
            mouse_sgr: false,
            focus_events: false,
            alt_scroll: true,
            title: String::new(),
            title_changed: false,
            reported_cwd: None,
            clipboard_request: None,
            prompt_input_mark: None,
            selection: None,
            search_matches: Vec::new(),
            search_current: None,
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
    /// Absolute line number of visible row `r`.
    #[inline]
    fn abs_line(&self, r: usize) -> usize {
        if self.alt_active {
            self.lines_pushed + r
        } else {
            self.lines_pushed + r - self.display_offset
        }
    }

    /// Cells of absolute line `l`, if it is still in scrollback or on screen.
    fn line_cells(&self, l: usize) -> Option<&[Cell]> {
        let c = self.cols;
        if self.alt_active {
            let r = l.checked_sub(self.lines_pushed).filter(|&r| r < self.rows)?;
            return Some(&self.alt[r * c..(r + 1) * c]);
        }
        let i = l.checked_sub(self.lines_pushed - self.history).filter(|&i| i < self.history + self.rows)?;
        let p = (self.top + self.cap - self.history + i) % self.cap;
        Some(&self.cells[p * c..(p + 1) * c])
    }

    /// Whether absolute line `l` soft-wraps onto the next line.
    fn line_wrapped(&self, l: usize) -> bool {
        if self.alt_active {
            return false;
        }
        l.checked_sub(self.lines_pushed - self.history)
            .filter(|&i| i < self.history + self.rows)
            .is_some_and(|i| self.wrapped[(self.top + self.cap - self.history + i) % self.cap])
    }

    #[inline]
    fn set_wrapped(&mut self, r: usize, on: bool) {
        if !self.alt_active {
            let p = (self.top + r) % self.cap;
            self.wrapped[p] = on;
        }
    }

    pub fn cursor_pos(&self) -> Option<(usize, usize)> {
        if !self.cursor_visible {
            return None;
        }
        let off = if self.alt_active { 0 } else { self.display_offset };
        let r = self.cur.row + off;
        (r < self.rows).then(|| (self.cur.col.min(self.cols - 1), r))
    }

    /// Extract text typed by user on the active prompt line (before the cursor).
    pub fn current_prompt_input(&self) -> String {
        if self.cur.row >= self.rows || self.cols == 0 {
            return String::new();
        }
        let (cmd_start, _) = self.prompt_command_range();
        let r = self.display_row(self.cur.row);
        let end_col = self.cur.col.min(r.len());
        if cmd_start >= end_col {
            return String::new();
        }
        r[cmd_start..end_col]
            .iter()
            .filter_map(|c| if c.codepoint != 0 { char::from_u32(c.codepoint) } else { None })
            .collect::<String>()
            .trim()
            .to_string()
    }

    /// Returns the column range `(cmd_start_col, cmd_end_col)` for the active command line on `self.cur.row`.
    pub fn prompt_command_range(&self) -> (usize, usize) {
        if self.cur.row >= self.rows || self.cols == 0 {
            return (0, 0);
        }
        let r = self.display_row(self.cur.row);
        let cur_col = self.cur.col.min(self.cols);
        let cmd_start = match self.prompt_input_mark {
            // Shell integration (OSC 133;B) marked exactly where the input begins.
            Some((line, col)) if line == self.abs_line(self.cur.row) && col <= cur_col => col,
            _ => self.guess_command_start(r, cur_col),
        };

        let last_non_blank = r[cmd_start..]
            .iter()
            .rposition(|c| !Self::is_blank(c.codepoint))
            .map(|idx| cmd_start + idx + 1)
            .unwrap_or(cmd_start);

        let cmd_end = last_non_blank.max(cur_col).min(self.cols);
        (cmd_start, cmd_end)
    }

    /// Guess where the command input starts on a prompt line from common prompt
    /// glyphs (Powerline separators, `$`, `%`, `❯`, ...) when no OSC 133 mark exists.
    fn guess_command_start(&self, r: &[Cell], cur_col: usize) -> usize {
        // 1. Check for Powerline segment separator U+E0B0 ('') before or at cursor
        let mut pl_end = None;
        if let Some(last_pl) = r.iter().take(cur_col + 1).rposition(|c| c.codepoint == 0xE0B0) {
            let mut sc = last_pl + 1;
            while sc < self.cols && Self::is_blank(r[sc].codepoint) {
                sc += 1;
            }
            if sc < self.cols
                && matches!(
                    char::from_u32(r[sc].codepoint),
                    Some('$' | '%' | '#' | '❯' | '>' | '➜')
                )
            {
                sc += 1;
                while sc < self.cols && Self::is_blank(r[sc].codepoint) {
                    sc += 1;
                }
            }
            pl_end = Some(sc.min(cur_col));
        }

        // 2. Search for standard prompt signs using exact column-level matching
        let mut sign_end = None;
        const PROMPT_SIGNS: &[&[char]] = &[
            &['❯', ' '],
            &['➜', ' '],
            &['%', ' '],
            &['$', ' '],
            &['#', ' '],
            &['>', ' '],
            &[':', ' '],
            &['λ', ' '],
            &['»', ' '],
            &['›', ' '],
            &['>', '>', '>', ' '],
        ];

        let search_limit = (cur_col + 1).min(r.len());
        for &sign in PROMPT_SIGNS {
            let sign_len = sign.len();
            if search_limit >= sign_len {
                for i in (0..=search_limit - sign_len).rev() {
                    let matches = sign.iter().enumerate().all(|(k, &ch)| {
                        r[i + k].codepoint == ch as u32
                    });
                    if matches {
                        let end = i + sign_len;
                        if sign_end.is_none_or(|cur| end > cur) {
                            sign_end = Some(end);
                        }
                        break;
                    }
                }
            }
        }

        if sign_end.is_none() {
            for &sym in &['❯', '➜', '%', '$', '#', '>', 'λ', '»', '›'] {
                if let Some(pos) = r[..search_limit].iter().rposition(|c| c.codepoint == sym as u32) {
                    let end = pos + 1;
                    if end <= cur_col {
                        let mut sc = end;
                        while sc < self.cols && Self::is_blank(r[sc].codepoint) {
                            sc += 1;
                        }
                        sign_end = Some(sc.min(cur_col));
                        break;
                    }
                }
            }
        }

        pl_end.or(sign_end).unwrap_or(0).min(cur_col)
    }

    /// Calculate the arrow-key escape sequences needed to move the shell cursor to `(pane_row, pane_col)`.
    /// Returns `None` if the click is outside the active command prompt line or no movement is needed.
    pub fn calculate_cursor_movement_to(&self, pane_row: usize, pane_col: usize) -> Option<Vec<u8>> {
        if self.alt_active || self.display_offset != 0 {
            return None;
        }
        if pane_row != self.cur.row || self.cur.row >= self.rows || self.cols == 0 {
            return None;
        }

        let r = self.display_row(self.cur.row);
        let cur_col = self.cur.col.min(self.cols);
        let (cmd_start, cmd_end) = self.prompt_command_range();

        let mut target_col = pane_col.clamp(cmd_start, cmd_end);

        // If target_col lands on a wide-character dummy cell (codepoint == 0 following a width-2 character), snap to character start
        if target_col > cmd_start
            && target_col < self.cols
            && r[target_col].codepoint == 0
            && is_wide_cp(r[target_col - 1].codepoint)
        {
            target_col -= 1;
        }

        if target_col == cur_col {
            return None;
        }

        let seq_left = if self.app_cursor { b"\x1bOD" } else { b"\x1b[D" };
        let seq_right = if self.app_cursor { b"\x1bOC" } else { b"\x1b[C" };

        if target_col < cur_col {
            let mut count = 0;
            for c in &r[target_col..cur_col] {
                if c.codepoint != 0 {
                    count += 1;
                }
            }
            let count = count.max(1);
            let mut bytes = Vec::with_capacity(count * seq_left.len());
            for _ in 0..count {
                bytes.extend_from_slice(seq_left);
            }
            Some(bytes)
        } else {
            let mut count = 0;
            for c in &r[cur_col..target_col] {
                if c.codepoint != 0 {
                    count += 1;
                }
            }
            let count = count.max(1);
            let mut bytes = Vec::with_capacity(count * seq_right.len());
            for _ in 0..count {
                bytes.extend_from_slice(seq_right);
            }
            Some(bytes)
        }
    }

    /// Calculate bytes to delete all text on the command line before the cursor.
    /// If at a shell prompt, returns the exact number of backspaces needed to clear
    /// from the prompt end up to the cursor without affecting the prompt.
    /// In alternate screen mode, returns `Ctrl+U` (`\x15`).
    pub fn calculate_delete_before_cursor(&self) -> Option<Vec<u8>> {
        if self.alt_active {
            return Some(vec![0x15]); // Ctrl+U
        }
        if self.cur.row >= self.rows || self.cols == 0 {
            return None;
        }
        let (cmd_start, _) = self.prompt_command_range();
        let cur_col = self.cur.col.min(self.cols);
        if cur_col <= cmd_start {
            return None;
        }
        let r = self.display_row(self.cur.row);
        let mut count = 0;
        for c in &r[cmd_start..cur_col] {
            if c.codepoint != 0 {
                count += 1;
            }
        }
        if count == 0 {
            return None;
        }
        Some(vec![0x7f; count])
    }

    /// Calculate bytes to delete all text on the command line after the cursor.
    /// Sends `Ctrl+K` (`\x0b`, standard kill-line).
    #[allow(dead_code)]
    pub fn calculate_delete_after_cursor(&self) -> Option<Vec<u8>> {
        if self.alt_active {
            return Some(vec![0x0b]);
        }
        if self.cur.row >= self.rows || self.cols == 0 {
            return Some(vec![0x0b]);
        }
        let (_, cmd_end) = self.prompt_command_range();
        let cur_col = self.cur.col.min(self.cols);
        if cur_col >= cmd_end {
            return None;
        }
        Some(vec![0x0b])
    }

    /// Scroll the viewport through history (positive = towards older lines).
    /// The selection stays attached to its text.
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
        if self.selection.is_some() {
            self.selection = None;
            self.all_dirty = true;
        }
    }

    pub fn clear_history(&mut self) {
        self.history = 0;
        self.display_offset = 0;
        self.selection = None;
        self.clear_search();
        self.all_dirty = true;
    }

    /// Set the selection from visible `(row, col)` coordinates.
    pub fn set_selection(&mut self, sel: Option<Selection>) {
        let sel = sel.map(|s| Selection {
            start: (self.abs_line(s.start.0), s.start.1),
            end: (self.abs_line(s.end.0), s.end.1),
        });
        if self.selection != sel {
            self.selection = sel;
            self.all_dirty = true;
        }
    }

    #[inline]
    pub fn is_selected(&self, row: usize, col: usize) -> bool {
        let Some(sel) = self.selection else {
            return false;
        };
        let (start, end) = sel.ordered();
        let pos = (self.abs_line(row), col);
        pos >= start && pos <= end
    }

    #[inline]
    fn is_path_char(ch: char) -> bool {
        if ('\u{E000}'..='\u{F8FF}').contains(&ch)
            || ('\u{F0000}'..='\u{FFFFD}').contains(&ch)
            || ('\u{100000}'..='\u{10FFFD}').contains(&ch)
        {
            return false;
        }
        ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '~' | '+' | '@' | '%' | '=')
    }

    /// Detect a URL (http:// or https://) or a file/directory path containing `(row, col)`.
    pub fn find_hyperlink_at(
        &self,
        row: usize,
        col: usize,
        cwd: Option<&std::path::Path>,
    ) -> Option<Hyperlink> {
        if row >= self.rows || self.cols == 0 || col >= self.cols {
            return None;
        }
        let r = self.display_row(row);
        let chars: Vec<char> = r
            .iter()
            .map(|c| char::from_u32(c.codepoint).filter(|&ch| ch != '\0').unwrap_or(' '))
            .collect();

        // 1. Try URL matching first
        let mut idx = 0;
        while idx < chars.len() {
            let starts_with = |pat: &str| pat.chars().enumerate().all(|(k, p)| chars.get(idx + k) == Some(&p));
            let is_https = starts_with("https://");
            let is_http = !is_https && starts_with("http://");
            if is_https || is_http {
                let start = idx;
                let mut end = if is_https { idx + 8 } else { idx + 7 };
                while end < chars.len() {
                    let ch = chars[end];
                    if ch.is_ascii_whitespace()
                        || ch == '\0'
                        || ('\u{E000}'..='\u{F8FF}').contains(&ch)
                        || matches!(ch, '<' | '>' | '"' | '{' | '}' | '|' | '\\' | '^' | '`')
                    {
                        break;
                    }
                    end += 1;
                }
                let end = trim_trailing_punctuation(&chars, start, end);
                if col >= start && col < end && end > start + 7 {
                    let url_str: String = chars[start..end].iter().collect();
                    return Some(Hyperlink {
                        row,
                        start_col: start,
                        end_col: end - 1,
                        url: url_str,
                        is_path: false,
                    });
                }
                idx = end.max(idx + 1);
            } else {
                idx += 1;
            }
        }

        // 2. Try file/directory path matching at `col`
        if !Self::is_path_char(chars[col]) {
            return None;
        }
        let mut start = col;
        while start > 0 && Self::is_path_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = col + 1;
        while end < chars.len() && Self::is_path_char(chars[end]) {
            end += 1;
        }

        let end = trim_trailing_punctuation(&chars, start, end);

        if start >= end || end - start < 2 {
            return None;
        }

        let token_str: String = chars[start..end].iter().collect();

        // Must look like a path: contain '/' or start with '~'
        if !token_str.contains('/') && token_str != "~" {
            return None;
        }

        // Validate that it looks like a path or exists
        let is_valid_path = if token_str.starts_with("~/") || token_str == "~" {
            if let Ok(home) = std::env::var("HOME") {
                let rest = token_str.strip_prefix("~/").unwrap_or("");
                let full = std::path::Path::new(&home).join(rest);
                full.exists() || !rest.is_empty()
            } else {
                true
            }
        } else if token_str.starts_with('/') {
            std::path::Path::new(&token_str).exists() || token_str.len() > 1
        } else if let Some(base) = cwd {
            base.join(&token_str).exists() || std::path::Path::new(&token_str).exists()
        } else {
            std::path::Path::new(&token_str).exists()
        };

        if is_valid_path {
            Some(Hyperlink {
                row,
                start_col: start,
                end_col: end - 1,
                url: token_str,
                is_path: true,
            })
        } else {
            None
        }
    }

    #[inline]
    fn is_blank(codepoint: u32) -> bool {
        codepoint == 0 || codepoint == b' ' as u32
    }

    #[inline]
    fn trim_col_range(row: &[Cell], start: usize, end_exclusive: usize) -> Option<(usize, usize)> {
        let end_exclusive = end_exclusive.min(row.len());
        if start >= end_exclusive {
            return None;
        }
        let slice = &row[start..end_exclusive];
        let first = slice.iter().position(|c| !Self::is_blank(c.codepoint))?;
        let last = slice.iter().rposition(|c| !Self::is_blank(c.codepoint))?;
        Some((start + first, start + last))
    }

    /// Smart double-click selection on `row`:
    /// - On a Powerline prompt line (` ~/path   main ± `), selects `~/path`
    ///   (or the command after the last `` if present and clicked).
    /// - On a command line starting with a shell prompt (`$ cd ...`, `% ...`, `# ...`, `❯ ...`),
    ///   selects the full command line excluding the leading `$ ` prompt (`cd ...`).
    /// - On any other line, selects the trimmed line content.
    pub fn select_smart_line(&mut self, row: usize, col: usize) -> String {
        if row >= self.rows || self.cols == 0 {
            return String::new();
        }
        let r = self.display_row(row);

        // Check for Powerline segment separator U+E0B0 ('')
        let first_pl = r.iter().position(|c| c.codepoint == 0xE0B0);
        let last_pl = r.iter().rposition(|c| c.codepoint == 0xE0B0);
        if let (Some(f_pl), Some(l_pl)) = (first_pl, last_pl) {
            let after_pl = Self::trim_col_range(r, l_pl + 1, self.cols);
            let before_pl = Self::trim_col_range(r, 0, f_pl);
            let chosen = match (before_pl, after_pl) {
                (Some(b), Some(a)) => {
                    if col >= f_pl {
                        Some(a)
                    } else {
                        Some(b)
                    }
                }
                (Some(b), None) => Some(b),
                (None, Some(a)) => Some(a),
                (None, None) => None,
            };
            if let Some(( mut sc, ec)) = chosen {
                if matches!(
                    char::from_u32(r[sc].codepoint),
                    Some('$' | '%' | '#' | '❯' | '>' | '➜')
                ) && sc < ec
                    && Self::is_blank(r[sc + 1].codepoint)
                {
                    sc += 1;
                    while sc <= ec && Self::is_blank(r[sc].codepoint) {
                        sc += 1;
                    }
                }
                if sc <= ec {
                    self.set_selection(Some(Selection {
                        start: (row, sc),
                        end: (row, ec),
                    }));
                    return self.selected_text().unwrap_or_default();
                }
            }
        }

        let Some((mut start_col, end_col)) = Self::trim_col_range(r, 0, self.cols) else {
            self.set_selection(None);
            return String::new();
        };

        // Strip leading shell prompt symbol (`$ `, `% `, `# `, `❯ `, `> `, `➜ `)
        if matches!(
            char::from_u32(r[start_col].codepoint),
            Some('$' | '%' | '#' | '❯' | '>' | '➜')
        ) && start_col < end_col
            && Self::is_blank(r[start_col + 1].codepoint)
        {
            start_col += 1;
            while start_col <= end_col && Self::is_blank(r[start_col].codepoint) {
                start_col += 1;
            }
        }

        if start_col > end_col {
            self.set_selection(None);
            return String::new();
        }

        self.set_selection(Some(Selection {
            start: (row, start_col),
            end: (row, end_col),
        }));
        self.selected_text().unwrap_or_default()
    }

    /// Select the entire visible line `row` (up to the last non-blank character)
    /// and return its text.
    pub fn select_line(&mut self, row: usize) -> String {
        if row >= self.rows {
            return String::new();
        }
        let r = self.display_row(row);
        let last_non_blank = r
            .iter()
            .rposition(|c| c.codepoint != 0 && c.codepoint != b' ' as u32);
        let Some(end_col) = last_non_blank else {
            self.set_selection(None);
            return String::new();
        };
        self.set_selection(Some(Selection {
            start: (row, 0),
            end: (row, end_col),
        }));
        self.selected_text().unwrap_or_default()
    }

    /// Extract the currently selected text. Soft-wrapped lines are joined without
    /// a newline, so a long command or URL copies as one line.
    pub fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        let ((l0, c0), (l1, c1)) = sel.ordered();
        let last_col = self.cols.saturating_sub(1);
        let mut out = String::new();
        for l in l0..=l1 {
            let Some(row) = self.line_cells(l) else { continue };
            let sc = if l == l0 { c0.min(last_col) } else { 0 };
            let ec = if l == l1 { c1.min(last_col) } else { last_col };
            let mut line = String::new();
            if sc <= ec {
                let mut skip_next = false;
                for cell in &row[sc..=ec] {
                    if skip_next {
                        skip_next = false;
                        continue;
                    }
                    let ch = char::from_u32(cell.codepoint).filter(|&c| c != '\0').unwrap_or(' ');
                    if char_width(ch) == 2 {
                        skip_next = true;
                    }
                    line.push(ch);
                }
            }
            let wraps = l < l1 && self.line_wrapped(l);
            if wraps {
                out.push_str(&line);
            } else if l < l1 || ec == last_col {
                out.push_str(line.trim_end_matches(' '));
            } else {
                out.push_str(&line);
            }
            if l < l1 && !wraps {
                out.push('\n');
            }
        }
        let trimmed = out.trim_end_matches(' ').to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    // ------------------------------------------------------------------
    // Find in terminal
    // ------------------------------------------------------------------

    /// Snapshot of the scrollback and screen for `SearchText::find`, so matching
    /// runs without holding the grid lock.
    pub fn search_text(&self) -> SearchText {
        let first = if self.alt_active { self.lines_pushed } else { self.lines_pushed - self.history };
        let last = self.lines_pushed + self.rows;
        let mut text = String::new();
        let mut rows = Vec::with_capacity(last - first);
        for l in first..last {
            let Some(cells) = self.line_cells(l) else { continue };
            let row_start = text.len();
            rows.push((row_start, l));
            let mut skip_next = false;
            for cell in cells {
                if std::mem::take(&mut skip_next) {
                    continue;
                }
                let ch = char::from_u32(cell.codepoint).filter(|&c| c != '\0').unwrap_or(' ');
                skip_next = char_width(ch) == 2;
                text.push(ch);
            }
            if !self.line_wrapped(l) {
                text.truncate(text.trim_end_matches(' ').len().max(row_start));
                text.push('\n');
            }
        }
        SearchText { text, rows }
    }

    /// Replace the search matches and focus the one closest to the bottom of the
    /// viewport (searching upwards from what the user is looking at).
    pub fn set_search_matches(&mut self, matches: Vec<Selection>) {
        let bottom = self.abs_line(self.rows - 1);
        self.search_current = match matches.partition_point(|m| m.start.0 <= bottom) {
            0 if matches.is_empty() => None,
            0 => Some(0),
            n => Some(n - 1),
        };
        self.search_matches = matches;
        self.all_dirty = true;
        self.reveal_search_current();
    }

    pub fn clear_search(&mut self) {
        if !self.search_matches.is_empty() {
            self.search_matches = Vec::new();
            self.all_dirty = true;
        }
        self.search_current = None;
    }

    /// Focus the previous (`up`, older) or next match, wrapping around.
    pub fn search_step(&mut self, up: bool) {
        let n = self.search_matches.len();
        let Some(cur) = self.search_current.filter(|_| n > 0) else { return };
        self.search_current = Some(if up { (cur + n - 1) % n } else { (cur + 1) % n });
        self.all_dirty = true;
        self.reveal_search_current();
    }

    /// `(focused index, match count)`.
    pub fn search_status(&self) -> (Option<usize>, usize) {
        (self.search_current, self.search_matches.len())
    }

    /// Scroll the viewport so the focused match is visible (centered when it was not).
    fn reveal_search_current(&mut self) {
        let Some(m) = self.search_current.and_then(|i| self.search_matches.get(i)) else { return };
        if self.alt_active {
            return;
        }
        let line = m.start.0;
        let top = self.lines_pushed - self.display_offset;
        if line < top || line >= top + self.rows {
            let offset = (self.lines_pushed + self.rows / 2).saturating_sub(line).min(self.history);
            if offset != self.display_offset {
                self.display_offset = offset;
                self.all_dirty = true;
            }
        }
    }

    /// Matches intersecting visible row `row`, for per-cell highlighting.
    pub fn search_hits(&self, row: usize) -> SearchHits<'_> {
        let line = self.abs_line(row);
        let lo = self.search_matches.partition_point(|m| m.end.0 < line);
        let hi = self.search_matches.partition_point(|m| m.start.0 <= line).max(lo);
        SearchHits {
            line,
            current: self.search_current.and_then(|c| c.checked_sub(lo)),
            hits: &self.search_matches[lo..hi],
        }
    }

    // ------------------------------------------------------------------
    // Primitive edits (all in place, no allocation)
    // ------------------------------------------------------------------

    /// Fold a combining mark (e.g. the NFD accents in "Việt") into the previously
    /// printed character when a precomposed form exists. Marks with no precomposed
    /// form are dropped, as before, since a cell holds a single codepoint.
    fn combine_with_previous(&mut self, mark: char) {
        let row = self.cur.row;
        let col = if self.wrap_pending {
            self.cur.col
        } else if self.cur.col > 0 {
            self.cur.col - 1
        } else {
            return;
        };
        let r = self.row_mut(row);
        // Step back over the placeholder half of a double-width character.
        let col = if r[col].codepoint == 0
            && col > 0
            && is_wide_cp(r[col - 1].codepoint)
        {
            col - 1
        } else {
            col
        };
        let Some(base) = char::from_u32(r[col].codepoint).filter(|&c| c != '\0') else { return };
        if let Some(composed) = unicode_normalization::char::compose(base, mark) {
            r[col].codepoint = composed as u32;
            self.mark(row);
        }
    }

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
        if c1 == self.cols {
            // The wrap marker lives in the last column; erasing it ends the wrap.
            self.set_wrapped(r, false);
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
        if !self.alt_active {
            self.wrapped[d / c] = self.wrapped[s / c];
        }
        self.mark(dst);
    }

    fn grow_capacity(&mut self) {
        let max_cap = SCROLLBACK + self.rows;
        if self.cap >= max_cap {
            return;
        }
        let new_cap = (self.cap * 2).min(max_cap);
        let mut new_cells = bytemuck::zeroed_vec::<Cell>(self.cols * new_cap);
        let mut new_wrapped = vec![false; new_cap];
        let n_lines = self.history + self.rows;
        let w = self.cols;
        for i in 0..n_lines {
            let op = (self.top + self.cap - self.history + i) % self.cap;
            let src = op * self.cols;
            new_cells[i * self.cols..i * self.cols + w].copy_from_slice(&self.cells[src..src + w]);
            new_wrapped[i] = self.wrapped[op];
        }
        self.cells = new_cells;
        self.wrapped = new_wrapped;
        self.cap = new_cap;
        self.top = self.history;
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
                if self.history + self.rows >= self.cap && self.cap < SCROLLBACK + self.rows {
                    self.grow_capacity();
                }
                self.top = (self.top + 1) % self.cap;
                self.lines_pushed += 1;
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
            let f = if fg == 0 { default_fg() } else { fg & !0xFF };
            let b = if bg == 0 { default_bg() } else { bg & !0xFF };
            fg = b | FG_SET;
            bg = f | BG_SET;
        }
        if p.attrs & ATTR_DIM != 0 {
            let rgb = if fg == 0 { default_fg() } else { fg & !0xFF };
            fg = ((rgb >> 1) & 0x7F7F_7F00) | FG_SET;
        }
        if p.attrs & ATTR_HIDDEN != 0 {
            fg = if bg == 0 { default_bg() | FG_SET } else { (bg & !0xFF) | FG_SET };
        }
        if p.attrs & ATTR_UNDERLINE != 0 {
            bg |= BG_UNDERLINE;
        }
        if p.attrs & ATTR_BOLD != 0 {
            fg |= FG_BOLD;
        }
        if p.attrs & ATTR_ITALIC != 0 {
            fg |= FG_ITALIC;
        }
        (fg, bg)
    }

    fn tab_forward(&mut self, n: usize) {
        for _ in 0..n {
            let next = (self.cur.col + 1..self.cols).find(|&c| self.tab_stops[c]);
            self.cur.col = next.unwrap_or(self.cols - 1);
        }
    }

    fn tab_backward(&mut self, n: usize) {
        for _ in 0..n {
            let prev = (1..self.cur.col).rev().find(|&c| self.tab_stops[c]);
            self.cur.col = prev.unwrap_or(0);
        }
    }

    /// Move to a 1-based row parameter, honoring origin mode (DECOM).
    fn goto_row(&mut self, row_param: usize) {
        if self.cur.origin_mode {
            self.cur.row = (self.scroll_top + row_param - 1).min(self.scroll_bot);
        } else {
            self.cur.row = (row_param - 1).min(self.rows - 1);
        }
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
        self.selection = None;
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
        self.cursor_shape = 0;
        self.app_cursor = false;
        self.bracketed_paste = false;
        self.autowrap = true;
        self.insert_mode = false;
        self.sync_output = false;
        self.sync_started = None;
        self.mouse_tracking = MouseTracking::Off;
        self.mouse_sgr = false;
        self.focus_events = false;
        self.alt_scroll = true;
        self.tab_stops = default_tab_stops(self.cols);
        self.selection = None;
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
        // Reflow renumbers lines; the caller re-runs an open search.
        self.clear_search();

        // 1. Main screen + scrollback -> logical lines (soft wraps joined), tracking
        //    the main cursor as (line, offset).
        let main_cur = if self.alt_active { self.saved_main } else { self.cur };
        let total = self.history + self.rows;
        let cursor_row = self.history + main_cur.row.min(self.rows - 1);
        let mut lines: Vec<Vec<Cell>> = Vec::new();
        let mut current: Vec<Cell> = Vec::new();
        let mut cursor = (0, 0);
        for i in 0..total {
            let p = (self.top + self.cap - self.history + i) % self.cap;
            if i == cursor_row {
                cursor = (lines.len(), current.len() + main_cur.col);
            }
            current.extend_from_slice(&self.cells[p * self.cols..(p + 1) * self.cols]);
            if self.wrapped[p] && i + 1 < total {
                continue;
            }
            let keep = if lines.len() == cursor.0 && i >= cursor_row { cursor.1 } else { 0 };
            let content = current.iter().rposition(|c| c.codepoint != 0 || c.bg & BG_SET != 0).map_or(0, |i| i + 1);
            current.truncate(content.max(keep).min(current.len()));
            lines.push(std::mem::take(&mut current));
        }
        // Blank lines below the cursor are not content worth keeping.
        while lines.len() > cursor.0 + 1 && lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }

        // 2. Re-wrap every logical line to the new width.
        let mut out: Vec<Cell> = Vec::new();
        let mut out_wrapped: Vec<bool> = Vec::new();
        let mut new_cursor = (0, 0);
        for (li, line) in lines.iter().enumerate() {
            let first_row = out_wrapped.len();
            out.resize(out.len() + cols, Cell::default());
            out_wrapped.push(false);
            let mut col = 0;
            let mut i = 0;
            while i < line.len() {
                let cell = line[i];
                let w = if is_wide_cp(cell.codepoint) { 2.min(cols) } else { 1 };
                if col + w > cols {
                    *out_wrapped.last_mut().unwrap() = true;
                    out.resize(out.len() + cols, Cell::default());
                    out_wrapped.push(false);
                    col = 0;
                }
                if li == cursor.0 && (i..i + w).contains(&cursor.1) {
                    new_cursor = (out_wrapped.len() - 1, col);
                }
                let base = out.len() - cols;
                out[base + col] = cell;
                if w == 2 {
                    out[base + col + 1] = Cell { codepoint: 0, ..cell };
                }
                col += w;
                i += if w == 2 && line.get(i + 1).is_some_and(|c| c.codepoint == 0) { 2 } else { 1 };
            }
            if li == cursor.0 && cursor.1 >= line.len() {
                let extra = cursor.1 - line.len();
                let row = (out_wrapped.len() - 1).max(first_row);
                new_cursor = (row, (col + extra).min(cols - 1));
            }
        }

        // 3. Keep the cursor on screen; everything above it becomes scrollback.
        let total_rows = out_wrapped.len().max(new_cursor.0 + 1);
        let screen_start = total_rows.saturating_sub(rows).min(new_cursor.0).max((new_cursor.0 + 1).saturating_sub(rows));
        let new_hist = screen_start.min(SCROLLBACK);
        let first = screen_start - new_hist;
        let new_cap = (new_hist + rows).max(rows * 2).max(64).min(SCROLLBACK + rows);
        let mut cells = bytemuck::zeroed_vec::<Cell>(cols * new_cap);
        let mut wrapped = vec![false; new_cap];
        for (dst, src) in (first..(screen_start + rows).min(out_wrapped.len())).enumerate() {
            cells[dst * cols..(dst + 1) * cols].copy_from_slice(&out[src * cols..(src + 1) * cols]);
            wrapped[dst] = out_wrapped[src];
        }

        let mut alt = bytemuck::zeroed_vec::<Cell>(cols * rows);
        let w = cols.min(self.cols);
        for r in 0..rows.min(self.rows) {
            alt[r * cols..r * cols + w].copy_from_slice(&self.alt[r * self.cols..r * self.cols + w]);
        }

        self.cells = cells;
        self.wrapped = wrapped;
        self.alt = alt;
        self.cap = new_cap;
        self.top = new_hist;
        self.history = new_hist;
        self.display_offset = 0;
        let main_row = new_cursor.0 - screen_start;
        if self.alt_active {
            self.saved_main.row = main_row;
            self.saved_main.col = new_cursor.1;
        } else {
            self.cur.row = main_row;
            self.cur.col = new_cursor.1;
        }
        self.cols = cols;
        self.rows = rows;
        self.clamp_cursor();
        for c in [&mut self.saved, &mut self.saved_main] {
            c.row = c.row.min(rows - 1);
            c.col = c.col.min(cols - 1);
        }
        self.tab_stops = default_tab_stops(cols);
        self.scroll_top = 0;
        self.scroll_bot = rows - 1;
        self.wrap_pending = false;
        self.selection = None;
        self.prompt_input_mark = None;
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
    match i {
        0..=15 => PALETTE_BASE[i as usize].load(Ordering::Relaxed) << 8,
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

/// Display width of a character: 0 (combining / zero-width), 1, or 2 (CJK / emoji),
/// following the Unicode East Asian Width and emoji presentation tables.
pub fn char_width(c: char) -> usize {
    use unicode_width::UnicodeWidthChar;
    c.width().unwrap_or(0).min(2)
}

/// Smallest codepoint `char_width` reports as double-width (Hangul Jamo).
const FIRST_WIDE: u32 = 0x1100;

/// Whether a cell's codepoint is a double-width character. Called for every
/// printed character, so ASCII and other narrow text skip the width tables.
#[inline]
fn is_wide_cp(cp: u32) -> bool {
    cp >= FIRST_WIDE && char::from_u32(cp).is_some_and(|c| char_width(c) == 2)
}

/// Extract the path from an OSC 7 payload (`file://host/percent%20encoded/path`).
fn parse_osc7_path(payload: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(payload).ok()?;
    let rest = s.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let mut out = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Standard base64 decoder for OSC 52 payloads (capped at 1 MiB of input).
fn decode_base64(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() > 1 << 20 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for &c in data {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Drop trailing sentence punctuation from a detected link `chars[start..end]`,
/// keeping a closing `)` / `]` that has a matching opener inside the link.
fn trim_trailing_punctuation(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start {
        let unbalanced = |close: char, open: char| chars[end - 1] == close && !chars[start..end].contains(&open);
        if matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!' | '?' | '\'') || unbalanced(')', '(') || unbalanced(']', '[') {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

fn default_tab_stops(cols: usize) -> Vec<bool> {
    (0..cols).map(|c| c > 0 && c % 8 == 0).collect()
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
            self.combine_with_previous(c);
            return;
        }
        if w == 2 && self.cols < 2 {
            w = 1;
        }
        if self.wrap_pending {
            self.wrap_pending = false;
            if self.autowrap {
                self.set_wrapped(self.cur.row, true);
                self.cur.col = 0;
                self.linefeed();
            }
        }
        if w == 2 && self.cur.col + 1 >= self.cols {
            if !self.autowrap {
                return;
            }
            self.clear_cols(self.cur.row, self.cur.col, self.cols);
            self.set_wrapped(self.cur.row, true);
            self.cur.col = 0;
            self.linefeed();
        }
        let (fg, bg) = self.colors();
        let (row, col) = (self.cur.row, self.cur.col);
        let blank = self.blank();
        let cols = self.cols;
        let insert = self.insert_mode;
        {
            let r = self.row_mut(row);
            if insert && col + w < cols {
                r.copy_within(col..cols - w, col + w);
            }
            // Never leave half of a double-width character behind.
            if col > 0 && r[col].codepoint == 0 && is_wide_cp(r[col - 1].codepoint) {
                r[col - 1] = blank;
            }
            let last = col + w - 1;
            if last + 1 < cols && is_wide_cp(r[last].codepoint) {
                r[last + 1] = blank;
            }
            r[col] = Cell { codepoint: c as u32, fg, bg };
            if w == 2 {
                r[col + 1] = Cell { codepoint: 0, fg, bg };
            }
        }
        if col + w >= cols {
            self.set_wrapped(row, false);
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
            0x09 => self.tab_forward(1),
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
            'd' => self.goto_row(arg(0, 1)),
            'H' | 'f' => {
                self.goto_row(arg(0, 1));
                self.cur.col = (arg(1, 1) - 1).min(cols - 1);
            }
            'I' => self.tab_forward(arg(0, 1)),
            'Z' => self.tab_backward(arg(0, 1)),
            'g' => match raw(0) {
                0 => self.tab_stops[self.cur.col] = false,
                3 => self.tab_stops.fill(false),
                _ => {}
            },
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
                self.set_wrapped(r, false);
                self.mark(r);
            }
            'P' => {
                let (r, c) = (self.cur.row, self.cur.col);
                let k = arg(0, 1).min(cols - c);
                let b = self.blank();
                let row = self.row_mut(r);
                row.copy_within(c + k..cols, c);
                row[cols - k..].fill(b);
                self.set_wrapped(r, false);
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
                    self.cur.row = if self.cur.origin_mode { t } else { 0 };
                    self.cur.col = 0;
                }
            }
            's' if !private => self.saved = self.cur,
            'u' if !private => {
                self.cur = self.saved;
                self.clamp_cursor();
            }
            'h' | 'l' if !private => {
                let on = action == 'h';
                for &mode in &p[..n] {
                    if mode == 4 {
                        self.insert_mode = on;
                    }
                }
            }
            'h' | 'l' if private => {
                let on = action == 'h';
                for &mode in &p[..n] {
                    match mode {
                        1 => self.app_cursor = on,
                        6 => {
                            self.cur.origin_mode = on;
                            self.cur.row = if on { self.scroll_top } else { 0 };
                            self.cur.col = 0;
                        }
                        7 => self.autowrap = on,
                        9 | 1000 => self.mouse_tracking = if on { MouseTracking::Click } else { MouseTracking::Off },
                        1002 => self.mouse_tracking = if on { MouseTracking::Drag } else { MouseTracking::Off },
                        1003 => self.mouse_tracking = if on { MouseTracking::Motion } else { MouseTracking::Off },
                        1004 => self.focus_events = on,
                        1006 => self.mouse_sgr = on,
                        1007 => self.alt_scroll = on,
                        25 => {
                            if self.cursor_visible != on {
                                self.cursor_visible = on;
                                self.mark(self.cur.row);
                            }
                        }
                        47 | 1047 => self.set_alt(on, false),
                        1049 => self.set_alt(on, true),
                        2004 => self.bracketed_paste = on,
                        2026 => {
                            self.sync_output = on;
                            self.sync_started = on.then(std::time::Instant::now);
                        }
                        _ => {}
                    }
                }
            }
            'n' => match raw(0) {
                5 => self.response.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let row = if self.cur.origin_mode { self.cur.row - self.scroll_top } else { self.cur.row } + 1;
                    let q = if private { "?" } else { "" };
                    let _ = write!(self.response, "\x1b[{q}{row};{}R", self.cur.col + 1);
                }
                _ => {}
            },
            'p' if private && intermediates.last() == Some(&b'$') => {
                // DECRQM (Request Mode): reply with 2 (reset) or 1 (set) so shells never hang waiting.
                let mode = raw(0);
                let state = match mode {
                    1 => if self.app_cursor { 1 } else { 2 },
                    6 => if self.cur.origin_mode { 1 } else { 2 },
                    7 => if self.autowrap { 1 } else { 2 },
                    1000 => if self.mouse_tracking == MouseTracking::Click { 1 } else { 2 },
                    1002 => if self.mouse_tracking == MouseTracking::Drag { 1 } else { 2 },
                    1003 => if self.mouse_tracking == MouseTracking::Motion { 1 } else { 2 },
                    1004 => if self.focus_events { 1 } else { 2 },
                    1006 => if self.mouse_sgr { 1 } else { 2 },
                    1007 => if self.alt_scroll { 1 } else { 2 },
                    25 => if self.cursor_visible { 1 } else { 2 },
                    1049 => if self.alt_active { 1 } else { 2 },
                    2004 => if self.bracketed_paste { 1 } else { 2 },
                    2026 => if self.sync_output { 1 } else { 2 },
                    _ => 0,
                };
                let _ = write!(self.response, "\x1b[?{mode};{state}$y");
            }
            'q' if intermediates == b" " => {
                // DECSCUSR: set cursor style.
                let shape = raw(0).min(6) as u8;
                if self.cursor_shape != shape {
                    self.cursor_shape = shape;
                    self.mark(self.cur.row);
                }
            }
            'c' if intermediates.is_empty() => self.response.extend_from_slice(b"\x1b[?6c"),
            'c' if intermediates == b">" => self.response.extend_from_slice(b"\x1b[>0;0;0c"),
            'c' if intermediates == b"=" => self.response.extend_from_slice(b"\x1bP!|00000000\x1b\\"),
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        if params.len() < 2 {
            return;
        }
        match params[0] {
            b"0" | b"2" => {
                if let Ok(s) = std::str::from_utf8(params[1]) {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() && self.title != trimmed {
                        self.title.clear();
                        self.title.push_str(trimmed);
                        self.title_changed = true;
                    }
                }
                return;
            }
            b"7" => {
                let cwd = parse_osc7_path(params[1]);
                if cwd.is_some() && cwd != self.reported_cwd {
                    self.reported_cwd = cwd;
                    self.title_changed = true;
                }
                return;
            }
            b"52" => {
                // `52;<targets>;<base64>`. Only setting is supported: reading the
                // clipboard from the terminal would leak it to any program.
                if let Some(text) = params.get(2).filter(|d| **d != b"?").and_then(|d| decode_base64(d)) {
                    if let Ok(text) = String::from_utf8(text) {
                        self.clipboard_request = Some(text);
                    }
                }
                return;
            }
            b"133" => {
                match params[1].first() {
                    Some(b'B') => {
                        self.prompt_input_mark = Some((self.abs_line(self.cur.row), self.cur.col));
                    }
                    Some(b'A' | b'C' | b'D') => self.prompt_input_mark = None,
                    _ => {}
                }
                return;
            }
            _ => {}
        }
        if params[1] != b"?" {
            return;
        }
        let st = if bell_terminated { "\x07" } else { "\x1b\\" };
        match params[0] {
            b"10" => {
                let r = (default_fg() >> 24) & 0xFF;
                let g = (default_fg() >> 16) & 0xFF;
                let b = (default_fg() >> 8) & 0xFF;
                let _ = write!(self.response, "\x1b]10;rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{st}");
            }
            b"11" => {
                let r = (default_bg() >> 24) & 0xFF;
                let g = (default_bg() >> 16) & 0xFF;
                let b = (default_bg() >> 8) & 0xFF;
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
            ([], b'H') => self.tab_stops[self.cur.col] = true,
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
    fn combining_marks_compose_into_precomposed_chars() {
        let mut g = Grid::new(20, 3);
        // NFD "Việt Nam": e + U+0323 (dot below) + U+0302 (circumflex).
        feed(&mut g, "Vie\u{0323}\u{0302}t Nam");
        assert_eq!(line(&g, 0), "Việt Nam");
        assert_eq!(g.cur.col, 8);

        // Mark at the very start of a line has no base and is dropped.
        feed(&mut g, "\r\n\u{0301}a");
        assert_eq!(line(&g, 1), "a");
    }

    #[test]
    fn combining_mark_at_right_margin_uses_pending_wrap_cell() {
        let mut g = Grid::new(4, 2);
        feed(&mut g, "abce\u{0301}");
        assert_eq!(line(&g, 0), "abcé");
        feed(&mut g, "x");
        assert_eq!(line(&g, 1), "x");
    }

    #[test]
    fn unicode_widths_follow_east_asian_and_emoji_tables() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('ệ'), 1);
        assert_eq!(char_width('中'), 2);
        assert_eq!(char_width('🚀'), 2);
        assert_eq!(char_width('\u{0301}'), 0);
        assert_eq!(char_width('\u{200D}'), 0);

        let mut g = Grid::new(10, 2);
        feed(&mut g, "🚀x");
        assert_eq!(g.display_row(0)[0].codepoint, '🚀' as u32);
        assert_eq!(g.display_row(0)[1].codepoint, 0);
        assert_eq!(g.display_row(0)[2].codepoint, 'x' as u32);
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
    fn bold_and_italic_are_stored_in_fg_flags() {
        let mut g = Grid::new(8, 1);
        feed(&mut g, "\x1b[1ma\x1b[3mb\x1b[22mc\x1b[0;31;1md");
        let row = g.display_row(0);
        assert_eq!(row[0].fg, FG_BOLD);
        assert_eq!(row[1].fg, FG_BOLD | FG_ITALIC);
        assert_eq!(row[2].fg, FG_ITALIC);
        assert_eq!(row[3].fg, palette(1) | FG_SET | FG_BOLD);
    }

    #[test]
    fn mouse_and_focus_modes() {
        let mut g = Grid::new(8, 2);
        assert_eq!(g.mouse_tracking, MouseTracking::Off);
        feed(&mut g, "\x1b[?1002h\x1b[?1006h\x1b[?1004h");
        assert_eq!(g.mouse_tracking, MouseTracking::Drag);
        assert!(g.mouse_sgr && g.focus_events);
        feed(&mut g, "\x1b[?1002$p");
        assert_eq!(g.response, b"\x1b[?1002;1$y");
        feed(&mut g, "\x1b[?1002l\x1bc");
        assert_eq!(g.mouse_tracking, MouseTracking::Off);
        assert!(!g.mouse_sgr && !g.focus_events);
    }

    #[test]
    fn tab_stops_set_clear_and_move() {
        let mut g = Grid::new(30, 1);
        feed(&mut g, "\tA");
        assert_eq!(g.display_row(0)[8].codepoint, 'A' as u32);
        feed(&mut g, "\r\x1b[3g\x1b[5G\x1bH\r\tB");
        assert_eq!(g.display_row(0)[4].codepoint, 'B' as u32);
        feed(&mut g, "\x1b[Z");
        assert_eq!(g.cur.col, 4);
        feed(&mut g, "\x1b[3g\r\x1b[2I");
        assert_eq!(g.cur.col, 29);
    }

    #[test]
    fn insert_mode_shifts_text_right() {
        let mut g = Grid::new(8, 1);
        feed(&mut g, "abc\r\x1b[4hX\x1b[4lY");
        assert_eq!(line(&g, 0), "XYbc");
        let mut g = Grid::new(8, 1);
        feed(&mut g, "abc\r\x1b[4hXY");
        assert_eq!(line(&g, 0), "XYabc");
    }

    #[test]
    fn origin_mode_positions_relative_to_scroll_region() {
        let mut g = Grid::new(10, 10);
        feed(&mut g, "\x1b[3;6r\x1b[?6h");
        assert_eq!(g.cur.row, 2);
        feed(&mut g, "\x1b[2;1H");
        assert_eq!(g.cur.row, 3);
        feed(&mut g, "\x1b[99;1H");
        assert_eq!(g.cur.row, 5);
        feed(&mut g, "\x1b[6n");
        assert_eq!(g.response, b"\x1b[4;1R");
        feed(&mut g, "\x1b[?6l");
        assert_eq!(g.cur.row, 0);
    }

    #[test]
    fn osc_7_52_and_133() {
        let mut g = Grid::new(20, 2);
        feed(&mut g, "\x1b]7;file://host/Users/me/My%20Dir\x07");
        assert_eq!(g.reported_cwd.as_deref(), Some("/Users/me/My Dir"));
        assert!(g.title_changed);

        feed(&mut g, "\x1b]52;c;aGVsbG8gVmnhu4d0\x07");
        assert_eq!(g.clipboard_request.as_deref(), Some("hello Việt"));
        g.clipboard_request = None;
        feed(&mut g, "\x1b]52;c;?\x07");
        assert!(g.clipboard_request.is_none());

        // Prompt "my-prompt>>" with the input start marked by OSC 133;B.
        feed(&mut g, "\x1b]133;A\x07my-prompt>>\x1b]133;B\x07ls -la");
        assert_eq!(g.current_prompt_input(), "ls -la");
        assert_eq!(g.prompt_command_range().0, 11);
    }

    #[test]
    fn is_wide_cp_matches_char_width_for_every_codepoint() {
        for cp in 0..=0x10FFFF {
            let expected = char::from_u32(cp).is_some_and(|c| char_width(c) == 2);
            assert_eq!(is_wide_cp(cp), expected, "U+{cp:04X}");
        }
    }

    #[test]
    fn overwriting_half_of_a_wide_char_clears_the_other_half() {
        let mut g = Grid::new(6, 1);
        feed(&mut g, "中\rx");
        assert_eq!(g.display_row(0)[1].codepoint, 0);
        assert_eq!(line(&g, 0), "x");
        let mut g = Grid::new(6, 1);
        feed(&mut g, "中\x1b[2Gy");
        assert_eq!(g.display_row(0)[0].codepoint, 0);
        assert_eq!(line(&g, 0), " y");
    }

    #[test]
    fn selection_follows_text_when_output_scrolls() {
        let mut g = Grid::new(10, 3);
        feed(&mut g, "one\r\ntwo\r\nthree");
        g.set_selection(Some(Selection { start: (1, 0), end: (1, 2) }));
        assert_eq!(g.selected_text().as_deref(), Some("two"));
        feed(&mut g, "\r\nfour");
        // "two" moved up to row 0, and the selection moved with it.
        assert!(g.is_selected(0, 0) && !g.is_selected(1, 0));
        assert_eq!(g.selected_text().as_deref(), Some("two"));
        g.scroll_display(1);
        assert!(g.is_selected(1, 0));
        assert_eq!(g.selected_text().as_deref(), Some("two"));
    }

    #[test]
    fn selection_joins_soft_wrapped_lines() {
        let mut g = Grid::new(4, 3);
        feed(&mut g, "abcdefg\r\nxy");
        g.set_selection(Some(Selection { start: (0, 0), end: (2, 3) }));
        assert_eq!(g.selected_text().as_deref(), Some("abcdefg\nxy"));
    }

    #[test]
    fn search_focuses_nearest_match_and_scrolls_to_it() {
        let mut g = Grid::new(10, 3);
        for i in 0..10 {
            feed(&mut g, &format!("line{i}\r\n"));
        }
        let re = regex::Regex::new(r"line[13]").unwrap();
        let matches = g.search_text().find(&re);
        assert_eq!(matches.len(), 2);
        g.set_search_matches(matches);
        // Bottom-most match is focused first and scrolled into view.
        assert_eq!(g.search_status(), (Some(1), 2));
        assert!((0..g.rows).any(|r| line(&g, r) == "line3"));
        let r = (0..g.rows).find(|&r| line(&g, r) == "line3").unwrap();
        assert_eq!(g.search_hits(r).at(0), SearchHit::Current);
        assert_eq!(g.search_hits(r).at(5), SearchHit::None);

        g.search_step(true);
        assert_eq!(g.search_status(), (Some(0), 2));
        let r = (0..g.rows).find(|&r| line(&g, r) == "line1").unwrap();
        assert_eq!(g.search_hits(r).at(4), SearchHit::Current);
        g.search_step(true);
        assert_eq!(g.search_status(), (Some(1), 2), "wraps around");

        g.clear_search();
        assert_eq!(g.search_status(), (None, 0));
    }

    #[test]
    fn resize_reflows_wrapped_lines() {
        let mut g = Grid::new(10, 4);
        feed(&mut g, "0123456789abcde\r\n$ ");
        assert_eq!(line(&g, 0), "0123456789");
        assert_eq!(line(&g, 1), "abcde");

        g.resize(20, 4);
        assert_eq!(line(&g, 0), "0123456789abcde");
        assert_eq!(line(&g, 1), "$");
        assert_eq!((g.cur.row, g.cur.col), (1, 2));

        g.resize(5, 4);
        assert_eq!(line(&g, 0), "01234");
        assert_eq!(line(&g, 1), "56789");
        assert_eq!(line(&g, 2), "abcde");
        assert_eq!(line(&g, 3), "$");
        assert_eq!((g.cur.row, g.cur.col), (3, 2));

        g.resize(4, 2);
        // Older rows go to scrollback; the cursor line stays visible.
        assert_eq!(line(&g, 1), "$");
        assert_eq!(g.cur.row, 1);
        g.scroll_display(g.history as isize);
        assert_eq!(line(&g, 0), "0123");
    }

    #[test]
    fn resize_reflow_keeps_wide_chars_whole() {
        let mut g = Grid::new(6, 2);
        feed(&mut g, "ab中文");
        g.resize(3, 3);
        assert_eq!(line(&g, 0), "ab");
        assert_eq!(g.display_row(1)[0].codepoint, '中' as u32);
        assert_eq!(g.display_row(2)[0].codepoint, '文' as u32);
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

    #[test]
    fn selection_and_line_copy() {
        let mut g = Grid::new(80, 5);
        feed(
            &mut g,
            " ~/Desktop/personal/laplace-terninal \u{e0b0} \u{e0a0} main \u{00b1} \u{e0b0}\r\n\
             $ cd Desktop/personal/laplace-terninal\r\n\
             $ ./target/release/laplace",
        );
        // Double-click on prompt line selects only ~/Desktop/personal/laplace-terninal
        assert_eq!(
            g.select_smart_line(0, 10),
            "~/Desktop/personal/laplace-terninal"
        );
        assert_eq!(
            g.select_smart_line(0, 0),
            "~/Desktop/personal/laplace-terninal"
        );
        // Double-click on command line selects full line minus "$ "
        assert_eq!(
            g.select_smart_line(1, 15),
            "cd Desktop/personal/laplace-terninal"
        );
        assert_eq!(
            g.select_smart_line(1, 2),
            "cd Desktop/personal/laplace-terninal"
        );
        // Double-click on ./target/release/laplace line selects ./target/release/laplace
        assert_eq!(g.select_smart_line(2, 10), "./target/release/laplace");
        // Triple-click selects raw full line
        assert_eq!(g.select_line(1), "$ cd Desktop/personal/laplace-terninal");
    }

    #[test]
    fn url_detection() {
        let mut g = Grid::new(80, 5);
        feed(
            &mut g,
            "Docs at https://ghostty.org/docs and repo (http://github.com/ghostty-org).\r\n\
             ~/Desktop/personal/laplace-terminal \u{e0b0} main\r\n\
             $ cd ./target/release",
        );
        let link1 = g.find_hyperlink_at(0, 15, None).expect("find https url");
        assert_eq!(link1.url, "https://ghostty.org/docs");
        assert_eq!(link1.start_col, 8);
        assert_eq!(link1.end_col, 31);
        assert!(!link1.is_path);

        let link2 = g.find_hyperlink_at(0, 50, None).expect("find http url");
        assert_eq!(link2.url, "http://github.com/ghostty-org");
        assert!(!link2.is_path);

        // Path detection on prompt
        let path1 = g.find_hyperlink_at(1, 10, None).expect("find path");
        assert_eq!(path1.url, "~/Desktop/personal/laplace-terminal");
        assert_eq!(path1.start_col, 0);
        assert_eq!(path1.end_col, 34);
        assert!(path1.is_path);

        // Relative path detection
        let path2 = g.find_hyperlink_at(2, 10, None).expect("find relative path");
        assert_eq!(path2.url, "./target/release");
        assert!(path2.is_path);

        assert!(g.find_hyperlink_at(0, 2, None).is_none());
    }

    #[test]
    fn dynamic_ring_buffer_growth_and_scrolling() {
        let mut g = Grid::new(20, 4);
        assert!(g.cap <= 64, "initial cap should be lean, was {}", g.cap);

        for i in 0..150 {
            feed(&mut g, &format!("line {:03}\r\n", i));
        }
        assert_eq!(g.history, 147);
        assert!(g.cap >= g.history + g.rows);

        // Scroll back to line 0 and verify contents
        g.scroll_display(g.history as isize);
        assert_eq!(line(&g, 0), "line 000");

        // Reset, resize, and scroll back to line 0 again
        g.reset_display();
        g.resize(30, 8);
        g.scroll_display(g.history as isize);
        assert_eq!(line(&g, 0), "line 000");
    }

    #[test]
    fn cursor_movement_on_click() {
        let mut g = Grid::new(80, 5);
        feed(&mut g, "$ cargo build");
        // Prompt is "$ " (cols 0..1), command is "cargo build" (cols 2..12), cursor is at col 13
        assert_eq!(g.cur.row, 0);
        assert_eq!(g.cur.col, 13);
        assert_eq!(g.prompt_command_range(), (2, 13));

        // Click at col 4 (letter 'r' in cargo): moves left by 9 columns
        let left_bytes = g.calculate_cursor_movement_to(0, 4).expect("move left");
        assert_eq!(left_bytes, b"\x1b[D".repeat(9));

        // Click before prompt at col 0: clamped to col 2 (start of command), moves left by 11
        let clamp_left = g.calculate_cursor_movement_to(0, 0).expect("clamp to start");
        assert_eq!(clamp_left, b"\x1b[D".repeat(11));

        // Click at col 50: clamped to col 13 (end of command) -> already at col 13 -> None
        assert!(g.calculate_cursor_movement_to(0, 50).is_none());

        // Now move cursor to col 2 (start of cargo)
        g.cur.col = 2;
        // Click at col 8 (start of build): moves right by 6
        let right_bytes = g.calculate_cursor_movement_to(0, 8).expect("move right");
        assert_eq!(right_bytes, b"\x1b[C".repeat(6));

        // Click on different row returns None
        assert!(g.calculate_cursor_movement_to(1, 8).is_none());

        // Alt screen active returns None
        g.alt_active = true;
        assert!(g.calculate_cursor_movement_to(0, 8).is_none());
        g.alt_active = false;

        // Scrolled into history returns None
        g.display_offset = 1;
        assert!(g.calculate_cursor_movement_to(0, 8).is_none());
        g.display_offset = 0;

        // Wide character test
        let mut g_emoji = Grid::new(80, 5);
        feed(&mut g_emoji, "❯ 🦀 test");
        // "❯ " (cols 0..1), 🦀 (cols 2..3, width 2), " test" (cols 4..8), cursor at col 9
        assert_eq!(g_emoji.cur.col, 9);
        // Click at col 2 on 🦀: 5 characters between col 2 and 9 (test=4, space=1, 🦀=1, dummy cell skipped)
        let emoji_click = g_emoji.calculate_cursor_movement_to(0, 2).expect("emoji move");
        assert_eq!(emoji_click, b"\x1b[D".repeat(6));

        // Click at dummy cell 3 on 🦀: snaps to col 2 and also moves 6 chars left
        let dummy_click = g_emoji.calculate_cursor_movement_to(0, 3).expect("dummy snap");
        assert_eq!(dummy_click, b"\x1b[D".repeat(6));
    }

    #[test]
    fn delete_before_and_after_cursor() {
        let mut g = Grid::new(80, 5);
        feed(&mut g, "$ cargo build");
        // Prompt ends at 2, cursor is at 13
        assert_eq!(g.cur.col, 13);

        // At end of line: delete before cursor deletes 11 characters ("cargo build")
        assert_eq!(g.calculate_delete_before_cursor(), Some(vec![0x7f; 11]));
        // At end of line: nothing to delete after cursor
        assert_eq!(g.calculate_delete_after_cursor(), None);

        // Move cursor to middle (col 7, after "cargo ")
        g.cur.col = 7;
        // Deletes 5 characters ("cargo ") before cursor
        assert_eq!(g.calculate_delete_before_cursor(), Some(vec![0x7f; 5]));
        // Deletes text after cursor ("build") via Ctrl+K
        assert_eq!(g.calculate_delete_after_cursor(), Some(vec![0x0b]));

        // Move cursor to start of command (col 2)
        g.cur.col = 2;
        // Nothing before cursor (prompt is protected)
        assert_eq!(g.calculate_delete_before_cursor(), None);
        // Deletes text after cursor via Ctrl+K
        assert_eq!(g.calculate_delete_after_cursor(), Some(vec![0x0b]));

        // In alt screen mode
        g.alt_active = true;
        assert_eq!(g.calculate_delete_before_cursor(), Some(vec![0x15])); // Ctrl+U
        assert_eq!(g.calculate_delete_after_cursor(), Some(vec![0x0b]));  // Ctrl+K
    }
}
