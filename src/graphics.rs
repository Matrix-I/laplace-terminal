//! Inline images: the Kitty graphics protocol (`ESC _ G ... ESC \`) and Sixel
//! (`ESC P q ... ESC \`). Images are decoded to RGBA and kept per terminal;
//! placements are anchored to absolute lines, so they scroll with the text.
//! The renderer uploads each image once and draws its placements on top of
//! the cells.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Decoded images kept per terminal, in bytes of RGBA; the oldest go first.
const STORE_LIMIT: usize = 64 << 20;
/// Largest image accepted, per side.
const MAX_SIDE: u32 = 8192;
/// Maximum number of additional animation frames per image.
const MAX_ANIMATION_FRAMES: usize = 256;
/// Maximum bytes allocated to a single animated image.
const MAX_ANIMATION_BYTES: usize = STORE_LIMIT / 2;

/// Process-wide image keys, so the renderer's texture cache never confuses
/// images of different terminals.
fn next_key() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub struct Image {
    /// The root frame (frame 1).
    pub key: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
    /// Kitty animation frames 2.. (`a=f`) and playback state (`a=a`).
    frames: Vec<Frame>,
    anim: Animation,
}

struct Frame {
    key: u64,
    rgba: Arc<[u8]>,
    /// Milliseconds before the next frame; 0 or less = skipped (gapless).
    gap: i32,
}

/// Kitty's default gap of a new frame.
const DEFAULT_GAP: i32 = 40;

/// Set once any terminal starts an animation, so the event loop only looks
/// for frame deadlines after that.
static ANIMATIONS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn any_animation() -> bool {
    ANIMATIONS.load(Ordering::Relaxed)
}

/// Playback is derived from the clock (no timer ticks): frame `i` shows while
/// the time since `started`, modulo the cycle, falls into its gap.
#[derive(Default)]
struct Animation {
    /// 1 = stopped, 2 = loading (play once, then wait), 3 = running.
    state: u8,
    /// Frame shown while stopped (0-based).
    current: usize,
    /// Start of the timeline (frame 0), while playing.
    started: Option<Instant>,
    /// Plays of the whole cycle, 0 = forever.
    plays: u32,
    root_gap: i32,
}

impl Image {
    fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        let anim = Animation { root_gap: DEFAULT_GAP, ..Animation::default() };
        Image { key: next_key(), width, height, rgba: rgba.into(), frames: Vec::new(), anim }
    }

    fn frame_count(&self) -> usize {
        1 + self.frames.len()
    }

    fn frame(&self, i: usize) -> (u64, &Arc<[u8]>) {
        match i.checked_sub(1).and_then(|j| self.frames.get(j)) {
            Some(f) => (f.key, &f.rgba),
            None => (self.key, &self.rgba),
        }
    }

    fn gap(&self, i: usize) -> u64 {
        let gap = if i == 0 { self.anim.root_gap } else { self.frames.get(i - 1).map_or(0, |f| f.gap) };
        gap.max(0) as u64
    }

    fn bytes(&self) -> usize {
        self.rgba.len() + self.frames.iter().map(|f| f.rgba.len()).sum::<usize>()
    }

    /// Start of frame `i` on the timeline, in ms.
    fn offset(&self, i: usize) -> u64 {
        (0..i).map(|j| self.gap(j)).sum()
    }

    /// The frame shown at `now` and when that changes (`None`: it does not).
    fn frame_at(&self, now: Instant) -> (usize, Option<Instant>) {
        let a = &self.anim;
        let n = self.frame_count();
        let cycle = self.offset(n);
        let (Some(started), true, true) = (a.started, n > 1, cycle > 0) else { return (a.current.min(n - 1), None) };
        let plays = if a.state == 2 { 1 } else { a.plays };
        let elapsed = now.saturating_duration_since(started).as_millis() as u64;
        if plays != 0 && elapsed >= cycle * plays as u64 {
            return (n - 1, None);
        }
        let mut t = elapsed % cycle;
        for i in 0..n {
            let gap = self.gap(i);
            if t < gap {
                return (i, Some(now + Duration::from_millis(gap - t)));
            }
            t -= gap;
        }
        (n - 1, None)
    }

    /// Key and pixels of the frame to draw at `now`.
    pub fn shown(&self, now: Instant) -> (u64, &Arc<[u8]>) {
        if self.frames.is_empty() {
            return (self.key, &self.rgba);
        }
        self.frame(self.frame_at(now).0)
    }
}

/// One displayed copy of an image.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub image_id: u32,
    pub placement_id: u32,
    /// Absolute line and column of the top-left cell.
    pub line: usize,
    pub col: usize,
    /// Cells covered.
    pub cols: usize,
    pub rows: usize,
    /// Source rectangle in image pixels: x, y, width, height.
    pub src: [u32; 4],
    pub z: i32,
    /// Shown on the alternate screen (cleared when it is left).
    pub alt: bool,
    /// Stretch to `cols` x `rows` cells (given explicitly) instead of drawing
    /// at the image's own pixel size.
    pub fit: bool,
}

/// What the renderer needs to draw one placement.
pub struct ImageDraw {
    pub key: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
    /// Source rectangle in image pixels: x, y, width, height.
    pub src: [f32; 4],
    /// Top-left cell relative to the visible screen (may be negative: partly
    /// scrolled off the top).
    pub row: isize,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
    pub fit: bool,
    pub z: i32,
    /// Exact destination in px relative to the top-left cell (x, y, width,
    /// height), overriding `fit` (unicode placeholders cover parts of cells).
    pub px: Option<[f32; 4]>,
}

/// A Kitty virtual placement (`U=1`): drawn wherever the text holds U+10EEEE
/// placeholder cells naming its image (and placement) id.
#[derive(Clone, Debug, PartialEq)]
pub struct VirtualPlacement {
    pub image_id: u32,
    pub placement_id: u32,
    /// Size of the box the image is fitted into (keeping its aspect ratio), in cells.
    pub cols: usize,
    pub rows: usize,
    pub z: i32,
    pub alt: bool,
}

/// Combining marks that number the rows / columns of unicode placeholder
/// cells (index = value), from Kitty's `rowcolumn-diacritics.txt`.
pub const PLACEHOLDER_DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// Kitty: placements with a z-index below this are drawn under cells with a
/// non-default background too, not only under the text.
pub const Z_BELOW_BG: i32 = -1_073_741_824;

#[derive(Default)]
pub struct Store {
    images: HashMap<u32, Image>,
    /// Insertion order of image ids, for eviction.
    order: Vec<u32>,
    pub placements: Vec<Placement>,
    pub virtuals: Vec<VirtualPlacement>,
    bytes: usize,
    /// Ids handed out for images transmitted without one (sixel, kitty `I=`).
    next_auto_id: u32,
    /// A chunked Kitty transmission in progress (`m=1`), with last activity timestamp.
    pending: Option<(KittyCommand, Vec<u8>, Instant)>,
}

impl Store {
    pub fn clear(&mut self) {
        *self = Store { next_auto_id: self.next_auto_id, ..Store::default() };
    }

