//! wgpu setup and the single-draw-call instanced renderer.
//!
//! Each frame: lock the grid briefly, rebuild instances only for dirty rows,
//! upload that contiguous range, then issue `draw(0..4, 0..cols*rows)`.

use std::sync::Arc;

use parking_lot::Mutex;
use winit::window::Window;

use crate::atlas::{Atlas, FontSet};
use crate::grid::{default_bg, default_fg, Grid, Hyperlink};
use crate::history::HistorySearch;
use crate::split::PaneRect;

pub struct RenderPane<'a> {
    #[allow(dead_code)]
    pub id: u64,
    pub grid: &'a Mutex<Grid>,
    pub rect: PaneRect,
    pub is_active: bool,
}

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
    cursor_style: u32,
    default_fg: u32,
    default_bg: u32,
    tab_info: u32,
    bg_opacity: f32,
    _pad: [u32; 2],
}

const _: () = assert!(std::mem::size_of::<Uniforms>() == 80);
const _: () = assert!(std::mem::size_of::<Instance>() == 12);

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    caps_alpha_modes: Vec<wgpu::CompositeAlphaMode>,
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    sampler: wgpu::Sampler,
    instance_buf: wgpu::Buffer,
    instances: Vec<Instance>,
    atlas: Atlas,
    pad: u32,
    bg_opacity: f32,
    cursor_style: u32,
    force_full: bool,
    last_cursor: Option<(usize, usize)>,
    last_cursor_visible: bool,
    last_hovered_link: Option<Hyperlink>,
}

