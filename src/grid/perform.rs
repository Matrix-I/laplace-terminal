//! `vte::Perform` implementation: translates ANSI / VT escape sequences
//! into state mutations on the Grid.

use std::io::Write;
use vte::{Params, Perform};

use super::*;

impl Perform for Grid {
    fn print(&mut self, c: char) {
        let c = if self.cur.line_drawing { dec_line_drawing(c) } else { c };
        let cp = c as u32;
        // Fast path: printable ASCII, Latin-1 supplement (except ©/® which
        // can join an emoji sequence), Vietnamese precomposed Latin Extended,
        // and Box Drawing / Block Elements are always 1 column wide and cannot
        // continue a grapheme cluster.
        let is_narrow_plain = (' '..='~').contains(&c)
            || ((0xA0..=0xFF).contains(&cp) && cp != 0xA9 && cp != 0xAE)
            || (0x1EA0..=0x1EF9).contains(&cp)
            || (0x2500..=0x259F).contains(&cp);

        if is_narrow_plain && !self.joiner {
            self.put_char(cp, 1);
        } else if let Some((codepoint, w)) = self.char_cell(c) {
            self.put_char(codepoint, w);
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
