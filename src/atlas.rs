//! Glyph atlas: `fontdue` rasterizes glyphs into fixed, cell-sized slots of a
//! single 512×512 `R8Unorm` texture (256 KB of VRAM). Printable ASCII is baked
//! at startup; every other codepoint is rasterized on first use.
//!
//! Like Ghostty (`font/sprite`), Powerline (`U+E0A0..=U+E0D7`), Box Drawing
//! (`U+2500..=U+257F`), Block Elements (`U+2580..=U+259F`), Geometric shapes
//! (`U+25E2..=U+25E5`), and Braille (`U+2800..=U+28FF`) are synthesized
//! procedurally to exact cell dimensions (`cell_w × cell_h`) with 4×4
//! supersampled anti-aliasing so segments and borders connect seamlessly
//! without requiring a patched Nerd Font. For other Unicode characters missing
//! from the primary font, a fallback font chain and macOS CoreText fallback
//! are used instead of drawing `.notdef` tofu boxes (`▯`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

/// `adjust-cell-width` / `adjust-cell-height`: grow or shrink the cell by
/// pixels or by a percentage of the font's own metric.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub enum MetricAdjust {
    #[default]
    None,
    Px(i32),
    Percent(f32),
}

impl MetricAdjust {
    /// Parse `2`, `-1` (pixels) or `10%`, `-5%`.
    pub fn parse(v: &str) -> Result<Self, String> {
        let v = v.trim();
        if let Some(p) = v.strip_suffix('%') {
            let p: f32 = p.trim().parse().ok().filter(|p: &f32| p.is_finite()).ok_or_else(|| format!("invalid percentage `{v}`"))?;
            return Ok(MetricAdjust::Percent(p));
        }
        v.parse().map(MetricAdjust::Px).map_err(|_| format!("expected pixels or a percentage, got `{v}`"))
    }

    fn apply(self, v: u32) -> u32 {
        let adjusted = match self {
            MetricAdjust::None => v as f32,
            MetricAdjust::Px(px) => v as f32 + px as f32,
            MetricAdjust::Percent(p) => v as f32 * (1.0 + p / 100.0),
        };
        adjusted.round().max(1.0) as u32
    }
}

/// Cell size adjustments (width, height), read when the atlas is built.
static CELL_ADJUST: Mutex<(MetricAdjust, MetricAdjust)> = parking_lot::const_mutex((MetricAdjust::None, MetricAdjust::None));
/// `font-thicken`: heavier stems for every rasterized glyph.
static THICKEN: AtomicBool = AtomicBool::new(false);

/// Set `adjust-cell-width`, `adjust-cell-height` and `font-thicken`; they take
/// effect when an atlas is (re)built.
pub fn set_font_tweaks(width: MetricAdjust, height: MetricAdjust, thicken: bool) {
    let mut adj = CELL_ADJUST.lock();
    if *adj != (width, height) || THICKEN.load(Ordering::Relaxed) != thicken {
        *adj = (width, height);
        THICKEN.store(thicken, Ordering::Relaxed);
        *FONT_CACHE.lock() = None;
    }
}

/// Slot 0 is always blank (space / empty cell).
pub const BLANK_SLOT: u16 = 0;

/// Slot-map key tags for the two halves of a double-width glyph (CJK, emoji).
/// Codepoints use at most 21 bits, so the high bits are free.
const WIDE_LEFT: u32 = 0x4000_0000;
const WIDE_RIGHT: u32 = 0x8000_0000;
/// Slot-map key tags for synthetic bold / italic variants.
const STYLE_BOLD: u32 = 0x1000_0000;
const STYLE_ITALIC: u32 = 0x2000_0000;
const CODEPOINT_MASK: u32 = 0x001F_FFFF;
/// Slot-map key tag for a grapheme cluster: the low bits index `Atlas::clusters`.
const CLUSTER_KEY: u32 = 0x0400_0000;

/// Glyph style requested by a cell.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GlyphStyle {
    pub bold: bool,
    pub italic: bool,
}

pub struct FontSet {
    pub primary: fontdue::Font,
    pub fallbacks: Vec<fontdue::Font>,
    /// HarfBuzz-compatible shaper over the primary font, for ligatures and
    /// `font-feature`; its bytes stay mapped for the lifetime of this FontSet.
    shaper: Option<rustybuzz::Face<'static>>,
    _mmap: Option<Arc<memmap2::Mmap>>,
}

/// Slot-map key tag for a shaped glyph piece: bits 5..27 index `Atlas::pieces`,
/// bits 0..5 select the cell of a glyph that spans several cells.
const GLYPH_PIECE: u32 = 0x0800_0000;
/// Shaped key for a cell whose glyph has no ink (a ligature spacer).
pub const SHAPED_BLANK: u32 = u32::MAX;

/// A glyph by index, drawn across `span` cells with its pen at cell `origin`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
struct GlyphPiece {
    gid: u16,
    span: u8,
    origin: u8,
    bold: bool,
    italic: bool,
}

/// Ligature shaping state: features, a reusable buffer, interned glyph pieces.
struct Shaper {
    features: Vec<rustybuzz::Feature>,
    buf: Option<rustybuzz::UnicodeBuffer>,
    pieces: Vec<GlyphPiece>,
    piece_ids: HashMap<GlyphPiece, u32>,
    /// Advance of one cell in font units (0 = not measured yet, -1 = unknown).
    cell_units: i32,
}

impl Shaper {
    fn new(features: Vec<rustybuzz::Feature>) -> Self {
        Shaper { features, buf: None, pieces: Vec::new(), piece_ids: HashMap::new(), cell_units: 0 }
    }

    /// Shape the ligature candidates of a row: runs of printable ASCII in one
    /// style that contain punctuation (where coding-font ligatures live).
    /// `out[col]` becomes a slot key for a substituted glyph, `SHAPED_BLANK`
    /// for an empty spacer, or 0 for "draw the codepoint as usual". Returns
    /// whether any cell changed. Allocation-free once the buffers are warm.
    pub fn shape_row(&mut self, fonts: &FontSet, px: f32, cell_w: u32, row: &[crate::grid::Cell], out: &mut Vec<u32>) -> bool {
        out.clear();
        out.resize(row.len(), 0);
        let Some(face) = fonts.shaper.as_ref() else { return false };
        if self.cell_units == 0 {
            self.cell_units = face.glyph_index('M').and_then(|g| face.glyph_hor_advance(g)).map_or(-1, i32::from);
        }
        if self.cell_units <= 0 {
            return false;
        }
        let style_of = |c: &crate::grid::Cell| (c.fg & crate::grid::FG_BOLD != 0, c.fg & crate::grid::FG_ITALIC != 0);
        let shapeable = |c: &crate::grid::Cell| (0x21..0x7F).contains(&c.codepoint);
        let mut changed = false;
        let mut i = 0;
        while i < row.len() {
            if !shapeable(&row[i]) {
                i += 1;
                continue;
            }
            let style = style_of(&row[i]);
            let start = i;
            while i < row.len() && shapeable(&row[i]) && style_of(&row[i]) == style {
                i += 1;
            }
            let run = &row[start..i];
            let punct = run.iter().any(|c| !(c.codepoint as u8).is_ascii_alphanumeric());
            if run.len() < 2 || !punct {
                continue;
            }

            let mut buf = self.buf.take().unwrap_or_default();
            for c in run {
                buf.add(char::from_u32(c.codepoint).unwrap_or(' '), 0);
            }
            buf.set_direction(rustybuzz::Direction::LeftToRight);
            let glyphs = rustybuzz::shape(face, &self.features, buf);
            let mut pen = 0i32;
            for (info, pos) in glyphs.glyph_infos().iter().zip(glyphs.glyph_positions()) {
                let rel = ((pen + self.cell_units / 2) / self.cell_units) as usize;
                pen += pos.x_advance;
                let (Some(cell), Ok(gid)) = (run.get(rel), u16::try_from(info.glyph_id)) else { continue };
                let ch = char::from_u32(cell.codepoint).unwrap_or(' ');
                let nominal = face.glyph_index(ch).map_or(0, |g| g.0);
                if gid == nominal && pos.x_offset == 0 && pos.y_offset == 0 {
                    continue;
                }
                let m = fonts.primary.metrics_indexed(gid, px);
                if m.width == 0 || m.height == 0 {
                    if out[start + rel] == 0 {
                        out[start + rel] = SHAPED_BLANK;
                        changed = true;
                    }
                    continue;
                }
                // Cells the glyph's ink covers, relative to its pen cell.
                let cw = cell_w as i32;
                let x0 = m.xmin + pos.x_offset * px as i32 / face.units_per_em().max(1);
                let first = rel as i32 + x0.div_euclid(cw);
                let last = rel as i32 + (x0 + m.width as i32 - 1).div_euclid(cw);
                let (first, last) = (first.max(0), last.min(run.len() as i32 - 1));
                let (span, origin) = (last - first + 1, rel as i32 - first);
                if !(1..=31).contains(&span) || !(0..32).contains(&origin) {
                    continue;
                }
                let piece = GlyphPiece { gid, span: span as u8, origin: origin as u8, bold: style.0, italic: style.1 };
                let next = self.pieces.len() as u32;
                let id = *self.piece_ids.entry(piece).or_insert(next);
                if id == next {
                    self.pieces.push(piece);
                }
                for k in 0..span {
                    out[start + (first + k) as usize] = GLYPH_PIECE | (id << 5) | k as u32;
                }
                changed = true;
            }
            self.buf = Some(glyphs.clear());
        }
        changed
    }

}

/// `font-feature` values (`-calt`, `ss01`, `liga=0`), read when an atlas is built.
static FONT_FEATURES: Mutex<Vec<rustybuzz::Feature>> = parking_lot::const_mutex(Vec::new());

/// Parse `font-feature` values; unknown syntax is an error.
pub fn parse_font_feature(v: &str) -> Result<rustybuzz::Feature, String> {
    v.trim().parse::<rustybuzz::Feature>().map_err(|_| format!("invalid font feature `{v}`"))
}

pub fn set_font_features(features: &[rustybuzz::Feature]) {
    let mut feat = FONT_FEATURES.lock();
    if *feat != features {
        *feat = features.to_vec();
        *FONT_CACHE.lock() = None;
    }
}

pub struct Atlas {
    fonts: Arc<FontSet>,
    px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    baseline: i32,

    pub size: u32,
    pub slots_per_row: u32,
    n_slots: u32,
    next_free: u32,

    ascii: [u16; 128],
    map: HashMap<u32, u16>,
    slot_cp: Vec<u32>,
    slot_used: Vec<u64>,
    frame: u64,
    evicted: bool,
    scratch: Vec<u8>,
    shaper: Shaper,

    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    /// Color glyphs (emoji), premultiplied RGBA in the same slot layout as `texture`.
    color_texture: wgpu::Texture,
    pub color_view: wgpu::TextureView,
    slot_color: Vec<bool>,
    color_scratch: Vec<u8>,
    /// Grapheme clusters drawn so far, interned for their slot keys.
    clusters: Vec<Box<str>>,
    cluster_ids: HashMap<Box<str>, u32>,
}

impl Atlas {
    /// Vertical positions inside a cell, in px from its top: where the underline
    /// starts (just below the baseline) and the center of a strikethrough
    /// (half the x-height above the baseline).
    pub fn decoration_offsets(&self) -> (f32, f32) {
        let underline = self.baseline as f32 + (self.px * 0.08).round().max(1.0);
        let strike = self.baseline as f32 - (self.px * 0.26).round();
        (underline, strike)
    }