    /// Store an image under `id` (0 = pick one), replacing an older one.
    pub fn insert(&mut self, id: u32, width: u32, height: u32, rgba: Vec<u8>) -> u32 {
        let id = if id == 0 { self.auto_id() } else { id };
        self.remove_image(id, false);
        self.bytes += rgba.len();
        self.images.insert(id, Image::new(width, height, rgba));
        self.order.push(id);
        self.evict();
        id
    }

    /// Over the limit: evict the oldest images (and their placements).
    fn evict(&mut self) {
        while self.bytes > STORE_LIMIT && self.order.len() > 1 {
            let oldest = self.order[0];
            self.remove_image(oldest, true);
        }
    }

    /// When an animation shown in lines `top..top + rows` (or through
    /// placeholders) next changes frame.
    pub fn next_frame_at(&self, now: Instant, top: usize, rows: usize, alt: bool) -> Option<Instant> {
        let on_screen = |p: &Placement| p.alt == alt && p.line < top + rows && p.line + p.rows > top;
        let shown = |id: &u32| {
            self.placements.iter().any(|p| p.image_id == *id && on_screen(p)) || self.virtuals.iter().any(|v| v.image_id == *id)
        };
        self.images.iter().filter(|(id, img)| img.anim.started.is_some() && shown(id)).filter_map(|(_, img)| img.frame_at(now).1).min()
    }

    /// `a=f`: add (or with `r=`, edit) an animation frame. The frame data
    /// (`s`/`v` sized, default the whole image) goes at `x`,`y` over a copy
    /// of frame `c` (or a transparent canvas), blended unless `X=1`.
    fn kitty_frame(&mut self, cmd: &KittyCommand, data: &[u8]) -> Result<(), String> {
        let img = self.images.get(&cmd.id).ok_or("ENOENT:image not found")?;
        let (edit, base) = (cmd.rows as usize, cmd.cols as usize);
        let n = img.frame_count();
        if edit > n || base > n {
            return Err("EINVAL:no such frame".into());
        }
        let (iw, ih) = (img.width, img.height);
        if edit == 0 {
            if img.frames.len() >= MAX_ANIMATION_FRAMES {
                return Err("EBIG:too many animation frames".into());
            }
            let frame_bytes = (iw as usize).saturating_mul(ih as usize).saturating_mul(4);
            if img.bytes().saturating_add(frame_bytes) > MAX_ANIMATION_BYTES {
                return Err("EBIG:animation exceeds memory limit".into());
            }
        }
        let mut sized = cmd.clone();
        sized.width = if cmd.width == 0 { iw } else { cmd.width };
        sized.height = if cmd.height == 0 { ih } else { cmd.height };
        let (fw, fh, pixels) = decode_kitty(&sized, data)?;
        let mut canvas = match (edit, base) {
            (0, 0) => vec![0u8; (iw * ih * 4) as usize],
            (0, b) => img.frame(b - 1).1.to_vec(),
            (e, _) => img.frame(e - 1).1.to_vec(),
        };
        for y in 0..fh.min(ih.saturating_sub(cmd.y)) {
            for x in 0..fw.min(iw.saturating_sub(cmd.x)) {
                let src = &pixels[((y * fw + x) * 4) as usize..][..4];
                let dst = &mut canvas[(((cmd.y + y) * iw + cmd.x + x) * 4) as usize..][..4];
                compose_pixel(dst, src, cmd.replace);
            }
        }
        let len = canvas.len();
        let img = self.images.get_mut(&cmd.id).ok_or("ENOENT:image not found")?;
        let gap = if cmd.z != 0 { cmd.z } else { DEFAULT_GAP };
        match edit {
            0 => img.frames.push(Frame { key: next_key(), rgba: canvas.into(), gap }),
            1 => {
                img.key = next_key();
                img.rgba = canvas.into();
                if cmd.z != 0 {
                    img.anim.root_gap = cmd.z;
                }
                self.bytes -= len;
            }
            e => {
                let f = &mut img.frames[e - 2];
                *f = Frame { key: next_key(), rgba: canvas.into(), gap: if cmd.z != 0 { cmd.z } else { f.gap } };
                self.bytes -= len;
            }
        }
        self.bytes += len;
        self.evict();
        Ok(())
    }

    /// `a=c`: copy a `w` x `h` rectangle at `X`,`Y` of frame `r` to `x`,`y`
    /// of frame `c`, blended unless `C=1`.
    fn kitty_compose(&mut self, cmd: &KittyCommand) -> Result<(), String> {
        let img = self.images.get_mut(&cmd.id).ok_or("ENOENT:image not found")?;
        let (src, dst) = (cmd.rows as usize, cmd.cols as usize);
        let n = img.frame_count();
        if !(1..=n).contains(&src) || !(1..=n).contains(&dst) {
            return Err("ENOENT:no such frame".into());
        }
        let (iw, ih) = (img.width, img.height);
        let w = if cmd.src_w == 0 { iw } else { cmd.src_w };
        let h = if cmd.src_h == 0 { ih } else { cmd.src_h };
        if cmd.src_left + w > iw || cmd.src_top + h > ih || cmd.x + w > iw || cmd.y + h > ih {
            return Err("EINVAL:rectangle out of bounds".into());
        }
        let source = img.frame(src - 1).1.clone();
        let mut canvas = img.frame(dst - 1).1.to_vec();
        for y in 0..h {
            for x in 0..w {
                let s = &source[(((cmd.src_top + y) * iw + cmd.src_left + x) * 4) as usize..][..4];
                let d = &mut canvas[(((cmd.y + y) * iw + cmd.x + x) * 4) as usize..][..4];
                compose_pixel(d, s, cmd.overwrite);
            }
        }
        match dst {
            1 => {
                img.key = next_key();
                img.rgba = canvas.into();
            }
            d => {
                let f = &mut img.frames[d - 2];
                f.key = next_key();
                f.rgba = canvas.into();
            }
        }
        Ok(())
    }

