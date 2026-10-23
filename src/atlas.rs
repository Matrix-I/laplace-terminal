//! Glyph atlas: `fontdue` rasterizes glyphs into fixed, cell-sized slots of a
//! single 512×512 `R8Unorm` texture (256 KB of VRAM). Printable ASCII is baked
//! at startup; every other codepoint is rasterized on first use. When the atlas
//! is full, the least-recently-used slot that is not on screen gets evicted.

use std::collections::HashMap;
use std::sync::Arc;

/// Slot 0 is always blank (space / empty cell).
pub const BLANK_SLOT: u16 = 0;

pub struct Atlas {
    font: Arc<fontdue::Font>,
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
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, font: Arc<fontdue::Font>, px: f32) -> Self {
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
            font,
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
        *self = Atlas::new(device, queue, self.font.clone(), px);
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
        let ch = char::from_u32(cp).unwrap_or('?');
        let (m, bmp) = self.font.rasterize(ch, self.px);
        let (cw, chh) = (self.cell_w as i32, self.cell_h as i32);
        let x0 = m.xmin;
        let y0 = self.baseline - (m.ymin + m.height as i32);
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
                let v = bmp[(gy * m.width as i32 + gx) as usize];
                let d = &mut self.scratch[(y * cw + x) as usize];
                *d = (*d).max(v);
            }
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

/// Load the first available monospace font (override with `LAPLACE_FONT=/path/to/font.ttf`).
pub fn load_font() -> Arc<fontdue::Font> {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(p) = std::env::var("LAPLACE_FONT") {
        candidates.push(p);
    }
    #[cfg(target_os = "macos")]
    candidates.extend(
        ["/System/Library/Fonts/Menlo.ttc", "/System/Library/Fonts/Monaco.ttf", "/System/Library/Fonts/SFNSMono.ttf"]
            .map(String::from),
    );
    #[cfg(not(target_os = "macos"))]
    candidates.extend(
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
    for path in &candidates {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let settings = fontdue::FontSettings { scale: 32.0, ..Default::default() };
        match fontdue::Font::from_bytes(bytes, settings) {
            Ok(f) => return Arc::new(f),
            Err(e) => eprintln!("laplace: failed to parse font {path}: {e}"),
        }
    }
    panic!("laplace: no usable monospace font found; set LAPLACE_FONT=/path/to/font.ttf");
}