    /// Cell dimensions in logical points for a given font and point size.
    pub fn cell_size_pt(fonts: &FontSet, pt: f32) -> (f64, f64) {
        let font = &fonts.primary;
        let lm = font
            .horizontal_line_metrics(pt)
            .expect("font has no horizontal line metrics");
        let natural_w = font.metrics('M', pt).advance_width.round().max(1.0) as u32;
        let natural_h = (lm.ascent - lm.descent + lm.line_gap).ceil().max(1.0) as u32;
        let (adjust_w, adjust_h) = *CELL_ADJUST.lock();
        (adjust_w.apply(natural_w) as f64, adjust_h.apply(natural_h) as f64)
    }

    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, fonts: Arc<FontSet>, px: f32) -> Self {
        let font = &fonts.primary;
        let lm = font
            .horizontal_line_metrics(px)
            .expect("font has no horizontal line metrics");
        let natural_w = font.metrics('M', px).advance_width.round().max(1.0) as u32;
        let natural_h = (lm.ascent - lm.descent + lm.line_gap).ceil().max(1.0) as u32;
        let (adjust_w, adjust_h) = *CELL_ADJUST.lock();
        let cell_w = adjust_w.apply(natural_w);
        let cell_h = adjust_h.apply(natural_h);
        // Extra (or missing) height is split above and below the text.
        let baseline = (lm.ascent + lm.line_gap / 2.0 + (cell_h as f32 - natural_h as f32) / 2.0).round() as i32;

        // 512×512 normally; only grow for huge font sizes so ASCII still fits.
        let mut size = 512u32;
        while (size / cell_w) * (size / cell_h) < 256 && size < 4096 {
            size *= 2;
        }
        let slots_per_row = size / cell_w;
        let n_slots = (slots_per_row * (size / cell_h)).min(u16::MAX as u32);

        let (texture, view) = Self::make_texture(device, size, wgpu::TextureFormat::R8Unorm);
        let (color_texture, color_view) = Self::make_texture(device, size, wgpu::TextureFormat::Rgba8Unorm);

        let mut atlas = Atlas {
            fonts,
            px,
            cell_w,
            cell_h,
            baseline,
            size,
            slots_per_row,
            n_slots,
            next_free: 1,
            ascii: [BLANK_SLOT; 128],
            map: HashMap::with_capacity(n_slots as usize),
            slot_cp: vec![0; n_slots as usize],
            slot_used: vec![0; n_slots as usize],
            frame: 1,
            evicted: false,
            scratch: vec![0; (cell_w * cell_h) as usize],
            shaper: Shaper::new(FONT_FEATURES.lock().clone()),
            texture,
            view,
            color_texture,
            color_view,
            slot_color: vec![false; n_slots as usize],
            color_scratch: vec![0; (cell_w * cell_h * 4) as usize],
            clusters: Vec::new(),
            cluster_ids: HashMap::new(),
        };
        atlas.preload_ascii(queue);
        atlas
    }

    fn preload_ascii(&mut self, queue: &wgpu::Queue) {
        let total_slots = 95u32.min(self.n_slots);
        let rows_needed = total_slots.div_ceil(self.slots_per_row);
        let upload_height = (rows_needed * self.cell_h).min(self.size);
        let mut staging = vec![0u8; (self.size * upload_height) as usize];
        let bytes_per_row = self.size;

        for c in 33u32..=126 {
            if self.next_free >= self.n_slots {
                break;
            }
            let s = self.next_free as u16;
            self.next_free += 1;

            self.rasterize(c);
            let slot = s as u32;
            let sx = (slot % self.slots_per_row) * self.cell_w;
            let sy = (slot / self.slots_per_row) * self.cell_h;
            if sy + self.cell_h <= upload_height && sx + self.cell_w <= self.size {
                for y in 0..self.cell_h {
                    let src_start = (y * self.cell_w) as usize;
                    let src_end = src_start + self.cell_w as usize;
                    let dst_start = ((sy + y) * bytes_per_row + sx) as usize;
                    staging[dst_start..dst_start + self.cell_w as usize]
                        .copy_from_slice(&self.scratch[src_start..src_end]);
                }
            } else {
                self.upload(queue, s);
            }
            self.slot_color[s as usize] = false;
            self.ascii[c as usize] = s;
            self.slot_cp[s as usize] = c;
            self.slot_used[s as usize] = self.frame;
        }

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &staging,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(upload_height),
            },
            wgpu::Extent3d { width: self.size, height: upload_height, depth_or_array_layers: 1 },
        );
    }

    /// Rebuild the atlas in place for a new pixel size (DPI or zoom change).
    pub fn set_px(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, px: f32) {
        *self = Atlas::new(device, queue, self.fonts.clone(), px);
    }

    /// Rebuild the atlas with a different font set (config reload).
    pub fn set_fonts(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, fonts: Arc<FontSet>, px: f32) {
        *self = Atlas::new(device, queue, fonts, px);
    }

    fn make_texture(device: &wgpu::Device, size: u32, format: wgpu::TextureFormat) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        (texture, view)
    }

    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// True if a slot was recycled since the last call (other rows may now be stale).
    pub fn take_evicted(&mut self) -> bool {
        std::mem::take(&mut self.evicted)
    }

    /// Atlas slots for a glyph of display width `width`. Double-width glyphs are
    /// rasterized across two cells and split into a left and a right slot; for
    /// narrow glyphs the right slot is blank.
    pub fn glyph_slots(&mut self, queue: &wgpu::Queue, cp: u32, width: usize, style: GlyphStyle) -> (u16, u16) {
        let mut key = cp;
        // Cell-exact sprites (box drawing, Powerline...) must keep connecting seamlessly.
        if !is_builtin_sprite(cp) {
            if style.bold {
                key |= STYLE_BOLD;
            }
            if style.italic {
                key |= STYLE_ITALIC;
            }
        }
        if width == 2 {
            (self.slot(queue, key | WIDE_LEFT), self.slot(queue, key | WIDE_RIGHT))
        } else {
            (self.slot(queue, key), BLANK_SLOT)
        }
    }

    /// Atlas slot for `cp` (optionally tagged with `WIDE_LEFT` / `WIDE_RIGHT`),
    /// rasterizing and uploading it on a cache miss.
    pub fn slot(&mut self, queue: &wgpu::Queue, cp: u32) -> u16 {
        if cp == 0 || cp == 32 {
            return BLANK_SLOT;
        }
        if cp < 128 {
            let s = self.ascii[cp as usize];
            if s != BLANK_SLOT {
                self.slot_used[s as usize] = self.frame;
                return s;
            }
        } else if let Some(&s) = self.map.get(&cp) {
            self.slot_used[s as usize] = self.frame;
            return s;
        }
        let s = if self.next_free < self.n_slots {
            self.next_free += 1;
            (self.next_free - 1) as u16
        } else {
            // LRU among slots not already used for this frame.
            let mut best = None;
            let mut best_used = u64::MAX;
            for i in 1..self.n_slots as usize {
                let u = self.slot_used[i];
                if u < best_used && u != self.frame {
                    best_used = u;
                    best = Some(i);
                }
            }
            let Some(i) = best else { return BLANK_SLOT };
            let old_cp = self.slot_cp[i];
            if old_cp < 128 {
                self.ascii[old_cp as usize] = BLANK_SLOT;
            } else {
                self.map.remove(&old_cp);
            }
            self.evicted = true;
            i as u16
        };
        let color = self.rasterize(cp);
        self.upload(queue, s);
        if color {
            self.upload_color(queue, s);
        }
        self.slot_color[s as usize] = color;
        if cp < 128 {
            self.ascii[cp as usize] = s;
        } else {
            self.map.insert(cp, s);
        }
        self.slot_cp[s as usize] = cp;
        self.slot_used[s as usize] = self.frame;
        s
    }

    /// Atlas slots for a grapheme cluster (see [`Atlas::glyph_slots`]).
    pub fn cluster_slots(&mut self, queue: &wgpu::Queue, text: &str, width: usize, style: GlyphStyle) -> (u16, u16) {
        if self.clusters.len() >= 4096 {
            self.clusters.clear();
            self.cluster_ids.clear();
            self.map.retain(|&k, _| k & CLUSTER_KEY == 0);
            for i in 1..self.n_slots as usize {
                if self.slot_cp[i] & CLUSTER_KEY != 0 {
                    self.slot_cp[i] = 0;
                    self.slot_used[i] = 0;
                }
            }
            self.evicted = true;
        }
        let id = match self.cluster_ids.get(text) {
            Some(&id) => id,
            None if self.clusters.len() <= CODEPOINT_MASK as usize => {
                let id = self.clusters.len() as u32;
                self.clusters.push(text.into());
                self.cluster_ids.insert(text.into(), id);
                id
            }
            None => return (BLANK_SLOT, BLANK_SLOT),
        };
        let mut key = CLUSTER_KEY | id;
        if style.bold {
            key |= STYLE_BOLD;
        }
        if style.italic {
            key |= STYLE_ITALIC;
        }
        if width == 2 {
            (self.slot(queue, key | WIDE_LEFT), self.slot(queue, key | WIDE_RIGHT))
        } else {
            (self.slot(queue, key), BLANK_SLOT)
        }
    }

    /// True if `slot` holds a color glyph (drawn from `color_view`).
    pub fn is_color(&self, slot: u16) -> bool {
        self.slot_color.get(slot as usize).copied().unwrap_or(false)
    }

    /// See [`Shaper::shape_row`].
    pub fn shape_row(&mut self, row: &[crate::grid::Cell], out: &mut Vec<u32>) -> bool {
        self.shaper.shape_row(&self.fonts, self.px, self.cell_w, row, out)
    }

    /// Rasterize a glyph piece (see [`GLYPH_PIECE`]) into `buf`.
    fn rasterize_piece(&self, buf: &mut [u8], key: u32) {
        let Some(&p) = self.shaper.pieces.get(((key >> 5) & 0x3F_FFFF) as usize) else { return };
        let k = (key & 31) as usize;
        let (cw, ch) = (self.cell_w as usize, self.cell_h as usize);
        let w = cw * p.span as usize;
        let mut canvas = vec![0u8; w * ch];
        let (m, bmp) = self.fonts.primary.rasterize_indexed(p.gid, self.px);
        let x0 = p.origin as i32 * cw as i32 + m.xmin;
        let y0 = self.baseline - (m.ymin + m.height as i32);
        for gy in 0..m.height as i32 {
            let y = y0 + gy;
            if y < 0 || y >= ch as i32 {
                continue;
            }
            for gx in 0..m.width as i32 {
                let x = x0 + gx;
                if x < 0 || x >= w as i32 {
                    continue;
                }
                let v = stem_boost(bmp[(gy * m.width as i32 + gx) as usize]);
                let d = &mut canvas[y as usize * w + x as usize];
                *d = (*d).max(v);
            }
        }
        self.apply_style(&mut canvas, w, GlyphStyle { bold: p.bold, italic: p.italic });
        for y in 0..ch {
            buf[y * cw..(y + 1) * cw].copy_from_slice(&canvas[y * w + k * cw..y * w + (k + 1) * cw]);
        }
    }

    /// Rasterize `key` into `scratch`, or into `color_scratch` (returning true)
    /// for a color emoji.
    fn rasterize(&mut self, key: u32) -> bool {
        if key & GLYPH_PIECE != 0 && key != SHAPED_BLANK {
            let mut buf = std::mem::take(&mut self.scratch);
            buf.fill(0);
            self.rasterize_piece(&mut buf, key);
            self.scratch = buf;
            return false;
        }
        let half = key & (WIDE_LEFT | WIDE_RIGHT);
        let mut cp = key & CODEPOINT_MASK;
        let cluster = if key & CLUSTER_KEY != 0 { self.clusters.get(cp as usize).cloned() } else { None };
        // A lone regional indicator (a flag half the shell's line editor
        // prints on its own) has no emoji glyph: show its letter.
        if cluster.is_none() && (0x1F1E6..=0x1F1FF).contains(&cp) {
            cp = 'A' as u32 + (cp - 0x1F1E6);
        }
        // Wide emoji come from the system's color emoji font when there is one.
        let emoji = cluster.as_deref().map_or(is_emoji(cp), is_emoji_text);
        // So does an emoji sequence squeezed into one cell (❤️ at a zsh prompt).
        if half == 0 && emoji && cluster.is_some() {
            let text = cluster.as_deref().unwrap_or_default();
            let mut buf = std::mem::take(&mut self.color_scratch);
            buf.fill(0);
            let drawn = rasterize_color(&mut buf, self.cell_w, self.cell_h, self.px, text);
            self.color_scratch = buf;
            if drawn {
                self.scratch.fill(0);
                return true;
            }
        }
        if half != 0 && emoji {
            let (cw, ch) = (self.cell_w as usize, self.cell_h as usize);
            let mut wide = vec![0u8; 2 * cw * ch * 4];
            let mut units = [0u8; 4];
            let text = match cluster.as_deref() {
                Some(t) => t,
                None => char::from_u32(cp).unwrap_or(' ').encode_utf8(&mut units),
            };
            if rasterize_color(&mut wide, 2 * self.cell_w, self.cell_h, self.px, text) {
                let x0 = if half == WIDE_RIGHT { cw * 4 } else { 0 };
                for y in 0..ch {
                    let row = &wide[y * 2 * cw * 4 + x0..][..cw * 4];
                    self.color_scratch[y * cw * 4..(y + 1) * cw * 4].copy_from_slice(row);
                }
                self.scratch.fill(0);
                return true;
            }
        }
        let style = GlyphStyle { bold: key & STYLE_BOLD != 0, italic: key & STYLE_ITALIC != 0 };
        let mut buf = std::mem::take(&mut self.scratch);
        if half == 0 {
            buf.fill(0);
            match cluster.as_deref() {
                Some(text) => self.rasterize_cluster_into(&mut buf, self.cell_w, text),
                None => self.rasterize_into(&mut buf, self.cell_w, cp),
            }
            self.apply_style(&mut buf, self.cell_w as usize, style);
        } else {
            // Rasterize across two cells, then keep the requested half.
            let (cw, ch) = (self.cell_w as usize, self.cell_h as usize);
            let mut wide = vec![0u8; 2 * cw * ch];
            match cluster.as_deref() {
                Some(text) => self.rasterize_cluster_into(&mut wide, 2 * self.cell_w, text),
                None => self.rasterize_into(&mut wide, 2 * self.cell_w, cp),
            }
            self.apply_style(&mut wide, 2 * cw, style);
            let x0 = if half == WIDE_RIGHT { cw } else { 0 };
            for y in 0..ch {
                buf[y * cw..(y + 1) * cw].copy_from_slice(&wide[y * 2 * cw + x0..y * 2 * cw + x0 + cw]);
            }
        }
        self.scratch = buf;
        false
    }

    /// Rasterize a grapheme cluster (base + marks) into a `w × cell_h` canvas:
    /// shaped with the primary font so marks are positioned, else its first
    /// character alone.
    fn rasterize_cluster_into(&self, buf: &mut [u8], w: u32, text: &str) {
        let first = text.chars().next().map_or(0, |c| c as u32);
        let shaped = self.fonts.shaper.as_ref().map(|face| {
            let mut ub = rustybuzz::UnicodeBuffer::new();
            ub.push_str(text);
            (face, rustybuzz::shape(face, &[], ub))
        });
        let Some((face, shaped)) = shaped.filter(|(_, s)| s.glyph_infos().iter().all(|g| g.glyph_id != 0)) else {
            // The primary font lacks a glyph: the system's shaping and fonts, else the base alone.
            if !rasterize_text_cluster(buf, w, self.cell_h, self.baseline, self.px, text) {
                buf.fill(0);
                self.rasterize_into(buf, w, first);
            }
            return;
        };
        let scale = self.px / face.units_per_em() as f32;
        let advance: f32 = shaped.glyph_positions().iter().map(|p| p.x_advance as f32 * scale).sum();
        // Centered like single glyphs are.
        let mut pen = ((w as f32 - advance) / 2.0).max(0.0);
        let h = self.cell_h as i32;
        for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            let (m, bmp) = self.fonts.primary.rasterize_indexed(info.glyph_id as u16, self.px);
            let x0 = (pen + pos.x_offset as f32 * scale).round() as i32 + m.xmin;
            let y0 = self.baseline - (m.ymin + m.height as i32) - (pos.y_offset as f32 * scale).round() as i32;
            for gy in 0..m.height as i32 {
                let y = y0 + gy;
                if !(0..h).contains(&y) {
                    continue;
                }
                for gx in 0..m.width as i32 {
                    let x = x0 + gx;
                    if !(0..w as i32).contains(&x) {
                        continue;
                    }
                    let v = stem_boost(bmp[(gy * m.width as i32 + gx) as usize]);
                    let d = &mut buf[y as usize * w as usize + x as usize];
                    *d = (*d).max(v);
                }
            }
            pen += pos.x_advance as f32 * scale;
        }
    }

    /// Synthesize bold (horizontal dilation) and italic (shear around the baseline)
    /// on a `w × cell_h` coverage canvas, so any font works without bold/italic faces.
    fn apply_style(&self, buf: &mut [u8], w: usize, style: GlyphStyle) {
        let h = self.cell_h as usize;
        if style.italic {
            let mut row = vec![0u8; w];
            for y in 0..h {
                // ~12° slant: rows above the baseline move right, rows below move left.
                let shift = ((self.baseline - y as i32) as f32 * 0.21).round() as isize;
                let src = &buf[y * w..(y + 1) * w];
                row.fill(0);
                for (x, &v) in src.iter().enumerate() {
                    let nx = x as isize + shift;
                    if (0..w as isize).contains(&nx) {
                        row[nx as usize] = v;
                    }
                }
                buf[y * w..(y + 1) * w].copy_from_slice(&row);
            }
        }
        if style.bold {
            let stroke = ((self.px / 16.0).round() as usize).max(1);
            for y in 0..h {
                let line = &mut buf[y * w..(y + 1) * w];
                for x in (1..w).rev() {
                    let lo = x.saturating_sub(stroke);
                    let m = line[lo..x].iter().copied().max().unwrap_or(0);
                    line[x] = line[x].max(m);
                }
            }
        }
    }

    /// Rasterize `cp` into `buf`, a zeroed `cw × cell_h` canvas.
    fn rasterize_into(&self, buf: &mut [u8], cw: u32, cp: u32) {
        let Some(ch) = char::from_u32(cp) else { return };

        // 1. Ghostty-style built-in cell-exact sprite glyphs:
        //    Powerline, Box Drawing, Block Elements, Geometric triangles, Braille.
        if draw_builtin_sprite(buf, cw, self.cell_h, cp) {
            return;
        }

        // 2. Primary font (only if the font actually contains a glyph for `ch`).
        if self.fonts.primary.lookup_glyph_index(ch) != 0 {
            rasterize_with_font(
                buf,
                cw as i32,
                self.cell_h as i32,
                self.baseline,
                self.px,
                &self.fonts.primary,
                ch,
                false,
            );
            return;
        }

        // 3. Fallback fonts (e.g., Symbols Nerd Font or user/system symbol fonts).
        for fb in &self.fonts.fallbacks {
            if fb.lookup_glyph_index(ch) != 0 {
                rasterize_with_font(
                    buf,
                    cw as i32,
                    self.cell_h as i32,
                    self.baseline,
                    self.px,
                    fb,
                    ch,
                    true,
                );
                return;
            }
        }

        // 4. Built-in Nerd Font PUA icon fallbacks when no Nerd Font is installed.
        if draw_nerd_icon_fallback(buf, cw, self.cell_h, cp) {
            return;
        }

        // 5. macOS CoreText system font cascade (Apple Symbols, PingFang, Supplemental, etc.).
        // Skip Private Use Area codepoints (0xE000..=0xF8FF, 0xF0000..=0x10FFFF) because
        // macOS system fonts do not map Nerd Font PUA glyphs and searching the CoreText
        // cascade list for unmapped PUA characters takes ~50ms on first encounter.
        if !(0xE000..=0xF8FF).contains(&cp) && cp < 0xF0000 {
            macos_coretext_rasterize(buf, cw, self.cell_h, self.baseline, self.px, ch);
        }
    }

    fn upload_color(&self, queue: &wgpu::Queue, slot: u16) {
        let s = slot as u32;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.color_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: (s % self.slots_per_row) * self.cell_w,
                    y: (s / self.slots_per_row) * self.cell_h,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &self.color_scratch,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * self.cell_w),
                rows_per_image: Some(self.cell_h),
            },
            wgpu::Extent3d { width: self.cell_w, height: self.cell_h, depth_or_array_layers: 1 },
        );
    }

    fn upload(&self, queue: &wgpu::Queue, slot: u16) {
        let s = slot as u32;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: (s % self.slots_per_row) * self.cell_w,
                    y: (s / self.slots_per_row) * self.cell_h,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &self.scratch,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.cell_w),
                rows_per_image: Some(self.cell_h),
            },
            wgpu::Extent3d { width: self.cell_w, height: self.cell_h, depth_or_array_layers: 1 },
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn rasterize_with_font(
    scratch: &mut [u8],
    cw: i32,
    chh: i32,
    baseline: i32,
    mut px: f32,
    font: &fontdue::Font,
    ch: char,
    center_if_symbol: bool,
) {
    // Scale down if a fallback symbol glyph is wider/taller than the cell.
    if center_if_symbol {
        let m0 = font.metrics(ch, px);
        if m0.width as i32 > cw || m0.height as i32 > chh {
            let sx = cw as f32 / (m0.width.max(1) as f32);
            let sy = chh as f32 / (m0.height.max(1) as f32);
            px = (px * sx.min(sy) * 0.92).max(6.0);
        }
    }
    let (m, bmp) = font.rasterize(ch, px);
    if m.width == 0 || m.height == 0 {
        return;
    }
    let mut x0 = if center_if_symbol {
        (cw - m.width as i32) / 2
    } else {
        m.xmin
    };
    let mut y0 = baseline - (m.ymin + m.height as i32);
    if (m.width as i32) <= cw {
        x0 = x0.clamp(0, cw - m.width as i32);
    }
    if (m.height as i32) <= chh {
        y0 = y0.clamp(0, chh - m.height as i32);
    }
    for gy in 0..m.height as i32 {
        let y = y0 + gy;
        if y < 0 || y >= chh {
            continue;
        }
        for gx in 0..m.width as i32 {
            let x = x0 + gx;
            if x < 0 || x >= cw {
                continue;
            }
            let v = stem_boost(bmp[(gy * m.width as i32 + gx) as usize]);
            let d = &mut scratch[(y * cw + x) as usize];
            *d = (*d).max(v);
        }
    }
}