    /// `a=a`: control playback. `s` = 1 stop / 2 loading / 3 run, `c` = frame
    /// to show, `r` + `z` = that frame's gap, `v` = loops (1 = forever).
    fn kitty_animate(&mut self, cmd: &KittyCommand, now: Instant) -> Result<(), String> {
        let img = self.images.get_mut(&cmd.id).ok_or("ENOENT:image not found")?;
        let n = img.frame_count();
        if cmd.rows > 0 && cmd.z != 0 {
            match cmd.rows as usize {
                1 => img.anim.root_gap = cmd.z,
                r if r <= n => img.frames[r - 2].gap = cmd.z,
                _ => return Err("EINVAL:no such frame".into()),
            }
        }
        if cmd.height > 0 {
            img.anim.plays = if cmd.height == 1 { 0 } else { cmd.height - 1 };
        }
        let mut current = img.frame_at(now).0;
        if cmd.cols > 0 {
            current = (cmd.cols as usize - 1).min(n - 1);
        }
        let state = if cmd.width > 0 { cmd.width as u8 } else { img.anim.state };
        img.anim.state = state;
        img.anim.current = current;
        // Playing: put frame `current` at `now` on the timeline.
        img.anim.started = matches!(state, 2 | 3).then(|| now - Duration::from_millis(img.offset(current)));
        if img.anim.started.is_some() {
            ANIMATIONS.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    fn auto_id(&mut self) -> u32 {
        loop {
            // Kitty reserves small ids for applications; count down from the top.
            self.next_auto_id = self.next_auto_id.wrapping_sub(1).max(1 << 24);
            if !self.images.contains_key(&self.next_auto_id) {
                return self.next_auto_id;
            }
        }
    }

    fn remove_image(&mut self, id: u32, with_placements: bool) {
        if let Some(img) = self.images.remove(&id) {
            self.bytes -= img.bytes();
            self.order.retain(|&i| i != id);
        }
        if with_placements {
            self.placements.retain(|p| p.image_id != id);
            self.virtuals.retain(|v| v.image_id != id);
        }
    }

    pub fn image(&self, id: u32) -> Option<&Image> {
        self.images.get(&id)
    }

    /// Add a placement, replacing one with the same image and placement id.
    pub fn place(&mut self, p: Placement) {
        if p.placement_id != 0 {
            self.placements.retain(|q| !(q.image_id == p.image_id && q.placement_id == p.placement_id));
        }
        self.placements.push(p);
    }

    /// Add a virtual placement, replacing one with the same image and placement id.
    pub fn place_virtual(&mut self, v: VirtualPlacement) {
        self.virtuals.retain(|q| !(q.image_id == v.image_id && q.placement_id == v.placement_id));
        self.virtuals.push(v);
    }

    /// The virtual placement placeholder cells with these ids refer to: the
    /// exact placement, else the newest one of the image.
    pub fn virtual_for(&self, image_id: u32, placement_id: u32) -> Option<&VirtualPlacement> {
        let mut of_image = self.virtuals.iter().rev().filter(|v| v.image_id == image_id);
        of_image.clone().find(|v| v.placement_id == placement_id).or_else(|| of_image.next())
    }

    /// Prune any auto-assigned images (`id >= 1 << 24`) that have no remaining
    /// regular or virtual placements.
    pub fn prune_dead_auto_ids(&mut self) {
        let dead: Vec<u32> = self
            .images
            .keys()
            .copied()
            .filter(|&id| id >= (1 << 24))
            .filter(|&id| !self.placements.iter().any(|p| p.image_id == id) && !self.virtuals.iter().any(|v| v.image_id == id))
            .collect();
        for id in dead {
            self.remove_image(id, false);
        }
    }

    /// Drop placements that scrolled out of the scrollback, and images no
    /// placement shows that were only ever meant for display.
    pub fn prune(&mut self, first_line: usize) {
        self.placements.retain(|p| p.alt || p.line + p.rows > first_line);
        self.prune_dead_auto_ids();
    }

    /// Placements visible in `top..top + rows` on the current screen.
    pub fn visible(&self, top: usize, rows: usize, alt: bool, now: Instant, out: &mut Vec<ImageDraw>) {
        for p in self.placements.iter().filter(|p| p.alt == alt) {
            if p.line >= top + rows || p.line + p.rows <= top {
                continue;
            }
            let Some(img) = self.images.get(&p.image_id) else { continue };
            let (key, rgba) = img.shown(now);
            out.push(ImageDraw {
                key,
                width: img.width,
                height: img.height,
                rgba: rgba.clone(),
                src: p.src.map(|v| v as f32),
                row: p.line as isize - top as isize,
                col: p.col,
                cols: p.cols,
                rows: p.rows,
                fit: p.fit,
                z: p.z,
                px: None,
            });
        }
    }

    /// Remove the placements covering any of `lines` (screen clears).
    pub fn delete_lines(&mut self, lines: std::ops::Range<usize>, alt: bool) {
        self.placements.retain(|p| p.alt != alt || p.line + p.rows <= lines.start || p.line >= lines.end);
        self.prune_dead_auto_ids();
    }
}

// ---------------------------------------------------------------------------
// Kitty graphics protocol
// ---------------------------------------------------------------------------

/// The `key=value` control data of a Kitty graphics command.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KittyCommand {
    pub action: u8,
    pub quiet: u8,
    pub format: u32,
    pub medium: u8,
    pub compression: u8,
    pub more: bool,
    pub id: u32,
    pub number: u32,
    pub placement_id: u32,
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    pub src_w: u32,
    pub src_h: u32,
    pub cols: u32,
    pub rows: u32,
    pub z: i32,
    pub no_cursor_move: bool,
    /// `U=1`: a virtual placement shown through unicode placeholders.
    pub unicode: bool,
    /// `X=1` (`a=f`): replace the frame's pixels instead of blending over them.
    pub replace: bool,
    /// `X=` / `Y=` (`a=c`): top-left of the source rectangle; `C=1`: overwrite.
    pub src_left: u32,
    pub src_top: u32,
    pub overwrite: bool,
    /// `S=` / `O=`: bytes to read and where to start, for file and shared memory.
    pub size: u32,
    pub offset: u32,
    /// `d=` delete selector.
    pub delete: u8,
}

/// Parse `G<control>;<payload>` (the APC body after `ESC _`).
pub fn parse_kitty(body: &[u8]) -> Option<(KittyCommand, &[u8])> {
    let body = body.strip_prefix(b"G")?;
    let (control, payload) = match body.iter().position(|&b| b == b';') {
        Some(i) => (&body[..i], &body[i + 1..]),
        None => (body, &b""[..]),
    };
    let mut c = KittyCommand { action: b't', format: 32, medium: b'd', ..Default::default() };
    for kv in control.split(|&b| b == b',') {
        let Some(eq) = kv.iter().position(|&b| b == b'=') else { continue };
        let (k, v) = (&kv[..eq], &kv[eq + 1..]);
        let num = || std::str::from_utf8(v).ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        let ch = v.first().copied().unwrap_or(0);
        match k {
            b"a" => c.action = ch,
            b"q" => c.quiet = num() as u8,
            b"f" => c.format = num(),
            b"t" => c.medium = ch,
            b"o" => c.compression = ch,
            b"m" => c.more = num() == 1,
            b"i" => c.id = num(),
            b"I" => c.number = num(),
            b"p" => c.placement_id = num(),
            b"s" => c.width = num(),
            b"v" => c.height = num(),
            b"x" => c.x = num(),
            b"y" => c.y = num(),
            b"w" => c.src_w = num(),
            b"h" => c.src_h = num(),
            b"c" => c.cols = num(),
            b"r" => c.rows = num(),
            b"z" => c.z = std::str::from_utf8(v).ok().and_then(|s| s.parse().ok()).unwrap_or(0),
            // `C=1`: no cursor movement when placing; overwrite when composing (`a=c`).
            b"C" => {
                c.no_cursor_move = num() == 1;
                c.overwrite = c.no_cursor_move;
            }
            b"U" => c.unicode = num() == 1,
            b"X" => {
                c.replace = num() == 1;
                c.src_left = num();
            }
            b"Y" => c.src_top = num(),
            b"S" => c.size = num(),
            b"O" => c.offset = num(),
            b"d" => c.delete = ch,
            _ => {}
        }
    }
    Some((c, payload))
}

/// What the terminal must do after a Kitty command.
pub enum KittyOutcome {
    /// Nothing visible (transmit only, delete, chunk in progress).
    None,
    /// Show image `id` at the cursor.
    Place { id: u32 },
}

impl Store {
    /// Run a Kitty command. Returns the outcome and the reply body (`OK` or an
    /// error such as `ENOENT:...`), already filtered by `q=`.
    pub fn kitty(&mut self, cmd: KittyCommand, payload: &[u8]) -> (KittyOutcome, Option<String>) {
        let reply = |c: &KittyCommand, msg: &str| -> Option<String> {
            let ok = msg == "OK";
            if (c.id == 0 && c.number == 0) || (ok && c.quiet >= 1) || c.quiet >= 2 {
                return None;
            }
            let mut r = format!("i={}", c.id);
            if c.number != 0 {
                r.push_str(&format!(",I={}", c.number));
            }
            if c.placement_id != 0 {
                r.push_str(&format!(",p={}", c.placement_id));
            }
            Some(format!("{r};{msg}"))
        };
        // Abandoned chunked transmission timeout: free buffers if sender stalled > 5s.
        const CHUNKED_TIMEOUT: Duration = Duration::from_secs(5);
        let now = Instant::now();
        if let Some((_, _, last_active)) = self.pending {
            if now.duration_since(last_active) > CHUNKED_TIMEOUT {
                self.pending = None;
            }
        }

        // Chunked transmissions: collect until `m=0`; only the first chunk has the keys.
        let (cmd, data) = match self.pending.take() {
            Some((first, mut data, _)) if matches!(first.action, b't' | b'T' | b'q' | b'f') => {
                data.extend_from_slice(payload);
                if data.len() > STORE_LIMIT {
                    return (KittyOutcome::None, reply(&first, "EBIG:chunked transmission exceeds size limit"));
                }
                if cmd.more {
                    self.pending = Some((first, data, now));
                    return (KittyOutcome::None, None);
                }
                (first, data)
            }
            _ if cmd.more && matches!(cmd.action, b't' | b'T' | b'q' | b'f') => {
                if payload.len() > STORE_LIMIT {
                    return (KittyOutcome::None, reply(&cmd, "EBIG:chunked transmission exceeds size limit"));
                }
                self.pending = Some((cmd, payload.to_vec(), now));
                return (KittyOutcome::None, None);
            }
            _ => (cmd, payload.to_vec()),
        };
        match cmd.action {
            b't' | b'T' | b'q' => match decode_kitty(&cmd, &data) {
                Ok((w, h, rgba)) => {
                    if cmd.action == b'q' {
                        return (KittyOutcome::None, reply(&cmd, "OK"));
                    }
                    let id = self.insert(cmd.id, w, h, rgba);
                    let out = if cmd.action == b'T' { KittyOutcome::Place { id } } else { KittyOutcome::None };
                    (out, reply(&cmd, "OK"))
                }
                Err(e) => (KittyOutcome::None, reply(&cmd, &e)),
            },
            b'p' => {
                if self.images.contains_key(&cmd.id) {
                    (KittyOutcome::Place { id: cmd.id }, reply(&cmd, "OK"))
                } else {
                    (KittyOutcome::None, reply(&cmd, "ENOENT:image not found"))
                }
            }
            b'd' => {
                self.kitty_delete(&cmd);
                (KittyOutcome::None, None)
            }
            b'f' => match self.kitty_frame(&cmd, &data) {
                Ok(()) => (KittyOutcome::None, reply(&cmd, "OK")),
                Err(e) => (KittyOutcome::None, reply(&cmd, &e)),
            },
            b'c' => match self.kitty_compose(&cmd) {
                Ok(()) => (KittyOutcome::None, reply(&cmd, "OK")),
                Err(e) => (KittyOutcome::None, reply(&cmd, &e)),
            },
            b'a' => match self.kitty_animate(&cmd, Instant::now()) {
                Ok(()) => (KittyOutcome::None, reply(&cmd, "OK")),
                Err(e) => (KittyOutcome::None, reply(&cmd, &e)),
            },
            _ => (KittyOutcome::None, reply(&cmd, "EINVAL:unsupported action")),
        }
    }

