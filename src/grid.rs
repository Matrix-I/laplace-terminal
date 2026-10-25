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

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub start: (usize, usize), // (row, col)
    pub end: (usize, usize),   // (row, col)
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

    pub cursor_visible: bool,
    /// Last DECSCUSR (`CSI Ps SP q`) value: 0 = config default,
    /// 1/2 = blinking/steady block, 3/4 = underline, 5/6 = bar.
    pub cursor_shape: u8,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub sync_output: bool,
    autowrap: bool,

    /// Window/tab title set via OSC 0 / OSC 2 (e.g. `user@host:~/dir` or running command).
    pub title: String,

    /// Active mouse text selection in visible screen coordinates.
    pub selection: Option<Selection>,

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
            cursor_visible: true,
            cursor_shape: 0,
            app_cursor: false,
            bracketed_paste: false,
            sync_output: false,
            autowrap: true,
            title: String::new(),
            selection: None,
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
                        if sign_end.map_or(true, |cur| end > cur) {
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

        let cmd_start = pl_end.or(sign_end).unwrap_or(0).min(cur_col);

        let last_non_blank = r[cmd_start..]
            .iter()
            .rposition(|c| !Self::is_blank(c.codepoint))
            .map(|idx| cmd_start + idx + 1)
            .unwrap_or(cmd_start);

        let cmd_end = last_non_blank.max(cur_col).min(self.cols);
        (cmd_start, cmd_end)
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
            && char::from_u32(r[target_col - 1].codepoint).map_or(false, |c| char_width(c) == 2)
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
    pub fn scroll_display(&mut self, delta: isize) {
        if self.alt_active {
            return;
        }
        let new = (self.display_offset as isize + delta).clamp(0, self.history as isize) as usize;
        if new != self.display_offset {
            self.display_offset = new;
            self.selection = None;
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
        self.all_dirty = true;
    }

    pub fn set_selection(&mut self, sel: Option<Selection>) {
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
        (row, col) >= start && (row, col) <= end
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
            let rem: String = chars[idx..].iter().collect();
            let is_https = rem.starts_with("https://");
            let is_http = rem.starts_with("http://");
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
                while end > start {
                    let last_ch = chars[end - 1];
                    if matches!(last_ch, '.' | ',' | ';' | ':' | '!' | '?' | '\'') {
                        end -= 1;
                    } else if last_ch == ')' && !chars[start..end].contains(&'(') {
                        end -= 1;
                    } else if last_ch == ']' && !chars[start..end].contains(&'[') {
                        end -= 1;
                    } else {
                        break;
                    }
                }
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

        // Strip trailing sentence punctuation
        while end > start {
            let last_ch = chars[end - 1];
            if matches!(last_ch, '.' | ',' | ';' | ':' | '!' | '?' | '\'') {
                end -= 1;
            } else if last_ch == ')' && !chars[start..end].contains(&'(') {
                end -= 1;
            } else if last_ch == ']' && !chars[start..end].contains(&'[') {
                end -= 1;
            } else {
                break;
            }
        }

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
                ) && sc + 1 <= ec
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
        ) && start_col + 1 <= end_col
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

    /// Extract the currently selected text across one or more rows.
    pub fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        let ((r0, c0), (r1, c1)) = sel.ordered();
        if r0 >= self.rows {
            return None;
        }
        let r1 = r1.min(self.rows.saturating_sub(1));
        let mut out = String::new();
        for r in r0..=r1 {
            if r > r0 {
                out.push('\n');
            }
            let row = self.display_row(r);
            let sc = if r == r0 { c0.min(self.cols.saturating_sub(1)) } else { 0 };
            let ec = if r == r1 { c1.min(self.cols.saturating_sub(1)) } else { self.cols.saturating_sub(1) };
            if sc > ec {
                continue;
            }
            let mut line = String::new();
            let mut skip_next = false;
            for cell in &row[sc..=ec] {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                let ch = char::from_u32(cell.codepoint)
                    .filter(|&c| c != '\0')
                    .unwrap_or(' ');
                if char_width(ch) == 2 {
                    skip_next = true;
                }
                line.push(ch);
            }
            if r < r1 || ec + 1 == self.cols {
                out.push_str(line.trim_end_matches(' '));
            } else {
                out.push_str(&line);
            }
        }
        let trimmed = out.trim_end_matches(' ').to_string();
        (!trimmed.is_empty()).then_some(trimmed)
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

    fn grow_capacity(&mut self) {
        let max_cap = SCROLLBACK + self.rows;
        if self.cap >= max_cap {
            return;
        }
        let new_cap = (self.cap * 2).min(max_cap);
        let mut new_cells = bytemuck::zeroed_vec::<Cell>(self.cols * new_cap);
        let n_lines = self.history + self.rows;
        let w = self.cols;
        for i in 0..n_lines {
            let op = (self.top + self.cap - self.history + i) % self.cap;
            let src = op * self.cols;
            new_cells[i * self.cols..i * self.cols + w].copy_from_slice(&self.cells[src..src + w]);
        }
        self.cells = new_cells;
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
        self.cursor_shape = 0;
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
        let max_cap = SCROLLBACK + rows;
        let needed = (self.history + self.rows + 1).max(rows * 2).max(64);
        let new_cap = needed.min(max_cap);
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
        if matches!(params[0], b"0" | b"2") {
            if let Ok(s) = std::str::from_utf8(params[1]) {
                let trimmed = s.trim();
                if !trimmed.is_empty() && self.title != trimmed {
                    self.title.clear();
                    self.title.push_str(trimmed);
                }
            }
            return;
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