/// Slight gamma/stem darkening for fontdue grayscale coverage so Retina stems
/// match Ghostty/CoreText weight while keeping 0 -> 0 and 255 -> 255 exact.
#[inline]
fn stem_boost(v: u8) -> u8 {
    let gamma = if THICKEN.load(Ordering::Relaxed) { 0.55 } else { 0.78 };
    match v {
        0 | 255 => v,
        _ => ((v as f32 / 255.0).powf(gamma) * 255.0 + 0.5) as u8,
    }
}

// ============================================================================
// Built-in Procedural Sprite Renderer (Ghostty-style `font/sprite`)
// ============================================================================

/// Codepoints drawn procedurally by [`draw_builtin_sprite`].
fn is_builtin_sprite(cp: u32) -> bool {
    matches!(cp, 0xE0A0..=0xE0D7 | 0x2500..=0x259F | 0x25E2..=0x25E5 | 0x2800..=0x28FF)
}

fn draw_builtin_sprite(buf: &mut [u8], cw: u32, ch: u32, cp: u32) -> bool {
    if cw == 0 || ch == 0 {
        return false;
    }
    match cp {
        0xE0A0..=0xE0D7 => draw_powerline(buf, cw, ch, cp),
        0x2580..=0x259F | 0x25E2..=0x25E5 => draw_block_element(buf, cw, ch, cp),
        0x2500..=0x257F => draw_box_drawing(buf, cw, ch, cp),
        0x2800..=0x28FF => {
            draw_braille(buf, cw, ch, (cp - 0x2800) as u8);
            true
        }
        _ => false,
    }
}