    /// `a=d`: lowercase keeps the image data, uppercase frees it too.
    fn kitty_delete(&mut self, cmd: &KittyCommand) {
        let free = cmd.delete.is_ascii_uppercase();
        match cmd.delete.to_ascii_lowercase() {
            b'i' => {
                let pid = cmd.placement_id;
                self.placements.retain(|p| !(p.image_id == cmd.id && (pid == 0 || p.placement_id == pid)));
                self.virtuals.retain(|v| !(v.image_id == cmd.id && (pid == 0 || v.placement_id == pid)));
                if free {
                    self.remove_image(cmd.id, true);
                }
            }
            // `a`/`A` (the default) and every selector Litty does not track
            // individually: all placements.
            _ => {
                self.placements.clear();
                self.virtuals.clear();
                if free {
                    let ids: Vec<u32> = self.images.keys().copied().collect();
                    for id in ids {
                        self.remove_image(id, true);
                    }
                }
            }
        }
    }
}

fn is_temp_dir(dir: &std::path::Path) -> bool {
    #[allow(unused_mut)]
    let mut temp_dirs = vec![
        std::env::temp_dir(),
        std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from("/var/tmp"),
    ];
    #[cfg(target_os = "macos")]
    {
        temp_dirs.push(std::path::PathBuf::from("/private/tmp"));
        temp_dirs.push(std::path::PathBuf::from("/private/var/tmp"));
    }
    let Ok(canonical_dir) = dir.canonicalize() else {
        return false;
    };
    temp_dirs.iter().any(|td| {
        td.canonicalize().is_ok_and(|ctd| canonical_dir.starts_with(&ctd))
    })
}

fn is_sensitive_path(path: &std::path::Path) -> bool {
    let path_str = path.to_string_lossy();
    for c in path.components() {
        let name = c.as_os_str().to_string_lossy();
        if name.starts_with('.') && name != "." && name != ".." {
            return true;
        }
    }
    path_str.starts_with("/etc")
        || path_str.starts_with("/private/etc")
        || path_str.starts_with("/dev")
        || path_str.starts_with("/proc")
        || path_str.starts_with("/sys")
        || path_str.starts_with("/var/root")
        || path_str.starts_with("/root")
}

/// Decode a transmission into `(width, height, rgba)`.
fn decode_kitty(c: &KittyCommand, payload: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let raw = base64_decode(payload).ok_or("EINVAL:bad base64")?;
    let data = match c.medium {
        b'd' => raw,
        b'f' | b't' => {
            let path_str = String::from_utf8(raw).map_err(|_| "EINVAL:bad path")?;
            let path = std::path::Path::new(&path_str);
            let canonical = path.canonicalize().map_err(|_| "ENOENT:file not found")?;

            if c.medium == b't' {
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let parent = path.parent();
                let canonical_parent = canonical.parent();
                if !file_name.contains("tty-graphics-protocol")
                    || !parent.is_some_and(is_temp_dir)
                    || !canonical_parent.is_some_and(is_temp_dir)
                {
                    return Err("EPERM:temporary file must be in temporary directory".into());
                }
            } else if is_sensitive_path(&canonical) {
                return Err("EPERM:access denied".into());
            }

            let meta = std::fs::metadata(&canonical).map_err(|_| "ENOENT:file not found")?;
            if !meta.is_file() || meta.len() > STORE_LIMIT as u64 {
                return Err("EINVAL:not a regular file".into());
            }
            let file = std::fs::File::open(&canonical).map_err(|_| "ENOENT:cannot read file")?;
            let data = read_range(file, c.offset, c.size)?;
            // Temporary files are deleted once read, but only verified temp files.
            if c.medium == b't' {
                let _ = std::fs::remove_file(&canonical);
            }
            data
        }
        b's' => {
            // POSIX shared memory, unlinked once read (as kitty does).
            let name_str = String::from_utf8(raw.clone()).map_err(|_| "EINVAL:bad name")?;
            let valid_prefix = name_str.starts_with("/kitty-")
                || name_str.starts_with("/litty-")
                || name_str.starts_with("/ghostty-")
                || name_str.starts_with("/shm-");
            if !valid_prefix {
                return Err("EPERM:unauthorized shared memory prefix".into());
            }

            let name = std::ffi::CString::new(raw).map_err(|_| "EINVAL:bad name")?;
            // SAFETY: a NUL-terminated name; the descriptor is owned by `file`.
            let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
            if fd < 0 {
                return Err("ENOENT:shared memory not found".into());
            }
            let file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
            // Mapped, since macOS cannot read() shared memory objects.
            // SAFETY: a read-only private view, dropped before the object is unlinked.
            let data = unsafe { memmap2::Mmap::map(&file) }.map_err(|_| "EIO:cannot map shared memory".to_string()).and_then(|map| {
                let start = (c.offset as usize).min(map.len());
                let end = if c.size == 0 { map.len() } else { (start + c.size as usize).min(map.len()) };
                if end - start > STORE_LIMIT {
                    return Err("EINVAL:too large".into());
                }
                Ok(map[start..end].to_vec())
            });
            unsafe { libc::shm_unlink(name.as_ptr()) };
            data?
        }
        _ => return Err("EINVAL:unsupported transmission medium".into()),
    };
    let data = if c.compression == b'z' {
        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data, STORE_LIMIT).map_err(|_| "EINVAL:bad zlib data")?
    } else {
        data
    };
    match c.format {
        100 => decode_png(&data),
        24 | 32 => {
            let (w, h) = (c.width, c.height);
            let bpp = if c.format == 24 { 3 } else { 4 };
            if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || data.len() < (w * h * bpp) as usize {
                return Err("EINVAL:bad image size".into());
            }
            let rgba = if bpp == 4 {
                data[..(w * h * 4) as usize].to_vec()
            } else {
                data[..(w * h * 3) as usize].as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect()
            };
            Ok((w, h, rgba))
        }
        _ => Err("EINVAL:unsupported format".into()),
    }
}

