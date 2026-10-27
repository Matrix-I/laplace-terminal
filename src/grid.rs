//! Compact 12-byte cells, a zero-allocation ring buffer with scrollback, and the
//! `vte::Perform` implementation that turns the PTY byte stream into grid state.
//!
//! Memory layout: the whole scrollback + screen lives in one flat `Vec<Cell>`
//! (`cols * (SCROLLBACK + rows)`), allocated with `alloc_zeroed` so the OS only
//! commits pages once history actually reaches them. Scrolling the full screen
//! is O(1): bump `top` and clear one row in place. Nothing is allocated while
//! printing or scrolling; allocation only happens on window resize.

use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use vte::{Params, Perform};

use crate::search::SearchText;

/// Number of scrollback lines kept above the visible screen by default.
pub const SCROLLBACK: usize = 5000;
/// Upper bound for a configured scrollback, whatever its byte size.
const MAX_SCROLLBACK: usize = 1_000_000;

/// `scrollback-limit` in bytes (Ghostty semantics); `usize::MAX` = the default
/// line count. Read when a terminal is created or resized.
static SCROLLBACK_BYTES: AtomicUsize = AtomicUsize::new(usize::MAX);

pub fn set_scrollback_limit(bytes: Option<usize>) {
    SCROLLBACK_BYTES.store(bytes.unwrap_or(usize::MAX), Ordering::Relaxed);
}

/// Scrollback lines for a terminal `cols` wide.
fn scrollback_lines(cols: usize) -> usize {
    lines_for_bytes(SCROLLBACK_BYTES.load(Ordering::Relaxed), cols)
}

fn lines_for_bytes(bytes: usize, cols: usize) -> usize {
    match bytes {
        usize::MAX => SCROLLBACK,
        bytes => (bytes / (cols.max(1) * std::mem::size_of::<Cell>())).min(MAX_SCROLLBACK),
    }
}

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

/// Whether the system appearance is dark, for color scheme reports (DSR 996 / mode 2031).
static COLOR_SCHEME_DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_color_scheme_dark(dark: bool) {
    COLOR_SCHEME_DARK.store(dark, Ordering::Relaxed);
}

pub fn color_scheme_is_dark() -> bool {
    COLOR_SCHEME_DARK.load(Ordering::Relaxed)
}

/// `CSI ? 997 ; 1 n` (dark) or `; 2 n` (light): the color scheme report.
pub fn color_scheme_report() -> &'static [u8] {
    if COLOR_SCHEME_DARK.load(Ordering::Relaxed) {
        b"\x1b[?997;1n"
    } else {
        b"\x1b[?997;2n"
    }
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
const UNDERLINE_DOUBLE: u8 = 2;
const UNDERLINE_DASHED: u8 = 5;

const ATTR_BOLD: u8 = 1 << 0;
const ATTR_DIM: u8 = 1 << 1;
const ATTR_ITALIC: u8 = 1 << 2;
const ATTR_STRIKE: u8 = 1 << 3;
const ATTR_INVERSE: u8 = 1 << 4;
const ATTR_HIDDEN: u8 = 1 << 5;

/// Prompts remembered for jump-to-prompt and output selection.
const MAX_PROMPTS: usize = 1000;

/// One shell prompt from OSC 133 marks, as absolute lines.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PromptMark {
    /// Line of the prompt (OSC 133;A).
    prompt: usize,
    /// First output line (OSC 133;C) and the line after the last (OSC 133;D).
    output: Option<(usize, Option<usize>)>,
}

/// Kitty unicode placeholder: a cell showing part of a virtual image placement.
const PLACEHOLDER: char = '\u{10EEEE}';
/// A placeholder cell's `codepoint`: `PH_FLAG | count << 27 | msb << 18 | row << 9 | col`,
/// outside the Unicode range. `count` is how many of the row / column / image id
/// high byte diacritics followed it (each an index into `PLACEHOLDER_DIACRITICS`).
pub const PH_FLAG: u32 = 1 << 31;

/// A multi-codepoint grapheme (emoji ZWJ sequence, flag, skin tone, a base
/// with marks that have no precomposed form): `codepoint` is
/// `CLUSTER_FLAG | CLUSTER_WIDE? | index into Grid::clusters`.
pub const CLUSTER_FLAG: u32 = 1 << 30;
pub const CLUSTER_WIDE: u32 = 1 << 29;
const CLUSTER_ID: u32 = CLUSTER_WIDE - 1;
/// Distinct clusters one grid keeps; past that, marks are dropped as before.
const MAX_CLUSTERS: usize = 1 << 16;
/// Longest cluster kept (UTF-8 bytes); longer sequences stop growing.
const MAX_CLUSTER_BYTES: usize = 64;

/// `grapheme-width-method = unicode`: new terminals start in DEC mode 2027.
static GRAPHEME_DEFAULT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Set the initial state of DEC mode 2027 (grapheme clustering) for new terminals.
pub fn set_grapheme_default(on: bool) {
    GRAPHEME_DEFAULT.store(on, Ordering::Relaxed);
}

/// Upper bound on an OSC 8 URI; longer links are ignored.
const MAX_LINK_LEN: usize = 2048;

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
fn with_extra(fg: u32, bg: u32, extra: u8) -> (u32, u32) {
    let e = extra as u32;
    ((fg & !FG_EXTRA) | ((e & 0x1F) << 2), (bg & !BG_EXTRA) | ((e >> 5) << 4))
}

/// Attributes too large for a `Cell`, shared by every cell that uses them: an
/// OSC 8 hyperlink and an SGR 58 underline color. Cells refer to them by index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CellExtra {
    link: Option<OscLink>,
    /// `0xRRGGBB00` | 1, or 0 for "same as the text color".
    underline_color: u32,
}

/// An OSC 8 hyperlink: `ESC ] 8 ; id=... ; URI ST`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OscLink {
    id: Box<str>,
    uri: Box<str>,
}

#[derive(Copy, Clone, Default)]
struct Pen {
    fg: u32, // 0 = default, else rgb<<8 | FG_SET
    bg: u32, // 0 = default, else rgb<<8 | BG_SET
    attrs: u8,
    /// Underline style (`UNDERLINE_*`).
    underline: u8,
    /// Index into `Grid::extras` (hyperlink + underline color), 0 = none.
    extra: u8,
}

#[derive(Copy, Clone, Default)]
struct Cursor {
    row: usize,
    col: usize,
    pen: Pen,
    line_drawing: bool,
    origin_mode: bool,
}

/// Kitty keyboard protocol flags for one screen: the active flags plus the
/// entries saved below them by `CSI > flags u` (at most 8, oldest dropped first).
#[derive(Copy, Clone, Debug, Default)]
struct KittyKeyboard {
    flags: u8,
    saved: [u8; 8],
    len: usize,
}

impl KittyKeyboard {
    /// The five progressive-enhancement bits the protocol defines.
    const MASK: u8 = 0b1_1111;

    fn push(&mut self, flags: u8) {
        if self.len == self.saved.len() {
            self.saved.copy_within(1.., 0);
            self.len -= 1;
        }
        self.saved[self.len] = self.flags;
        self.len += 1;
        self.flags = flags & Self::MASK;
    }