/// Evaluate a 4×4 supersampled coverage predicate `(x, y)` in pixel coordinates
/// where `x in [0.0, cw]` and `y in [0.0, ch]`.
fn rasterize_supersampled(buf: &mut [u8], cw: u32, ch: u32, mut f: impl FnMut(f32, f32) -> bool) {
    for py in 0..ch {
        for px in 0..cw {
            let mut hits = 0u32;
            for sy in 0..4 {
                let y = py as f32 + (sy as f32 + 0.5) * 0.25;
                for sx in 0..4 {
                    let x = px as f32 + (sx as f32 + 0.5) * 0.25;
                    if f(x, y) {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                let a = ((hits * 255 + 8) / 16) as u8;
                let dst = &mut buf[(py * cw + px) as usize];
                *dst = (*dst).max(a);
            }
        }
    }
}

#[inline]
fn dist_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let dx = bx - ax;
    let dy = by - ay;
    let len_sq = dx * dx + dy * dy;
    let t = if len_sq > 1e-6 {
        ((px - ax) * dx + (py - ay) * dy) / len_sq
    } else {
        0.0
    }
    .clamp(0.0, 1.0);
    let cx = ax + t * dx;
    let cy = ay + t * dy;
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

fn draw_powerline(buf: &mut [u8], cw: u32, ch: u32, cp: u32) -> bool {
    let w = cw as f32;
    let h = ch as f32;
    let mid_y = h * 0.5;
    let stroke = (w * 0.12).max(1.4);
    let half_s = stroke * 0.5;

    match cp {
        //  U+E0A0: Powerline Git Branch symbol
        0xE0A0 => {
            let x1 = w * 0.28;
            let x2 = w * 0.72;
            let y_top = h * 0.24;
            let y_bot = h * 0.76;
            let y_rt = h * 0.35;
            let y_join = h * 0.60;
            let r_outer = (w * 0.155).max(1.6);
            let r_inner = if w >= 12.0 { r_outer * 0.36 } else { 0.0 };
            let stem_r = (w * 0.082).max(0.85);

            rasterize_supersampled(buf, cw, ch, |x, y| {
                let d_tl = ((x - x1).powi(2) + (y - y_top).powi(2)).sqrt();
                let d_bl = ((x - x1).powi(2) + (y - y_bot).powi(2)).sqrt();
                let d_tr = ((x - x2).powi(2) + (y - y_rt).powi(2)).sqrt();

                // Carve center holes of the 3 branch nodes first.
                if d_tl < r_inner || d_bl < r_inner || d_tr < r_inner {
                    return false;
                }
                if d_tl <= r_outer || d_bl <= r_outer || d_tr <= r_outer {
                    return true;
                }
                // Left vertical trunk between top-left and bottom-left nodes.
                if dist_to_segment(x, y, x1, y_top, x1, y_bot) <= stem_r {
                    return true;
                }
                // Right vertical drop from top-right node down to curve start.
                let y_curve_top = h * 0.45;
                if dist_to_segment(x, y, x2, y_rt, x2, y_curve_top) <= stem_r {
                    return true;
                }
                // Smooth S-curve connecting (x2, y_curve_top) to (x1, y_join).
                let steps = 8;
                let mut prev_x = x2;
                let mut prev_y = y_curve_top;
                for i in 1..=steps {
                    let t = i as f32 / steps as f32;
                    let u = 1.0 - t;
                    // Cubic Bezier with vertical tangent at x2 and smooth join at x1.
                    let bx = u * u * u * x2
                        + 3.0 * u * u * t * x2
                        + 3.0 * u * t * t * (x1 + (x2 - x1) * 0.25)
                        + t * t * t * x1;
                    let by = u * u * u * y_curve_top
                        + 3.0 * u * u * t * (y_join - (y_join - y_curve_top) * 0.15)
                        + 3.0 * u * t * t * y_join
                        + t * t * t * y_join;
                    if dist_to_segment(x, y, prev_x, prev_y, bx, by) <= stem_r {
                        return true;
                    }
                    prev_x = bx;
                    prev_y = by;
                }
                false
            });
            true
        }

        //  U+E0A1: LN (line number) symbol
        0xE0A1 => {
            let sw = (w * 0.075).max(0.75);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                // 'L' in upper-left: (0.18w, 0.18h) -> (0.18w, 0.52h) -> (0.52w, 0.52h)
                let l1 = dist_to_segment(x, y, w * 0.18, h * 0.18, w * 0.18, h * 0.52) <= sw;
                let l2 = dist_to_segment(x, y, w * 0.18, h * 0.52, w * 0.52, h * 0.52) <= sw;
                // 'N' in lower-right: (0.42w, 0.82h) -> (0.42w, 0.46h) -> (0.82w, 0.82h) -> (0.82w, 0.46h)
                let n1 = dist_to_segment(x, y, w * 0.42, h * 0.82, w * 0.42, h * 0.46) <= sw;
                let n2 = dist_to_segment(x, y, w * 0.42, h * 0.46, w * 0.82, h * 0.82) <= sw;
                let n3 = dist_to_segment(x, y, w * 0.82, h * 0.82, w * 0.82, h * 0.46) <= sw;
                l1 || l2 || n1 || n2 || n3
            });
            true
        }

        //  U+E0A2: Padlock (read-only) symbol
        0xE0A2 => {
            let cx = w * 0.5;
            let body_l = w * 0.18;
            let body_r = w * 0.82;
            let body_t = h * 0.46;
            let body_b = h * 0.82;
            let arch_cy = body_t;
            let arch_r = w * 0.22;
            let arch_sw = (w * 0.075).max(0.8);
            let hole_r = (w * 0.07).max(0.75);
            let hole_cy = h * 0.60;

            rasterize_supersampled(buf, cw, ch, |x, y| {
                // Body with slightly rounded corners
                if x >= body_l && x <= body_r && y >= body_t && y <= body_b {
                    // Keyhole cutout
                    let d_hole = ((x - cx).powi(2) + (y - hole_cy).powi(2)).sqrt();
                    if d_hole <= hole_r {
                        return false;
                    }
                    if dist_to_segment(x, y, cx, hole_cy, cx, h * 0.73) <= hole_r * 0.65 {
                        return false;
                    }
                    return true;
                }
                // Shackle arch above body
                if y <= arch_cy {
                    let y_arch_top = h * 0.30;
                    if y >= y_arch_top {
                        let d_l = (x - (cx - arch_r)).abs();
                        let d_r = (x - (cx + arch_r)).abs();
                        if d_l <= arch_sw || d_r <= arch_sw {
                            return true;
                        }
                    } else {
                        let dy = (y - y_arch_top) * (arch_r / (h * 0.12).max(1.0));
                        let d = ((x - cx).powi(2) + dy * dy).sqrt();
                        if (d - arch_r).abs() <= arch_sw {
                            return true;
                        }
                    }
                }
                false
            });
            true
        }

        //  U+E0A3: CN (column number) symbol
        0xE0A3 => {
            let sw = (w * 0.075).max(0.75);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                // 'C' in upper-left
                let c_top = dist_to_segment(x, y, w * 0.48, h * 0.20, w * 0.18, h * 0.20) <= sw;
                let c_left = dist_to_segment(x, y, w * 0.18, h * 0.20, w * 0.18, h * 0.50) <= sw;
                let c_bot = dist_to_segment(x, y, w * 0.18, h * 0.50, w * 0.48, h * 0.50) <= sw;
                // 'N' in lower-right
                let n1 = dist_to_segment(x, y, w * 0.42, h * 0.82, w * 0.42, h * 0.46) <= sw;
                let n2 = dist_to_segment(x, y, w * 0.42, h * 0.46, w * 0.82, h * 0.82) <= sw;
                let n3 = dist_to_segment(x, y, w * 0.82, h * 0.82, w * 0.82, h * 0.46) <= sw;
                c_top || c_left || c_bot || n1 || n2 || n3
            });
            true
        }

        //  U+E0B0: Rightwards black arrowhead (solid triangle filling cell)
        0xE0B0 => {
            rasterize_supersampled(buf, cw, ch, |x, y| {
                let dy = (y - mid_y).abs() / mid_y.max(0.5);
                x <= w * (1.0 - dy)
            });
            true
        }

        //  U+E0B1: Rightwards thin chevron
        0xE0B1 => {
            let tip_x = (w - half_s).max(0.0);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                dist_to_segment(x, y, 0.0, 0.0, tip_x, mid_y) <= half_s
                    || dist_to_segment(x, y, tip_x, mid_y, 0.0, h) <= half_s
            });
            true
        }

        //  U+E0B2: Leftwards black arrowhead (solid triangle filling cell)
        0xE0B2 => {
            rasterize_supersampled(buf, cw, ch, |x, y| {
                let dy = (y - mid_y).abs() / mid_y.max(0.5);
                (w - x) <= w * (1.0 - dy)
            });
            true
        }

        //  U+E0B3: Leftwards thin chevron
        0xE0B3 => {
            let tip_x = half_s.min(w);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                dist_to_segment(x, y, w, 0.0, tip_x, mid_y) <= half_s
                    || dist_to_segment(x, y, tip_x, mid_y, w, h) <= half_s
            });
            true
        }

        //  U+E0B4: Right half-circle (rounded pill cap)
        0xE0B4 => {
            rasterize_supersampled(buf, cw, ch, |x, y| {
                let nx = x / w.max(1.0);
                let ny = (y - mid_y) / mid_y.max(0.5);
                nx * nx + ny * ny <= 1.0
            });
            true
        }

        //  U+E0B5: Right half-circle thin arc
        0xE0B5 => {
            let rx = (w - half_s).max(1.0);
            let ry = (mid_y - half_s).max(1.0);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                let nx = x / rx;
                let ny = (y - mid_y) / ry;
                let r = (nx * nx + ny * ny).sqrt();
                let approx_px = (r - 1.0).abs() * rx.min(ry);
                approx_px <= half_s
            });
            true
        }

        //  U+E0B6: Left half-circle (rounded pill cap)
        0xE0B6 => {
            rasterize_supersampled(buf, cw, ch, |x, y| {
                let nx = (w - x) / w.max(1.0);
                let ny = (y - mid_y) / mid_y.max(0.5);
                nx * nx + ny * ny <= 1.0
            });
            true
        }

        //  U+E0B7: Left half-circle thin arc
        0xE0B7 => {
            let rx = (w - half_s).max(1.0);
            let ry = (mid_y - half_s).max(1.0);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                let nx = (w - x) / rx;
                let ny = (y - mid_y) / ry;
                let r = (nx * nx + ny * ny).sqrt();
                let approx_px = (r - 1.0).abs() * rx.min(ry);
                approx_px <= half_s
            });
            true
        }

        //  U+E0B8: Lower-left triangle
        0xE0B8 => {
            rasterize_supersampled(buf, cw, ch, |x, y| (x / w) <= (y / h));
            true
        }

        //  U+E0B9 /  U+E0BF: Backslash thin diagonal
        0xE0B9 | 0xE0BF => {
            rasterize_supersampled(buf, cw, ch, |x, y| {
                dist_to_segment(x, y, 0.0, 0.0, w, h) <= half_s
            });
            true
        }

        //  U+E0BA: Lower-right triangle
        0xE0BA => {
            rasterize_supersampled(buf, cw, ch, |x, y| ((w - x) / w) <= (y / h));
            true
        }

        //  U+E0BB /  U+E0BD: Forwardslash thin diagonal
        0xE0BB | 0xE0BD => {
            rasterize_supersampled(buf, cw, ch, |x, y| {
                dist_to_segment(x, y, w, 0.0, 0.0, h) <= half_s
            });
            true
        }

        //  U+E0BC: Upper-left triangle
        0xE0BC => {
            rasterize_supersampled(buf, cw, ch, |x, y| (x / w) <= ((h - y) / h));
            true
        }

        //  U+E0BE: Upper-right triangle
        0xE0BE => {
            rasterize_supersampled(buf, cw, ch, |x, y| ((w - x) / w) <= ((h - y) / h));
            true
        }

        // Powerline Extra right-pointing variants (U+E0C0, E0C4, E0C6, E0C8, E0CC, E0CE, E0D2)
        0xE0C0 | 0xE0C4 | 0xE0C6 | 0xE0C8 | 0xE0CC | 0xE0CE | 0xE0D2 => {
            draw_powerline(buf, cw, ch, 0xE0B0)
        }
        // Powerline Extra left-pointing variants (U+E0C2, E0C5, E0C7, E0CA, E0D4)
        0xE0C2 | 0xE0C5 | 0xE0C7 | 0xE0CA | 0xE0D4 => draw_powerline(buf, cw, ch, 0xE0B2),
        // Powerline Extra thin variants
        0xE0C1 | 0xE0CD | 0xE0CF..=0xE0D1 => draw_powerline(buf, cw, ch, 0xE0B1),
        0xE0C3 | 0xE0C9 | 0xE0CB | 0xE0D3 | 0xE0D5..=0xE0D7 => draw_powerline(buf, cw, ch, 0xE0B3),

        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_rect(buf: &mut [u8], cw: u32, ch: u32, x0: u32, y0: u32, x1: u32, y1: u32, val: u8) {
    let x0 = x0.min(cw);
    let x1 = x1.min(cw);
    let y0 = y0.min(ch);
    let y1 = y1.min(ch);
    for y in y0..y1 {
        for x in x0..x1 {
            let d = &mut buf[(y * cw + x) as usize];
            *d = (*d).max(val);
        }
    }
}

