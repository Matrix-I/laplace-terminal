//! wgpu setup and the single-draw-call instanced renderer.
//!
//! Each frame: lock the grid briefly, rebuild instances only for dirty rows,
//! upload that contiguous range, then issue `draw(0..4, 0..cols*rows)`.

use std::sync::Arc;

use parking_lot::Mutex;
use winit::window::Window;

use crate::atlas::{Atlas, FontSet};
use crate::grid::{Grid, Hyperlink, DEFAULT_BG, DEFAULT_FG};

/// Per-cell GPU instance: 12 bytes, mirrors `Cell` with codepoint → atlas slot.
#[repr(C)]
#[derive(Copy, Clone, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    glyph: u32,
    fg: u32,
    bg: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    screen: [f32; 2],
    cell: [f32; 2],
    atlas_size: [f32; 2],
    offset: [f32; 2],
    cols: u32,
    slots_per_row: u32,
    cursor: [u32; 2],
    cursor_visible: u32,
    default_fg: u32,
    default_bg: u32,
    _pad: u32,
}

const _: () = assert!(std::mem::size_of::<Uniforms>() == 64);
const _: () = assert!(std::mem::size_of::<Instance>() == 12);

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    instance_buf: wgpu::Buffer,
    instances: Vec<Instance>,
    atlas: Atlas,
    pad: u32,
    force_full: bool,
    last_cursor: Option<(usize, usize)>,
    last_hovered_link: Option<Hyperlink>,
}