    fn pop(&mut self, n: usize) {
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
    fn set(&mut self, flags: u8, mode: usize) {
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
    /// Scrollback lines kept above the screen (`scrollback-limit`).
    scrollback: usize,
    /// Ring capacity in rows (at most `scrollback + rows`).
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
    /// SGR mouse reports carry pixel instead of cell coordinates (DECSET 1016).
    pub mouse_sgr_pixels: bool,
    /// Report system light/dark appearance changes (DECSET 2031).
    pub color_scheme_reports: bool,
    /// Cell size in pixels, for XTWINOPS size reports.
    pub cell_px: (u16, u16),
    /// DCS request being collected (`DCS + q` XTGETTCAP or `DCS $ q` DECRQSS).
    dcs: Option<DcsRequest>,
    dcs_buf: Vec<u8>,
    /// Inline images (Kitty graphics protocol, Sixel).
    pub graphics: crate::graphics::Store,
    /// APC filter state in front of vte (which drops APC strings): 0 ground,
    /// 1 after ESC, 2 inside `ESC _`, 3 ESC inside it.
    apc_state: u8,
    apc_buf: Vec<u8>,
    /// xterm `modifyOtherKeys` level (0 off, 1, 2) set by `CSI > 4 ; level m`.
    pub modify_other_keys: u8,
    /// Kitty keyboard protocol state for the main and the alternate screen.
    kitty_keyboard: [KittyKeyboard; 2],

    /// Window/tab title set via OSC 0 / OSC 2 (e.g. `user@host:~/dir` or running command).
    pub title: String,
    /// Set when the title or the reported working directory changes.
    pub title_changed: bool,
    /// Working directory reported by the shell via OSC 7 (`file://host/path`).
    pub reported_cwd: Option<String>,
    /// Text an application asked to place on the clipboard via OSC 52.
    pub clipboard_request: Option<String>,
    /// Desktop notification requested by OSC 9 / OSC 777 as `(title, body)`.
    pub notification: Option<(String, String)>,
    /// A BEL arrived since the UI last looked.
    pub bell: bool,
    /// OSC 9;4 progress as `(state, percent)`: 1 normal, 2 error, 3 indeterminate,
    /// 4 paused. `None` when no progress is shown.
    pub progress: Option<(u8, u8)>,
    /// OSC 22 mouse pointer shape (a CSS or X11 cursor name; empty = default).
    pub pointer_shape: String,
    /// OSC 133 prompts on the main screen, oldest first (at most `MAX_PROMPTS`).
    prompts: Vec<PromptMark>,
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

    /// Palette entries changed by OSC 4, as `rgb | 1` (0 = config palette). Empty
    /// until an application first sets one.
    palette_override: Vec<u32>,
    /// Default foreground, background and cursor colors set by OSC 10 / 11 / 12,
    /// as `rgb | 1` (0 = config default).
    color_override: [u32; 3],

    /// Hyperlinks and underline colors referenced by `Cell::extra` (index 0 is
    /// reserved for "none"). Slots no cell uses any more are reclaimed when full.
    extras: Vec<Option<CellExtra>>,
    /// Multi-codepoint graphemes cells refer to (see `CLUSTER_FLAG`), interned.
    clusters: Vec<Box<str>>,
    cluster_ids: std::collections::HashMap<Box<str>, u32>,
    /// Reused buffer for building a cluster.
    cluster_buf: String,
    /// DEC mode 2027: emoji sequences (ZWJ, flags, skin tones, VS16) share one cell pair.
    grapheme_mode: bool,
    /// The last character joined a ZWJ to a cluster: a pictograph after it joins too.
    joiner: bool,
}

impl Grid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let scrollback = scrollback_lines(cols);
        let max_cap = scrollback + rows;
        // Start lean: allocate only 2x rows (minimum 64) instead of the full 5000+ scrollback ring.
        // The ring buffer doubles dynamically on demand up to max_cap when lines scroll off screen.
        let cap = (rows * 2).max(64).min(max_cap);
        Grid {
            cols,
            rows,
            scrollback,
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
            mouse_sgr_pixels: false,
            color_scheme_reports: false,
            cell_px: (0, 0),
            dcs: None,
            dcs_buf: Vec::new(),
            graphics: crate::graphics::Store::default(),
            apc_state: 0,
            apc_buf: Vec::new(),
            modify_other_keys: 0,
            kitty_keyboard: [KittyKeyboard::default(); 2],
            title: String::new(),
            title_changed: false,
            reported_cwd: None,
            clipboard_request: None,
            notification: None,
            bell: false,
            progress: None,
            pointer_shape: String::new(),
            prompt_input_mark: None,
            prompts: Vec::new(),
            selection: None,
            search_matches: Vec::new(),
            search_current: None,
            dirty: vec![true; rows],
            all_dirty: true,
            response: Vec::with_capacity(256),
            extras: vec![None],
            clusters: Vec::new(),
            cluster_ids: std::collections::HashMap::new(),
            cluster_buf: String::new(),
            grapheme_mode: GRAPHEME_DEFAULT.load(Ordering::Relaxed),
            joiner: false,
            palette_override: Vec::new(),
            color_override: [0; 3],
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
        let mut text = String::new();
        for c in &r[cmd_start..end_col] {
            if c.codepoint != 0 {
                self.push_cell_text(c, &mut text);
            }
        }
        text.trim().to_string()
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
        self.prompts.clear();
        self.graphics.prune(self.lines_pushed);
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

    /// Select everything in scrollback and on screen, from the first to the last
    /// line with text (`Cmd+A`).
    pub fn select_all(&mut self) {
        let first = if self.alt_active { self.lines_pushed } else { self.lines_pushed - self.history };
        let has_text = |l: &usize| self.line_cells(*l).is_some_and(|r| r.iter().any(|c| !Self::is_blank(c.codepoint)));
        let lines = first..self.lines_pushed + self.rows;
        let sel = lines.clone().find(has_text).zip(lines.rev().find(has_text)).map(|(top, bottom)| Selection {
            start: (top, 0),
            end: (bottom, self.cols - 1),
        });
        if self.selection != sel {
            self.selection = sel;
            self.all_dirty = true;
        }
    }

    /// Shift+Click: move the selection end nearest to visible `(row, col)` there,
    /// keeping the other end as the anchor. Returns `false` without a selection.
    pub fn extend_selection(&mut self, row: usize, col: usize) -> bool {
        let Some(sel) = self.selection else {
            return false;
        };
        let pos = (self.abs_line(row), col);
        let (a, b) = sel.ordered();
        let offset = |p: (usize, usize)| p.0 * self.cols + p.1;
        let anchor = if offset(pos).abs_diff(offset(a)) <= offset(pos).abs_diff(offset(b)) { b } else { a };
        self.selection = Some(Selection { start: anchor, end: pos });
        self.all_dirty = true;
        true
    }

    /// Dragging after Shift+Click: move the selection end, keeping its anchor
    /// (which may be in scrollback, off screen).
    pub fn set_selection_end(&mut self, row: usize, col: usize) {
        let end = (self.abs_line(row), col);
        if let Some(sel) = self.selection.as_mut().filter(|s| s.end != end) {
            sel.end = end;
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

        // 1. An explicit OSC 8 hyperlink wins over anything detected in the text.
        if let Some(link) = self.cell_link(&r[col]) {
            let same = |c: &Cell| self.cell_link(c) == Some(link);
            let start = r[..col].iter().rposition(|c| !same(c)).map_or(0, |i| i + 1);
            let end = r[col..].iter().position(|c| !same(c)).map_or(self.cols, |i| col + i);
            let (url, is_path) = osc8_target(&link.uri)?;
            return Some(Hyperlink { row, start_col: start, end_col: end - 1, url, is_path });
        }

        let chars: Vec<char> = r.iter().map(|c| self.cell_char(c)).collect();

        // 2. Try URL matching
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

        // 3. Try file/directory path matching at `col`
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

    /// Columns of the run of non-blank cells around `(row, col)`; the placeholder
    /// cell of a wide glyph belongs to the glyph.
    fn block_range(&self, row: usize, col: usize) -> Option<(usize, usize)> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let r = self.display_row(row);
        let filled = |c: usize| {
            !Self::is_blank(r[c].codepoint) || (c > 0 && r[c].codepoint == 0 && is_wide_cp(r[c - 1].codepoint))
        };
        if !filled(col) {
            return None;
        }
        let mut sc = col;
        while sc > 0 && filled(sc - 1) {
            sc -= 1;
        }
        let mut ec = col;
        while ec + 1 < r.len() && filled(ec + 1) {
            ec += 1;
        }
        Some((sc, ec))
    }

    /// Double-click: select the contiguous block of text at `(row, col)` and return
    /// it. On a blank cell, fall back to the smart line selection.
    pub fn select_block(&mut self, row: usize, col: usize) -> String {
        let Some((sc, ec)) = self.block_range(row, col) else {
            return self.select_smart_line(row, col);
        };
        self.set_selection(Some(Selection { start: (row, sc), end: (row, ec) }));
        self.selected_text().unwrap_or_default()
    }

    /// Whether the selection is exactly the block at `(row, col)`, so a further
    /// double-click there widens it to the line.
    pub fn is_block_selected(&self, row: usize, col: usize) -> bool {
        let (Some(sel), Some((sc, ec))) = (self.selection, self.block_range(row, col)) else {
            return false;
        };
        let line = self.abs_line(row);
        sel.ordered() == ((line, sc), (line, ec))
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
                    skip_next = is_wide_cp(cell.codepoint);
                    if cell.codepoint == 0 {
                        line.push(' ');
                    } else {
                        self.push_cell_text(cell, &mut line);
                    }
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
                // One char per cell (a cluster's first) keeps offsets mapped to columns.
                skip_next = is_wide_cp(cell.codepoint);
                text.push(self.cell_char(cell));
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

    /// The cell the last printed character went into (cursor row), if any.
    fn previous_cell(&mut self) -> Option<(usize, usize)> {
        let row = self.cur.row;
        let col = if self.wrap_pending { self.cur.col } else { self.cur.col.checked_sub(1)? };
        let r = self.row_mut(row);
        // Step back over the placeholder half of a double-width character.
        if r[col].codepoint == 0 && col > 0 && is_wide_cp(r[col - 1].codepoint) {
            return Some((row, col - 1));
        }
        Some((row, col))
    }

    /// Visual order of display row `r` (see [`crate::bidi::visual_order`]),
    /// with `chars` and `levels` as scratch space. False when the row has no
    /// right-to-left text, or on the alternate screen: full-screen
    /// applications lay out their own bidirectional text.
    pub fn bidi_order(&self, r: usize, chars: &mut Vec<char>, levels: &mut Vec<u8>, order: &mut Vec<u16>) -> bool {
        order.clear();
        let row = self.display_row(r);
        // Nothing below the Hebrew block is right-to-left.
        if self.alt_active || !row.iter().any(|c| c.codepoint >= 0x0590) {
            return false;
        }
        chars.clear();
        chars.extend(row.iter().map(|c| if c.codepoint == 0 { '\0' } else { self.cell_char(c) }));
        crate::bidi::visual_order(chars, levels, order)
    }

    /// The logical column drawn at visual column `col` of display row `r`.
    pub fn logical_col(&self, r: usize, col: usize) -> usize {
        let mut order = Vec::new();
        if r < self.rows && self.bidi_order(r, &mut Vec::new(), &mut Vec::new(), &mut order) {
            return order.get(col).map_or(col, |&l| l as usize);
        }
        col
    }

    /// The visual column the cursor at logical column `col` of display row
    /// `r` is drawn at. After left-to-right text the cursor stays right after
    /// that text, even when the cell under it starts a right-to-left run
    /// (drawn at the run's right end): typing `echo '` before a right-to-left
    /// autosuggestion keeps the cursor next to the quote.
    pub fn cursor_visual_col(&self, r: usize, col: usize) -> usize {
        let (mut levels, mut order) = (Vec::new(), Vec::new());
        if r < self.rows && self.bidi_order(r, &mut Vec::new(), &mut levels, &mut order) {
            let visual = |l: usize| order.iter().position(|&o| o as usize == l);
            if let Some(prev) = col.checked_sub(1).filter(|&p| levels.get(p) == Some(&0)) {
                if let Some(v) = visual(prev).map(|v| v + 1).filter(|&v| v < order.len()) {
                    return v;
                }
            }
            return visual(col).unwrap_or(col);
        }
        col
    }

    /// The grapheme cluster text of a cell's `codepoint`, if it holds one.
    pub fn cluster(&self, cp: u32) -> Option<&str> {
        if cp & (CLUSTER_FLAG | PH_FLAG) != CLUSTER_FLAG {
            return None;
        }
        self.clusters.get((cp & CLUSTER_ID) as usize).map(|s| &**s)
    }

    /// A cell's character for searching and link detection: a cluster's first
    /// one, blank for empty cells.
    pub fn cell_char(&self, cell: &Cell) -> char {
        match self.cluster(cell.codepoint) {
            Some(text) => text.chars().next().unwrap_or(' '),
            None => char::from_u32(cell.codepoint).filter(|&c| c != '\0').unwrap_or(' '),
        }
    }

    /// Append a cell's text (the whole cluster) to `out`.
    pub fn push_cell_text(&self, cell: &Cell, out: &mut String) {
        match self.cluster(cell.codepoint) {
            Some(text) => out.push_str(text),
            None => out.push(self.cell_char(cell)),
        }
    }

    /// Append `c` to the grapheme in cell `(row, col)`, turning it into a
    /// cluster. `widen` makes a narrow one double-width (emoji presentation)
    /// when the cell after it is where the cursor is. False if it could not.
    fn append_to_cell(&mut self, row: usize, col: usize, c: char, widen: bool) -> bool {
        let cp = self.row_mut(row)[col].codepoint;
        let mut text = std::mem::take(&mut self.cluster_buf);
        text.clear();
        match self.cluster(cp) {
            Some(s) => text.push_str(s),
            None => match char::from_u32(cp).filter(|&b| b != '\0') {
                Some(base) => text.push(base),
                None => {
                    self.cluster_buf = text;
                    return false;
                }
            },
        }
        text.push(c);
        let id = match self.cluster_ids.get(text.as_str()) {
            _ if text.len() > MAX_CLUSTER_BYTES => None,
            Some(&id) => Some(id),
            None if self.clusters.len() < MAX_CLUSTERS => {
                let id = self.clusters.len() as u32;
                self.clusters.push(text.as_str().into());
                self.cluster_ids.insert(text.as_str().into(), id);
                Some(id)
            }
            None => None,
        };
        self.cluster_buf = text;
        let Some(id) = id else { return false };
        let was_wide = is_wide_cp(cp);
        let widen = widen && !was_wide && !self.wrap_pending && self.cur.col == col + 1 && col + 1 < self.cols;
        let r = self.row_mut(row);
        r[col].codepoint = CLUSTER_FLAG | if was_wide || widen { CLUSTER_WIDE } else { 0 } | id;
        if widen {
            // The right half takes the cell the cursor is on.
            r[col + 1] = Cell { codepoint: 0, ..r[col] };
            if col + 2 >= self.cols {
                self.cur.col = self.cols - 1;
                self.wrap_pending = self.autowrap;
            } else {
                self.cur.col = col + 2;
            }
        }
        self.mark(row);
        true
    }

    /// Whether the shell's line editor owns the cursor line (OSC 133 B..C on
    /// the main screen). It counts columns codepoint by codepoint with its own
    /// width table, so cells must follow that count there.
    fn in_line_editor(&self) -> bool {
        self.prompt_input_mark.is_some() && !self.alt_active
    }

    /// A double-width cell for `c`, a pictograph that is narrow by default
    /// (🏳, 🌡, 👁): zsh counts every codepoint in U+1F300..U+1F6FF as two
    /// columns. None if the cluster table is full.
    fn wide_pictograph(&mut self, c: char) -> Option<u32> {
        let mut buf = [0u8; 4];
        let text: &str = c.encode_utf8(&mut buf);
        let id = match self.cluster_ids.get(text) {
            Some(&id) => id,
            None if self.clusters.len() < MAX_CLUSTERS => {
                let id = self.clusters.len() as u32;
                self.clusters.push(text.into());
                self.cluster_ids.insert(text.into(), id);
                id
            }
            None => return None,
        };
        Some(CLUSTER_FLAG | CLUSTER_WIDE | id)
    }

    /// Mode 2027: join `c` to the grapheme before the cursor when it continues
    /// it (UAX #29 emoji rules: ZWJ sequences, VS16, skin tones, flags, tags).
    fn extend_grapheme(&mut self, c: char) -> bool {
        let joiner = std::mem::take(&mut self.joiner);
        let cp = c as u32;
        let is_ri = |cp: u32| (0x1F1E6..=0x1F1FF).contains(&cp);
        let Some((row, col)) = self.previous_cell() else { return false };
        let prev = self.row_mut(row)[col].codepoint;
        if prev == 0 || prev & PH_FLAG != 0 {
            return false;
        }
        let base = self.cluster(prev).and_then(|t| t.chars().next()).map_or(prev, |b| b as u32);
        let keycap = matches!(base, 0x23 | 0x2A | 0x30..=0x39);
        let joins = match cp {
            0x200D | 0xFE0F | 0xE0020..=0xE007F => true,
            0x1F3FB..=0x1F3FF => is_pictographic(base),
            // A flag is two regional indicators; a third starts the next flag.
            _ if is_ri(cp) => is_ri(prev),
            _ => joiner && is_pictographic(cp),
        };
        // While the shell edits the command line (OSC 133 B..C) its line editor
        // counts columns with the system wcwidth, codepoint by codepoint: only
        // zero-width ones may join, and nothing widens, or its cursor drifts.
        let line_editor = self.in_line_editor();
        if !joins || (line_editor && char_width(c) > 0) {
            return false;
        }
        let widen = !line_editor && ((cp == 0xFE0F && (is_pictographic(base) || keycap)) || is_ri(cp));
        if !self.append_to_cell(row, col, c, widen) {
            return false;
        }
        self.joiner = cp == 0x200D;
        true
    }

    /// Fold a combining mark (e.g. the NFD accents in "Việt") into the previously
    /// printed character: its precomposed form when there is one, else a cluster.
    fn combine_with_previous(&mut self, mark: char) {
        let Some((row, col)) = self.previous_cell() else { return };
        let r = self.row_mut(row);
        let cp = r[col].codepoint;
        if cp & PH_FLAG != 0 {
            // Placeholder diacritics: row, then column, then the image id's high byte.
            let n = (cp >> 27) & 3;
            if let (true, Ok(i)) = (n < 3, crate::graphics::PLACEHOLDER_DIACRITICS.binary_search(&(mark as u32))) {
                let shift = [9, 0, 18][n as usize];
                r[col].codepoint = (cp & !(3 << 27)) | (i as u32) << shift | (n + 1) << 27;
                self.mark(row);
            }
            return;
        }
        if self.cluster(cp).is_none() {
            let Some(base) = char::from_u32(cp).filter(|&c| c != '\0') else { return };
            if let Some(composed) = unicode_normalization::char::compose(base, mark) {
                self.row_mut(row)[col].codepoint = composed as u32;
                self.mark(row);
                return;
            }
        }
        self.append_to_cell(row, col, mark, false);
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
        let max_cap = self.scrollback + self.rows;
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
                if self.history + self.rows >= self.cap && self.cap < self.scrollback + self.rows {
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

    /// Feed one byte from the PTY. APC strings (the Kitty graphics protocol)
    /// are handled here, because vte drops them; everything else goes to `parser`.
    #[inline]
    pub fn feed_byte(&mut self, parser: &mut vte::Parser, b: u8) {
        match (self.apc_state, b) {
            (0, 0x1b) => self.apc_state = 1,
            (0, _) => parser.advance(self, b),
            (1, b'_') => {
                self.apc_state = 2;
                self.apc_buf.clear();
            }
            (1, _) => {
                self.apc_state = 0;
                parser.advance(self, 0x1b);
                self.feed_byte(parser, b);
            }
            (2, 0x1b) => self.apc_state = 3,
            // CAN / SUB abort the string.
            (2, 0x18 | 0x1a) => self.apc_state = 0,
            (2, _) => {
                if self.apc_buf.len() < MAX_IMAGE_STRING {
                    self.apc_buf.push(b);
                }
            }
            (_, b'\\') => {
                self.apc_state = 0;
                self.apc_dispatch();
            }
            // Any other ESC sequence ends the string unfinished.
            (_, _) => {
                self.apc_state = 0;
                parser.advance(self, 0x1b);
                self.feed_byte(parser, b);
            }
        }
    }

    fn apc_dispatch(&mut self) {
        let body = std::mem::take(&mut self.apc_buf);
        if let Some((cmd, payload)) = crate::graphics::parse_kitty(&body) {
            let (src, cols, rows, z, place_id, no_move, unicode) = (
                [cmd.x, cmd.y, cmd.src_w, cmd.src_h],
                cmd.cols,
                cmd.rows,
                cmd.z,
                cmd.placement_id,
                cmd.no_cursor_move,
                cmd.unicode,
            );
            let (outcome, reply) = self.graphics.kitty(cmd, payload);
            if let crate::graphics::KittyOutcome::Place { id } = outcome {
                if unicode {
                    self.place_virtual(id, cols, rows, z, place_id);
                } else {
                    self.place_image(id, src, cols, rows, z, place_id, no_move, false);
                }
            }
            if let Some(r) = reply {
                self.response.extend_from_slice(b"\x1b_G");
                self.response.extend_from_slice(r.as_bytes());
                self.response.extend_from_slice(b"\x1b\\");
            }
        }
        // Keep the buffer's capacity only for ordinary-sized chunks.
        self.apc_buf = if body.capacity() > 65536 { Vec::new() } else { body };
    }

    /// Show image `id` at the cursor. `src` is x, y, width, height in image
    /// pixels (0 = to the edge); `cols` / `rows` 0 = the image's own size in
    /// cells. Kitty moves the cursor to after the image's last column on its
    /// last row; Sixel (`sixel`) to the line below it, same column.
    #[allow(clippy::too_many_arguments)]
    fn place_image(&mut self, id: u32, src: [u32; 4], cols: u32, rows: u32, z: i32, placement_id: u32, no_move: bool, sixel: bool) {
        let Some(img) = self.graphics.image(id) else { return };
        let (iw, ih) = (img.width, img.height);
        let x = src[0].min(iw);
        let y = src[1].min(ih);
        let w = if src[2] == 0 { iw - x } else { src[2].min(iw - x) };
        let h = if src[3] == 0 { ih - y } else { src[3].min(ih - y) };
        if w == 0 || h == 0 {
            return;
        }
        let (cw, ch) = self.image_cell_px();
        let fit = cols > 0 || rows > 0;
        let cols = if cols > 0 { cols } else { w.div_ceil(cw) }.clamp(1, 1000) as usize;
        let rows = if rows > 0 { rows } else { h.div_ceil(ch) }.clamp(1, 1000) as usize;
        let start_col = self.cur.col;
        let line = self.lines_pushed + self.cur.row;
        self.graphics.place(crate::graphics::Placement {
            image_id: id,
            placement_id,
            line,
            col: start_col,
            cols,
            rows,
            src: [x, y, w, h],
            z,
            alt: self.alt_active,
            fit,
        });
        if !no_move {
            // Scrolling here keeps the image's absolute line right.
            let down = if sixel { rows } else { rows - 1 };
            for _ in 0..down {
                self.linefeed();
            }
            self.cur.col = if sixel { start_col } else { (start_col + cols).min(self.cols - 1) };
            self.wrap_pending = false;
        }
        self.graphics.prune(self.lines_pushed - self.history);
        self.all_dirty = true;
    }

    /// Cell size in px for image layout (a guess before the first resize reports it).
    fn image_cell_px(&self) -> (u32, u32) {
        match self.cell_px {
            (0, _) | (_, 0) => (8, 16),
            (cw, ch) => (cw as u32, ch as u32),
        }
    }

    /// Kitty `U=1`: remember a virtual placement; placeholder cells show it.
    /// `cols` / `rows` 0 = the image's own size in cells.
    fn place_virtual(&mut self, id: u32, cols: u32, rows: u32, z: i32, placement_id: u32) {
        let Some(img) = self.graphics.image(id) else { return };
        let (cw, ch) = self.image_cell_px();
        let cols = if cols > 0 { cols } else { img.width.div_ceil(cw) }.clamp(1, 1000) as usize;
        let rows = if rows > 0 { rows } else { img.height.div_ceil(ch) }.clamp(1, 1000) as usize;
        self.graphics.place_virtual(crate::graphics::VirtualPlacement { image_id: id, placement_id, cols, rows, z });
        self.all_dirty = true;
    }

    /// When an animated image on this terminal next changes frame.
    pub fn next_image_frame(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        let top = if self.alt_active { self.lines_pushed } else { self.lines_pushed - self.display_offset };
        self.graphics.next_frame_at(now, top, self.rows, self.alt_active)
    }

    /// Images to draw on the visible screen (see [`crate::graphics::ImageDraw`]).
    pub fn visible_images(&self, out: &mut Vec<crate::graphics::ImageDraw>) {
        if !self.graphics.placements.is_empty() {
            let top = if self.alt_active { self.lines_pushed } else { self.lines_pushed - self.display_offset };
            self.graphics.visible(top, self.rows, self.alt_active, std::time::Instant::now(), out);
        }
        if !self.graphics.virtuals.is_empty() {
            for r in 0..self.rows {
                self.placeholder_images(r, out);
            }
        }
    }

    /// The image of a placeholder cell: `(image id, placement id)`. The id is
    /// the text color (24-bit, or a 256-color index) plus the high byte from
    /// the third diacritic; the placement id is the underline color.
    fn placeholder_ids(&self, cell: &Cell) -> Option<(u32, u32)> {
        if cell.fg & FG_SET == 0 {
            return None;
        }
        let rgb = cell.fg & !0xFF;
        let msb = if (cell.codepoint >> 27) & 3 == 3 { (cell.codepoint >> 18) & 0xFF } else { 0 };
        let pid = self.extra_at(cell.extra()).map_or(0, |e| e.underline_color >> 8);
        let id = (rgb >> 8) | msb << 24;
        if self.graphics.virtual_for(id, pid).is_some() {
            return Some((id, pid));
        }
        let index = (0..=255u8).find(|&i| palette_with(&self.palette_override, i) == rgb)? as u32;
        let id = index | msb << 24;
        self.graphics.virtual_for(id, pid).map(|_| (id, pid))
    }

    /// Image pieces for the placeholder cells of visible row `r`. A cell
    /// without a column diacritic continues the one to its left, and one
    /// without any diacritics also keeps its row.
    fn placeholder_images(&self, r: usize, out: &mut Vec<crate::graphics::ImageDraw>) {
        // Run in progress: (image id, placement id, image row, image column, screen column, length).
        let mut run: Option<(u32, u32, u32, u32, usize, usize)> = None;
        let mut prev: Option<(u32, u32, u32, u32)> = None;
        for (c, cell) in self.display_row(r).iter().enumerate() {
            let cp = cell.codepoint;
            let ids = if cp & PH_FLAG != 0 { self.placeholder_ids(cell) } else { None };
            let Some((id, pid)) = ids else {
                if let Some(done) = run.take() {
                    self.placeholder_draw(r, done, out);
                }
                prev = None;
                continue;
            };
            let follows = |p: &(u32, u32, u32, u32)| p.0 == id && p.1 == pid;
            let (row, col) = match (cp >> 27) & 3 {
                0 => prev.filter(follows).map_or((0, 0), |p| (p.2, p.3 + 1)),
                1 => {
                    let row = (cp >> 9) & 0x1FF;
                    (row, prev.filter(|p| follows(p) && p.2 == row).map_or(0, |p| p.3 + 1))
                }
                _ => ((cp >> 9) & 0x1FF, cp & 0x1FF),
            };
            prev = Some((id, pid, row, col));
            match &mut run {
                Some(cur) if cur.0 == id && cur.1 == pid && cur.2 == row && cur.3 + cur.5 as u32 == col => cur.5 += 1,
                _ => {
                    if let Some(done) = run.replace((id, pid, row, col, c, 1)) {
                        self.placeholder_draw(r, done, out);
                    }
                }
            }
        }
        if let Some(done) = run {
            self.placeholder_draw(r, done, out);
        }
    }

    /// Draw a run of placeholder cells: the part of the image (fitted into the
    /// placement's box, aspect ratio kept, centered) that the cells cover.
    fn placeholder_draw(
        &self,
        r: usize,
        (id, pid, row, col, screen_col, len): (u32, u32, u32, u32, usize, usize),
        out: &mut Vec<crate::graphics::ImageDraw>,
    ) {
        let Some(vp) = self.graphics.virtual_for(id, pid) else { return };
        let Some(img) = self.graphics.image(id) else { return };
        let (key, rgba) = img.shown(std::time::Instant::now());
        let (cw, ch) = self.image_cell_px();
        let (cw, ch) = (cw as f32, ch as f32);
        let (iw, ih) = (img.width as f32, img.height as f32);
        let (bw, bh) = (vp.cols as f32 * cw, vp.rows as f32 * ch);
        let s = (bw / iw).min(bh / ih);
        let (ox, oy) = ((bw - iw * s) / 2.0, (bh - ih * s) / 2.0);
        // The run's rectangle in the box, clipped to the image.
        let (cx, cy) = (col as f32 * cw, row as f32 * ch);
        let x0 = cx.max(ox);
        let x1 = (cx + len as f32 * cw).min(ox + iw * s);
        let y0 = cy.max(oy);
        let y1 = (cy + ch).min(oy + ih * s);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        out.push(crate::graphics::ImageDraw {
            key,
            width: img.width,
            height: img.height,
            rgba: rgba.clone(),
            src: [(x0 - ox) / s, (y0 - oy) / s, (x1 - x0) / s, (y1 - y0) / s],
            row: r as isize,
            col: screen_col,
            cols: len,
            rows: 1,
            fit: true,
            z: vp.z,
            px: Some([x0 - cx, y0 - cy, x1 - x0, y1 - y0]),
        });
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
            let f = if fg == 0 { self.fg_color() } else { fg & !0xFF };
            let b = if bg == 0 { self.bg_color() } else { bg & !0xFF };
            fg = b | FG_SET;
            bg = f | BG_SET;
        }
        if p.attrs & ATTR_DIM != 0 {
            let rgb = if fg == 0 { self.fg_color() } else { fg & !0xFF };
            fg = ((rgb >> 1) & 0x7F7F_7F00) | FG_SET;
        }
        if p.attrs & ATTR_HIDDEN != 0 {
            fg = if bg == 0 { self.bg_color() | FG_SET } else { (bg & !0xFF) | FG_SET };
        }
        bg |= p.underline as u32;
        if p.attrs & ATTR_STRIKE != 0 {
            bg |= BG_STRIKE;
        }
        if p.attrs & ATTR_BOLD != 0 {
            fg |= FG_BOLD;
        }
        if p.attrs & ATTR_ITALIC != 0 {
            fg |= FG_ITALIC;
        }
        with_extra(fg, bg, p.extra)
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
            self.graphics.placements.retain(|p| !p.alt);
        }
        self.wrap_pending = false;
        self.selection = None;
        self.all_dirty = true;
    }

    fn full_reset(&mut self) {
        self.set_alt(false, false);
        self.graphics.clear();
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
        self.mouse_sgr_pixels = false;
        self.color_scheme_reports = false;
        self.grapheme_mode = GRAPHEME_DEFAULT.load(Ordering::Relaxed);
        self.joiner = false;
        self.kitty_keyboard = [KittyKeyboard::default(); 2];
        self.modify_other_keys = 0;
        self.progress = None;
        self.pointer_shape.clear();
        self.title_changed = true;
        self.palette_override = Vec::new();
        self.color_override = [0; 3];
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
        // Logical line and offset into it of every old row, to move prompt marks.
        let remap = !self.prompts.is_empty() || self.graphics.placements.iter().any(|p| !p.alt);
        let mut row_pos: Vec<(usize, usize)> = Vec::with_capacity(if remap { total } else { 0 });
        for i in 0..total {
            let p = (self.top + self.cap - self.history + i) % self.cap;
            if i == cursor_row {
                cursor = (lines.len(), current.len() + main_cur.col);
            }
            if remap {
                row_pos.push((lines.len(), current.len()));
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
        let mut line_rows: Vec<usize> = Vec::with_capacity(if remap { lines.len() + 1 } else { 0 });
        for (li, line) in lines.iter().enumerate() {
            let first_row = out_wrapped.len();
            if remap {
                line_rows.push(first_row);
            }
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
        let scrollback = scrollback_lines(cols);
        let new_hist = screen_start.min(scrollback);
        let first = screen_start - new_hist;
        let new_cap = (new_hist + rows).max(rows * 2).max(64).min(scrollback + rows);
        let mut cells = bytemuck::zeroed_vec::<Cell>(cols * new_cap);
        let mut wrapped = vec![false; new_cap];
        for (dst, src) in (first..(screen_start + rows).min(out_wrapped.len())).enumerate() {
            cells[dst * cols..(dst + 1) * cols].copy_from_slice(&out[src * cols..(src + 1) * cols]);
            wrapped[dst] = out_wrapped[src];
        }

        // 4. Move prompt marks and images from old absolute lines to the reflowed ones.
        let new_lines_pushed = self.lines_pushed.max(new_hist);
        if remap {
            line_rows.push(out_wrapped.len());
            let old_first = self.first_line();
            let map_row = |i: usize| -> Option<usize> {
                let &(li, off) = row_pos.get(i)?;
                let (start, end) = (*line_rows.get(li)?, *line_rows.get(li + 1)?);
                let row = (start + off / cols).min(end.saturating_sub(1).max(start));
                (row >= first).then(|| new_lines_pushed + row - screen_start)
            };
            let map_line = |l: usize| -> Option<usize> {
                let i = l.checked_sub(old_first)?;
                // An output end may be the line after the last one.
                if i >= total && i > 0 { map_row(total - 1).map(|r| r + i - total + 1) } else { map_row(i) }
            };
            self.prompts.retain_mut(|m| {
                let Some(prompt) = map_line(m.prompt) else { return false };
                m.prompt = prompt;
                m.output = m.output.and_then(|(start, end)| Some((map_line(start)?, end.and_then(map_line))));
                true
            });
            self.graphics.placements.retain_mut(|p| {
                if p.alt {
                    return true;
                }
                let Some(line) = map_line(p.line) else { return false };
                p.line = line;
                true
            });
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
        self.scrollback = scrollback;
        self.top = new_hist;
        self.history = new_hist;
        self.lines_pushed = new_lines_pushed;
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
        // Moved out for the loop so palette lookups do not borrow `self`.
        let pal = std::mem::take(&mut self.palette_override);
        let palette = |i: u8| palette_with(&pal, i);
        let mut it = params.iter();
        while let Some(sp) = it.next() {
            let pen = &mut self.cur.pen;
            match sp[0] {
                0 => {
                    // SGR 0 resets the underline color but not the OSC 8 hyperlink.
                    let extra = pen.extra;
                    *pen = Pen::default();
                    self.set_underline_color(extra, 0);
                }
                1 => pen.attrs |= ATTR_BOLD,
                2 => pen.attrs |= ATTR_DIM,
                3 => pen.attrs |= ATTR_ITALIC,
                // `4:0` .. `4:5`: none, single, double, curly, dotted, dashed.
                4 => pen.underline = sp.get(1).map_or(UNDERLINE_SINGLE, |&s| s.min(UNDERLINE_DASHED as u16) as u8),
                7 => pen.attrs |= ATTR_INVERSE,
                8 => pen.attrs |= ATTR_HIDDEN,
                9 => pen.attrs |= ATTR_STRIKE,
                21 => pen.underline = UNDERLINE_DOUBLE,
                22 => pen.attrs &= !(ATTR_BOLD | ATTR_DIM),
                23 => pen.attrs &= !ATTR_ITALIC,
                24 => pen.underline = 0,
                27 => pen.attrs &= !ATTR_INVERSE,
                28 => pen.attrs &= !ATTR_HIDDEN,
                29 => pen.attrs &= !ATTR_STRIKE,
                n @ 30..=37 => pen.fg = palette((n - 30) as u8) | FG_SET,
                38 => {
                    if let Some(c) = ext_color(sp, &mut it, palette) {
                        self.cur.pen.fg = c | FG_SET;
                    }
                }
                39 => pen.fg = 0,
                n @ 40..=47 => pen.bg = palette((n - 40) as u8) | BG_SET,
                48 => {
                    if let Some(c) = ext_color(sp, &mut it, palette) {
                        self.cur.pen.bg = c | BG_SET;
                    }
                }
                49 => pen.bg = 0,
                58 => {
                    if let Some(c) = ext_color(sp, &mut it, palette) {
                        let extra = self.cur.pen.extra;
                        self.set_underline_color(extra, c | 1);
                    }
                }
                59 => {
                    let extra = pen.extra;
                    self.set_underline_color(extra, 0);
                }
                n @ 90..=97 => pen.fg = palette((n - 90 + 8) as u8) | FG_SET,
                n @ 100..=107 => pen.bg = palette((n - 100 + 8) as u8) | BG_SET,
                _ => {}
            }
        }
        self.palette_override = pal;
    }

    /// Default foreground as `0xRRGGBB00`: OSC 10 override, else the config color.
    pub fn fg_color(&self) -> u32 {
        match self.color_override[0] {
            0 => default_fg(),
            c => c & !0xFF,
        }
    }

    /// Default background as `0xRRGGBB00`: OSC 11 override, else the config color.
    pub fn bg_color(&self) -> u32 {
        match self.color_override[1] {
            0 => default_bg(),
            c => c & !0xFF,
        }
    }

    /// Cursor color set by OSC 12, as `0xRRGGBB00`.
    pub fn cursor_color(&self) -> Option<u32> {
        (self.color_override[2] != 0).then(|| self.color_override[2] & !0xFF)
    }

    /// OSC 4: `4 ; index ; spec ; index ; spec ...`, where `spec` may be `?`.
    fn osc4(&mut self, params: &[&[u8]], st: &str) {
        for pair in params[1..].chunks(2) {
            let Some(i) = std::str::from_utf8(pair[0]).ok().and_then(|s| s.parse::<u8>().ok()) else { continue };
            match pair.get(1) {
                Some(&b"?") => {
                    let c = palette_with(&self.palette_override, i);
                    self.reply_color(&format!("4;{i}"), c, st);
                }
                Some(spec) => {
                    if let Some(c) = parse_x11_color(spec) {
                        let old = palette_with(&self.palette_override, i);
                        if self.palette_override.is_empty() {
                            self.palette_override = vec![0; 256];
                        }
                        self.palette_override[i as usize] = c | 1;
                        self.recolor(old, c);
                    }
                }
                None => {}
            }
        }
    }

    /// Repaint text already drawn with palette color `old` in `new`. Cells keep
    /// RGB, not palette indices (no spare bits in 12 bytes), so this matches by
    /// color: a true-color cell of exactly `old` changes too.
    fn recolor(&mut self, old: u32, new: u32) {
        let (old, new) = (old & !0xFF, new & !0xFF);
        if old == new {
            return;
        }
        for cell in self.cells.iter_mut().chain(self.alt.iter_mut()) {
            if cell.fg & FG_SET != 0 && cell.fg & !0xFF == old {
                cell.fg = new | (cell.fg & 0xFF);
            }
            if cell.bg & BG_SET != 0 && cell.bg & !0xFF == old {
                cell.bg = new | (cell.bg & 0xFF);
            }
        }
        self.all_dirty = true;
    }

    /// OSC 10 / 11 / 12 set or query the default fg, bg and cursor color. Extra
    /// parameters continue with the next code (`10;?;?` queries fg and bg).
    fn osc_dynamic_color(&mut self, first: usize, specs: &[&[u8]], st: &str) {
        for (slot, spec) in (first..3).zip(specs) {
            if *spec == b"?" {
                let c = match slot {
                    0 => self.fg_color(),
                    1 => self.bg_color(),
                    _ => self.cursor_color().unwrap_or_else(default_fg),
                };
                self.reply_color(&(10 + slot).to_string(), c, st);
            } else if let Some(c) = parse_x11_color(spec) {
                self.color_override[slot] = c | 1;
                self.all_dirty = true;
            }
        }
    }

    /// Answer a color query as xterm does: `OSC code ; rgb:rrrr/gggg/bbbb ST`.
    fn reply_color(&mut self, code: &str, c: u32, st: &str) {
        let (r, g, b) = ((c >> 24) & 0xFF, (c >> 16) & 0xFF, (c >> 8) & 0xFF);
        let _ = write!(self.response, "\x1b]{code};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{st}");
    }

    /// XTGETTCAP: answer one hex-encoded terminfo capability name.
    fn reply_termcap(&mut self, hex_name: &[u8]) {
        let name = decode_hex(hex_name);
        let value = name.as_deref().and_then(termcap);
        match value {
            Some(v) => {
                let _ = write!(self.response, "\x1bP1+r{}", std::str::from_utf8(hex_name).unwrap_or(""));
                if !v.is_empty() {
                    self.response.push(b'=');
                    for b in v.bytes() {
                        let _ = write!(self.response, "{b:02X}");
                    }
                }
            }
            None => {
                let _ = write!(self.response, "\x1bP0+r{}", std::str::from_utf8(hex_name).unwrap_or(""));
            }
        }
        self.response.extend_from_slice(b"\x1b\\");
    }

    /// DECRQSS: report the current SGR, scroll region or cursor style.
    fn reply_decrqss(&mut self, request: &[u8]) {
        let body = match request {
            b"m" => Some(format!("{}m", self.sgr_report())),
            b"r" => Some(format!("{};{}r", self.scroll_top + 1, self.scroll_bot + 1)),
            b" q" => Some(format!("{} q", self.cursor_shape.max(1))),
            b"\"q" => Some("0\"q".to_string()),
            _ => None,
        };
        let _ = match body {
            Some(b) => write!(self.response, "\x1bP1$r{b}\x1b\\"),
            None => write!(self.response, "\x1bP0$r\x1b\\"),
        };
    }

    /// The pen as an SGR parameter list (`0;1;38;2;r;g;b`).
    fn sgr_report(&self) -> String {
        use std::fmt::Write as _;
        let p = self.cur.pen;
        let mut out = String::from("0");
        for (bit, code) in [(ATTR_BOLD, 1), (ATTR_DIM, 2), (ATTR_ITALIC, 3), (ATTR_INVERSE, 7), (ATTR_HIDDEN, 8), (ATTR_STRIKE, 9)] {
            if p.attrs & bit != 0 {
                let _ = write!(out, ";{code}");
            }
        }
        if p.underline != 0 {
            let _ = write!(out, ";4:{}", p.underline);
        }
        for (color, base) in [(p.fg, 38), (p.bg, 48)] {
            if color != 0 {
                let _ = write!(out, ";{base};2;{};{};{}", color >> 24, (color >> 16) & 0xFF, (color >> 8) & 0xFF);
            }
        }
        out
    }

    /// Scrollback length and how far the view is scrolled back, in lines.
    pub fn scroll_position(&self) -> (usize, usize) {
        (self.history, self.display_offset)
    }

    /// Absolute line of the cursor (independent of the viewport).
    fn cursor_line(&self) -> usize {
        self.lines_pushed + self.cur.row
    }

    /// Track OSC 133 `A` (prompt), `C` (output starts) and `D` (output ends).
    fn record_prompt_mark(&mut self, kind: Option<u8>) {
        let line = self.cursor_line();
        match kind {
            Some(b'A') => {
                if self.prompts.last().is_some_and(|p| p.prompt == line) {
                    return;
                }
                if self.prompts.len() == MAX_PROMPTS {
                    self.prompts.remove(0);
                }
                self.prompts.push(PromptMark { prompt: line, output: None });
            }
            Some(b'C') => {
                if let Some(p) = self.prompts.last_mut() {
                    p.output = Some((line, None));
                }
            }
            Some(b'D') => {
                // Output that did not end in a newline still owns the cursor line.
                let end = line + usize::from(self.cur.col > 0);
                if let Some(PromptMark { output: Some((_, e @ None)), .. }) = self.prompts.last_mut() {
                    *e = Some(end);
                }
            }
            _ => {}
        }
    }

    /// First absolute line still in scrollback.
    fn first_line(&self) -> usize {
        self.lines_pushed - self.history
    }

    /// Scroll so absolute line `line` is at the top of the viewport (clamped).
    fn scroll_to_line(&mut self, line: usize) {
        let offset = self.lines_pushed.saturating_sub(line).min(self.history);
        if self.display_offset != offset {
            self.display_offset = offset;
            self.all_dirty = true;
        }
    }

    /// Jump to the previous (`delta < 0`) or next prompt relative to the top of
    /// the viewport. Returns `false` when there is none in that direction.
    pub fn jump_to_prompt(&mut self, delta: isize) -> bool {
        if self.alt_active {
            return false;
        }
        let top = self.lines_pushed - self.display_offset;
        let first = self.first_line();
        let mut lines = self.prompts.iter().map(|p| p.prompt).filter(|&l| l >= first);
        let target = if delta < 0 { lines.rfind(|&l| l < top) } else { lines.find(|&l| l > top) };
        match target {
            Some(line) => {
                self.scroll_to_line(line);
                true
            }
            // Past the last prompt: back to the live screen.
            None if delta > 0 && self.display_offset != 0 => {
                self.scroll_to_line(self.lines_pushed);
                true
            }
            None => false,
        }
    }

    /// Select the output of the most recent finished command (OSC 133 C..D) and
    /// return its text, scrolling it into view.
    pub fn select_last_output(&mut self) -> Option<String> {
        let first = self.first_line();
        let (start, end) = self.prompts.iter().rev().find_map(|p| match p.output {
            Some((s, Some(e))) if e > s && s >= first => Some((s, e)),
            _ => None,
        })?;
        self.selection = Some(Selection { start: (start, 0), end: (end - 1, self.cols - 1) });
        let top = self.lines_pushed - self.display_offset;
        if start < top || start >= top + self.rows {
            self.scroll_to_line(start);
        }
        self.all_dirty = true;
        self.selected_text()
    }

    /// Active Kitty keyboard protocol flags (`keymap::KITTY_*`) of the current screen.
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.kitty_keyboard[self.alt_active as usize].flags
    }

    // ------------------------------------------------------------------
    // Extra cell attributes (OSC 8 hyperlinks, SGR 58 underline colors)
    // ------------------------------------------------------------------

    fn extra_at(&self, idx: u8) -> Option<&CellExtra> {
        self.extras.get(idx as usize)?.as_ref()
    }

    /// Point the pen at `extra` with its underline color replaced.
    fn set_underline_color(&mut self, extra: u8, color: u32) {
        let link = self.extra_at(extra).and_then(|e| e.link.clone());
        self.cur.pen.extra = self.intern_extra(CellExtra { link, underline_color: color });
    }

    /// Point the pen at the current extra with its hyperlink replaced.
    fn set_link(&mut self, link: Option<OscLink>) {
        let underline_color = self.extra_at(self.cur.pen.extra).map_or(0, |e| e.underline_color);
        self.cur.pen.extra = self.intern_extra(CellExtra { link, underline_color });
    }

    /// Index of `extra` in the table, adding it if needed. Runs only when an escape
    /// sequence changes the pen, never per printed character. Returns 0 (plain text)
    /// if all 255 slots are still referenced by cells.
    fn intern_extra(&mut self, extra: CellExtra) -> u8 {
        if extra == CellExtra::default() {
            return 0;
        }
        if let Some(i) = self.extras.iter().position(|e| e.as_ref() == Some(&extra)) {
            return i as u8;
        }
        if self.extras.len() <= u8::MAX as usize {
            self.extras.push(Some(extra));
            return (self.extras.len() - 1) as u8;
        }
        let free = match self.extras.iter().skip(1).position(Option::is_none) {
            Some(i) => Some(i + 1),
            None => self.collect_extras(),
        };
        match free {
            Some(i) => {
                self.extras[i] = Some(extra);
                i as u8
            }
            None => 0,
        }
    }

    /// Free every extra slot no cell or saved pen refers to; returns the first free one.
    fn collect_extras(&mut self) -> Option<usize> {
        let mut used = [false; 256];
        for c in self.cells.iter().chain(self.alt.iter()) {
            used[c.extra() as usize] = true;
        }
        for c in [&self.cur, &self.saved, &self.saved_main] {
            used[c.pen.extra as usize] = true;
        }
        for (slot, used) in self.extras.iter_mut().zip(used).skip(1) {
            if !used {
                *slot = None;
            }
        }
        self.extras.iter().skip(1).position(Option::is_none).map(|i| i + 1)
    }

    /// Explicit underline color of a cell as `0xRRGGBB00`, if SGR 58 set one.
    pub fn underline_color(&self, cell: &Cell) -> Option<u32> {
        match cell.extra() {
            0 => None,
            i => self.extra_at(i).map(|e| e.underline_color).filter(|&c| c != 0).map(|c| c & !0xFF),
        }
    }

    /// One-line description of the visible cell `(row, col)` for the inspector.
    pub fn describe_cell(&self, row: usize, col: usize) -> Option<String> {
        use std::fmt::Write as _;
        let cell = *self.display_row(row).get(col)?;
        let mut out = match (self.cluster(cell.codepoint), char::from_u32(cell.codepoint).filter(|_| cell.codepoint != 0)) {
            (Some(text), _) => {
                let cps: Vec<String> = text.chars().map(|c| format!("U+{:04X}", c as u32)).collect();
                format!("{} {text:?}", cps.join(" "))
            }
            (None, Some(c)) => format!("U+{:04X} {c:?}", cell.codepoint),
            (None, None) if cell.codepoint & PH_FLAG != 0 => "image placeholder".to_string(),
            (None, None) => "empty".to_string(),
        };
        let color = |v: u32, set: u32| if v & set != 0 { format!("#{:06x}", v >> 8) } else { "default".to_string() };
        let _ = write!(out, "  fg {}  bg {}", color(cell.fg, FG_SET), color(cell.bg, BG_SET));
        if cell.fg & FG_BOLD != 0 {
            out.push_str("  bold");
        }
        if cell.fg & FG_ITALIC != 0 {
            out.push_str("  italic");
        }
        let underline = ["", "underline", "double-underline", "curly-underline", "dotted-underline", "dashed-underline"];
        if let Some(name) = underline.get((cell.bg & BG_UNDERLINE_MASK) as usize).filter(|n| !n.is_empty()) {
            let _ = write!(out, "  {name}");
            if let Some(c) = self.underline_color(&cell) {
                let _ = write!(out, " #{:06x}", c >> 8);
            }
        }
        if cell.bg & BG_STRIKE != 0 {
            out.push_str("  strike");
        }
        if let Some(link) = self.cell_link(&cell) {
            let _ = write!(out, "  link {}", link.uri);
        }
        Some(out)
    }

    /// The OSC 8 link of a cell as `(id, uri)`.
    fn cell_link(&self, cell: &Cell) -> Option<&OscLink> {
        match cell.extra() {
            0 => None,
            i => self.extra_at(i)?.link.as_ref(),
        }
    }

    /// OSC 8: `8 ; params ; URI` starts a hyperlink, an empty URI ends it.
    fn osc8(&mut self, params: &[&[u8]]) {
        let uri = params.get(2..).unwrap_or_default();
        // The URI may itself contain ';', which vte splits into extra parameters.
        let len = uri.iter().map(|p| p.len() + 1).sum::<usize>().saturating_sub(1);
        let link = if len == 0 || len > MAX_LINK_LEN {
            None
        } else {
            let uri = uri.join(&b';');
            let id = params[1]
                .split(|&b| b == b':')
                .find_map(|kv| kv.strip_prefix(b"id="))
                .unwrap_or_default();
            match (std::str::from_utf8(&uri), std::str::from_utf8(id)) {
                (Ok(uri), Ok(id)) => Some(OscLink { id: id.into(), uri: uri.into() }),
                _ => None,
            }
        };
        self.set_link(link);
    }
}

/// Parse `38;5;n`, `38;2;r;g;b` and their colon sub-parameter forms.
fn ext_color<'a>(sp: &[u16], it: &mut impl Iterator<Item = &'a [u16]>, palette: impl Fn(u8) -> u32) -> Option<u32> {
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

/// A DCS request Laplace answers.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum DcsRequest {
    XtGetTcap,
    Decrqss,
    /// Sixel image; `true` = background stays transparent (DCS P2 = 1).
    Sixel(bool),
}

/// Largest APC (Kitty graphics chunk) or sixel string accepted, in bytes.
const MAX_IMAGE_STRING: usize = 64 << 20;

/// Decode an XTGETTCAP hex string (`544E` → `TN`).
fn decode_hex(hex: &[u8]) -> Option<String> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = hex
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

/// Terminfo capabilities reported through XTGETTCAP; "" marks a boolean.
fn termcap(name: &str) -> Option<&'static str> {
    Some(match name {
        "TN" | "name" => "xterm-256color",
        "Co" | "colors" => "256",
        "RGB" => "8",
        "Tc" | "Su" | "fullkbd" => "",
        "Smulx" => "\x1b[4:%p1%dm",
        "Setulc" => "\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%dm",
        "Ss" => "\x1b[%p1%d q",
        "Se" => "\x1b[2 q",
        "Ms" => "\x1b]52;%p1%s;%p2%s\x07",
        "Sync" => "\x1b[?2026%?%p1%{1}%-%tl%eh%;",
        "smxx" => "\x1b[9m",
        "rmxx" => "\x1b[29m",
        "BE" => "\x1b[?2004h",
        "BD" => "\x1b[?2004l",
        "fe" => "\x1b[?1004h",
        "fd" => "\x1b[?1004l",
        _ => return None,
    })
}

/// OSC parameters joined back with `;` (vte splits on it), as lossy UTF-8 without
/// control characters, cut to at most `max` characters.
fn lossy_join(parts: &[&[u8]], max: usize) -> String {
    let joined = parts.join(&b';');
    String::from_utf8_lossy(&joined).chars().filter(|c| !c.is_control()).take(max).collect()
}

/// `palette(i)` with the OSC 4 overrides of one terminal applied.
#[inline]
fn palette_with(overrides: &[u32], i: u8) -> u32 {
    match overrides.get(i as usize) {
        Some(&c) if c != 0 => c & !0xFF,
        _ => palette(i),
    }
}

/// Parse an X11 color spec as used by OSC 4 / 10 / 11 / 12: `rgb:R/G/B` with 1-4
/// hex digits per channel (scaled to 8 bits), `#RGB`, `#RRGGBB`, `#RRRGGGBBB`,
/// `#RRRRGGGGBBBB` (most significant bits, as in `XParseColor`), or a color name
/// such as `slate blue`. Returns `0xRRGGBB00`.
fn parse_x11_color(spec: &[u8]) -> Option<u32> {
    let spec = std::str::from_utf8(spec).ok()?;
    let hex = |s: &str| (!s.is_empty() && s.len() <= 4).then(|| u32::from_str_radix(s, 16).ok()).flatten();
    let (r, g, b) = if let Some(rest) = spec.strip_prefix("rgb:") {
        let mut parts = rest.split('/');
        let mut channel = || {
            let p = parts.next()?;
            let max = (1u32 << (4 * p.len())) - 1;
            Some((hex(p)? * 255 + max / 2) / max)
        };
        let rgb = (channel()?, channel()?, channel()?);
        if parts.next().is_some() {
            return None;
        }
        rgb
    } else if let Some(digits) = spec.strip_prefix('#') {
        if digits.is_empty() || digits.len() % 3 != 0 || digits.len() > 12 {
            return None;
        }
        let n = digits.len() / 3;
        let channel = |i: usize| {
            let v = hex(&digits[i * n..(i + 1) * n])?;
            Some(if n == 1 { v << 4 } else { v >> (4 * (n - 2)) })
        };
        (channel(0)?, channel(1)?, channel(2)?)
    } else {
        return crate::x11colors::lookup(spec).map(|rgb| rgb << 8);
    };
    Some((r << 24) | (g << 16) | (b << 8))
}

/// Display width of a character: 0 (combining / zero-width), 1, or 2 (CJK / emoji),
/// following the Unicode East Asian Width and emoji presentation tables.
pub fn char_width(c: char) -> usize {
    use unicode_width::UnicodeWidthChar;
    // Regional indicators have emoji presentation: two columns each, as in
    // zsh, kitty and Ghostty (a flag pair joins into one two-column cluster).
    if (0x1F1E6..=0x1F1FF).contains(&(c as u32)) {
        return 2;
    }
    let w = c.width().unwrap_or(0).min(2);
    #[cfg(target_os = "macos")]
    if w == 1 && is_spacing_mark_range(c as u32) && macos_wcwidth(c) == Some(0) {
        return 0;
    }
    w
}

/// Scripts with spacing combining marks (Indic, Southeast Asian, Brahmic).
#[cfg(target_os = "macos")]
fn is_spacing_mark_range(cp: u32) -> bool {
    matches!(cp, 0x0900..=0x0DFF | 0x0F00..=0x109F | 0x1700..=0x1CFF | 0xA800..=0xABFF | 0x11000..=0x11FFF)
}

/// What macOS's own `wcwidth` says. Shells (zsh, bash, readline) count with it, so
/// for spacing marks such as Devanagari U+093F, which it treats as zero-width,
/// disagreeing would leave the line editor's cursor columns off.
#[cfg(target_os = "macos")]
fn macos_wcwidth(c: char) -> Option<usize> {
    use std::sync::OnceLock;
    unsafe extern "C" {
        fn wcwidth_l(wc: libc::wchar_t, loc: libc::locale_t) -> libc::c_int;
    }
    static LOCALE: OnceLock<usize> = OnceLock::new();
    // SAFETY: newlocale gets a valid NUL-terminated name and a null base; the
    // locale is never freed, so the pointer stays valid for wcwidth_l.
    let loc = *LOCALE.get_or_init(|| unsafe {
        libc::newlocale(libc::LC_CTYPE_MASK, c"en_US.UTF-8".as_ptr(), std::ptr::null_mut()) as usize
    });
    if loc == 0 {
        return None;
    }
    // SAFETY: loc is a live locale from newlocale.
    let w = unsafe { wcwidth_l(c as libc::wchar_t, loc as libc::locale_t) };
    usize::try_from(w).ok()
}

/// Smallest codepoint `char_width` reports as double-width (Hangul Jamo).
const FIRST_WIDE: u32 = 0x1100;

/// Whether a cell's codepoint is a double-width character. Called for every
/// printed character, so ASCII and other narrow text skip the width tables.
#[inline]
pub fn is_wide_cp(cp: u32) -> bool {
    if cp & (CLUSTER_FLAG | PH_FLAG) == CLUSTER_FLAG {
        return cp & CLUSTER_WIDE != 0;
    }
    cp >= FIRST_WIDE && char::from_u32(cp).is_some_and(|c| char_width(c) == 2)
}

/// Extended pictographic (emoji) codepoints that join a ZWJ sequence.
fn is_pictographic(cp: u32) -> bool {
    matches!(cp, 0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2194..=0x21AA | 0x231A..=0x23FF
        | 0x24C2 | 0x25AA..=0x25FE | 0x2600..=0x27BF | 0x2934 | 0x2935 | 0x2B05..=0x2B55 | 0x3030 | 0x303D
        | 0x3297 | 0x3299 | 0x1F000..=0x1FAFF)
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

/// What Cmd/Ctrl+Click on an OSC 8 link opens: `(target, is_path)`. `file://` URIs
/// become local paths; other URIs are only opened for a few well-known schemes, so
/// a program cannot make a click launch an arbitrary URL handler.
fn osc8_target(uri: &str) -> Option<(String, bool)> {
    if uri.starts_with("file://") {
        return parse_osc7_path(uri.as_bytes()).map(|path| (path, true));
    }
    let (scheme, _) = uri.split_once(':')?;
    const SCHEMES: [&str; 5] = ["http", "https", "ftp", "ftps", "mailto"];
    SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)).then(|| (uri.to_string(), false))
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
        if self.grapheme_mode && !c.is_ascii() && self.extend_grapheme(c) {
            return;
        }
        let mut w = char_width(c);
        if w == 0 {
            self.combine_with_previous(c);
            return;
        }
        let mut codepoint = if c == PLACEHOLDER { PH_FLAG } else { c as u32 };
        if w == 1 && (0x1F300..=0x1F6FF).contains(&codepoint) && self.cols >= 2 && self.in_line_editor() {
            if let Some(wide) = self.wide_pictograph(c) {
                (codepoint, w) = (wide, 2);
            }
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
            r[col] = Cell { codepoint, fg, bg };
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
            0x07 => self.bell = true,
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
            let first = params.iter().next().map_or(0, |p| p[0]);
            match intermediates {
                [] => self.sgr(params),
                // XTMODKEYS: `CSI > 4 ; level m` sets modifyOtherKeys; `CSI > 4 m` and
                // `CSI > m` reset it.
                b">" if first == 4 => {
                    self.modify_other_keys = params.iter().nth(1).map_or(0, |p| p[0].min(2) as u8);
                }
                b">" if first == 0 => self.modify_other_keys = 0,
                // XTQMODKEYS: `CSI ? 4 m`.
                b"?" if first == 4 => {
                    let level = self.modify_other_keys;
                    let _ = write!(self.response, "\x1b[>4;{level}m");
                }
                _ => {}
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
                    2 => {
                        self.clear_rows(0, rows);
                        let top = self.lines_pushed;
                        self.graphics.delete_lines(top..top + rows, self.alt_active);
                    }
                    3 => {
                        self.history = 0;
                        self.display_offset = 0;
                        self.graphics.prune(self.lines_pushed);
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
            'u' if intermediates.is_empty() => {
                self.cur = self.saved;
                self.clamp_cursor();
            }
            // Kitty keyboard protocol: query, push, pop and set the flags.
            'u' => {
                let kb = &mut self.kitty_keyboard[self.alt_active as usize];
                match intermediates {
                    b"?" => {
                        let flags = kb.flags;
                        let _ = write!(self.response, "\x1b[?{flags}u");
                    }
                    b">" => kb.push(raw(0) as u8),
                    b"<" => kb.pop(arg(0, 1)),
                    b"=" => kb.set(raw(0) as u8, arg(1, 1)),
                    _ => {}
                }
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
                        1016 => self.mouse_sgr_pixels = on,
                        2031 => self.color_scheme_reports = on,
                        2027 => self.grapheme_mode = on,
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
            // `CSI > 4 n` disables modifyOtherKeys.
            'n' if intermediates == b">" => {
                if raw(0) == 4 {
                    self.modify_other_keys = 0;
                }
            }
            'n' => match raw(0) {
                5 => self.response.extend_from_slice(b"\x1b[0n"),
                996 if private => self.response.extend_from_slice(color_scheme_report()),
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
                    1016 => if self.mouse_sgr_pixels { 1 } else { 2 },
                    2031 => if self.color_scheme_reports { 1 } else { 2 },
                    2027 => if self.grapheme_mode { 1 } else { 2 },
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
            // XTWINOPS size reports: text area in pixels, cell in pixels, text area in cells.
            't' if intermediates.is_empty() => {
                let (cw, ch) = (self.cell_px.0 as usize, self.cell_px.1 as usize);
                let _ = match raw(0) {
                    14 => write!(self.response, "\x1b[4;{};{}t", self.rows * ch, self.cols * cw),
                    16 => write!(self.response, "\x1b[6;{ch};{cw}t"),
                    18 => write!(self.response, "\x1b[8;{};{}t", self.rows, self.cols),
                    _ => Ok(()),
                };
            }
            // VT220 class with Sixel graphics (4) and ANSI color (22).
            'c' if intermediates.is_empty() => self.response.extend_from_slice(b"\x1b[?62;4;22c"),
            'c' if intermediates == b">" => self.response.extend_from_slice(b"\x1b[>0;0;0c"),
            'c' if intermediates == b"=" => self.response.extend_from_slice(b"\x1bP!|00000000\x1b\\"),
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        let st = if bell_terminated { "\x07" } else { "\x1b\\" };
        match params.first().copied().unwrap_or_default() {
            // OSC 104 [; index ...]: reset palette entries (all without indices).
            b"104" => {
                let all = params.len() < 2 || params[1].is_empty();
                if !self.palette_override.is_empty() {
                    let listed = params[1..].iter().filter_map(|p| std::str::from_utf8(p).ok()?.parse::<u8>().ok());
                    let mut reset = |i: u8| {
                        let old = palette_with(&self.palette_override, i);
                        self.palette_override[i as usize] = 0;
                        self.recolor(old, palette(i));
                    };
                    if all {
                        (0..=255).for_each(&mut reset);
                    } else {
                        listed.for_each(&mut reset);
                    }
                }
                if all {
                    self.palette_override = Vec::new();
                }
                return;
            }
            code @ (b"110" | b"111" | b"112") => {
                self.color_override[(code[2] - b'0') as usize] = 0;
                self.all_dirty = true;
                return;
            }
            _ => {}
        }
        if params.len() < 2 {
            return;
        }
        match params[0] {
            b"4" => {
                self.osc4(params, st);
            }
            // ConEmu progress: `9 ; 4 ; state ; percent`.
            b"9" if params[1] == b"4" => {
                let num = |i: usize| params.get(i).and_then(|p| std::str::from_utf8(p).ok()?.parse::<u8>().ok());
                let progress = match num(2).unwrap_or(0) {
                    0 => None,
                    state @ 1..=4 => Some((state, num(3).unwrap_or(0).min(100))),
                    _ => self.progress,
                };
                if progress != self.progress {
                    self.progress = progress;
                    self.title_changed = true;
                }
            }
            // iTerm2 notification: `9 ; body`.
            b"9" => self.notification = Some((String::new(), lossy_join(&params[1..], 512))),
            // `777 ; notify ; title ; body` (rxvt / Ghostty).
            b"777" if params[1] == b"notify" => {
                let title = params.get(2).map_or(String::new(), |t| lossy_join(&[t], 128));
                self.notification = Some((title, lossy_join(params.get(3..).unwrap_or_default(), 512)));
            }
            b"22" => {
                let name = lossy_join(&params[1..2], 32);
                if name != self.pointer_shape {
                    self.pointer_shape = name;
                }
            }
            code @ (b"10" | b"11" | b"12") => {
                self.osc_dynamic_color((code[1] - b'0') as usize, &params[1..], st);
            }
            b"0" | b"2" => {
                if let Ok(s) = std::str::from_utf8(params[1]) {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() && self.title != trimmed {
                        self.title.clear();
                        self.title.push_str(trimmed);
                        self.title_changed = true;
                    }
                }
            }
            b"7" => {
                let cwd = parse_osc7_path(params[1]);
                if cwd.is_some() && cwd != self.reported_cwd {
                    self.reported_cwd = cwd;
                    self.title_changed = true;
                }
            }
            b"52" => {
                // `52;<targets>;<base64>`. Only setting is supported: reading the
                // clipboard from the terminal would leak it to any program.
                if let Some(text) = params.get(2).filter(|d| **d != b"?").and_then(|d| decode_base64(d)) {
                    if let Ok(text) = String::from_utf8(text) {
                        self.clipboard_request = Some(text);
                    }
                }
            }
            b"8" => {
                self.osc8(params);
            }
            b"133" => {
                match params[1].first() {
                    Some(b'B') => {
                        self.prompt_input_mark = Some((self.abs_line(self.cur.row), self.cur.col));
                    }
                    Some(b'A' | b'C' | b'D') => self.prompt_input_mark = None,
                    _ => {}
                }
                if !self.alt_active {
                    self.record_prompt_mark(params[1].first().copied());
                }
            }
            _ => {}
        }
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        self.dcs = match (intermediates, action) {
            (b"+", 'q') => Some(DcsRequest::XtGetTcap),
            (b"$", 'q') => Some(DcsRequest::Decrqss),
            (b"", 'q') => Some(DcsRequest::Sixel(params.iter().nth(1).and_then(|p| p.first()) == Some(&1))),
            _ => None,
        };
        self.dcs_buf.clear();
    }

    fn put(&mut self, byte: u8) {
        let limit = if matches!(self.dcs, Some(DcsRequest::Sixel(_))) { MAX_IMAGE_STRING } else { 1024 };
        if self.dcs.is_some() && self.dcs_buf.len() < limit {
            self.dcs_buf.push(byte);
        }
    }

    fn unhook(&mut self) {
        let buf = std::mem::take(&mut self.dcs_buf);
        match self.dcs.take() {
            Some(DcsRequest::XtGetTcap) => {
                for name in buf.split(|&b| b == b';') {
                    self.reply_termcap(name);
                }
            }
            Some(DcsRequest::Decrqss) => self.reply_decrqss(&buf),
            Some(DcsRequest::Sixel(transparent)) => {
                if let Some((w, h, rgba)) = crate::graphics::decode_sixel(&buf, transparent) {
                    let id = self.graphics.insert(0, w, h, rgba);
                    self.place_image(id, [0, 0, w, h], 0, 0, 0, 0, false, true);
                }
            }
            None => {}
        }
        // A large sixel buffer is not kept around.
        self.dcs_buf = if buf.capacity() > 4096 { Vec::new() } else { buf };
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
            g.feed_byte(&mut p, b);
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
    fn sgr_underline_styles_and_strikethrough() {
        let mut g = Grid::new(10, 1);
        feed(&mut g, "\x1b[4ma\x1b[4:3mb\x1b[21mc\x1b[4:0md\x1b[4:4;9me\x1b[29mf\x1b[4:5mg\x1b[24mh");
        let style = |c: &Cell| (c.bg & BG_UNDERLINE_MASK, c.bg & BG_STRIKE != 0);
        let row: Vec<_> = g.display_row(0)[..8].iter().map(style).collect();
        assert_eq!(
            row,
            [(1, false), (3, false), (2, false), (0, false), (4, true), (4, false), (5, false), (0, false)]
        );
    }

    #[test]
    fn sgr_58_sets_underline_color_without_touching_colors() {
        let mut g = Grid::new(8, 1);
        feed(&mut g, "\x1b[4;31;58;2;255;0;16ma\x1b[58:5:2mb\x1b[59mc\x1b[58:2::1:2:3;0md");
        let row = g.display_row(0);
        assert_eq!(row[0].fg & !FG_EXTRA, palette(1) | FG_SET);
        assert_eq!(row[0].bg & !BG_EXTRA, BG_UNDERLINE);
        assert_ne!(row[0].extra(), 0);
        assert_eq!(g.underline_color(&row[0]), Some(0xFF00_1000));
        assert_eq!(g.underline_color(&row[1]), Some(palette(2)));
        assert_eq!(g.underline_color(&row[2]), None);
        assert_eq!(g.underline_color(&row[3]), None, "SGR 0 resets the underline color");
        assert_eq!(row[3], Cell { codepoint: 'd' as u32, fg: 0, bg: 0 });
    }

    #[test]
    fn osc8_hyperlink_spans_its_cells() {
        let mut g = Grid::new(20, 1);
        feed(&mut g, "\x1b]8;id=x;https://example.com/a;b\x1b\\link\x1b]8;;\x1b\\ plain");
        let link = g.find_hyperlink_at(0, 2, None).expect("osc 8 link");
        assert_eq!((link.start_col, link.end_col), (0, 3));
        assert_eq!(link.url, "https://example.com/a;b");
        assert!(!link.is_path);
        assert_eq!(g.find_hyperlink_at(0, 6, None), None);
    }

    #[test]
    fn osc8_survives_sgr_reset_and_adjacent_links_stay_apart() {
        let mut g = Grid::new(20, 1);
        feed(&mut g, "\x1b]8;;http://a.test\x07a\x1b[0;1mb\x1b]8;;http://b.test\x07c\x1b]8;;\x07d");
        let a = g.find_hyperlink_at(0, 1, None).expect("link a");
        assert_eq!((a.start_col, a.end_col, a.url.as_str()), (0, 1, "http://a.test"));
        let b = g.find_hyperlink_at(0, 2, None).expect("link b");
        assert_eq!((b.start_col, b.end_col, b.url.as_str()), (2, 2, "http://b.test"));
        assert_eq!(g.find_hyperlink_at(0, 3, None), None);
    }

    #[test]
    fn osc8_targets() {
        assert_eq!(osc8_target("file://host/tmp/a%20b.txt"), Some(("/tmp/a b.txt".into(), true)));
        assert_eq!(osc8_target("HTTPS://x.test"), Some(("HTTPS://x.test".into(), false)));
        assert_eq!(osc8_target("mailto:me@x.test"), Some(("mailto:me@x.test".into(), false)));
        assert_eq!(osc8_target("javascript:alert(1)"), None);
        assert_eq!(osc8_target("-a Calculator"), None);
        let mut g = Grid::new(10, 1);
        feed(&mut g, "\x1b]8;;x-evil://run\x1b\\click\x1b]8;;\x1b\\");
        assert_eq!(g.find_hyperlink_at(0, 1, None), None);
    }

    #[test]
    fn extra_slots_are_reclaimed_once_cells_stop_using_them() {
        let mut g = Grid::new(4, 2);
        for i in 0..600 {
            feed(&mut g, &format!("\x1b]8;;https://e.test/{i}\x1b\\x\x1b]8;;\x1b\\\r"));
        }
        let link = g.find_hyperlink_at(0, 0, None).expect("latest link");
        assert_eq!(link.url, "https://e.test/599");
        assert!(g.extras.len() <= 256);
    }

    #[test]
    fn kitty_keyboard_stack_per_screen() {
        let mut g = Grid::new(10, 3);
        feed(&mut g, "\x1b[?u");
        assert_eq!(std::mem::take(&mut g.response), b"\x1b[?0u");
        feed(&mut g, "\x1b[>1u\x1b[>11u");
        assert_eq!(g.kitty_keyboard_flags(), 11);
        feed(&mut g, "\x1b[=4;2u");
        assert_eq!(g.kitty_keyboard_flags(), 15);
        feed(&mut g, "\x1b[=1;3u\x1b[?u");
        assert_eq!(std::mem::take(&mut g.response), b"\x1b[?14u");
        feed(&mut g, "\x1b[<u");
        assert_eq!(g.kitty_keyboard_flags(), 1);

        // The alternate screen has its own stack.
        feed(&mut g, "\x1b[?1049h");
        assert_eq!(g.kitty_keyboard_flags(), 0);
        feed(&mut g, "\x1b[>31u");
        assert_eq!(g.kitty_keyboard_flags(), 31);
        feed(&mut g, "\x1b[?1049l");
        assert_eq!(g.kitty_keyboard_flags(), 1);

        // Popping more than was pushed leaves the flags at zero.
        feed(&mut g, "\x1b[<5u");
        assert_eq!(g.kitty_keyboard_flags(), 0);
    }

    #[test]
    fn kitty_stack_is_bounded_and_does_not_restore_the_cursor() {
        let mut g = Grid::new(10, 3);
        feed(&mut g, "\x1b[2;3H\x1b[s\x1b[1;1H");
        for i in 1..=12 {
            feed(&mut g, &format!("\x1b[>{i}u"));
        }
        assert_eq!((g.cur.row, g.cur.col), (0, 0), "CSI > u is not DECRC");
        feed(&mut g, "\x1b[<8u");
        assert_eq!(g.kitty_keyboard_flags(), 4);
        feed(&mut g, "\x1b[<u");
        assert_eq!(g.kitty_keyboard_flags(), 0, "oldest entries were dropped");
        feed(&mut g, "\x1b[u");
        assert_eq!((g.cur.row, g.cur.col), (1, 2));
    }

    #[test]
    fn scrollback_limit_is_bytes_per_width() {
        assert_eq!(lines_for_bytes(usize::MAX, 80), SCROLLBACK);
        assert_eq!(lines_for_bytes(80 * 12 * 100, 80), 100);
        assert_eq!(lines_for_bytes(80 * 12 * 100, 160), 50);
        assert_eq!(lines_for_bytes(0, 80), 0);
        assert_eq!(lines_for_bytes(usize::MAX - 1, 1), MAX_SCROLLBACK);
    }

    #[test]
    fn select_all_spans_scrollback_and_skips_blank_edges() {
        let mut g = Grid::new(6, 3);
        feed(&mut g, "\r\none\r\ntwo\r\nthree\r\nfour");
        g.select_all();
        assert_eq!(g.selected_text().as_deref(), Some("one\ntwo\nthree\nfour"));
        g.clear_history();
        feed(&mut g, "\x1b[2J\x1b[H");
        g.select_all();
        assert_eq!(g.selected_text(), None, "nothing to select on an empty screen");
    }

    #[test]
    fn shift_click_extends_from_the_far_end() {
        let mut g = Grid::new(10, 3);
        feed(&mut g, "abcdefghij\r\nklmnopqrst\r\nuvwxyz");
        assert!(!g.extend_selection(0, 0), "no selection to extend");
        g.set_selection(Some(Selection { start: (1, 2), end: (1, 4) }));
        // After the selection: the start stays.
        assert!(g.extend_selection(2, 1));
        assert_eq!(g.selected_text().as_deref(), Some("mnopqrst\nuv"));
        // Before it: the far end (now 2,1) stays the anchor.
        assert!(g.extend_selection(0, 8));
        assert_eq!(g.selected_text().as_deref(), Some("ij\nklmnopqrst\nuv"));
        // Dragging on keeps that anchor.
        g.set_selection_end(1, 0);
        assert_eq!(g.selected_text().as_deref(), Some("klmnopqrst\nuv"));
    }

    #[test]
    fn parses_x11_color_specs() {
        assert_eq!(parse_x11_color(b"rgb:ff/80/00"), Some(0xFF80_0000));
        assert_eq!(parse_x11_color(b"rgb:ffff/8080/0000"), Some(0xFF80_0000));
        assert_eq!(parse_x11_color(b"rgb:f/8/0"), Some(0xFF88_0000));
        assert_eq!(parse_x11_color(b"#1e1e2e"), Some(0x1E1E_2E00));
        assert_eq!(parse_x11_color(b"#f80"), Some(0xF080_0000), "XParseColor: most significant bits");
        assert_eq!(parse_x11_color(b"#ffff80800000"), Some(0xFF80_0000));
        assert_eq!(parse_x11_color(b"red"), Some(0xFF00_0000));
        assert_eq!(parse_x11_color(b"Slate Blue"), Some(0x6A5A_CD00));
        assert_eq!(parse_x11_color(b"nope"), None);
        assert_eq!(parse_x11_color(b"rgb:ff/80"), None);
        assert_eq!(parse_x11_color(b"#12345"), None);
    }

    #[test]
    fn kitty_and_sixel_images_are_placed_at_the_cursor() {
        let mut g = Grid::new(20, 5);
        g.cell_px = (10, 20);
        // 30x40 px RGBA (all zero) = 3x2 cells; base64 of 4800 zero bytes.
        let payload = "A".repeat(6400);
        feed(&mut g, &format!("ab\x1b_Ga=T,f=32,s=30,v=40,i=1;{payload}\x1b\\"));
        assert_eq!(String::from_utf8(std::mem::take(&mut g.response)).unwrap(), "\x1b_Gi=1;OK\x1b\\");
        let p = &g.graphics.placements[0];
        assert_eq!((p.line, p.col, p.cols, p.rows), (0, 2, 3, 2));
        assert_eq!((g.cur.row, g.cur.col), (1, 5), "cursor after the image's last column, on its last row");
        assert_eq!(line(&g, 0), "ab", "the APC string is not printed");

        let mut drawn = Vec::new();
        g.visible_images(&mut drawn);
        assert_eq!((drawn.len(), drawn[0].row, drawn[0].col), (1, 0, 2));

        // Scrolling moves it up with the text, then out of view.
        feed(&mut g, "\r\n\n\n\n");
        drawn.clear();
        g.visible_images(&mut drawn);
        assert_eq!(drawn[0].row, -1);

        // Sixel: 6 px tall = 1 row at 20 px cells; cursor goes below it.
        feed(&mut g, "\x1b[H\x1bPq#0;2;100;0;0#0~~~\x1b\\");
        assert_eq!(g.graphics.placements.len(), 2);
        assert_eq!((g.cur.row, g.cur.col), (1, 0));

        // A full reset forgets everything; DA1 advertises Sixel.
        feed(&mut g, "\x1bc\x1b[c");
        assert!(g.graphics.placements.is_empty());
        assert_eq!(g.response, b"\x1b[?62;4;22c");
    }

    /// Visible text of row `r` with clusters spelled out and wide halves skipped.
    fn cluster_line(g: &Grid, r: usize) -> String {
        let mut out = String::new();
        for c in g.display_row(r).iter().filter(|c| c.codepoint != 0) {
            g.push_cell_text(c, &mut out);
        }
        out
    }

    #[test]
    fn grapheme_clusters_in_mode_2027() {
        let mut g = Grid::new(20, 3);
        g.grapheme_mode = true;
        // ZWJ family, flags (two indicators each), skin tone: two cells apiece.
        feed(&mut g, "👨\u{200D}👩\u{200D}👧|🇻🇳🇺🇸|👍🏽|");
        assert_eq!(cluster_line(&g, 0), "👨\u{200D}👩\u{200D}👧|🇻🇳🇺🇸|👍🏽|");
        let row = g.display_row(0);
        assert!(row[0].codepoint & CLUSTER_FLAG != 0 && is_wide_cp(row[0].codepoint));
        assert_eq!((row[2].codepoint, g.cluster(row[3].codepoint)), ('|' as u32, Some("🇻🇳")));
        assert!(is_wide_cp(row[3].codepoint));
        assert_eq!(g.cluster(row[5].codepoint), Some("🇺🇸"));
        assert_eq!(g.cur.col, 11);

        // VS16 widens a text-default symbol; the cursor moves past both halves.
        feed(&mut g, "\r\n❤\u{FE0F}x#\u{FE0F}\u{20E3}y");
        let row = g.display_row(1);
        assert_eq!((g.cluster(row[0].codepoint), row[2].codepoint), (Some("❤\u{FE0F}"), 'x' as u32));
        assert_eq!((g.cluster(row[3].codepoint), row[5].codepoint), (Some("#\u{FE0F}\u{20E3}"), 'y' as u32));

        // Copy keeps the whole sequence; search sees one char per cell.
        g.select_all();
        assert_eq!(g.selected_text().unwrap(), "👨\u{200D}👩\u{200D}👧|🇻🇳🇺🇸|👍🏽|\n❤\u{FE0F}x#\u{FE0F}\u{20E3}y");
        assert_eq!(g.cell_char(&g.display_row(0)[0]), '👨');

        // Mode 2027 off: every emoji keeps its own cells (ZWJ folds into the one before).
        feed(&mut g, "\x1b[?2027l\x1b[?2027$p\r\n👨\u{200D}👩|");
        assert_eq!(std::mem::take(&mut g.response), b"\x1b[?2027;2$y");
        assert_eq!(g.cluster(g.display_row(2)[0].codepoint), Some("👨\u{200D}"));
        assert_eq!(g.display_row(2)[4].codepoint, '|' as u32);
    }

    #[test]
    fn rtl_rows_map_between_visual_and_logical_columns() {
        let mut g = Grid::new(6, 1);
        feed(&mut g, "a אבג");
        // Drawn as "a גבא": visual column 2 shows logical column 4 (ג).
        assert_eq!((g.logical_col(0, 2), g.cursor_visual_col(0, 4)), (4, 2));
        assert_eq!(g.logical_col(0, 0), 0);
        // A cursor right after left-to-right text stays next to it, not at
        // the right end of the run its cell starts.
        assert_eq!(g.cursor_visual_col(0, 2), 2);
        // After a right-to-left letter it follows its own cell.
        assert_eq!(g.cursor_visual_col(0, 3), 3);
        // Typing `echo '` before a right-to-left autosuggestion.
        let mut g = Grid::new(14, 1);
        feed(&mut g, "echo 'سلام'\x1b[7G");
        assert_eq!(g.cursor_pos().map(|(x, _)| g.cursor_visual_col(0, x)), Some(6));
        // Full-screen applications get the cells as they are.
        feed(&mut g, "\x1b[?1049h\x1b[Hאב");
        assert_eq!(g.logical_col(0, 0), 0);
    }

    #[test]
    fn marks_without_a_precomposed_form_become_clusters() {
        let mut g = Grid::new(10, 1);
        g.grapheme_mode = false;
        feed(&mut g, "q\u{0303}e\u{0301}z");
        let row = g.display_row(0);
        assert_eq!(g.cluster(row[0].codepoint), Some("q\u{0303}"));
        assert_eq!(row[1].codepoint, 'é' as u32, "precomposed when possible");
        assert_eq!((row[2].codepoint, g.cur.col), ('z' as u32, 3));
        assert_eq!(cluster_line(&g, 0), "q\u{0303}éz");
    }

    #[test]
    fn command_line_input_keeps_per_codepoint_widths() {
        // At the prompt (OSC 133 B..C) zsh counts 👍🏽 as 4 columns and ❤️ as 1.
        let mut g = Grid::new(20, 2);
        g.grapheme_mode = true;
        feed(&mut g, "\x1b]133;A\x07$ \x1b]133;B\x07👍🏽❤\u{FE0F}x");
        assert_eq!(g.cur.col, 2 + 4 + 1 + 1);
        assert_eq!(g.cluster(g.display_row(0)[6].codepoint), Some("❤\u{FE0F}"));
        // Command output clusters as usual.
        feed(&mut g, "\r\n\x1b]133;C\x07👍🏽❤\u{FE0F}x");
        assert_eq!(g.cur.col, 2 + 2 + 1);
        // Narrow-by-default pictographs are two columns there too (zsh's table).
        let mut g = Grid::new(20, 2);
        feed(&mut g, "\x1b]133;A\x07$ \x1b]133;B\x07🏳\u{FE0F}x");
        assert_eq!(g.cur.col, 2 + 2 + 1);
        assert_eq!(g.cluster(g.display_row(0)[2].codepoint), Some("🏳\u{FE0F}"));
        // A flag: two regional indicators of two columns each at the prompt,
        // one two-column cluster in the output.
        let mut g = Grid::new(20, 2);
        g.grapheme_mode = true;
        feed(&mut g, "\x1b]133;A\x07$ \x1b]133;B\x07🇻🇳");
        assert_eq!(g.cur.col, 2 + 4);
        feed(&mut g, "\r\n\x1b]133;C\x07🇻🇳");
        assert_eq!(g.cur.col, 2);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn spacing_marks_follow_the_system_wcwidth() {
        // zsh on macOS counts "कि नमस्ते" as 6 columns (U+093F is zero-width
        // there); any other count desyncs the line editor's cursor.
        let mut g = Grid::new(20, 1);
        feed(&mut g, "कि नमस्ते");
        assert_eq!(g.cur.col, 6);
        assert_eq!(g.cluster(g.display_row(0)[0].codepoint), Some("कि"));
    }

    #[test]
    fn kitty_unicode_placeholders_show_virtual_placements() {
        let mut g = Grid::new(10, 3);
        g.cell_px = (10, 20);
        // 20x40 px image, virtual placement in a 2x2 cell box (20x40 px): each
        // cell row shows half of it.
        let payload = "A".repeat(4267);
        feed(&mut g, &format!("\x1b_Ga=t,f=32,s=20,v=40,i=7,q=2;{payload}\x1b\\\x1b_Ga=p,U=1,i=7,c=2,r=2,q=2\x1b\\"));
        assert!(g.graphics.placements.is_empty(), "nothing placed at the cursor");
        assert_eq!((g.cur.row, g.cur.col), (0, 0), "a virtual placement does not move the cursor");
        let d = |i: usize| char::from_u32(crate::graphics::PLACEHOLDER_DIACRITICS[i]).unwrap();
        // Row 0 with explicit row + column, the next cell inherits; row 1 by
        // 256-color index 7 with only a row diacritic.
        feed(&mut g, &format!("\x1b[38;2;0;0;7m\u{10EEEE}{}{}\u{10EEEE}\r\n", d(0), d(0)));
        feed(&mut g, &format!("\x1b[38;5;7mx\u{10EEEE}{}\u{10EEEE}\x1b[0m", d(1)));
        assert_eq!(line(&g, 0), "", "placeholders have no text");
        let mut drawn = Vec::new();
        g.visible_images(&mut drawn);
        assert_eq!(drawn.len(), 2);
        assert_eq!((drawn[0].row, drawn[0].col, drawn[0].cols), (0, 0, 2));
        assert_eq!(drawn[0].src, [0.0, 0.0, 20.0, 20.0]);
        assert_eq!(drawn[0].px, Some([0.0, 0.0, 20.0, 20.0]));
        assert_eq!((drawn[1].row, drawn[1].col), (1, 1));
        assert_eq!(drawn[1].src, [0.0, 20.0, 20.0, 20.0], "second image row");

        // A wider box keeps the aspect ratio: the image is centered horizontally.
        feed(&mut g, "\x1b_Ga=p,U=1,i=7,c=4,r=2,q=2\x1b\\");
        drawn.clear();
        g.visible_images(&mut drawn);
        assert_eq!(drawn[0].px, Some([10.0, 0.0, 10.0, 20.0]), "starts half a cell into the box");

        // Deleting the image removes the virtual placement.
        feed(&mut g, "\x1b_Ga=d,d=I,i=7\x1b\\");
        drawn.clear();
        g.visible_images(&mut drawn);
        assert!(drawn.is_empty());
    }

    #[test]
    fn osc4_recolors_text_already_on_screen() {
        let mut g = Grid::new(8, 1);
        feed(&mut g, "\x1b[31mr\x1b[42mg\x1b[0mn");
        feed(&mut g, "\x1b]4;1;#00ff00\x07");
        let row = g.display_row(0);
        assert_eq!(row[0].fg, 0x00FF_0000 | FG_SET, "red text is now green");
        assert_eq!(row[1].bg & !0xFF, palette(2), "other colors untouched");
        assert_eq!(row[2].fg, 0, "default color untouched");
        feed(&mut g, "\x1b]104;1\x07");
        assert_eq!(g.display_row(0)[0].fg, palette(1) | FG_SET, "reset brings it back");
    }

    #[test]
    fn osc4_sets_queries_and_resets_the_palette() {
        let mut g = Grid::new(8, 1);
        feed(&mut g, "\x1b]4;1;rgb:12/34/56;200;#abcdef\x07\x1b[31mx\x1b[38;5;200my");
        let row = g.display_row(0);
        assert_eq!(row[0].fg, 0x1234_5600 | FG_SET);
        assert_eq!(row[1].fg, 0xABCD_EF00 | FG_SET);
        feed(&mut g, "\x1b]4;1;?;2;?\x1b\\");
        let reply = format!(
            "\x1b]4;1;rgb:1212/3434/5656\x1b\\\x1b]4;2;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x1b\\",
            palette(2) >> 24,
            (palette(2) >> 16) & 0xFF,
            (palette(2) >> 8) & 0xFF
        );
        assert_eq!(String::from_utf8(std::mem::take(&mut g.response)).unwrap(), reply);
        feed(&mut g, "\x1b]104;1\x07\x1b[31mz");
        assert_eq!(g.display_row(0)[2].fg, palette(1) | FG_SET);
        assert_eq!(palette_with(&g.palette_override, 200), 0xABCD_EF00, "only index 1 was reset");
        feed(&mut g, "\x1b]104\x07");
        assert!(g.palette_override.is_empty());
    }

    #[test]
    fn osc_10_11_12_set_query_and_reset_dynamic_colors() {
        let mut g = Grid::new(8, 1);
        feed(&mut g, "\x1b]10;?\x07");
        let fg = default_fg();
        let expected = format!(
            "\x1b]10;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x07",
            fg >> 24,
            (fg >> 16) & 0xFF,
            (fg >> 8) & 0xFF
        );
        assert_eq!(String::from_utf8(std::mem::take(&mut g.response)).unwrap(), expected);

        // `10;fg;bg` sets fg and then bg; `11;?;?` queries bg and cursor.
        feed(&mut g, "\x1b]10;#102030;#405060\x07\x1b]12;rgb:ff/00/00\x07");
        assert_eq!((g.fg_color(), g.bg_color(), g.cursor_color()), (0x1020_3000, 0x4050_6000, Some(0xFF00_0000)));
        feed(&mut g, "\x1b]11;?;?\x07");
        assert_eq!(
            std::mem::take(&mut g.response),
            b"\x1b]11;rgb:4040/5050/6060\x07\x1b]12;rgb:ffff/0000/0000\x07"
        );

        // Inverse video resolves the overridden defaults.
        feed(&mut g, "\x1b[7mx");
        assert_eq!(g.display_row(0)[0].fg & !0xFF, 0x4050_6000);

        feed(&mut g, "\x1b]110\x07\x1b]111\x07\x1b]112\x07");
        assert_eq!((g.fg_color(), g.bg_color(), g.cursor_color()), (default_fg(), default_bg(), None));

        feed(&mut g, "\x1b]11;#000000\x07\x1bc");
        assert_eq!(g.bg_color(), default_bg(), "RIS drops OSC color overrides");
    }

    #[test]
    fn modify_other_keys_set_query_and_reset() {
        let mut g = Grid::new(8, 1);
        feed(&mut g, "\x1b[>4;2m");
        assert_eq!(g.modify_other_keys, 2);
        feed(&mut g, "\x1b[?4m");
        assert_eq!(std::mem::take(&mut g.response), b"\x1b[>4;2m");
        feed(&mut g, "\x1b[>4m");
        assert_eq!(g.modify_other_keys, 0);
        feed(&mut g, "\x1b[>4;1m\x1b[>4n");
        assert_eq!(g.modify_other_keys, 0);
        feed(&mut g, "\x1b[>4;9m");
        assert_eq!(g.modify_other_keys, 2, "clamped");
        assert_eq!(g.display_row(0)[0], Cell::default(), "not mistaken for SGR");
    }

    #[test]
    fn notifications_progress_pointer_and_bell() {
        let mut g = Grid::new(8, 1);
        feed(&mut g, "\x1b]9;build done; 3 warnings\x07");
        assert_eq!(g.notification.take(), Some((String::new(), "build done; 3 warnings".into())));
        feed(&mut g, "\x1b]777;notify;Tests;all \x1b[31mpassed\x1b\\");
        assert_eq!(g.notification.take(), Some(("Tests".into(), "all ".into())));

        feed(&mut g, "\x1b]9;4;1;45\x07");
        assert_eq!(g.progress, Some((1, 45)));
        assert!(std::mem::take(&mut g.title_changed));
        feed(&mut g, "\x1b]9;4;3\x07");
        assert_eq!(g.progress, Some((3, 0)));
        feed(&mut g, "\x1b]9;4;2;250\x07");
        assert_eq!(g.progress, Some((2, 100)));
        feed(&mut g, "\x1b]9;4;0\x07");
        assert_eq!(g.progress, None);

        feed(&mut g, "\x1b]22;pointer\x07");
        assert_eq!(g.pointer_shape, "pointer");
        // The ESC inside the OSC 777 body above ended it; the rest printed as text.
        assert_eq!(line(&g, 0), "passed");
        feed(&mut g, "a\x07b");
        assert!(g.bell);
        assert_eq!(line(&g, 0), "passedab");
    }

    #[test]
    fn xtgettcap_and_decrqss() {
        let mut g = Grid::new(8, 4);
        // "TN" and "Co" known, "zz" unknown, "Tc" boolean.
        feed(&mut g, "\x1bP+q544E;436F;7A7A;5463\x1b\\");
        let reply = String::from_utf8(std::mem::take(&mut g.response)).unwrap();
        assert_eq!(
            reply,
            "\x1bP1+r544E=787465726D2D323536636F6C6F72\x1b\\\x1bP1+r436F=323536\x1b\\\x1bP0+r7A7A\x1b\\\x1bP1+r5463\x1b\\"
        );

        feed(&mut g, "\x1b[1;3;4:3;38;2;1;2;3m\x1bP$qm\x1b\\");
        assert_eq!(String::from_utf8(std::mem::take(&mut g.response)).unwrap(), "\x1bP1$r0;1;3;4:3;38;2;1;2;3m\x1b\\");
        feed(&mut g, "\x1b[2;3r\x1bP$qr\x1b\\\x1b[5 q\x1bP$q q\x1b\\\x1bP$qx\x1b\\");
        assert_eq!(
            String::from_utf8(std::mem::take(&mut g.response)).unwrap(),
            "\x1bP1$r2;3r\x1b\\\x1bP1$r5 q\x1b\\\x1bP0$r\x1b\\"
        );
        assert_eq!(line(&g, 0), "", "DCS payloads are not printed");
    }

    #[test]
    fn pixel_mouse_color_scheme_and_window_size_reports() {
        let mut g = Grid::new(80, 24);
        g.cell_px = (9, 18);
        feed(&mut g, "\x1b[?1016h\x1b[?2031h\x1b[?1016$p\x1b[?2027$p");
        assert!(g.mouse_sgr_pixels && g.color_scheme_reports);
        assert_eq!(std::mem::take(&mut g.response), b"\x1b[?1016;1$y\x1b[?2027;1$y");
        set_color_scheme_dark(false);
        feed(&mut g, "\x1b[?996n");
        assert_eq!(std::mem::take(&mut g.response), b"\x1b[?997;2n");
        set_color_scheme_dark(true);
        feed(&mut g, "\x1b[14t\x1b[16t\x1b[18t");
        assert_eq!(std::mem::take(&mut g.response), b"\x1b[4;432;720t\x1b[6;18;9t\x1b[8;24;80t");
    }

    #[test]
    fn prompt_marks_jump_and_select_output() {
        let mut g = Grid::new(20, 3);
        let cmd = |g: &mut Grid, name: &str, out: &str| {
            feed(g, &format!("\x1b]133;A\x07$ \x1b]133;B\x07{name}\r\n\x1b]133;C\x07{out}\x1b]133;D;0\x07"));
        };
        cmd(&mut g, "one", "1a\r\n1b\r\n");
        cmd(&mut g, "two", "2a\r\n2b\r\n2c\r\n");
        feed(&mut g, "\x1b]133;A\x07$ ");
        assert_eq!(g.prompts.len(), 3);

        assert_eq!(g.select_last_output().as_deref(), Some("2a\n2b\n2c"));
        assert!(g.display_offset > 0, "scrolled up to show the output start");

        g.display_offset = 0;
        assert!(g.jump_to_prompt(-1));
        assert_eq!(line(&g, 0), "$ two");
        assert!(g.jump_to_prompt(-1));
        assert_eq!(line(&g, 0), "$ one");
        assert!(!g.jump_to_prompt(-1), "no older prompt");
        assert!(g.jump_to_prompt(1));
        assert_eq!(line(&g, 0), "$ two");
        assert!(g.jump_to_prompt(1));
        assert!(g.jump_to_prompt(1), "past the last prompt goes live");
        assert_eq!(g.display_offset, 0);
    }

    #[test]
    fn prompt_marks_survive_reflow() {
        let mut g = Grid::new(20, 3);
        let cmd = |g: &mut Grid, name: &str, out: &str| {
            feed(g, &format!("\x1b]133;A\x07$ \x1b]133;B\x07{name}\r\n\x1b]133;C\x07{out}\x1b]133;D;0\x07"));
        };
        cmd(&mut g, "one", "a long output line!!\r\n");
        cmd(&mut g, "two", "2a\r\n2b\r\n");
        feed(&mut g, "\x1b]133;A\x07$ ");

        // Narrower: the long line wraps, so every later line moves down.
        g.resize(10, 3);
        assert_eq!(g.prompts.len(), 3);
        assert_eq!(g.select_last_output().as_deref(), Some("2a\n2b"));
        g.display_offset = 0;
        assert!(g.jump_to_prompt(-1));
        assert_eq!(line(&g, 0), "$ two");
        assert!(g.jump_to_prompt(-1));
        assert_eq!(line(&g, 0), "$ one");

        // Wider again: back to one row per line.
        g.resize(30, 3);
        assert_eq!(g.select_last_output().as_deref(), Some("2a\n2b"));
        g.display_offset = 0;
        assert!(g.jump_to_prompt(-1));
        assert_eq!(line(&g, 0), "$ two");
        assert!(g.jump_to_prompt(-1));
        assert_eq!(line(&g, 0), "$ one");
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
    fn double_click_selects_block_then_line() {
        let mut g = Grid::new(40, 3);
        feed(&mut g, "$ git log --oneline 中文x\r\n");
        assert_eq!(g.select_block(0, 8), "log");
        assert!(g.is_block_selected(0, 6));
        assert!(!g.is_block_selected(0, 2), "a different block");
        assert_eq!(g.select_block(0, 12), "--oneline");
        // A wide glyph and its placeholder cell stay in one block.
        assert_eq!(g.select_block(0, 21), "中文x");
        // A blank cell falls back to the smart line.
        assert_eq!(g.select_block(0, 5), "git log --oneline 中文x");
        assert!(!g.is_block_selected(0, 8));
        g.set_selection(None);
        assert!(!g.is_block_selected(0, 8));
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