fn draw_block_element(buf: &mut [u8], cw: u32, ch: u32, cp: u32) -> bool {
    let w = cw as f32;
    let h = ch as f32;
    let hx = cw / 2;
    let hy = ch / 2;
    match cp {
        // ▀ Upper half block
        0x2580 => fill_rect(buf, cw, ch, 0, 0, cw, hy, 255),
        // ▁..=█ Lower 1/8 .. 8/8 blocks
        0x2581..=0x2588 => {
            let n = cp - 0x2580;
            let y0 = ((ch * (8 - n) + 4) / 8).min(ch);
            fill_rect(buf, cw, ch, 0, y0, cw, ch, 255);
        }
        // ▉..=▏ Left 7/8 .. 1/8 blocks
        0x2589..=0x258F => {
            let n = 8 - (cp - 0x2588);
            let x1 = ((cw * n + 4) / 8).max(1);
            fill_rect(buf, cw, ch, 0, 0, x1, ch, 255);
        }
        // ▐ Right half block
        0x2590 => fill_rect(buf, cw, ch, hx, 0, cw, ch, 255),
        // ░ Light shade (25%)
        0x2591 => fill_rect(buf, cw, ch, 0, 0, cw, ch, 64),
        // ▒ Medium shade (50%)
        0x2592 => fill_rect(buf, cw, ch, 0, 0, cw, ch, 128),
        // ▓ Dark shade (75%)
        0x2593 => fill_rect(buf, cw, ch, 0, 0, cw, ch, 192),
        // ▔ Upper 1/8 block
        0x2594 => fill_rect(buf, cw, ch, 0, 0, cw, (ch / 8).max(1), 255),
        // ▕ Right 1/8 block
        0x2595 => fill_rect(buf, cw, ch, cw - (cw / 8).max(1), 0, cw, ch, 255),
        // Quadrant blocks U+2596..=U+259F
        0x2596..=0x259F => {
            // Bitmask: bit0=UL, bit1=UR, bit2=LL, bit3=LR
            const QUADS: [u8; 10] = [
                0b0100, // 2596 ▖ LL
                0b1000, // 2597 ▗ LR
                0b0001, // 2598 ▘ UL
                0b1101, // 2599 ▙ UL + LL + LR
                0b1001, // 259A ▚ UL + LR
                0b0111, // 259B ▛ UL + UR + LL
                0b1011, // 259C ▜ UL + UR + LR
                0b0010, // 259D ▝ UR
                0b0110, // 259E ▞ UR + LL
                0b1110, // 259F ▟ UR + LL + LR
            ];
            let m = QUADS[(cp - 0x2596) as usize];
            if m & 1 != 0 {
                fill_rect(buf, cw, ch, 0, 0, hx, hy, 255);
            }
            if m & 2 != 0 {
                fill_rect(buf, cw, ch, hx, 0, cw, hy, 255);
            }
            if m & 4 != 0 {
                fill_rect(buf, cw, ch, 0, hy, hx, ch, 255);
            }
            if m & 8 != 0 {
                fill_rect(buf, cw, ch, hx, hy, cw, ch, 255);
            }
        }
        // ◢ U+25E2: Lower-right triangle
        0x25E2 => rasterize_supersampled(buf, cw, ch, |x, y| ((w - x) / w) <= (y / h)),
        // ◣ U+25E3: Lower-left triangle
        0x25E3 => rasterize_supersampled(buf, cw, ch, |x, y| (x / w) <= (y / h)),
        // ◤ U+25E4: Upper-left triangle
        0x25E4 => rasterize_supersampled(buf, cw, ch, |x, y| (x / w) <= ((h - y) / h)),
        // ◥ U+25E5: Upper-right triangle
        0x25E5 => rasterize_supersampled(buf, cw, ch, |x, y| ((w - x) / w) <= ((h - y) / h)),
        _ => return false,
    }
    true
}

fn draw_braille(buf: &mut [u8], cw: u32, ch: u32, bits: u8) {
    if bits == 0 {
        return;
    }
    let w = cw as f32;
    let h = ch as f32;
    let r = (w * 0.14).min(h * 0.08).max(0.85);
    let xs = [w * 0.30, w * 0.70];
    let ys = [h * 0.16, h * 0.38, h * 0.62, h * 0.84];
    // Braille dot bit mapping:
    // (col 0, row 0..3) -> bits 0, 1, 2, 6
    // (col 1, row 0..3) -> bits 3, 4, 5, 7
    const BIT_MAP: [[u8; 4]; 2] = [[0, 1, 2, 6], [3, 4, 5, 7]];
    rasterize_supersampled(buf, cw, ch, |x, y| {
        for (c, &cx) in xs.iter().enumerate() {
            for (row, &cy) in ys.iter().enumerate() {
                if (bits & (1 << BIT_MAP[c][row])) != 0
                    && (x - cx).powi(2) + (y - cy).powi(2) <= r * r {
                        return true;
                    }
            }
        }
        false
    });
}

fn draw_box_drawing(buf: &mut [u8], cw: u32, ch: u32, cp: u32) -> bool {
    let w = cw as f32;
    let h = ch as f32;
    let t_light = (w * 0.10).round().max(1.0) as u32;
    let t_heavy = (t_light * 2).max(2);
    let cx = cw / 2;
    let cy = ch / 2;

    // Diagonals U+2571..=U+2573
    match cp {
        0x2571 => {
            let hs = (t_light as f32 * 0.5).max(0.7);
            rasterize_supersampled(buf, cw, ch, |x, y| dist_to_segment(x, y, w, 0.0, 0.0, h) <= hs);
            return true;
        }
        0x2572 => {
            let hs = (t_light as f32 * 0.5).max(0.7);
            rasterize_supersampled(buf, cw, ch, |x, y| dist_to_segment(x, y, 0.0, 0.0, w, h) <= hs);
            return true;
        }
        0x2573 => {
            let hs = (t_light as f32 * 0.5).max(0.7);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                dist_to_segment(x, y, w, 0.0, 0.0, h) <= hs
                    || dist_to_segment(x, y, 0.0, 0.0, w, h) <= hs
            });
            return true;
        }
        // Rounded corners ╭ ╮ ╯ ╰ (U+256D..=U+2570)
        0x256D..=0x2570 => {
            let hs = (t_light as f32 * 0.5).max(0.65);
            let mcx = cx as f32 + 0.5;
            let mcy = cy as f32 + 0.5;
            let r = mcx.min(mcy).min(w - mcx).min(h - mcy).max(1.0);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                let (ax_x, ax_y, sx, sy) = match cp {
                    0x256D => (mcx + r, mcy + r, 1.0f32, 1.0f32), // ╭ right + down
                    0x256E => (mcx - r, mcy + r, -1.0, 1.0),      // ╮ left + down
                    0x256F => (mcx - r, mcy - r, -1.0, -1.0),     // ╯ left + up
                    _ => (mcx + r, mcy - r, 1.0, -1.0),           // ╰ right + up
                };
                if (x - ax_x) * sx >= 0.0 && (y - mcy).abs() <= hs {
                    return true;
                }
                if (y - ax_y) * sy >= 0.0 && (x - mcx).abs() <= hs {
                    return true;
                }
                if (x - ax_x) * sx <= 0.0 && (y - ax_y) * sy <= 0.0 {
                    let d = ((x - ax_x).powi(2) + (y - ax_y).powi(2)).sqrt();
                    if (d - r).abs() <= hs {
                        return true;
                    }
                }
                false
            });
            return true;
        }
        _ => {}
    }

    // Arm weights: (left, right, up, down) where 0=none, 1=light, 2=heavy, 3=double
    let arms: (u8, u8, u8, u8) = match cp {
        // Horizontal & Vertical lines (including dashed variants)
        0x2500 | 0x2504 | 0x2508 | 0x254C => (1, 1, 0, 0),
        0x2501 | 0x2505 | 0x2509 | 0x254D => (2, 2, 0, 0),
        0x2502 | 0x2506 | 0x250A | 0x254E => (0, 0, 1, 1),
        0x2503 | 0x2507 | 0x250B | 0x254F => (0, 0, 2, 2),
        // Corners ┌ ┍ ┎ ┏ ┐ ┑ ┒ ㄱ └ ┕ ┖ ┗ ┘ ┙ ┚ ┛
        0x250C => (0, 1, 0, 1),
        0x250D => (0, 2, 0, 1),
        0x250E => (0, 1, 0, 2),
        0x250F => (0, 2, 0, 2),
        0x2510 => (1, 0, 0, 1),
        0x2511 => (2, 0, 0, 1),
        0x2512 => (1, 0, 0, 2),
        0x2513 => (2, 0, 0, 2),
        0x2514 => (0, 1, 1, 0),
        0x2515 => (0, 2, 1, 0),
        0x2516 => (0, 1, 2, 0),
        0x2517 => (0, 2, 2, 0),
        0x2518 => (1, 0, 1, 0),
        0x2519 => (2, 0, 1, 0),
        0x251A => (1, 0, 2, 0),
        0x251B => (2, 0, 2, 0),
        // T-junctions ├..=┫, ┬..=┳, ┴..=┻
        0x251C => (0, 1, 1, 1),
        0x251D => (0, 2, 1, 1),
        0x251E => (0, 1, 2, 1),
        0x251F => (0, 1, 1, 2),
        0x2520 => (0, 1, 2, 2),
        0x2521 => (0, 2, 2, 1),
        0x2522 => (0, 2, 1, 2),
        0x2523 => (0, 2, 2, 2),
        0x2524 => (1, 0, 1, 1),
        0x2525 => (2, 0, 1, 1),
        0x2526 => (1, 0, 2, 1),
        0x2527 => (1, 0, 1, 2),
        0x2528 => (1, 0, 2, 2),
        0x2529 => (2, 0, 2, 1),
        0x252A => (2, 0, 1, 2),
        0x252B => (2, 0, 2, 2),
        0x252C => (1, 1, 0, 1),
        0x252D => (2, 1, 0, 1),
        0x252E => (1, 2, 0, 1),
        0x252F => (2, 2, 0, 1),
        0x2530 => (1, 1, 0, 2),
        0x2531 => (2, 1, 0, 2),
        0x2532 => (1, 2, 0, 2),
        0x2533 => (2, 2, 0, 2),
        0x2534 => (1, 1, 1, 0),
        0x2535 => (2, 1, 1, 0),
        0x2536 => (1, 2, 1, 0),
        0x2537 => (2, 2, 1, 0),
        0x2538 => (1, 1, 2, 0),
        0x2539 => (2, 1, 2, 0),
        0x253A => (1, 2, 2, 0),
        0x253B => (2, 2, 2, 0),
        // Crosses ┼..=╋
        0x253C => (1, 1, 1, 1),
        0x253D => (2, 1, 1, 1),
        0x253E => (1, 2, 1, 1),
        0x253F => (2, 2, 1, 1),
        0x2540 => (1, 1, 2, 1),
        0x2541 => (1, 1, 1, 2),
        0x2542 => (1, 1, 2, 2),
        0x2543 => (2, 1, 2, 1),
        0x2544 => (1, 2, 2, 1),
        0x2545 => (2, 1, 1, 2),
        0x2546 => (1, 2, 1, 2),
        0x2547 => (2, 2, 2, 1),
        0x2548 => (2, 2, 1, 2),
        0x2549 => (2, 1, 2, 2),
        0x254A => (1, 2, 2, 2),
        0x254B => (2, 2, 2, 2),
        // Double lines & mixed double/single U+2550..=U+256C
        0x2550 => (3, 3, 0, 0),
        0x2551 => (0, 0, 3, 3),
        0x2552 => (0, 3, 0, 1),
        0x2553 => (0, 1, 0, 3),
        0x2554 => (0, 3, 0, 3),
        0x2555 => (3, 0, 0, 1),
        0x2556 => (1, 0, 0, 3),
        0x2557 => (3, 0, 0, 3),
        0x2558 => (0, 3, 1, 0),
        0x2559 => (0, 1, 3, 0),
        0x255A => (0, 3, 3, 0),
        0x255B => (3, 0, 1, 0),
        0x255C => (1, 0, 3, 0),
        0x255D => (3, 0, 3, 0),
        0x255E => (0, 3, 1, 1),
        0x255F => (0, 1, 3, 3),
        0x2560 => (0, 3, 3, 3),
        0x2561 => (3, 0, 1, 1),
        0x2562 => (1, 0, 3, 3),
        0x2563 => (3, 0, 3, 3),
        0x2564 => (3, 3, 0, 1),
        0x2565 => (1, 1, 0, 3),
        0x2566 => (3, 3, 0, 3),
        0x2567 => (3, 3, 1, 0),
        0x2568 => (1, 1, 3, 0),
        0x2569 => (3, 3, 3, 0),
        0x256A => (3, 3, 1, 1),
        0x256B => (1, 1, 3, 3),
        0x256C => (3, 3, 3, 3),
        // Half-lines U+2574..=U+257F
        0x2574 => (1, 0, 0, 0),
        0x2575 => (0, 0, 1, 0),
        0x2576 => (0, 1, 0, 0),
        0x2577 => (0, 0, 0, 1),
        0x2578 => (2, 0, 0, 0),
        0x2579 => (0, 0, 2, 0),
        0x257A => (0, 2, 0, 0),
        0x257B => (0, 0, 0, 2),
        0x257C => (1, 2, 0, 0),
        0x257D => (0, 0, 1, 2),
        0x257E => (2, 1, 0, 0),
        0x257F => (0, 0, 2, 1),
        _ => return false,
    };

    let (l, r, u, d) = arms;
    let span = |center: u32, t: u32| -> (u32, u32) {
        let s = center.saturating_sub(t / 2);
        (s, s + t)
    };
    let double_off = t_light.max(1) + (t_light / 2).max(1);

    let draw_h = |buf: &mut [u8], x0: u32, x1: u32, style: u8| {
        match style {
            1 => {
                let (y0, y1) = span(cy, t_light);
                fill_rect(buf, cw, ch, x0, y0, x1, y1, 255);
            }
            2 => {
                let (y0, y1) = span(cy, t_heavy);
                fill_rect(buf, cw, ch, x0, y0, x1, y1, 255);
            }
            3 => {
                let (ya0, ya1) = span(cy.saturating_sub(double_off), t_light);
                let (yb0, yb1) = span(cy + double_off, t_light);
                fill_rect(buf, cw, ch, x0, ya0, x1, ya1, 255);
                fill_rect(buf, cw, ch, x0, yb0, x1, yb1, 255);
            }
            _ => {}
        }
    };
    let draw_v = |buf: &mut [u8], y0: u32, y1: u32, style: u8| {
        match style {
            1 => {
                let (x0, x1) = span(cx, t_light);
                fill_rect(buf, cw, ch, x0, y0, x1, y1, 255);
            }
            2 => {
                let (x0, x1) = span(cx, t_heavy);
                fill_rect(buf, cw, ch, x0, y0, x1, y1, 255);
            }
            3 => {
                let (xa0, xa1) = span(cx.saturating_sub(double_off), t_light);
                let (xb0, xb1) = span(cx + double_off, t_light);
                fill_rect(buf, cw, ch, xa0, y0, xa1, y1, 255);
                fill_rect(buf, cw, ch, xb0, y0, xb1, y1, 255);
            }
            _ => {}
        }
    };

    let pad_c = (t_heavy / 2).max(1);
    if l != 0 {
        draw_h(buf, 0, (cx + pad_c).min(cw), l);
    }
    if r != 0 {
        draw_h(buf, cx.saturating_sub(pad_c), cw, r);
    }
    if u != 0 {
        draw_v(buf, 0, (cy + pad_c).min(ch), u);
    }
    if d != 0 {
        draw_v(buf, cy.saturating_sub(pad_c), ch, d);
    }
    true
}