impl Renderer {
    pub fn new(window: Arc<Window>, font: Arc<FontSet>, font_px: f32) -> Self {
        let size = window.inner_size();

        #[cfg(target_os = "macos")]
        let backends = wgpu::Backends::METAL;
        #[cfg(not(target_os = "macos"))]
        let backends = wgpu::Backends::VULKAN;

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            flags: wgpu::InstanceFlags::empty(),
            ..Default::default()
        });
        let surface = instance.create_surface(window.clone()).expect("create surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("no suitable GPU adapter (Metal/Vulkan)");
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("laplace"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
            },
            None,
        ))
        .expect("request device");

        // Prefer a non-sRGB format so packed colors pass straight through.
        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let atlas = Atlas::new(&device, &queue, font, font_px);

        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cells"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Uint32, 1 => Uint32, 2 => Uint32],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let bind_group = make_bind_group(&device, &bgl, &uniform_buf, &atlas, &sampler);
        let instance_buf = make_instance_buf(&device, 1);
        let pad = (window.scale_factor() * 4.0).round() as u32;

        Renderer {
            surface,
            device,
            queue,
            config,
            pipeline,
            bgl,
            bind_group,
            uniform_buf,
            sampler,
            instance_buf,
            instances: Vec::new(),
            atlas,
            pad,
            force_full: true,
            last_cursor: None,
            last_hovered_link: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabBarAction {
    Select(usize),
    NewTab,
}

impl Renderer {
    pub fn cell_size(&self) -> (u32, u32) {
        (self.atlas.cell_w, self.atlas.cell_h)
    }

    /// Columns and rows that fit in the current surface.
    pub fn grid_size(&self) -> (usize, usize) {
        let w = self.config.width.saturating_sub(2 * self.pad);
        let h = self.config.height.saturating_sub(2 * self.pad);
        ((w / self.atlas.cell_w).max(1) as usize, (h / self.atlas.cell_h).max(1) as usize)
    }

    /// Columns and terminal PTY rows when accounting for the top tab bar + gap
    /// shown whenever 2 or more tabs are open.
    pub fn grid_size_for_tabs(&self, show_tab_bar: bool) -> (usize, usize) {
        let (cols, total_rows) = self.grid_size();
        let rows = if show_tab_bar && total_rows > 2 {
            total_rows - 2
        } else {
            total_rows
        };
        (cols, rows)
    }

    pub fn invalidate(&mut self) {
        self.force_full = true;
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.force_full = true;
    }

    pub fn set_font_px(&mut self, px: f32, scale_factor: f64) {
        self.atlas.set_px(&self.device, &self.queue, px);
        self.bind_group = make_bind_group(&self.device, &self.bgl, &self.uniform_buf, &self.atlas, &self.sampler);
        self.pad = (scale_factor * 4.0).round() as u32;
        self.force_full = true;
    }

    pub fn hit_test_tab_bar(&self, x_px: f64, y_px: f64, titles: &[String]) -> Option<TabBarAction> {
        if titles.len() <= 1 {
            return None;
        }
        let bar_h = (self.atlas.cell_h as f64) * 1.56;
        if y_px < 0.0 || y_px >= bar_h {
            return None;
        }
        let (cols, _) = self.grid_size();
        let tab_cols_total = cols.saturating_sub(2).max(1);
        let col = ((x_px - self.pad as f64).max(0.0) as u32 / self.atlas.cell_w.max(1)) as usize;
        if col >= tab_cols_total {
            return Some(TabBarAction::NewTab);
        }
        let n = titles.len();
        let mut tab_i = (col * n) / tab_cols_total;
        let next_start = ((tab_i + 1) * tab_cols_total) / n;
        if col >= next_start && tab_i + 1 < n {
            tab_i += 1;
        }
        Some(TabBarAction::Select(tab_i.min(n - 1)))
    }

    /// Map a physical window cursor position `(x_px, y_px)` to a terminal grid
    /// cell `(row, col)`. Returns `None` if the cursor is inside the tab bar.
    pub fn pixel_to_cell(
        &self,
        x_px: f64,
        y_px: f64,
        show_tab_bar: bool,
        cols: usize,
        rows: usize,
    ) -> Option<(usize, usize)> {
        if cols == 0 || rows == 0 {
            return None;
        }
        if show_tab_bar && y_px < (self.atlas.cell_h as f64) * 1.56 {
            return None;
        }
        let top_y = if show_tab_bar {
            (self.pad + self.atlas.cell_h * 2) as f64
        } else {
            self.pad as f64
        };
        let rel_x = (x_px - self.pad as f64).max(0.0);
        let rel_y = (y_px - top_y).max(0.0);
        let col = ((rel_x as u32) / self.atlas.cell_w.max(1)) as usize;
        let row = ((rel_y as u32) / self.atlas.cell_h.max(1)) as usize;
        Some((row.min(rows - 1), col.min(cols - 1)))
    }

    pub fn render_with_tabs(
        &mut self,
        grid: &Mutex<Grid>,
        tab_bar: Option<(&[String], usize)>,
        hovered_link: Option<&Hyperlink>,
    ) {
        let top_rows = usize::from(tab_bar.is_some());
        if self.last_hovered_link.as_ref() != hovered_link {
            self.force_full = true;
            self.last_hovered_link = hovered_link.cloned();
        }

        // Fast check before acquiring a Metal swapchain drawable: if no cells
        // or cursor state changed since the last frame, skip presenting entirely
        // so non-visual escape sequences never exhaust swapchain drawables.
        if !self.force_full {
            let g = grid.lock();
            if !g.all_dirty
                && self.instances.len() == g.cols * (g.rows + top_rows)
                && g.cursor_pos() == self.last_cursor
                && !g.dirty.iter().any(|&d| d)
            {
                return;
            }
        }

        // Acquire first: with Fifo this may block until V-Sync, and we must not
        // hold the grid lock (which would stall the PTY parser) while waiting.
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                self.force_full = true;
                return;
            }
            Err(e) => {
                eprintln!("laplace: surface error: {e}");
                return;
            }
        };

        let (cols, total_rows, mut lo, mut hi, cursor) = {
            let mut g = grid.lock();
            let (cols, rows) = (g.cols, g.rows);
            let total_rows = rows + top_rows;
            let n = cols * total_rows;
            if self.instances.len() != n {
                self.instances = vec![Instance::default(); n];
                if self.instance_buf.size() < (n * 12) as u64 {
                    self.instance_buf = make_instance_buf(&self.device, n);
                }
                self.force_full = true;
            }
            self.atlas.begin_frame();
            let full = self.force_full || g.all_dirty;
            let (mut lo, mut hi) = (usize::MAX, 0);
            if let Some((titles, active_idx)) = tab_bar {
                if full {
                    self.build_tab_bar_row(cols, titles, active_idx);
                    lo = 0;
                    hi = 0;
                }
            }
            for r in 0..rows {
                if full || g.dirty[r] {
                    let dst_row = r + top_rows;
                    self.build_row(&g, r, dst_row, hovered_link);
                    lo = lo.min(dst_row);
                    hi = hi.max(dst_row);
                }
            }
            if self.atlas.take_evicted() {
                // A recycled slot may be referenced by a row we skipped.
                if let Some((titles, active_idx)) = tab_bar {
                    self.build_tab_bar_row(cols, titles, active_idx);
                }
                for r in 0..rows {
                    self.build_row(&g, r, r + top_rows, hovered_link);
                }
                self.atlas.take_evicted();
                lo = 0;
                hi = total_rows - 1;
            }
            g.dirty.fill(false);
            g.all_dirty = false;
            self.force_full = false;
            let cur = g.cursor_pos();
            self.last_cursor = cur;
            (cols, total_rows, lo, hi, cur.map(|(cx, cy)| (cx, cy + top_rows)))
        };

        if let Some(link) = hovered_link {
            let max_chars = cols.saturating_sub(2).min(120);
            let display_url = if link.url.chars().count() > max_chars {
                format!("{}...", link.url.chars().take(max_chars - 3).collect::<String>())
            } else {
                link.url.clone()
            };
            let preview_row = total_rows - 1;
            let row_start = preview_row * cols;
            // Ghostty bottom-left URL preview pill: dark slate background #16171e (rgb<<8 | 0x80), white text #f2f2f7, flag 4 = top border
            const PREVIEW_BG: u32 = 0x1617_1E80 | 4;
            const PREVIEW_FG: u32 = 0xF2F2_F7FF;
            for (col, ch) in display_url.chars().enumerate() {
                if col < cols {
                    let slot = self.atlas.slot(&self.queue, ch as u32);
                    self.instances[row_start + col] = Instance {
                        glyph: slot as u32,
                        fg: PREVIEW_FG,
                        bg: PREVIEW_BG,
                    };
                }
            }
            let end_col = display_url.chars().count();
            if end_col < cols {
                self.instances[row_start + end_col] = Instance {
                    glyph: 0,
                    fg: PREVIEW_FG,
                    bg: PREVIEW_BG | 8,
                };
            } else if cols > 0 {
                self.instances[row_start + cols - 1].bg |= 8;
            }
            lo = lo.min(preview_row);
            hi = hi.max(preview_row);
        }

        if lo <= hi {
            let a = lo * cols;
            let b = (hi + 1) * cols;
            self.queue.write_buffer(
                &self.instance_buf,
                (a * std::mem::size_of::<Instance>()) as u64,
                bytemuck::cast_slice(&self.instances[a..b]),
            );
        }
        let (cx, cy) = cursor.unwrap_or((0, 0));
        let tab_info = match tab_bar {
            Some((titles, active_idx)) => ((active_idx as u32) << 16) | (titles.len() as u32 & 0xFFFF),
            None => 0,
        };

        let u = Uniforms {
            screen: [self.config.width as f32, self.config.height as f32],
            cell: [self.atlas.cell_w as f32, self.atlas.cell_h as f32],
            atlas_size: [self.atlas.size as f32; 2],
            offset: [self.pad as f32; 2],
            cols: cols as u32,
            slots_per_row: self.atlas.slots_per_row,
            cursor: [cx as u32, cy as u32],
            cursor_visible: cursor.is_some() as u32,
            default_fg: DEFAULT_FG,
            default_bg: DEFAULT_BG,
            _pad: tab_info,
        };
        self.queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));

        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let c = |s: u32| ((DEFAULT_BG >> s) & 0xFF) as f64 / 255.0;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cells"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: c(24), g: c(16), b: c(8), a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.instance_buf.slice(..));
            pass.draw(0..4, 0..(cols * total_rows) as u32);
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
    }

    fn build_row(
        &mut self,
        g: &Grid,
        src_row: usize,
        dst_row: usize,
        hovered_link: Option<&Hyperlink>,
    ) {
        const SEL_BG: u32 = 0x3D59_A180;
        const SEL_FG: u32 = 0xFFFF_FFFF;
        let cols = g.cols;
        let row = g.display_row(src_row);
        let out = &mut self.instances[dst_row * cols..(dst_row + 1) * cols];
        let is_link_row = hovered_link.map_or(false, |l| l.row == src_row);
        for (col, (dst, cell)) in out.iter_mut().zip(row).enumerate() {
            let slot = self.atlas.slot(&self.queue, cell.codepoint);
            let underline_flag = if is_link_row {
                if let Some(l) = hovered_link {
                    if col >= l.start_col && col <= l.end_col { 1u32 } else { 0u32 }
                } else { 0u32 }
            } else {
                0u32
            };
            if g.is_selected(src_row, col) {
                *dst = Instance {
                    glyph: slot as u32,
                    fg: SEL_FG,
                    bg: (SEL_BG | (cell.bg & 1)) | underline_flag,
                };
            } else {
                *dst = Instance {
                    glyph: slot as u32,
                    fg: cell.fg,
                    bg: cell.bg | underline_flag,
                };
            }
        }
    }

    fn build_tab_bar_row(&mut self, cols: usize, titles: &[String], active_idx: usize) {
        const ACTIVE_FG: u32 = 0xF2F2_F7FF;
        const ACTIVE_BADGE_FG: u32 = 0xA6A8_B3FF;
        const INACTIVE_FG: u32 = 0x8E90_99FF;
        const INACTIVE_BADGE_FG: u32 = 0x686A_73FF;
        const BAR_BG: u32 = 0x1C1D_2080;

        for dst in &mut self.instances[0..cols] {
            *dst = Instance { glyph: 0, fg: INACTIVE_FG, bg: BAR_BG };
        }

        let tab_cols_total = cols.saturating_sub(2).max(1);
        let n = titles.len().max(1);
        for (i, title) in titles.iter().enumerate() {
            let c0 = (i * tab_cols_total) / n;
            let c1 = ((i + 1) * tab_cols_total) / n;
            let span = c1.saturating_sub(c0);
            if span < 4 {
                continue;
            }
            let (fg, badge_fg) = if i == active_idx {
                (ACTIVE_FG, ACTIVE_BADGE_FG)
            } else {
                (INACTIVE_FG, INACTIVE_BADGE_FG)
            };

            // Right-aligned shortcut badge (`⌘1`..`⌘9` on macOS, `^1`..`^9` on Linux).
            let badge_reserve = if i < 9 && span >= 10 {
                #[cfg(target_os = "macos")]
                let mod_ch = '⌘';
                #[cfg(not(target_os = "macos"))]
                let mod_ch = '^';
                let digit = (b'1' + i as u8) as char;
                let bx = c1 - 3;
                let s0 = self.atlas.slot(&self.queue, mod_ch as u32);
                let s1 = self.atlas.slot(&self.queue, digit as u32);
                self.instances[bx] = Instance { glyph: s0 as u32, fg: badge_fg, bg: BAR_BG };
                self.instances[bx + 1] = Instance { glyph: s1 as u32, fg: badge_fg, bg: BAR_BG };
                4
            } else {
                1
            };

            let avail = span.saturating_sub(2 + badge_reserve);
            if avail == 0 {
                continue;
            }
            let chars: Vec<char> = title.chars().collect();
            let display: Vec<char> = if chars.len() <= avail {
                chars
            } else if avail > 1 {
                let mut v: Vec<char> = chars.into_iter().take(avail - 1).collect();
                v.push('…');
                v
            } else {
                vec!['…']
            };

            let start_col = c0 + 1 + (avail.saturating_sub(display.len())) / 2;
            for (k, ch) in display.into_iter().enumerate() {
                let x = start_col + k;
                if x < c1.saturating_sub(badge_reserve) && x < cols {
                    let slot = self.atlas.slot(&self.queue, ch as u32);
                    self.instances[x] = Instance { glyph: slot as u32, fg, bg: BAR_BG };
                }
            }
        }
    }
}

fn make_bind_group(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    uniform_buf: &wgpu::Buffer,
    atlas: &Atlas,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bind group"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: uniform_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas.view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}

fn make_instance_buf(device: &wgpu::Device, cells: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instances"),
        size: (cells.max(1) * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