/// Put RGBA `src` on `dst`: a copy, or source-over blending (straight alpha).
fn compose_pixel(dst: &mut [u8], src: &[u8], replace: bool) {
    if replace {
        dst.copy_from_slice(src);
        return;
    }
    let (sa, da) = (src[3] as u32, dst[3] as u32);
    let out_a = sa + da * (255 - sa) / 255;
    for k in 0..3 {
        if let Some(c) = (src[k] as u32 * sa + dst[k] as u32 * da * (255 - sa) / 255).checked_div(out_a) {
            dst[k] = c.min(255) as u8;
        }
    }
    dst[3] = out_a as u8;
}

/// Read `size` bytes (0 = to the end) from `offset`, within the store limit.
fn read_range(file: std::fs::File, offset: u32, size: u32) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = file;
    file.seek(SeekFrom::Start(offset as u64)).map_err(|_| "EINVAL:bad offset")?;
    let limit = if size == 0 { STORE_LIMIT as u64 } else { (size as u64).min(STORE_LIMIT as u64) };
    let mut data = Vec::new();
    file.take(limit).read_to_end(&mut data).map_err(|_| "EIO:cannot read")?;
    Ok(data)
}

pub fn decode_png(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|_| "EINVAL:bad PNG")?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err("EINVAL:bad image size".into());
    }
    let buf_size = reader.output_buffer_size().ok_or("EINVAL:bad PNG")?;
    if buf_size > STORE_LIMIT || (w as usize).saturating_mul(h as usize).saturating_mul(4) > STORE_LIMIT {
        return Err("EINVAL:image too large".into());
    }
    let mut buf = vec![0; buf_size];
    let info = reader.next_frame(&mut buf).map_err(|_| "EINVAL:bad PNG")?;
    let buf = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf.to_vec(),
        png::ColorType::Rgb => buf.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.as_chunks::<2>().0.iter().flat_map(|&[g, a]| [g, g, g, a]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("EINVAL:bad PNG".into()),
    };
    Ok((w, h, rgba))
}