/// Built-in vector fallback for common Nerd Font PUA prompt icons when no Nerd Font is installed.
fn draw_nerd_icon_fallback(buf: &mut [u8], cw: u32, ch: u32, cp: u32) -> bool {
    let w = cw as f32;
    let h = ch as f32;
    match cp {
        // Git branch / Git icons in Nerd Fonts (fa-code-fork, dev-git_branch, dev-git)
        0xF126 | 0xE725 | 0xE702 | 0xF0620 => draw_powerline(buf, cw, ch, 0xE0A0),
        // Lock icons in Nerd Fonts (fa-lock)
        0xF023 | 0xF033E => draw_powerline(buf, cw, ch, 0xE0A2),
        // Folder icons in Nerd Fonts (fa-folder, fa-folder-open, custom-folder)
        0xF07B | 0xF07C | 0xE5FF | 0xE5FE | 0xF024B => {
            rasterize_supersampled(buf, cw, ch, |x, y| {
                let in_body = x >= w * 0.12 && x <= w * 0.88 && y >= h * 0.34 && y <= h * 0.78;
                let in_tab = x >= w * 0.12 && x <= w * 0.46 && y >= h * 0.24 && y <= h * 0.36;
                in_body || in_tab
            });
            true
        }
        // Checkmark icons (fa-check)
        0xF00C | 0x2713 | 0x2714 => {
            let sw = (w * 0.09).max(0.85);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                dist_to_segment(x, y, w * 0.18, h * 0.54, w * 0.42, h * 0.74) <= sw
                    || dist_to_segment(x, y, w * 0.42, h * 0.74, w * 0.84, h * 0.28) <= sw
            });
            true
        }
        // Cross / X icons (fa-times / fa-close)
        0xF00D | 0x2717 | 0x2718 => {
            let sw = (w * 0.09).max(0.85);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                dist_to_segment(x, y, w * 0.22, h * 0.28, w * 0.78, h * 0.72) <= sw
                    || dist_to_segment(x, y, w * 0.78, h * 0.28, w * 0.22, h * 0.72) <= sw
            });
            true
        }
        // Heavy right-pointing angle bracket ❯ (Starship / Pure prompt) & Nerd Font chevron-right
        0x276F | 0xF054 => {
            let sw = (w * 0.10).max(0.9);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                dist_to_segment(x, y, w * 0.26, h * 0.24, w * 0.74, h * 0.50) <= sw
                    || dist_to_segment(x, y, w * 0.74, h * 0.50, w * 0.26, h * 0.76) <= sw
            });
            true
        }
        // Heavy right-pointing arrow ➜ (Oh-My-Zsh robbyrussell prompt) & Nerd Font arrow-right
        0x279C | 0xF061 | 0xF178 => {
            let sw = (w * 0.095).max(0.85);
            rasterize_supersampled(buf, cw, ch, |x, y| {
                dist_to_segment(x, y, w * 0.14, h * 0.50, w * 0.82, h * 0.50) <= sw
                    || dist_to_segment(x, y, w * 0.52, h * 0.28, w * 0.84, h * 0.50) <= sw
                    || dist_to_segment(x, y, w * 0.84, h * 0.50, w * 0.52, h * 0.72) <= sw
            });
            true
        }
        _ => false,
    }
}

// ============================================================================
// macOS CoreText + CoreGraphics System Fallback Rasterizer
// ============================================================================

/// System font fallback is CoreText-only; elsewhere the fontconfig-free fallback
/// chain above is all there is.
#[cfg(not(target_os = "macos"))]
fn macos_coretext_rasterize(_buf: &mut [u8], _cw: u32, _ch_px: u32, _baseline: i32, _px: f32, _ch: char) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn macos_coretext_rasterize(
    buf: &mut [u8],
    cw: u32,
    ch_px: u32,
    baseline: i32,
    px: f32,
    ch: char,
) -> bool {
    use std::ffi::c_void;
    use std::ptr;

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct CFRange {
        location: isize,
        length: isize,
    }

    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCharacters(
            alloc: *const c_void,
            chars: *const u16,
            num_chars: isize,
        ) -> *const c_void;
        fn CFStringGetLength(the_string: *const c_void) -> isize;
        fn CFStringGetCharacters(the_string: *const c_void, range: CFRange, buffer: *mut u16);
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGColorSpaceCreateDeviceGray() -> *mut c_void;
        fn CGColorSpaceRelease(space: *mut c_void);
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: *mut c_void,
            bitmap_info: u32,
        ) -> *mut c_void;
        fn CGContextRelease(c: *mut c_void);
        fn CGContextSetGrayFillColor(c: *mut c_void, gray: f64, alpha: f64);
        fn CGContextSetAllowsAntialiasing(c: *mut c_void, allows: bool);
        fn CGContextSetShouldAntialias(c: *mut c_void, should: bool);
        fn CGContextSetAllowsFontSmoothing(c: *mut c_void, allows: bool);
        fn CGContextSetShouldSmoothFonts(c: *mut c_void, should: bool);
    }

    #[link(name = "CoreText", kind = "framework")]
    extern "C" {
        fn CTFontCreateWithName(
            name: *const c_void,
            size: f64,
            matrix: *const c_void,
        ) -> *const c_void;
        fn CTFontCreateForString(
            current_font: *const c_void,
            string: *const c_void,
            range: CFRange,
        ) -> *const c_void;
        fn CTFontCopyFamilyName(font: *const c_void) -> *const c_void;
        fn CTFontGetGlyphsForCharacters(
            font: *const c_void,
            characters: *const u16,
            glyphs: *mut u16,
            count: isize,
        ) -> bool;
        fn CTFontGetBoundingRectsForGlyphs(
            font: *const c_void,
            orientation: u32,
            glyphs: *const u16,
            bounding_rects: *mut CGRect,
            count: isize,
        ) -> CGRect;
        fn CTFontDrawGlyphs(
            font: *const c_void,
            glyphs: *const u16,
            positions: *const CGPoint,
            count: usize,
            context: *mut c_void,
        );
    }

    unsafe {
        let mut u16_buf = [0u16; 2];
        let utf16 = ch.encode_utf16(&mut u16_buf);
        let cf_str = CFStringCreateWithCharacters(ptr::null(), utf16.as_ptr(), utf16.len() as isize);
        if cf_str.is_null() {
            return false;
        }

        let menlo: [u16; 5] = [b'M' as u16, b'e' as u16, b'n' as u16, b'l' as u16, b'o' as u16];
        let cf_menlo = CFStringCreateWithCharacters(ptr::null(), menlo.as_ptr(), menlo.len() as isize);
        let base_font = CTFontCreateWithName(cf_menlo, px as f64, ptr::null());
        CFRelease(cf_menlo);
        if base_font.is_null() {
            CFRelease(cf_str);
            return false;
        }

        let fb_font = CTFontCreateForString(
            base_font,
            cf_str,
            CFRange { location: 0, length: utf16.len() as isize },
        );
        CFRelease(base_font);
        CFRelease(cf_str);
        if fb_font.is_null() {
            return false;
        }

        // Ignore macOS ".LastResort" fallback font so we never draw LastResort boxes.
        let fam = CTFontCopyFamilyName(fb_font);
        if !fam.is_null() {
            let len = CFStringGetLength(fam);
            let mut fbuf = [0u16; 32];
            let n = len.min(32);
            CFStringGetCharacters(fam, CFRange { location: 0, length: n }, fbuf.as_mut_ptr());
            CFRelease(fam);
            let fam_str = String::from_utf16_lossy(&fbuf[..n as usize]);
            if fam_str.contains("LastResort") {
                CFRelease(fb_font);
                return false;
            }
        }

        let mut glyphs = [0u16; 2];
        let ok = CTFontGetGlyphsForCharacters(
            fb_font,
            utf16.as_ptr(),
            glyphs.as_mut_ptr(),
            utf16.len() as isize,
        );
        if !ok || glyphs[0] == 0 {
            CFRelease(fb_font);
            return false;
        }

        let mut bbox = CGRect::default();
        CTFontGetBoundingRectsForGlyphs(fb_font, 0, &glyphs[0], &mut bbox, 1);

        // If the fallback glyph is wider than a single cell, create a scaled font so it fits.
        let mut draw_font = fb_font;
        if bbox.size.width > cw as f64 * 1.02 && bbox.size.width > 1.0 {
            let scale = (cw as f64 / bbox.size.width) * 0.92;
            let fam = CTFontCopyFamilyName(fb_font);
            if !fam.is_null() {
                let scaled = CTFontCreateWithName(fam, px as f64 * scale, ptr::null());
                CFRelease(fam);
                if !scaled.is_null() {
                    let mut g2 = [0u16; 2];
                    if CTFontGetGlyphsForCharacters(
                        scaled,
                        utf16.as_ptr(),
                        g2.as_mut_ptr(),
                        utf16.len() as isize,
                    ) && g2[0] != 0
                    {
                        CFRelease(fb_font);
                        draw_font = scaled;
                        glyphs[0] = g2[0];
                        CTFontGetBoundingRectsForGlyphs(draw_font, 0, &glyphs[0], &mut bbox, 1);
                    } else {
                        CFRelease(scaled);
                    }
                }
            }
        }

        let cs = CGColorSpaceCreateDeviceGray();
        let ctx = CGBitmapContextCreate(
            buf.as_mut_ptr() as *mut c_void,
            cw as usize,
            ch_px as usize,
            8,
            cw as usize,
            cs,
            0, // kCGImageAlphaNone
        );
        CGColorSpaceRelease(cs);
        if ctx.is_null() {
            CFRelease(draw_font);
            return false;
        }

        CGContextSetAllowsAntialiasing(ctx, true);
        CGContextSetShouldAntialias(ctx, true);
        CGContextSetAllowsFontSmoothing(ctx, true);
        CGContextSetShouldSmoothFonts(ctx, true);
        CGContextSetGrayFillColor(ctx, 1.0, 1.0);

        let gx = ((cw as f64 - bbox.size.width) * 0.5 - bbox.origin.x).clamp(0.0, cw as f64);
        let gy = (ch_px as i32 - baseline) as f64;
        let pos = CGPoint { x: gx, y: gy };
        CTFontDrawGlyphs(draw_font, &glyphs[0], &pos, 1, ctx);

        CGContextRelease(ctx);
        CFRelease(draw_font);
        true
    }
}

// ============================================================================
// Color emoji
// ============================================================================

/// Codepoints drawn from the color emoji font when they are double-width
/// (emoji presentation): pictographs, symbols, flags.
fn is_emoji(cp: u32) -> bool {
    matches!(cp, 0x231A..=0x23FF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x1F000..=0x1FAFF)
}

/// A double-width cluster that is an emoji: an emoji base, or emoji
/// presentation (VS16, ZWJ, keycap) unless VS15 asks for text.
fn is_emoji_text(text: &str) -> bool {
    let first = text.chars().next().map_or(0, |c| c as u32);
    !text.contains('\u{FE0E}') && (is_emoji(first) || text.contains(['\u{FE0F}', '\u{200D}', '\u{20E3}']))
}