impl Renderer {
    pub fn new(
        window: Arc<Window>,
        font: Arc<FontSet>,
        font_px: f32,
        bg_opacity: f32,
        cursor_style: u32,
    ) -> Self {
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
        let caps_alpha_modes = caps.alpha_modes.clone();
        let alpha_mode = if bg_opacity < 1.0 {
            caps_alpha_modes
                .iter()
                .copied()
                .find(|&m| m == wgpu::CompositeAlphaMode::PreMultiplied || m == wgpu::CompositeAlphaMode::PostMultiplied)
                .unwrap_or(caps_alpha_modes[0])
        } else {
            caps_alpha_modes[0]
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode,
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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
            caps_alpha_modes,
            pipeline,
            bgl,
            bind_group,
            uniform_buf,
            sampler,
            instance_buf,
            instances: Vec::new(),
            atlas,
            pad,
            bg_opacity: bg_opacity.clamp(0.0, 1.0),
            cursor_style,
            force_full: true,
            last_cursor: None,
            last_cursor_visible: true,
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

    pub fn pad(&self) -> u32 {
        self.pad
    }

    pub fn set_bg_opacity(&mut self, opacity: f32) {
        let op = opacity.clamp(0.0, 1.0);
        if (self.bg_opacity - op).abs() > 0.001 {
            self.bg_opacity = op;
            let alpha_mode = if self.bg_opacity < 1.0 {
                self.caps_alpha_modes
                    .iter()
                    .copied()
                    .find(|&m| m == wgpu::CompositeAlphaMode::PreMultiplied || m == wgpu::CompositeAlphaMode::PostMultiplied)
                    .unwrap_or(self.caps_alpha_modes[0])
            } else {
                self.caps_alpha_modes[0]
            };
            self.config.alpha_mode = alpha_mode;
            self.surface.configure(&self.device, &self.config);
            self.force_full = true;
        }
    }

    pub fn set_cursor_style(&mut self, style: u32) {
        if self.cursor_style != style {
            self.cursor_style = style;
            self.force_full = true;
        }
    }

    pub fn set_fonts(&mut self, fonts: Arc<FontSet>, px: f32) {
        self.atlas.set_fonts(&self.device, &self.queue, fonts, px);
        self.bind_group = make_bind_group(&self.device, &self.bgl, &self.uniform_buf, &self.atlas, &self.sampler);
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

    pub fn render_panes(
        &mut self,
        panes: &[RenderPane],
        tab_bar: Option<(&[String], usize)>,
        hovered_link: Option<&Hyperlink>,
        cursor_blink_on: bool,
        history_search: Option<&HistorySearch>,
    ) {
        if panes.is_empty() {
            return;
        }
        let top_rows = usize::from(tab_bar.is_some());
        if self.last_hovered_link.as_ref() != hovered_link {
            self.force_full = true;
            self.last_hovered_link = hovered_link.cloned();
        }

        let is_search_open = history_search.map_or(false, |h| h.is_open);
        let (cols, term_rows) = self.grid_size_for_tabs(tab_bar.is_some());
        let total_rows = term_rows + top_rows;
        let n = cols * total_rows;

        let active_pane = panes.iter().find(|p| p.is_active).unwrap_or(&panes[0]);
        let active_cur = {
            let g = active_pane.grid.lock();
            g.cursor_pos().map(|(cx, cy)| (active_pane.rect.col + cx, active_pane.rect.row + cy + top_rows))
        };
        let effective_visible = active_cur.is_some() && cursor_blink_on;

        // Fast check before acquiring a Metal/Vulkan swapchain drawable
        if !self.force_full && !is_search_open {
            let any_dirty = panes.iter().any(|p| {
                let g = p.grid.lock();
                g.all_dirty || g.dirty.iter().any(|&d| d)
            });
            if !any_dirty
                && self.instances.len() == n
                && active_cur == self.last_cursor
                && effective_visible == self.last_cursor_visible
            {
                return;
            }
        }

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

        if self.instances.len() != n {
            self.instances = vec![Instance::default(); n];
            if self.instance_buf.size() < (n * 12) as u64 {
                self.instance_buf = make_instance_buf(&self.device, n);
            }
            self.force_full = true;
        }

        self.atlas.begin_frame();
        let full = self.force_full;
        let (mut lo, mut hi) = (usize::MAX, 0);

        if let Some((titles, active_idx)) = tab_bar {
            if full {
                self.build_tab_bar_row(cols, titles, active_idx);
                lo = 0;
                hi = 0;
            }
        }

        let active_rect = panes.iter().find(|p| p.is_active).map(|p| p.rect);

        for pane in panes {
            let mut g = pane.grid.lock();
            let pane_full = full || g.all_dirty;
            let p_cols = pane.rect.cols.min(g.cols);
            let p_rows = pane.rect.rows.min(g.rows);

            for r in 0..p_rows {
                if pane_full || g.dirty[r] {
                    let dst_row = top_rows + pane.rect.row + r;
                    if dst_row < total_rows {
                        self.build_pane_row(
                            &g,
                            r,
                            dst_row,
                            pane.rect.col,
                            p_cols,
                            cols,
                            &pane.rect,
                            pane.is_active,
                            active_rect.as_ref(),
                            hovered_link,
                        );
                        lo = lo.min(dst_row);
                        hi = hi.max(dst_row);
                    }
                }
            }
            g.dirty.fill(false);
            g.all_dirty = false;
        }

        if self.atlas.take_evicted() {
            if let Some((titles, active_idx)) = tab_bar {
                self.build_tab_bar_row(cols, titles, active_idx);
            }
            for pane in panes {
                let g = pane.grid.lock();
                let p_cols = pane.rect.cols.min(g.cols);
                let p_rows = pane.rect.rows.min(g.rows);
                for r in 0..p_rows {
                    let dst_row = top_rows + pane.rect.row + r;
                    if dst_row < total_rows {
                        self.build_pane_row(
                            &g,
                            r,
                            dst_row,
                            pane.rect.col,
                            p_cols,
                            cols,
                            &pane.rect,
                            pane.is_active,
                            active_rect.as_ref(),
                            hovered_link,
                        );
                    }
                }
            }
            self.atlas.take_evicted();
            lo = 0;
            hi = total_rows - 1;
        }

        self.force_full = false;
        self.last_cursor = active_cur;

        if let Some(link) = hovered_link {
            let max_chars = cols.saturating_sub(2).min(120);
            let display_url = if link.url.chars().count() > max_chars {
                format!("{}...", link.url.chars().take(max_chars - 3).collect::<String>())
            } else {
                link.url.clone()
            };
            let preview_row = total_rows - 1;
            let row_start = preview_row * cols;
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

        if let Some(search) = history_search {
            if search.is_open {
                self.build_history_popup(cols, total_rows, search, &mut lo, &mut hi);
            }
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
        let (cx, cy) = active_cur.unwrap_or((0, 0));
        let tab_info = match tab_bar {
            Some((titles, active_idx)) => ((active_idx as u32) << 16) | (titles.len() as u32 & 0xFFFF),
            None => 0,
        };
        let active_cursor_style = {
            let g = active_pane.grid.lock();
            match g.cursor_shape {
                1 | 2 => 0, // Block
                3 | 4 => 2, // Underline
                5 | 6 => 1, // Bar
                _ => self.cursor_style,
            }
        };

        let effective_cursor_visible = if is_search_open {
            false
        } else {
            effective_visible
        };
        self.last_cursor_visible = effective_cursor_visible;

        let u = Uniforms {
            screen: [self.config.width as f32, self.config.height as f32],
            cell: [self.atlas.cell_w as f32, self.atlas.cell_h as f32],
            atlas_size: [self.atlas.size as f32; 2],
            offset: [self.pad as f32; 2],
            cols: cols as u32,
            slots_per_row: self.atlas.slots_per_row,
            cursor: [cx as u32, cy as u32],
            cursor_visible: effective_cursor_visible as u32,
            cursor_style: active_cursor_style,
            default_fg: default_fg(),
            default_bg: default_bg(),
            tab_info,
            bg_opacity: self.bg_opacity,
            _pad: [0; 2],
        };
        self.queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));

        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let bg = default_bg();
            let c = |s: u32| ((bg >> s) & 0xFF) as f64 / 255.0;
            let clear_a = if self.bg_opacity < 1.0 { self.bg_opacity as f64 } else { 1.0 };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cells"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: c(24), g: c(16), b: c(8), a: clear_a }),
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

    #[allow(dead_code)]
    pub fn render_with_tabs(
        &mut self,
        grid: &Mutex<Grid>,
        tab_bar: Option<(&[String], usize)>,
        hovered_link: Option<&Hyperlink>,
        cursor_blink_on: bool,
        history_search: Option<&HistorySearch>,
    ) {
        let (cols, rows) = self.grid_size_for_tabs(tab_bar.is_some());
        let rect = PaneRect {
            id: 1,
            col: 0,
            row: 0,
            cols,
            rows,
            has_border_right: false,
            has_border_bottom: false,
        };
        let p = RenderPane {
            id: 1,
            grid,
            rect,
            is_active: true,
        };
        self.render_panes(&[p], tab_bar, hovered_link, cursor_blink_on, history_search);
    }

    #[allow(clippy::too_many_arguments)]
    fn build_pane_row(
        &mut self,
        g: &Grid,
        src_row: usize,
        dst_row: usize,
        dst_col: usize,
        p_cols: usize,
        window_cols: usize,
        rect: &PaneRect,
        is_active: bool,
        active_rect: Option<&PaneRect>,
        hovered_link: Option<&Hyperlink>,
    ) {
        const SEL_BG: u32 = 0x3D59_A180;
        const SEL_FG: u32 = 0xFFFF_FFFF;
        let row = g.display_row(src_row);
        let start = dst_row * window_cols + dst_col;
        let out = &mut self.instances[start..start + p_cols];
        let is_link_row = hovered_link.map_or(false, |l| l.row == src_row);
        let is_last_row = src_row + 1 == rect.rows;

        let is_border_right_active = is_active
            || active_rect.is_some_and(|ar| {
                ar.col == rect.col + rect.cols
                    && ar.row < rect.row + rect.rows
                    && ar.row + ar.rows > rect.row
            });

        let is_border_bottom_active = is_active
            || active_rect.is_some_and(|ar| {
                ar.row == rect.row + rect.rows
                    && ar.col < rect.col + rect.cols
                    && ar.col + ar.cols > rect.col
            });

        for (col, (dst, cell)) in out.iter_mut().zip(row).enumerate() {
            let slot = self.atlas.slot(&self.queue, cell.codepoint);
            let underline_flag = if is_link_row {
                if let Some(l) = hovered_link {
                    if col >= l.start_col && col <= l.end_col { 1u32 } else { 0u32 }
                } else { 0u32 }
            } else {
                0u32
            };

            // Divider flags (bit 0: right, bit 1: bottom, bit 2: active) packed into high 16 bits of glyph
            let mut div_flags = 0u32;
            if rect.has_border_right && col + 1 == rect.cols {
                div_flags |= 1;
                if is_border_right_active {
                    div_flags |= 4;
                }
            }
            if rect.has_border_bottom && is_last_row {
                div_flags |= 2;
                if is_border_bottom_active {
                    div_flags |= 4;
                }
            }
            let glyph_with_div = (slot as u32) | (div_flags << 16);

            if g.is_selected(src_row, col) {
                *dst = Instance {
                    glyph: glyph_with_div,
                    fg: SEL_FG,
                    bg: (SEL_BG | (cell.bg & 1)) | underline_flag,
                };
            } else {
                *dst = Instance {
                    glyph: glyph_with_div,
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

    /// Compute position and dimensions of the history search popup `(x0, y0, width, height, visible_rows)`.
    pub fn history_popup_geometry(
        &self,
        search: &HistorySearch,
    ) -> Option<(usize, usize, usize, usize, usize)> {
        if !search.is_open {
            return None;
        }
        let (cols, total_rows) = self.grid_size();
        if cols < 30 || total_rows < 6 {
            return None;
        }
        let popup_w = (cols * 85 / 100).clamp(48, 92).min(cols.saturating_sub(2));
        let popup_h = if total_rows >= 22 {
            16.min(total_rows.saturating_sub(4))
        } else {
            13.min(total_rows.saturating_sub(2))
        };
        if popup_w < 20 || popup_h < 5 {
            return None;
        }
        let x0 = (cols.saturating_sub(popup_w)) / 2;
        let y0 = (total_rows.saturating_sub(popup_h)) / 3;
        let y0 = if y0 == 0 && total_rows > popup_h + 1 { 1 } else { y0 };
        let visible_rows = if popup_h >= 13 {
            popup_h.saturating_sub(7)
        } else {
            popup_h.saturating_sub(4)
        };
        Some((x0, y0, popup_w, popup_h, visible_rows))
    }

    fn build_history_popup(
        &mut self,
        cols: usize,
        total_rows: usize,
        search: &HistorySearch,
        lo: &mut usize,
        hi: &mut usize,
    ) {
        let Some((x0, y0, popup_w, popup_h, visible_rows)) = self.history_popup_geometry(search) else {
            return;
        };

        const POPUP_BG: u32 = 0x1819_2480;       // Catppuccin deep slate (#181924) with bit 7 set
        const BORDER_FG: u32 = 0x585B_70FF;      // Slate gray border
        const TITLE_FG: u32 = 0x89DC_EBFF;       // Cyan title
        const BADGE_FG: u32 = 0xA6AD_C8FF;       // Lavender counter
        const SORT_FG: u32 = 0xCBA6_F7FF;        // Purple sort mode badge
        const PROMPT_FG: u32 = 0xA6E3_A1FF;      // Green prompt ❯
        const QUERY_FG: u32 = 0xF2F2_F7FF;       // Crisp white query text
        const DIM_FG: u32 = 0x6C70_86FF;         // Dim placeholder
        const CURSOR_FG: u32 = 0xCBA6_F7FF;      // Lavender block cursor
        const SEL_BG: u32 = 0x3134_4C80;         // Selection highlight background with bit 7 set
        const SEL_ARROW_FG: u32 = 0x00BF_E3FF;   // Cyan selection arrow ➜
        const SEL_TEXT_FG: u32 = 0xFFFF_FFFF;     // Bright white text for selected item
        const TEXT_FG: u32 = 0xBAC2_DEFF;        // Soft text for unselected item
        const MATCH_FG: u32 = 0xF9E2_AFFF;       // Warm gold for matched characters
        const PREVIEW_TITLE_FG: u32 = 0x89DC_EBFF;
        const PREVIEW_TEXT_FG: u32 = 0xCDD6_F4FF;
        const FOOTER_FG: u32 = 0x7F84_9CFF;      // Subdued footer text
        const KEY_FG: u32 = 0xCBA6_F7FF;         // Key indicator

        // 1. Fill popup bounding box with dark background
        for r in 0..popup_h {
            let dst_r = y0 + r;
            if dst_r >= total_rows {
                break;
            }
            let row_start = dst_r * cols;
            for c in 0..popup_w {
                let dst_c = x0 + c;
                if dst_c < cols {
                    self.instances[row_start + dst_c] = Instance {
                        glyph: 0,
                        fg: TEXT_FG,
                        bg: POPUP_BG,
                    };
                }
            }
        }
        *lo = (*lo).min(y0);
        *hi = (*hi).max((y0 + popup_h - 1).min(total_rows.saturating_sub(1)));

        let queue = &self.queue;

        // 2. Row 0: Top border
        let r0 = y0 * cols;
        let s_tl = self.atlas.slot(queue, 0x256D) as u32; // ╭
        let s_tr = self.atlas.slot(queue, 0x256E) as u32; // ╮
        let s_h = self.atlas.slot(queue, 0x2500) as u32;  // ─
        let s_v = self.atlas.slot(queue, 0x2502) as u32;  // │

        self.instances[r0 + x0] = Instance { glyph: s_tl, fg: BORDER_FG, bg: POPUP_BG };
        self.instances[r0 + x0 + popup_w - 1] = Instance { glyph: s_tr, fg: BORDER_FG, bg: POPUP_BG };
        for c in 1..(popup_w - 1) {
            self.instances[r0 + x0 + c] = Instance { glyph: s_h, fg: BORDER_FG, bg: POPUP_BG };
        }

        // Title: ╭─ History Search (fzf) ─...
        let title = " History Search (fzf) ";
        for (i, ch) in title.chars().enumerate() {
            if x0 + 2 + i < x0 + popup_w - 1 {
                let slot = self.atlas.slot(queue, ch as u32) as u32;
                self.instances[r0 + x0 + 2 + i] = Instance { glyph: slot, fg: TITLE_FG, bg: POPUP_BG };
            }
        }

        // Sort mode badge: [Relevance] or [Recent]
        let sort_label = format!(" [{}] ", search.sort_mode.label());
        let sort_start = x0 + 2 + title.chars().count();
        if sort_start + sort_label.chars().count() + 16 < x0 + popup_w {
            for (i, ch) in sort_label.chars().enumerate() {
                let slot = self.atlas.slot(queue, ch as u32) as u32;
                self.instances[r0 + sort_start + i] = Instance { glyph: slot, fg: SORT_FG, bg: POPUP_BG };
            }
        }

        let counter = format!(" [ {}/{} ] ", search.match_count(), search.total_items());
        let counter_len = counter.chars().count();
        if x0 + 2 + title.chars().count() + counter_len + 16 < x0 + popup_w {
            let start_c = x0 + popup_w - 1 - counter_len;
            for (i, ch) in counter.chars().enumerate() {
                let slot = self.atlas.slot(queue, ch as u32) as u32;
                self.instances[r0 + start_c + i] = Instance { glyph: slot, fg: BADGE_FG, bg: POPUP_BG };
            }
        }

        // 3. Row 1: Search input
        let r1 = (y0 + 1) * cols;
        self.instances[r1 + x0] = Instance { glyph: s_v, fg: BORDER_FG, bg: POPUP_BG };
        self.instances[r1 + x0 + popup_w - 1] = Instance { glyph: s_v, fg: BORDER_FG, bg: POPUP_BG };

        let s_prompt = self.atlas.slot(queue, 0x276F) as u32; // ❯
        self.instances[r1 + x0 + 2] = Instance { glyph: s_prompt, fg: PROMPT_FG, bg: POPUP_BG };

        let s_cursor = self.atlas.slot(queue, 0x258C) as u32; // ▋ left half block cursor
        if search.query.is_empty() {
            let placeholder = "FZF syntax: 'exact  ^prefix  suffix$  !exclude  fuzzy...";
            for (i, ch) in placeholder.chars().enumerate() {
                if x0 + 4 + i < x0 + popup_w - 1 {
                    let slot = self.atlas.slot(queue, ch as u32) as u32;
                    self.instances[r1 + x0 + 4 + i] = Instance { glyph: slot, fg: DIM_FG, bg: POPUP_BG };
                }
            }
            self.instances[r1 + x0 + 4] = Instance { glyph: s_cursor, fg: CURSOR_FG, bg: POPUP_BG };
        } else {
            let q_chars: Vec<char> = search.query.chars().collect();
            let max_q = popup_w.saturating_sub(6);
            let display_q = q_chars.len().min(max_q);
            for i in 0..display_q {
                let slot = self.atlas.slot(queue, q_chars[i] as u32) as u32;
                self.instances[r1 + x0 + 4 + i] = Instance { glyph: slot, fg: QUERY_FG, bg: POPUP_BG };
            }
            if x0 + 4 + display_q < x0 + popup_w - 1 {
                self.instances[r1 + x0 + 4 + display_q] = Instance { glyph: s_cursor, fg: CURSOR_FG, bg: POPUP_BG };
            }
        }

        // 4. Row 2: Separator line
        let r2 = (y0 + 2) * cols;
        let s_ml = self.atlas.slot(queue, 0x251C) as u32; // ├
        let s_mr = self.atlas.slot(queue, 0x2524) as u32; // ┤
        self.instances[r2 + x0] = Instance { glyph: s_ml, fg: BORDER_FG, bg: POPUP_BG };
        self.instances[r2 + x0 + popup_w - 1] = Instance { glyph: s_mr, fg: BORDER_FG, bg: POPUP_BG };
        for c in 1..(popup_w - 1) {
            self.instances[r2 + x0 + c] = Instance { glyph: s_h, fg: BORDER_FG, bg: POPUP_BG };
        }

        // 5. Result items
        let s_arrow = self.atlas.slot(queue, 0x279C) as u32; // ➜
        let s_ellipsis = self.atlas.slot(queue, '…' as u32) as u32;

        for row_i in 0..visible_rows {
            let dst_r = y0 + 3 + row_i;
            if dst_r >= total_rows {
                break;
            }
            let r_start = dst_r * cols;
            self.instances[r_start + x0] = Instance { glyph: s_v, fg: BORDER_FG, bg: POPUP_BG };
            self.instances[r_start + x0 + popup_w - 1] = Instance { glyph: s_v, fg: BORDER_FG, bg: POPUP_BG };

            let item_i = search.scroll_offset + row_i;
            if item_i < search.filtered.len() {
                let m = &search.filtered[item_i];
                let is_selected = item_i == search.selected;
                let row_bg = if is_selected { SEL_BG } else { POPUP_BG };

                // Fill interior with row background
                for c in 1..(popup_w - 1) {
                    self.instances[r_start + x0 + c] = Instance { glyph: 0, fg: TEXT_FG, bg: row_bg };
                }

                if is_selected {
                    self.instances[r_start + x0 + 2] = Instance { glyph: s_arrow, fg: SEL_ARROW_FG, bg: row_bg };
                }

                if let Some(cmd) = search.items.get(m.item_index) {
                    let avail_w = popup_w.saturating_sub(6);
                    let cmd_chars: Vec<char> = cmd.chars().collect();
                    let display_len = cmd_chars.len().min(avail_w);
                    let is_truncated = cmd_chars.len() > avail_w;

                    for ci in 0..display_len {
                        let ch = cmd_chars[ci];
                        let is_match = m.match_indices.contains(&ci);
                        let fg = if is_match {
                            MATCH_FG
                        } else if is_selected {
                            SEL_TEXT_FG
                        } else {
                            TEXT_FG
                        };
                        let slot = self.atlas.slot(queue, ch as u32) as u32;
                        self.instances[r_start + x0 + 4 + ci] = Instance { glyph: slot, fg, bg: row_bg };
                    }
                    if is_truncated && avail_w > 0 {
                        self.instances[r_start + x0 + 4 + display_len - 1] = Instance {
                            glyph: s_ellipsis,
                            fg: DIM_FG,
                            bg: row_bg,
                        };
                    }
                }
            } else if search.filtered.is_empty() && row_i == 0 {
                let no_res = "  No matching commands found";
                for (ci, ch) in no_res.chars().enumerate() {
                    if x0 + 4 + ci < x0 + popup_w - 1 {
                        let slot = self.atlas.slot(queue, ch as u32) as u32;
                        self.instances[r_start + x0 + 4 + ci] = Instance { glyph: slot, fg: DIM_FG, bg: POPUP_BG };
                    }
                }
            }
        }

        // 6. Command Preview Section (if popup is tall enough)
        let has_preview = popup_h >= 13;
        if has_preview {
            let prev_sep_r = y0 + 3 + visible_rows;
            let r_sep = prev_sep_r * cols;
            self.instances[r_sep + x0] = Instance { glyph: s_ml, fg: BORDER_FG, bg: POPUP_BG };
            self.instances[r_sep + x0 + popup_w - 1] = Instance { glyph: s_mr, fg: BORDER_FG, bg: POPUP_BG };
            for c in 1..(popup_w - 1) {
                self.instances[r_sep + x0 + c] = Instance { glyph: s_h, fg: BORDER_FG, bg: POPUP_BG };
            }
            let prev_title = " Preview ";
            for (i, ch) in prev_title.chars().enumerate() {
                if x0 + 2 + i < x0 + popup_w - 1 {
                    let slot = self.atlas.slot(queue, ch as u32) as u32;
                    self.instances[r_sep + x0 + 2 + i] = Instance { glyph: slot, fg: PREVIEW_TITLE_FG, bg: POPUP_BG };
                }
            }

            let preview_rows = popup_h.saturating_sub(4 + visible_rows + 1);
            let selected_cmd = search.selected_command().unwrap_or("");
            let avail_w = popup_w.saturating_sub(6);

            // Split into lines or wrap
            let mut preview_lines: Vec<String> = Vec::new();
            for raw_line in selected_cmd.lines() {
                let mut line = raw_line.to_string();
                while line.chars().count() > avail_w && avail_w > 0 {
                    let part: String = line.chars().take(avail_w).collect();
                    line = line.chars().skip(avail_w).collect();
                    preview_lines.push(part);
                }
                preview_lines.push(line);
            }

            for p_i in 0..preview_rows {
                let cur_r = prev_sep_r + 1 + p_i;
                if cur_r >= total_rows {
                    break;
                }
                let r_start = cur_r * cols;
                self.instances[r_start + x0] = Instance { glyph: s_v, fg: BORDER_FG, bg: POPUP_BG };
                self.instances[r_start + x0 + popup_w - 1] = Instance { glyph: s_v, fg: BORDER_FG, bg: POPUP_BG };
                if let Some(pline) = preview_lines.get(p_i) {
                    for (ci, ch) in pline.chars().take(avail_w).enumerate() {
                        let slot = self.atlas.slot(queue, ch as u32) as u32;
                        self.instances[r_start + x0 + 3 + ci] = Instance { glyph: slot, fg: PREVIEW_TEXT_FG, bg: POPUP_BG };
                    }
                }
            }
        }

        // 7. Last row: Bottom border and key hints
        let rf = (y0 + popup_h - 1) * cols;
        let s_bl = self.atlas.slot(queue, 0x2570) as u32; // ╰
        let s_br = self.atlas.slot(queue, 0x256F) as u32; // ╯
        self.instances[rf + x0] = Instance { glyph: s_bl, fg: BORDER_FG, bg: POPUP_BG };
        self.instances[rf + x0 + popup_w - 1] = Instance { glyph: s_br, fg: BORDER_FG, bg: POPUP_BG };
        for c in 1..(popup_w - 1) {
            self.instances[rf + x0 + c] = Instance { glyph: s_h, fg: BORDER_FG, bg: POPUP_BG };
        }

        // Key hints
        let hints: &[(&str, &str)] = if popup_w >= 70 {
            &[("Enter", "Run"), ("Tab", "Insert"), ("Ctrl+R", "Sort"), ("Shift+Del", "Del"), ("Esc", "Close")]
        } else if popup_w >= 54 {
            &[("Enter", "Run"), ("Tab", "Insert"), ("Ctrl+R", "Sort"), ("Esc", "Close")]
        } else {
            &[("Enter", "Run"), ("Tab", "Edit"), ("Esc", "Close")]
        };

        let mut total_hint_len = 0;
        for (i, (key, label)) in hints.iter().enumerate() {
            if i > 0 {
                total_hint_len += 3; // " • "
            }
            total_hint_len += key.chars().count() + 2 + label.chars().count(); // "key: label"
        }
        total_hint_len += 2; // leading and trailing spaces

        if total_hint_len + 4 <= popup_w {
            let mut start_c = x0 + (popup_w - total_hint_len) / 2;
            let s_space = 0;
            self.instances[rf + start_c] = Instance { glyph: s_space, fg: FOOTER_FG, bg: POPUP_BG };
            start_c += 1;

            for (i, (key, label)) in hints.iter().enumerate() {
                if i > 0 {
                    // " • "
                    self.instances[rf + start_c] = Instance { glyph: s_space, fg: FOOTER_FG, bg: POPUP_BG };
                    let s_dot = self.atlas.slot(queue, '•' as u32) as u32;
                    self.instances[rf + start_c + 1] = Instance { glyph: s_dot, fg: DIM_FG, bg: POPUP_BG };
                    self.instances[rf + start_c + 2] = Instance { glyph: s_space, fg: FOOTER_FG, bg: POPUP_BG };
                    start_c += 3;
                }
                for ch in key.chars() {
                    let slot = self.atlas.slot(queue, ch as u32) as u32;
                    self.instances[rf + start_c] = Instance { glyph: slot, fg: KEY_FG, bg: POPUP_BG };
                    start_c += 1;
                }
                let s_colon = self.atlas.slot(queue, ':' as u32) as u32;
                self.instances[rf + start_c] = Instance { glyph: s_colon, fg: FOOTER_FG, bg: POPUP_BG };
                self.instances[rf + start_c + 1] = Instance { glyph: s_space, fg: FOOTER_FG, bg: POPUP_BG };
                start_c += 2;
                for ch in label.chars() {
                    let slot = self.atlas.slot(queue, ch as u32) as u32;
                    self.instances[rf + start_c] = Instance { glyph: slot, fg: FOOTER_FG, bg: POPUP_BG };
                    start_c += 1;
                }
            }
            self.instances[rf + start_c] = Instance { glyph: s_space, fg: FOOTER_FG, bg: POPUP_BG };
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

#[cfg(test)]
mod tests {
    #[test]
    fn test_shader_wgsl_syntax() {
        let shader_src = include_str!("shader.wgsl");
        let module = wgpu::naga::front::wgsl::parse_str(shader_src).expect("Failed to parse WGSL");
        let mut validator = wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        );
        let info = validator.validate(&module);
        assert!(info.is_ok(), "Failed to validate WGSL module: {:?}", info.err());
    }
}