/// Standard base64 (padding optional, whitespace ignored).
fn base64_decode(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &b in s {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
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

// ---------------------------------------------------------------------------
// Sixel
// ---------------------------------------------------------------------------

/// VT340 default colors for the first 16 registers.
const SIXEL_PALETTE: [u32; 16] = [
    0x000000, 0x3333CC, 0xCC2121, 0x33CC33, 0xCC33CC, 0x33CCCC, 0xCCCC33, 0x878787, 0x424242, 0x545499, 0x994242,
    0x549954, 0x995499, 0x549999, 0x999954, 0xCCCCCC,
];

/// Decode sixel data (the DCS body after `q`). `transparent` is DCS P2 = 1:
/// pixels never drawn stay transparent instead of taking color 0.
pub fn decode_sixel(data: &[u8], transparent: bool) -> Option<(u32, u32, Vec<u8>)> {
    let mut palette = [0u32; 256];
    palette[..16].copy_from_slice(&SIXEL_PALETTE);
    // Pixels as palette color + 1 (0 = never drawn), grown as needed.
    let (mut w, mut h) = (0usize, 0usize);
    let (mut max_w, mut max_h) = (0usize, 0usize);
    let mut px: Vec<u32> = Vec::new();
    let (mut x, mut band) = (0usize, 0usize);
    let mut color = 0usize;
    let mut i = 0;
    let num = |i: &mut usize| -> u32 {
        let mut n = 0u32;
        while *i < data.len() && data[*i].is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((data[*i] - b'0') as u32);
            *i += 1;
        }
        n
    };
    let params = |i: &mut usize| -> [u32; 5] {
        let mut p = [0u32; 5];
        for (k, slot) in p.iter_mut().enumerate() {
            *slot = num(i);
            if k < 4 && *i < data.len() && data[*i] == b';' {
                *i += 1;
            } else {
                break;
            }
        }
        p
    };
    let mut canvas = SixelCanvas {
        px: &mut px,
        w: &mut w,
        h: &mut h,
        max_w: &mut max_w,
        max_h: &mut max_h,
    };
    while i < data.len() {
        let b = data[i];
        match b {
            b'"' => {
                i += 1;
                let p = params(&mut i);
                // Raster attributes: aspect, then the size hint Ph x Pv.
                let (pw, ph) = (p[2] as usize, p[3] as usize);
                if pw > 0 && ph > 0 && pw <= MAX_SIDE as usize && ph <= MAX_SIDE as usize && canvas.px.is_empty() {
                    *canvas.w = pw;
                    *canvas.h = ph;
                    *canvas.max_w = pw;
                    *canvas.max_h = ph;
                    *canvas.px = vec![0; pw * ph];
                }
            }
            b'#' => {
                i += 1;
                let p = params(&mut i);
                color = (p[0] as usize).min(255);
                if p[1] == 2 {
                    let pc = |v: u32| (v.min(100) * 255 / 100) & 0xFF;
                    palette[color] = (pc(p[2]) << 16) | (pc(p[3]) << 8) | pc(p[4]);
                } else if p[1] == 1 {
                    palette[color] = hls_to_rgb(p[2], p[3], p[4]);
                }
            }
            b'!' => {
                i += 1;
                let n = num(&mut i).max(1) as usize;
                if i < data.len() && (0x3F..=0x7E).contains(&data[i]) {
                    let bits = data[i] - 0x3F;
                    i += 1;
                    for _ in 0..n.min(MAX_SIDE as usize) {
                        canvas.put(x, band, bits, color)?;
                        x += 1;
                    }
                }
            }
            b'$' => {
                x = 0;
                i += 1;
            }
            b'-' => {
                x = 0;
                band += 1;
                i += 1;
            }
            0x3F..=0x7E => {
                canvas.put(x, band, b - 0x3F, color)?;
                x += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    if max_w == 0 || max_h == 0 {
        return None;
    }
    let mut rgba = Vec::with_capacity(max_w * max_h * 4);
    for y in 0..max_h {
        for x in 0..max_w {
            match px[y * w + x] {
                0 if transparent => rgba.extend_from_slice(&[0, 0, 0, 0]),
                0 => rgba.extend_from_slice(&rgb_bytes(palette[0])),
                c => rgba.extend_from_slice(&rgb_bytes(palette[(c - 1) as usize])),
            }
        }
    }
    Some((max_w as u32, max_h as u32, rgba))
}

fn rgb_bytes(c: u32) -> [u8; 4] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8, 255]
}

struct SixelCanvas<'a> {
    px: &'a mut Vec<u32>,
    w: &'a mut usize,
    h: &'a mut usize,
    max_w: &'a mut usize,
    max_h: &'a mut usize,
}

impl SixelCanvas<'_> {
    /// Set the six pixels of one sixel, growing the canvas geometrically when needed.
    fn put(&mut self, x: usize, band: usize, bits: u8, color: usize) -> Option<()> {
        let need_w = x + 1;
        let need_h = band * 6 + 6;
        if need_w > MAX_SIDE as usize || need_h > MAX_SIDE as usize {
            return None;
        }
        if need_w > *self.w || need_h > *self.h {
            let nw = need_w.max(*self.w).max(64).next_power_of_two().min(MAX_SIDE as usize).max(need_w);
            let nh = need_h.max(*self.h).max(64).next_power_of_two().min(MAX_SIDE as usize).max(need_h);
            let mut grown = vec![0u32; nw * nh];
            for y in 0..*self.max_h {
                grown[y * nw..y * nw + *self.max_w].copy_from_slice(&self.px[y * *self.w..y * *self.w + *self.max_w]);
            }
            *self.px = grown;
            *self.w = nw;
            *self.h = nh;
        }
        *self.max_w = (*self.max_w).max(need_w);
        *self.max_h = (*self.max_h).max(need_h);
        for k in 0..6 {
            if bits & (1 << k) != 0 {
                self.px[(band * 6 + k) * *self.w + x] = color as u32 + 1;
            }
        }
        Some(())
    }
}