/// Rasterize `text` (one emoji, possibly a ZWJ / flag / skin tone sequence)
/// from the color emoji font into `buf`, a zeroed `w × h` premultiplied RGBA
/// canvas, scaled to fit and centered.
#[cfg(target_os = "macos")]
fn rasterize_color(buf: &mut [u8], w: u32, h: u32, px: f32, text: &str) -> bool {
    coretext_line(buf, w, h, px, text, None)
}

/// A grapheme cluster as text (an Indic conjunct, a base with marks) through
/// CoreText's shaping and font cascade into a `w × h` coverage canvas, on the
/// baseline; for clusters the primary font cannot shape.
#[cfg(target_os = "macos")]
fn rasterize_text_cluster(buf: &mut [u8], w: u32, h: u32, baseline: i32, px: f32, text: &str) -> bool {
    coretext_line(buf, w, h, px, text, Some(baseline))
}

#[cfg(not(target_os = "macos"))]
fn rasterize_text_cluster(_buf: &mut [u8], _w: u32, _h: u32, _baseline: i32, _px: f32, _text: &str) -> bool {
    false
}

/// Draw `text` as one CoreText line: color emoji (RGBA, scaled to fit and
/// centered) when `baseline` is `None`, else text coverage (one byte per px)
/// from Menlo and the system cascade, centered horizontally on the baseline.
#[cfg(target_os = "macos")]
fn coretext_line(buf: &mut [u8], w: u32, h: u32, px: f32, text: &str, baseline: Option<i32>) -> bool {
    use std::ffi::c_void;
    use std::ptr;

    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    struct CGSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCharacters(alloc: *const c_void, chars: *const u16, n: isize) -> *const c_void;
        fn CFDictionaryCreate(
            alloc: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            n: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        fn CFAttributedStringCreate(alloc: *const c_void, s: *const c_void, attributes: *const c_void) -> *const c_void;
        fn CFRelease(cf: *const c_void);
        static kCFTypeDictionaryKeyCallBacks: [usize; 6];
        static kCFTypeDictionaryValueCallBacks: [usize; 5];
        static kCFBooleanTrue: *const c_void;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
        fn CGColorSpaceCreateDeviceGray() -> *mut c_void;
        fn CGContextSetGrayFillColor(c: *mut c_void, gray: f64, alpha: f64);
        fn CGColorSpaceRelease(space: *mut c_void);
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: *mut c_void,
            bitmap_info: u32,
        ) -> *mut c_void;
        fn CGContextRelease(c: *mut c_void);
        fn CGContextSetTextPosition(c: *mut c_void, x: f64, y: f64);
    }
    #[link(name = "CoreText", kind = "framework")]
    extern "C" {
        fn CTFontCreateWithName(name: *const c_void, size: f64, matrix: *const c_void) -> *const c_void;
        fn CTLineCreateWithAttributedString(s: *const c_void) -> *const c_void;
        fn CTLineGetBoundsWithOptions(line: *const c_void, options: usize) -> CGRect;
        fn CTLineDraw(line: *const c_void, ctx: *mut c_void);
        static kCTFontAttributeName: *const c_void;
        static kCTForegroundColorFromContextAttributeName: *const c_void;
    }
    /// `kCGImageAlphaPremultipliedLast`: RGBA bytes.
    const PREMULTIPLIED_LAST: u32 = 1;
    /// `kCTLineBoundsUseGlyphPathBounds`: the ink, not the typographic box.
    const GLYPH_PATH_BOUNDS: usize = 1 << 3;

    let units: Vec<u16> = text.encode_utf16().collect();
    let name: Vec<u16> = if baseline.is_some() { "Menlo" } else { "Apple Color Emoji" }.encode_utf16().collect();
    // SAFETY: every CoreFoundation object created here is released; the
    // context draws into `buf`, which outlives it and is `w * h * 4` bytes.
    unsafe {
        let cf_text = CFStringCreateWithCharacters(ptr::null(), units.as_ptr(), units.len() as isize);
        let cf_name = CFStringCreateWithCharacters(ptr::null(), name.as_ptr(), name.len() as isize);
        // A whole sequence shaped as one line, so ZWJ / flag / skin tone ligatures apply.
        let line_at = |size: f64| -> *const c_void {
            let font = CTFontCreateWithName(cf_name, size, ptr::null());
            if font.is_null() {
                return ptr::null();
            }
            // Text is drawn in the context's fill color (white coverage).
            let keys = [kCTFontAttributeName, kCTForegroundColorFromContextAttributeName];
            let values = [font, kCFBooleanTrue];
            let attrs = CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1 + baseline.is_some() as isize,
                &kCFTypeDictionaryKeyCallBacks as *const _ as *const c_void,
                &kCFTypeDictionaryValueCallBacks as *const _ as *const c_void,
            );
            CFRelease(font);
            if attrs.is_null() {
                return ptr::null();
            }
            let attributed = CFAttributedStringCreate(ptr::null(), cf_text, attrs);
            CFRelease(attrs);
            if attributed.is_null() {
                return ptr::null();
            }
            let line = CTLineCreateWithAttributedString(attributed);
            CFRelease(attributed);
            line
        };
        let mut line = if cf_text.is_null() || cf_name.is_null() { ptr::null() } else { line_at(px as f64) };
        if !line.is_null() {
            let b = CTLineGetBoundsWithOptions(line, GLYPH_PATH_BOUNDS);
            // Shrink to fit the cells (emoji are wider and taller than text at the same size).
            let scale = match baseline {
                None => (w as f64 / b.size.width.max(1.0)).min(h as f64 / b.size.height.max(1.0)) * 0.95,
                Some(_) => w as f64 / b.size.width.max(1.0),
            };
            if scale < 1.0 {
                let smaller = line_at(px as f64 * scale);
                if !smaller.is_null() {
                    CFRelease(line);
                    line = smaller;
                }
            }
        }
        for cf in [cf_text, cf_name] {
            if !cf.is_null() {
                CFRelease(cf);
            }
        }
        if line.is_null() {
            return false;
        }
        let b = CTLineGetBoundsWithOptions(line, GLYPH_PATH_BOUNDS);
        let (cs, bpp, info) = match baseline {
            None => (CGColorSpaceCreateDeviceRGB(), 4, PREMULTIPLIED_LAST),
            Some(_) => (CGColorSpaceCreateDeviceGray(), 1, 0),
        };
        let ctx = CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, w as usize, h as usize, 8, w as usize * bpp, cs, info);
        CGColorSpaceRelease(cs);
        if ctx.is_null() {
            CFRelease(line);
            return false;
        }
        CGContextSetGrayFillColor(ctx, 1.0, 1.0);
        // Centered in the canvas (CoreGraphics has its origin at the bottom left),
        // text on the cell's baseline.
        let x = (w as f64 - b.size.width) / 2.0 - b.origin.x;
        let y = match baseline {
            None => (h as f64 - b.size.height) / 2.0 - b.origin.y,
            Some(base) => (h as i32 - base) as f64,
        };
        CGContextSetTextPosition(ctx, x, y);
        CTLineDraw(line, ctx);
        CGContextRelease(ctx);
        CFRelease(line);
    }
    match baseline {
        None => buf.as_chunks::<4>().0.iter().any(|p| p[3] != 0),
        Some(_) => buf.iter().any(|&v| v != 0),
    }
}

/// Bitmap color emoji (CBDT / sbix PNG strikes, e.g. Noto Color Emoji) on
/// other systems; vector-only (COLR) emoji fonts fall back to monochrome.
#[cfg(not(target_os = "macos"))]
fn rasterize_color(buf: &mut [u8], w: u32, h: u32, _px: f32, text: &str) -> bool {
    use rustybuzz::ttf_parser::{GlyphId, RasterImageFormat};
    use std::sync::OnceLock;

    static FACE: OnceLock<Option<(rustybuzz::Face<'static>, Arc<memmap2::Mmap>)>> = OnceLock::new();
    let pair = FACE.get_or_init(|| {
        let face = crate::fontdb::find_family("Noto Color Emoji")?;
        shaper_for(&face.path, face.index)
    });
    let Some((face, _mmap)) = pair.as_ref() else { return false };
    // Shaped, so ZWJ / flag / skin tone sequences become their ligature glyph.
    let mut ub = rustybuzz::UnicodeBuffer::new();
    ub.push_str(text);
    let shaped = rustybuzz::shape(face, &[], ub);
    let Some(gid) = shaped.glyph_infos().first().map(|g| g.glyph_id).filter(|&g| g != 0) else { return false };
    let Some(img) = face.glyph_raster_image(GlyphId(gid as u16), h.min(u16::MAX as u32) as u16) else { return false };
    if img.format != RasterImageFormat::PNG {
        return false;
    }
    let Ok((sw, sh, rgba)) = crate::graphics::decode_png(img.data) else { return false };
    blit_fit(&rgba, sw, sh, buf, w, h);
    true
}

/// Scale straight-alpha RGBA `src` to fit `dst` (aspect kept, centered, area
/// averaged), writing premultiplied RGBA.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn blit_fit(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
    let scale = (dw as f32 / sw as f32).min(dh as f32 / sh as f32) * 0.95;
    let (tw, th) = ((sw as f32 * scale).round().max(1.0) as u32, (sh as f32 * scale).round().max(1.0) as u32);
    let (ox, oy) = ((dw.saturating_sub(tw)) / 2, (dh.saturating_sub(th)) / 2);
    for ty in 0..th.min(dh) {
        let (y0, y1) = ((ty as f32 / scale) as u32, (((ty + 1) as f32 / scale) as u32).clamp(1, sh));
        for tx in 0..tw.min(dw) {
            let (x0, x1) = ((tx as f32 / scale) as u32, (((tx + 1) as f32 / scale) as u32).clamp(1, sw));
            let mut acc = [0u32; 4];
            let mut n = 0;
            for y in y0..y1.max(y0 + 1).min(sh) {
                for x in x0..x1.max(x0 + 1).min(sw) {
                    let p = &src[((y * sw + x) * 4) as usize..][..4];
                    let a = p[3] as u32;
                    acc[0] += p[0] as u32 * a / 255;
                    acc[1] += p[1] as u32 * a / 255;
                    acc[2] += p[2] as u32 * a / 255;
                    acc[3] += a;
                    n += 1;
                }
            }
            let n = n.max(1);
            let d = &mut dst[(((oy + ty) * dw + ox + tx) * 4) as usize..][..4];
            for (d, a) in d.iter_mut().zip(acc) {
                *d = (a / n) as u8;
            }
        }
    }
}

// ============================================================================
// Font Discovery (Primary + Nerd Font / Symbol Fallbacks)
// ============================================================================

static FONT_CACHE: Mutex<Option<(Vec<String>, Arc<FontSet>)>> = parking_lot::const_mutex(None);

