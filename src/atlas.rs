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
use std::sync::Arc;

/// Slot 0 is always blank (space / empty cell).
pub const BLANK_SLOT: u16 = 0;

pub struct FontSet {
    pub primary: fontdue::Font,
    pub fallbacks: Vec<fontdue::Font>,
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

    map: HashMap<u32, u16>,
    slot_cp: Vec<u32>,
    slot_used: Vec<u64>,
    frame: u64,
    evicted: bool,
    scratch: Vec<u8>,

    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

impl Atlas {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, fonts: Arc<FontSet>, px: f32) -> Self {
        let font = &fonts.primary;
        let lm = font
            .horizontal_line_metrics(px)
            .expect("font has no horizontal line metrics");
        let cell_w = font.metrics('M', px).advance_width.round().max(1.0) as u32;
        let cell_h = (lm.ascent - lm.descent + lm.line_gap).ceil().max(1.0) as u32;
        let baseline = (lm.ascent + lm.line_gap / 2.0).round() as i32;

        // 512×512 normally; only grow for huge font sizes so ASCII still fits.
        let mut size = 512u32;
        while (size / cell_w) * (size / cell_h) < 256 && size < 4096 {
            size *= 2;
        }
        let slots_per_row = size / cell_w;
        let n_slots = (slots_per_row * (size / cell_h)).min(u16::MAX as u32);

        let (texture, view) = Self::make_texture(device, size);

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
            map: HashMap::with_capacity(n_slots as usize),
            slot_cp: vec![0; n_slots as usize],
            slot_used: vec![0; n_slots as usize],
            frame: 1,
            evicted: false,
            scratch: vec![0; (cell_w * cell_h) as usize],
            texture,
            view,
        };
        // Clear slot 0 (texture contents start zeroed, but be explicit).
        atlas.scratch.fill(0);
        atlas.upload(queue, BLANK_SLOT);
        for c in 33u32..=126 {
            atlas.slot(queue, c);
        }
        atlas
    }

    /// Rebuild the atlas in place for a new pixel size (DPI or zoom change).
    pub fn set_px(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, px: f32) {
        *self = Atlas::new(device, queue, self.fonts.clone(), px);
    }

    fn make_texture(device: &wgpu::Device, size: u32) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
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

    /// Atlas slot for `cp`, rasterizing and uploading it on a cache miss.
    pub fn slot(&mut self, queue: &wgpu::Queue, cp: u32) -> u16 {
        if cp == 0 || cp == 32 {
            return BLANK_SLOT;
        }
        if let Some(&s) = self.map.get(&cp) {
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
            self.map.remove(&self.slot_cp[i]);
            self.evicted = true;
            i as u16
        };
        self.rasterize(cp);
        self.upload(queue, s);
        self.map.insert(cp, s);
        self.slot_cp[s as usize] = cp;
        self.slot_used[s as usize] = self.frame;
        s
    }

    fn rasterize(&mut self, cp: u32) {
        self.scratch.fill(0);
        let Some(ch) = char::from_u32(cp) else { return };

        // 1. Ghostty-style built-in cell-exact sprite glyphs:
        //    Powerline, Box Drawing, Block Elements, Geometric triangles, Braille.
        if draw_builtin_sprite(&mut self.scratch, self.cell_w, self.cell_h, cp) {
            return;
        }

        // 2. Primary font (only if the font actually contains a glyph for `ch`).
        if self.fonts.primary.lookup_glyph_index(ch) != 0 {
            rasterize_with_font(
                &mut self.scratch,
                self.cell_w as i32,
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
                    &mut self.scratch,
                    self.cell_w as i32,
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
        if draw_nerd_icon_fallback(&mut self.scratch, self.cell_w, self.cell_h, cp) {
            return;
        }

        // 5. macOS CoreText system font cascade (Apple Symbols, PingFang, Supplemental, etc.).
        #[cfg(target_os = "macos")]
        if macos_coretext_rasterize(
            &mut self.scratch,
            self.cell_w,
            self.cell_h,
            self.baseline,
            self.px,
            ch,
        ) {
            return;
        }
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
    match v {
        0 | 255 => v,
        _ => ((v as f32 / 255.0).powf(0.78) * 255.0 + 0.5) as u8,
    }
}

// ============================================================================
// Built-in Procedural Sprite Renderer (Ghostty-style `font/sprite`)
// ============================================================================

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
                if (bits & (1 << BIT_MAP[c][row])) != 0 {
                    if (x - cx).powi(2) + (y - cy).powi(2) <= r * r {
                        return true;
                    }
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
// Font Discovery (Primary + Nerd Font / Symbol Fallbacks)
// ============================================================================

/// Load the primary monospace font and any available symbol/Nerd Font fallbacks
/// (override primary with `LAPLACE_FONT=/path/to/font.ttf`).
pub fn load_font() -> Arc<FontSet> {
    let mut primary_candidates: Vec<String> = Vec::new();
    if let Ok(p) = std::env::var("LAPLACE_FONT") {
        primary_candidates.push(p);
    }

    let (discovered_mono_nerd, discovered_symbols) = discover_nerd_fonts();
    // If the user has a full monospace Nerd Font installed, prefer it automatically.
    primary_candidates.extend(discovered_mono_nerd);

    #[cfg(target_os = "macos")]
    primary_candidates.extend(
        [
            "/System/Library/Fonts/Menlo.ttc",
            "/System/Library/Fonts/SFNSMono.ttf",
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

    let mut primary = None;
    for path in &primary_candidates {
        if let Some(f) = try_parse_font(path) {
            primary = Some(f);
            break;
        }
    }
    let primary = primary.expect("laplace: no usable monospace font found; set LAPLACE_FONT=/path/to/font.ttf");

    let mut fallbacks = Vec::new();
    for path in &discovered_symbols {
        if let Some(f) = try_parse_font(path) {
            fallbacks.push(f);
        }
    }

    Arc::new(FontSet { primary, fallbacks })
}

fn try_parse_font(path: &str) -> Option<fontdue::Font> {
    let bytes = std::fs::read(path).ok()?;
    let settings = fontdue::FontSettings { scale: 32.0, ..Default::default() };
    match fontdue::Font::from_bytes(bytes, settings) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("laplace: failed to parse font {path}: {e}");
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
}