/// Sixel HLS (hue 0..360 with blue at 0, lightness and saturation 0..100).
fn hls_to_rgb(h: u32, l: u32, s: u32) -> u32 {
    let (h, l, s) = (((h + 240) % 360) as f32 / 360.0, l.min(100) as f32 / 100.0, s.min(100) as f32 / 100.0);
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |t: f32| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u32
    };
    (channel(h + 1.0 / 3.0) << 16) | (channel(h) << 8) | channel(h - 1.0 / 3.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        assert_eq!(base64_decode(b"aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode(b"aGVsbG8").unwrap(), b"hello");
        assert!(base64_decode(b"a$").is_none());
    }

    #[test]
    fn kitty_rgb_transmit_and_display() {
        let mut store = Store::default();
        // 2x1 RGB: red, blue.
        let (cmd, payload) = parse_kitty(b"Ga=T,f=24,s=2,v=1,i=7;/wAAAAD/").unwrap();
        let (out, reply) = store.kitty(cmd, payload);
        assert!(matches!(out, KittyOutcome::Place { id: 7 }));
        assert_eq!(reply.as_deref(), Some("i=7;OK"));
        let img = store.image(7).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(&img.rgba[..], [255, 0, 0, 255, 0, 0, 255, 255]);

        // Quiet transmissions and missing images.
        let (cmd, p) = parse_kitty(b"Ga=t,f=24,s=1,v=1,i=8,q=1;AAAA").unwrap();
        assert!(store.kitty(cmd, p).1.is_none());
        let (cmd, p) = parse_kitty(b"Ga=p,i=99").unwrap();
        assert_eq!(store.kitty(cmd, p).1.as_deref(), Some("i=99;ENOENT:image not found"));
    }

    fn run(store: &mut Store, cmd: &str) -> Option<String> {
        let (c, p) = parse_kitty(cmd.as_bytes().to_vec().leak()).unwrap();
        store.kitty(c, p).1
    }

    #[test]
    fn kitty_animation_frames_play_from_the_clock() {
        let mut store = Store::default();
        // 1x1 red root frame, then green (100 ms) and blue (50 ms) frames.
        run(&mut store, "Ga=t,f=24,s=1,v=1,i=5,q=2;/wAA");
        assert_eq!(run(&mut store, "Ga=f,f=24,i=5,z=100;AP8A").as_deref(), Some("i=5;OK"));
        run(&mut store, "Ga=f,f=24,i=5,z=50,q=2;AAD/");
        assert_eq!(run(&mut store, "Ga=f,f=24,i=5,r=9;AAD/").as_deref(), Some("i=5;EINVAL:no such frame"));
        let t0 = Instant::now();
        let (cmd, _) = parse_kitty(b"Ga=a,i=5,s=3,v=1").unwrap();
        store.kitty_animate(&cmd, t0).unwrap();
        let img = store.image(5).unwrap();
        let at = |ms: u64| img.frame_at(t0 + Duration::from_millis(ms));
        let rgb = |ms: u64| img.frame(at(ms).0).1[..3].to_vec();
        assert_eq!((at(30).0, rgb(30)), (0, vec![255, 0, 0]), "root frame for its 40 ms");
        assert_eq!((at(50).0, rgb(50)), (1, vec![0, 255, 0]));
        assert_eq!((at(150).0, rgb(150)), (2, vec![0, 0, 255]));
        assert_eq!(at(200).0, 0, "loops forever with v=1");
        assert_eq!(at(150).1, Some(t0 + Duration::from_millis(190)), "next change at the end of the cycle");

        // Two plays (v=3 = 2 loops), then it rests on the last frame.
        let (cmd, _) = parse_kitty(b"Ga=a,i=5,v=3,c=1").unwrap();
        store.kitty_animate(&cmd, t0).unwrap();
        let img = store.image(5).unwrap();
        assert_eq!(img.frame_at(t0 + Duration::from_millis(200)).0, 0, "second play");
        assert_eq!(img.frame_at(t0 + Duration::from_millis(400)), (2, None));

        // Stop shows a fixed frame.
        let (cmd, _) = parse_kitty(b"Ga=a,i=5,s=1,c=2").unwrap();
        store.kitty_animate(&cmd, t0).unwrap();
        assert_eq!(store.image(5).unwrap().frame_at(t0 + Duration::from_millis(999)), (1, None));
    }

    #[test]
    fn kitty_frame_composes_over_a_base_frame() {
        let mut store = Store::default();
        // 2x1 transparent root, frame 2 = root with a red pixel at x=1, blended.
        run(&mut store, "Ga=t,f=32,s=2,v=1,i=6,q=2;AAAAAAAAAAA=");
        run(&mut store, "Ga=f,f=24,s=1,v=1,x=1,i=6,c=1,q=2;/wAA");
        assert_eq!(&store.image(6).unwrap().frame(1).1[..], [0, 0, 0, 0, 255, 0, 0, 255]);

        // a=c: copy that red pixel to the left of the root frame, overwriting.
        assert_eq!(run(&mut store, "Ga=c,i=6,r=2,c=1,X=1,w=1,h=1,C=1").as_deref(), Some("i=6;OK"));
        assert_eq!(&store.image(6).unwrap().rgba[..], [255, 0, 0, 255, 0, 0, 0, 0]);
        assert_eq!(run(&mut store, "Ga=c,i=6,r=2,c=1,X=1,w=2,h=1").as_deref(), Some("i=6;EINVAL:rectangle out of bounds"));
    }

    #[test]
    fn kitty_shared_memory_transmission() {
        let name = format!("/litty-test-{}", std::process::id());
        let cname = std::ffi::CString::new(name.clone()).unwrap();
        // SAFETY: plain POSIX calls on a descriptor this test owns.
        unsafe {
            let fd = libc::shm_open(cname.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600);
            assert!(fd >= 0);
            assert_eq!(libc::ftruncate(fd, 4), 0);
            let map = libc::mmap(std::ptr::null_mut(), 4, libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            std::ptr::copy_nonoverlapping([1u8, 2, 3, 255].as_ptr(), map as *mut u8, 4);
            libc::munmap(map, 4);
            libc::close(fd);
        }
        let b64 = |s: &str| -> String {
            const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let b = s.as_bytes();
            let mut out = String::new();
            for c in b.chunks(3) {
                let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
                for i in 0..=c.len() {
                    out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                }
            }
            out
        };
        let mut store = Store::default();
        assert_eq!(run(&mut store, &format!("Ga=t,t=s,f=32,s=1,v=1,S=4,i=4;{}", b64(&name))).as_deref(), Some("i=4;OK"));
        assert_eq!(&store.image(4).unwrap().rgba[..], [1, 2, 3, 255]);
        // SAFETY: as above.
        let gone = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0) } < 0;
        assert!(gone, "unlinked after reading");
    }

    #[test]
    fn kitty_chunked_png_and_query() {
        // A 1x1 red PNG.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
        let (a, b) = png.split_at(40);
        let mut store = Store::default();
        let (c1, p1) = parse_kitty(format!("Ga=t,f=100,i=3,m=1;{a}").as_bytes().to_vec().leak()).unwrap();
        assert!(store.kitty(c1, p1).1.is_none(), "no reply before the last chunk");
        let (c2, p2) = parse_kitty(format!("Gm=0;{b}").as_bytes().to_vec().leak()).unwrap();
        assert_eq!(store.kitty(c2, p2).1.as_deref(), Some("i=3;OK"));
        assert_eq!(&store.image(3).unwrap().rgba[..], [255, 0, 0, 255]);

        let (q, p) = parse_kitty(b"Ga=q,i=31,s=1,v=1,f=24;AAAA").unwrap();
        assert_eq!(store.kitty(q, p).1.as_deref(), Some("i=31;OK"));
        assert!(store.image(31).is_none(), "a query stores nothing");
    }

    #[test]
    fn kitty_delete_and_limits() {
        let mut store = Store::default();
        let id = store.insert(5, 1, 1, vec![0; 4]);
        store.place(Placement { image_id: id, placement_id: 0, line: 0, col: 0, cols: 1, rows: 1, src: [0, 0, 1, 1], z: 0, alt: false, fit: false });
        let (d, p) = parse_kitty(b"Ga=d,d=i,i=5").unwrap();
        store.kitty(d, p);
        assert!(store.placements.is_empty() && store.image(5).is_some(), "lowercase keeps the data");
        let (d, p) = parse_kitty(b"Ga=d,d=A").unwrap();
        store.kitty(d, p);
        assert!(store.images.is_empty());
    }

    #[test]
    fn sixel_decodes_bands_repeats_and_colors() {
        // Color 1 = pure red (RGB percent); one full sixel column, then `!3` of
        // the top pixel only, next band one pixel.
        let (w, h, rgba) = decode_sixel(b"#1;2;100;0;0#1~!3@-@", true).unwrap();
        assert!(w >= 4 && h == 12);
        let at = |x: usize, y: usize| &rgba[(y * w as usize + x) * 4..(y * w as usize + x) * 4 + 4];
        assert_eq!(at(0, 0), [255, 0, 0, 255]);
        assert_eq!(at(0, 5), [255, 0, 0, 255]);
        assert_eq!(at(3, 0), [255, 0, 0, 255]);
        assert_eq!(at(3, 1), [0, 0, 0, 0], "transparent background");
        assert_eq!(at(0, 6), [255, 0, 0, 255]);
        assert!(decode_sixel(b"", false).is_none());
        // Raster attributes size the image up front.
        let (w, h, _) = decode_sixel(b"\"1;1;8;6#0~", false).unwrap();
        assert_eq!((w, h), (8, 6));
    }

    #[test]
    fn kitty_file_transmission_security() {
        let b64 = |s: &str| -> String {
            const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let b = s.as_bytes();
            let mut out = String::new();
            for c in b.chunks(3) {
                let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
                for i in 0..=c.len() {
                    out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                }
            }
            out
        };

        let mut store = Store::default();

        // 1. Temporary file (t=t) outside temp dir must be rejected with EPERM
        let fake_temp = "/etc/tty-graphics-protocol-test";
        let res = run(&mut store, &format!("Ga=t,t=t,f=24,s=1,v=1,i=10;{}", b64(fake_temp)));
        assert!(res.as_deref().unwrap_or("").contains("EPERM") || res.as_deref().unwrap_or("").contains("ENOENT"));

        // 2. Sensitive paths (t=f) like /etc/passwd or .ssh or hidden dotfiles must be rejected
        let res = run(&mut store, &format!("Ga=t,t=f,f=24,s=1,v=1,i=11;{}", b64("/etc/passwd")));
        assert_eq!(res.as_deref(), Some("i=11;EPERM:access denied"));
        let res_hidden = run(&mut store, &format!("Ga=t,t=f,f=24,s=1,v=1,i=13;{}", b64("/tmp/.env")));
        assert!(res_hidden.as_deref().unwrap_or("").contains("EPERM") || res_hidden.as_deref().unwrap_or("").contains("ENOENT"));

        // 3. Unauthorized shared memory prefix must be rejected
        let res_shm = run(&mut store, &format!("Ga=t,t=s,f=24,s=1,v=1,i=14;{}", b64("/arbitrary_shm_seg")));
        assert_eq!(res_shm.as_deref(), Some("i=14;EPERM:unauthorized shared memory prefix"));

        // 4. Temporary file inside temp dir with tty-graphics-protocol in name is allowed and unlinked
        let temp_path = std::env::temp_dir().join("tty-graphics-protocol-unittest.raw");
        std::fs::write(&temp_path, [255u8, 0, 0]).expect("write temp file");
        let path_str = temp_path.to_str().unwrap();
        let res = run(&mut store, &format!("Ga=t,t=t,f=24,s=1,v=1,i=12;{}", b64(path_str)));
        assert_eq!(res.as_deref(), Some("i=12;OK"));
        assert!(!temp_path.exists(), "temp file must be deleted after reading");
    }

    #[test]
    fn kitty_chunked_transmission_size_limit() {
        let mut store = Store::default();
        let payload = vec![b'A'; 100];
        // Start a chunked transmission
        let (outcome, reply) = store.kitty(KittyCommand { action: b't', more: true, id: 99, ..KittyCommand::default() }, &payload);
        assert!(matches!(outcome, KittyOutcome::None));
        assert!(reply.is_none());
        assert!(store.pending.is_some());

        // Oversized chunk exceeding STORE_LIMIT
        let big_chunk = vec![b'A'; STORE_LIMIT + 1];
        let (outcome, reply) = store.kitty(KittyCommand { action: b't', more: true, ..KittyCommand::default() }, &big_chunk);
        assert!(matches!(outcome, KittyOutcome::None));
        assert_eq!(reply.as_deref(), Some("i=99;EBIG:chunked transmission exceeds size limit"));
        assert!(store.pending.is_none(), "pending must be cleared on overflow");
    }

    #[test]
    fn kitty_chunked_transmission_abandoned_timeout() {
        let mut store = Store::default();
        let payload = vec![b'A'; 100];
        let _ = store.kitty(KittyCommand { action: b't', more: true, id: 101, ..KittyCommand::default() }, &payload);
        assert!(store.pending.is_some());

        // Artificially age the pending buffer past CHUNKED_TIMEOUT (5 seconds)
        if let Some((_, _, ref mut started)) = store.pending {
            *started = Instant::now() - Duration::from_secs(6);
        }

        // Next kitty command cleans up abandoned pending buffer
        let _ = store.kitty(KittyCommand { action: b'd', ..KittyCommand::default() }, &[]);
        assert!(store.pending.is_none(), "stale pending buffer must be dropped");
    }

    #[test]
    fn auto_id_image_pruning() {
        let mut store = Store::default();
        let id = store.insert(0, 10, 10, vec![0; 400]);
        assert!(id >= (1 << 24), "auto id starts high");
        store.placements.push(Placement {
            image_id: id,
            placement_id: 1,
            line: 5,
            col: 0,
            cols: 2,
            rows: 2,
            src: [0, 0, 10, 10],
            z: 0,
            alt: false,
            fit: false,
        });
        assert!(store.image(id).is_some());

        // Prune before line 5: still kept
        store.prune(4);
        assert!(store.image(id).is_some());
        assert_eq!(store.placements.len(), 1);

        // Prune past line 5 + 2: placement removed and image freed
        store.prune(7);
        assert!(store.placements.is_empty());
        assert!(store.image(id).is_none(), "auto-assigned image must be removed when placements are gone");
    }

    #[test]
    fn decode_png_bounds_check() {
        // Build a minimal PNG header that declares dimensions exceeding STORE_LIMIT
        // 8192 x 8192 x 4 = 256MB > STORE_LIMIT (64MB)
        let mut png_data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_data, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0, 0, 0, 255]).unwrap();
        }
        // Patch width and height in IHDR (offsets 16..20 and 20..24) to 8192
        png_data[16..20].copy_from_slice(&8192u32.to_be_bytes());
        png_data[20..24].copy_from_slice(&8192u32.to_be_bytes());
        // Recompute IHDR CRC (chunk type + data: bytes 12..29)
        let mut crc = 0xffff_ffffu32;
        for &b in &png_data[12..29] {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        let crc = !crc;
        png_data[29..33].copy_from_slice(&crc.to_be_bytes());

        let res = decode_png(&png_data);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "EINVAL:image too large");
    }

    #[test]
    fn kitty_animation_frame_limits() {
        let mut store = Store::default();
        let id = store.insert(1, 10, 10, vec![0; 400]);

        // Frame count cap: fill up to MAX_ANIMATION_FRAMES
        let img = store.images.get_mut(&id).unwrap();
        for _ in 0..MAX_ANIMATION_FRAMES {
            img.frames.push(Frame { key: next_key(), rgba: vec![0; 400].into(), gap: 40 });
        }
        let (outcome, reply) = store.kitty(KittyCommand { action: b'f', id, ..KittyCommand::default() }, &[0; 400]);
        assert!(matches!(outcome, KittyOutcome::None));
        assert_eq!(reply.as_deref(), Some("i=1;EBIG:too many animation frames"));

        // Memory limit cap:
        // Create an image with 2048 x 2048 x 4 = 16MB per frame.
        // MAX_ANIMATION_BYTES is 32MB. Root frame takes 16MB.
        // 1 additional frame takes 16MB -> total 32MB <= 32MB.
        // 2nd additional frame would take 48MB > 32MB -> EBIG.
        let mut store2 = Store::default();
        let id2 = store2.insert(2, 2048, 2048, vec![0; 2048 * 2048 * 4]);
        let img2 = store2.images.get_mut(&id2).unwrap();
        img2.frames.push(Frame { key: next_key(), rgba: vec![0; 2048 * 2048 * 4].into(), gap: 40 });
        let (outcome2, reply2) = store2.kitty(KittyCommand { action: b'f', id: id2, ..KittyCommand::default() }, &[]);
        assert!(matches!(outcome2, KittyOutcome::None));
        assert_eq!(reply2.as_deref(), Some("i=2;EBIG:animation exceeds memory limit"));
    }
}