/// Load the primary monospace font and any available symbol/Nerd Font fallbacks.
///
/// `families` comes from the `font-family` config option: the first family
/// that resolves becomes the primary font, the remaining ones are tried as
/// fallbacks before the auto-discovered symbol fonts. Without configured
/// families, `LITTY_FONT=/path/to/font.ttf` or the platform defaults apply.
pub fn load_font(families: &[String]) -> Arc<FontSet> {
    let mut cache = FONT_CACHE.lock();
    if let Some((ref cached_families, ref font_set)) = *cache {
        if cached_families.as_slice() == families {
            return Arc::clone(font_set);
        }
    }

    let mut configured_faces: Vec<(String, u32)> = Vec::new();
    for fam in families {
        match crate::fontdb::find_family(fam) {
            Some(face) => {
                configured_faces.push((face.path.clone(), face.index));
            }
            None => eprintln!("litty: font-family `{fam}` not found; using fallback"),
        }
    }

    let mut primary_candidates: Vec<String> = Vec::new();
    if let Ok(p) = std::env::var("LITTY_FONT").or_else(|_| std::env::var("LAPLACE_FONT")) {
        primary_candidates.push(p);
    }

    #[cfg(target_os = "macos")]
    primary_candidates.extend(
        [
            "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/SF-Mono-Regular.otf",
            "/System/Library/Fonts/SFNSMono.ttf",
            "/System/Library/Fonts/Menlo.ttc",
            "/System/Library/Fonts/Monaco.ttf",
        ]
        .map(String::from),
    );

    #[cfg(not(target_os = "macos"))]
    primary_candidates.extend(
        [
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
            "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
            "/usr/share/fonts/liberation-mono/LiberationMono-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
        ]
        .map(String::from),
    );

    let (mut primary_path, mut primary, mut shaper) = (None, None, None);

    let mut configured_iter = configured_faces.into_iter();
    for (p, idx) in configured_iter.by_ref() {
        let s = shaper_for(&p, idx);
        if let Some(f) = parse_font(&p, idx, s.is_some()) {
            primary_path = Some(p);
            primary = Some(f);
            shaper = s;
            break;
        }
    }

    if primary.is_none() {
        for path in &primary_candidates {
            let s = shaper_for(path, 0);
            if let Some(f) = parse_font(path, 0, s.is_some()) {
                primary_path = Some(path.clone());
                primary = Some(f);
                shaper = s;
                break;
            }
        }
    }

    // Only if none of the standard system/configured fonts were found, fall back to scanning for Nerd Fonts.
    if primary.is_none() {
        let (discovered_mono_nerd, _) = discover_nerd_fonts();
        for path in &discovered_mono_nerd {
            let s = shaper_for(path, 0);
            if let Some(f) = parse_font(path, 0, s.is_some()) {
                primary_path = Some(path.clone());
                primary = Some(f);
                shaper = s;
                break;
            }
        }
    }
    let primary = primary.expect("litty: no usable monospace font found; set font-family in the config");

    let mut fallbacks: Vec<fontdue::Font> = Vec::new();
    for (p, idx) in configured_iter {
        if let Some(f) = try_parse_font_index(&p, idx) {
            fallbacks.push(f);
        }
    }
    if let Some(f) = find_first_symbol_font(primary_path.as_deref()) {
        fallbacks.push(f);
    }

    let (shaper_face, shaper_mmap) = match shaper {
        Some((f, m)) => (Some(f), Some(m)),
        None => (None, None),
    };
    let set = Arc::new(FontSet { primary, fallbacks, shaper: shaper_face, _mmap: shaper_mmap });
    *cache = Some((families.to_vec(), Arc::clone(&set)));
    set
}

/// A shaping face over `path`. The memory-mapped file is kept alive in the returned `Arc<Mmap>`.
fn shaper_for(path: &str, index: u32) -> Option<(rustybuzz::Face<'static>, Arc<memmap2::Mmap>)> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: font files are not modified while mapped (same as for fontdue).
    let map = Arc::new(unsafe { memmap2::Mmap::map(&file) }.ok()?);
    // SAFETY: the Face references bytes owned by `map`, which is kept alive alongside it in FontSet.
    let bytes: &'static [u8] = unsafe { &*(&**map as *const [u8]) };
    let face = rustybuzz::Face::from_slice(bytes, index)?;
    Some((face, map))
}

fn find_first_symbol_font(exclude_path: Option<&str>) -> Option<fontdue::Font> {
    let mut candidates = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(format!("{home}/Library/Fonts/SymbolsNerdFont-Regular.ttf"));
        candidates.push(format!("{home}/Library/Fonts/SymbolsNerdFontMono-Regular.ttf"));
        candidates.push(format!("{home}/.local/share/fonts/SymbolsNerdFont-Regular.ttf"));
        candidates.push(format!("{home}/.fonts/SymbolsNerdFont-Regular.ttf"));
    }
    candidates.extend(
        [
            "/Library/Fonts/SymbolsNerdFont-Regular.ttf",
            "/Library/Fonts/SymbolsNerdFontMono-Regular.ttf",
            "/opt/homebrew/share/fonts/SymbolsNerdFont-Regular.ttf",
            "/usr/share/fonts/truetype/nerd-fonts/SymbolsNerdFont-Regular.ttf",
            "/usr/share/fonts/TTF/SymbolsNerdFont-Regular.ttf",
        ]
        .map(String::from),
    );
    for path in &candidates {
        if exclude_path == Some(path.as_str()) {
            continue;
        }
        if let Some(f) = try_parse_font(path) {
            return Some(f);
        }
    }
    None
}

fn try_parse_font(path: &str) -> Option<fontdue::Font> {
    try_parse_font_index(path, 0)
}

fn try_parse_font_index(path: &str, collection_index: u32) -> Option<fontdue::Font> {
    parse_font(path, collection_index, false)
}

/// `substitutions`: also load glyphs only reachable through GSUB (ligatures,
/// stylistic sets), which shaping needs for the primary font.
fn parse_font(path: &str, collection_index: u32, substitutions: bool) -> Option<fontdue::Font> {
    let file = std::fs::File::open(path).ok()?;
    let mmap = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    let settings = fontdue::FontSettings {
        scale: 32.0,
        collection_index,
        load_substitutions: substitutions,
    };
    match fontdue::Font::from_bytes(mmap, settings) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("litty: failed to parse font {path}: {e}");
            None
        }
    }
}

/// Scan standard font directories for installed Nerd Fonts or Symbols Nerd Font.
fn discover_nerd_fonts() -> (Vec<String>, Vec<String>) {
    let mut mono_nerd = Vec::new();
    let mut symbols = Vec::new();

    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(format!("{home}/Library/Fonts"));
        dirs.push(format!("{home}/.local/share/fonts"));
        dirs.push(format!("{home}/.fonts"));
    }
    dirs.extend(
        [
            "/Library/Fonts",
            "/Applications/Ghostty.app/Contents/Resources/fonts",
            "/usr/share/fonts",
            "/usr/local/share/fonts",
            "/opt/homebrew/share/fonts",
        ]
        .map(String::from),
    );

    for d in dirs {
        let Ok(entries) = std::fs::read_dir(Path::new(&d)) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else { continue };
            let lower = name.to_ascii_lowercase();
            if !(lower.ends_with(".ttf") || lower.ends_with(".otf")) {
                continue;
            }
            if lower.contains("symbolsnerdfont") || lower.contains("nerd-fonts-symbols") {
                symbols.push(p.to_string_lossy().into_owned());
            } else if lower.contains("nerdfont")
                && (lower.contains("regular") || !lower.contains("italic") && !lower.contains("bold"))
            {
                mono_nerd.push(p.to_string_lossy().into_owned());
            }
        }
    }
    mono_nerd.sort();
    symbols.sort();
    (mono_nerd, symbols)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cell_size_pt_values() {
        let fonts = load_font(&["SFMono Nerd Font".to_string()]);
        let (cw, ch) = Atlas::cell_size_pt(&fonts, 15.0);
        println!("cell_size_pt for 15.0: cw = {cw}, ch = {ch}");
        println!("For 143x41: inner_w = {}, inner_h = {}", 143.0 * cw + 8.0, 41.0 * ch + 8.0);
        assert!(cw > 0.0 && ch > 0.0);
    }

    #[test]
    fn blit_fit_scales_premultiplies_and_centers() {
        // 4x2 opaque red over half-transparent blue, into an 8x8 canvas: 2x scale
        // would overflow the 0.95 margin, so it is ~1.9x and vertically centered.
        let mut src = Vec::new();
        for i in 0..8 {
            src.extend_from_slice(if i % 4 < 2 { &[255, 0, 0, 255] } else { &[0, 0, 255, 128] });
        }
        let mut dst = vec![0u8; 8 * 8 * 4];
        blit_fit(&src, 4, 2, &mut dst, 8, 8);
        let px = |x: usize, y: usize| &dst[(y * 8 + x) * 4..][..4];
        assert_eq!(px(0, 0), [0, 0, 0, 0], "transparent margin above");
        assert_eq!(px(0, 3), [255, 0, 0, 255]);
        assert_eq!(px(7, 3), [0, 0, 128, 128], "premultiplied");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn color_emoji_rasterizes_in_color() {
        let (w, h) = (36, 34);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        assert!(rasterize_color(&mut buf, w, h, 28.0, "😀"), "😀 from Apple Color Emoji");
        assert!(buf.as_chunks::<4>().0.iter().any(|p| p[3] > 200 && (p[0] as i32 - p[2] as i32).abs() > 60), "colored, not gray");
        buf.fill(0);
        assert!(rasterize_color(&mut buf, w, h, 28.0, "🇻🇳"), "a flag");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn text_clusters_render_through_coretext() {
        let (w, h) = (18, 34);
        let mut buf = vec![0u8; (w * h) as usize];
        assert!(rasterize_text_cluster(&mut buf, w, h, 26, 28.0, "\u{0915}\u{093F}"), "Devanagari ki");
        let ink_rows = (0..h as usize).filter(|y| buf[y * w as usize..][..w as usize].iter().any(|&v| v > 0)).count();
        assert!(ink_rows > 10, "a real glyph, not a dot");
    }

    /// Needs a font with programming ligatures, e.g.
    /// `LITTY_LIGATURE_FONT=JetBrainsMono-Regular.ttf cargo test ligatures`.
    #[test]
    fn ligatures_shape_into_glyph_pieces() {
        let Ok(path) = std::env::var("LITTY_LIGATURE_FONT").or_else(|_| std::env::var("LAPLACE_LIGATURE_FONT")) else { return };
        let (shaper, _mmap) = shaper_for(&path, 0)
            .map(|(f, m)| (Some(f), Some(m)))
            .unwrap_or((None, None));
        let fonts = FontSet { primary: parse_font(&path, 0, true).unwrap(), fallbacks: Vec::new(), shaper, _mmap };
        let row: Vec<crate::grid::Cell> =
            "ab -> cd != ef".chars().map(|c| crate::grid::Cell { codepoint: c as u32, ..Default::default() }).collect();
        let mut out = Vec::new();
        let mut on = Shaper::new(Vec::new());
        assert!(on.shape_row(&fonts, 28.0, 17, &row, &mut out));
        assert_eq!(&out[..2], [0, 0], "plain words are not shaped");
        assert!(out[3] != 0 && out[4] != 0, "`->` is substituted: {out:x?}");
        assert!(out[9] != 0 && out[10] != 0, "`!=` is substituted: {out:x?}");
        assert_eq!(out[5], 0);
        // JetBrains Mono draws `->` as one glyph over both cells, pen in the second.
        assert!(on.pieces.iter().all(|p| p.span == 2 && p.origin == 1), "{:?}", on.pieces);

        let off = ["-calt", "-liga"].map(|f| parse_font_feature(f).unwrap()).to_vec();
        assert!(!Shaper::new(off).shape_row(&fonts, 28.0, 17, &row, &mut out), "font-feature = -calt disables them");
    }

    #[test]
    fn metric_adjust_parses_pixels_and_percent() {
        assert_eq!(MetricAdjust::parse("2"), Ok(MetricAdjust::Px(2)));
        assert_eq!(MetricAdjust::parse("-1"), Ok(MetricAdjust::Px(-1)));
        assert_eq!(MetricAdjust::parse("10%"), Ok(MetricAdjust::Percent(10.0)));
        assert!(MetricAdjust::parse("big").is_err());
        assert_eq!(MetricAdjust::Px(2).apply(17), 19);
        assert_eq!(MetricAdjust::Percent(20.0).apply(20), 24);
        assert_eq!(MetricAdjust::Px(-50).apply(17), 1, "never below one pixel");
    }

    #[test]
    fn powerline_and_sprites_render_non_empty() {
        let mut buf = vec![0u8; 16 * 32];
        // U+E0B0 right arrow, U+E0A0 git branch, U+2500 box horizontal, U+2588 full block, U+28FF braille
        for cp in [0xE0B0, 0xE0A0, 0xE0A2, 0xE0B2, 0xE0B4, 0xE0B6, 0x2500, 0x256D, 0x2588, 0x28FF] {
            buf.fill(0);
            assert!(draw_builtin_sprite(&mut buf, 16, 32, cp), "sprite {cp:#X} not handled");
            assert!(buf.iter().any(|&b| b > 0), "sprite {cp:#X} produced blank bitmap");
        }
    }

    #[test]
    fn powerline_right_arrow_touches_left_edge_seamlessly() {
        let (cw, ch) = (16u32, 32u32);
        let mut buf = vec![0u8; (cw * ch) as usize];
        assert!(draw_builtin_sprite(&mut buf, cw, ch, 0xE0B0));
        // Middle rows at x=0 must be 100% solid (255) so there is zero gap with the previous cell.
        for y in 2..(ch - 2) {
            assert_eq!(buf[(y * cw) as usize], 255, "left edge gap at row {y}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_coretext_fallback_renders_missing_menlo_glyphs() {
        let mut buf = vec![0u8; 16 * 32];
        // U+279C (➜) or CJK '中' is handled via CoreText fallback on macOS
        assert!(macos_coretext_rasterize(&mut buf, 16, 32, 24, 28.0, '中'));
        assert!(buf.iter().any(|&b| b > 0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn loads_sfmono_nerd_font_successfully() {
        let set = load_font(&["SFMono Nerd Font".to_string()]);
        assert!(set.primary.lookup_glyph_index('A') > 0);
    }
}

