//! GPU side of inline images: one texture per image (uploaded once, dropped
//! when unused for a while) and one textured quad per visible placement,
//! drawn in the cell render pass below or above the cells by z.

use std::collections::HashMap;

use crate::graphics::ImageDraw;

/// Renders without an image before its texture is freed.
const KEEP_FRAMES: u64 = 600;

#[repr(C)]
#[derive(Copy, Clone, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Quad {
    /// x, y, width, height in px.
    rect: [f32; 4],
    /// u0, v0, u1, v1.
    uv: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    screen: [f32; 2],
    flip_y: u32,
    _pad: u32,
}

/// One placement to draw: the image, where (px) and the pane it is clipped to.
pub struct Placed {
    pub draw: ImageDraw,
    pub rect: [f32; 4],
    pub clip: [u32; 4],
}

pub struct ImageLayer {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    quads: wgpu::Buffer,
    quads_cap: usize,
    texture_format: wgpu::TextureFormat,
    /// Image key → bind group with its texture, and the frame it was last drawn.
    textures: HashMap<u64, (wgpu::BindGroup, u64)>,
    frame: u64,
    /// This frame's draws: image key, scissor rectangle and z, in quad order.
    batch: Vec<(u64, [u32; 4], i32)>,
}

impl ImageLayer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("image.wgsl"));
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("images"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
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
            label: Some("images"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("images"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Quad>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4],
                }],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
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
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // Image bytes are sRGB, like the colors written to an sRGB surface.
        let texture_format =
            if format.is_srgb() { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm };
        ImageLayer {
            pipeline,
            bgl,
            sampler,
            uniforms,
            quads: make_quads(device, 16),
            quads_cap: 16,
            texture_format,
            textures: HashMap::new(),
            frame: 0,
            batch: Vec::new(),
        }
    }

    /// Discard all cached GPU textures and batches (e.g. when shrinking drawables or idle in Dock).
    pub fn clear(&mut self) {
        self.textures.clear();
        self.batch.clear();
    }

    /// Upload new images and this frame's quads. `screen` is the target size.
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, placed: &[Placed], screen: (u32, u32), flip: bool) {
        self.frame += 1;
        self.batch.clear();
        if placed.is_empty() {
            self.textures.retain(|_, (_, used)| self.frame - *used < KEEP_FRAMES);
            return;
        }
        let u = Uniforms { screen: [screen.0 as f32, screen.1 as f32], flip_y: flip as u32, _pad: 0 };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
        if placed.len() > self.quads_cap {
            self.quads_cap = placed.len().next_power_of_two();
            self.quads = make_quads(device, self.quads_cap);
        }
        let mut quads = Vec::with_capacity(placed.len());
        for p in placed {
            let d = &p.draw;
            if !self.textures.contains_key(&d.key) {
                let bind_group = self.upload(device, queue, d);
                self.textures.insert(d.key, (bind_group, self.frame));
            }
            if let Some(entry) = self.textures.get_mut(&d.key) {
                entry.1 = self.frame;
            }
            let (w, h) = (d.width as f32, d.height as f32);
            let [sx, sy, sw, sh] = d.src;
            quads.push(Quad { rect: p.rect, uv: [sx / w, sy / h, (sx + sw) / w, (sy + sh) / h] });
            // Clip to the pane and the target (wgpu rejects scissors outside it).
            let [cx, cy, cw, ch] = p.clip;
            let (x, y) = (cx.min(screen.0), cy.min(screen.1));
            let clip = [x, y, cw.min(screen.0 - x), ch.min(screen.1 - y)];
            self.batch.push((d.key, clip, d.z));
        }
        queue.write_buffer(&self.quads, 0, bytemuck::cast_slice(&quads));
        self.textures.retain(|_, (_, used)| self.frame - *used < KEEP_FRAMES);
    }

    fn upload(&self, device: &wgpu::Device, queue: &wgpu::Queue, d: &ImageDraw) -> wgpu::BindGroup {
        let size = wgpu::Extent3d { width: d.width, height: d.height, depth_or_array_layers: 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.texture_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &d.rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * d.width), rows_per_image: Some(d.height) },
            size,
        );
        let view = texture.create_view(&Default::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    /// Draw the prepared quads whose z is in `z` (inside the cell render pass).
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, flip: bool, height: u32, z: std::ops::Range<i32>) {
        if !self.batch.iter().any(|b| z.contains(&b.2)) {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.quads.slice(..));
        for (i, (key, clip, zi)) in self.batch.iter().enumerate() {
            let Some((bind_group, _)) = self.textures.get(key) else { continue };
            if !z.contains(zi) {
                continue;
            }
            let Some([sx, sy, sw, sh]) = compute_scissor_rect(*clip, flip, height) else {
                continue;
            };
            pass.set_scissor_rect(sx, sy, sw, sh);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..4, i as u32..i as u32 + 1);
        }
    }
}

pub(crate) fn compute_scissor_rect([x, y, w, h]: [u32; 4], flip: bool, height: u32) -> Option<[u32; 4]> {
    if w == 0 || h == 0 {
        return None;
    }
    let clamped_h = h.min(height.saturating_sub(y));
    if clamped_h == 0 {
        return None;
    }
    let scissor_y = if flip {
        height.checked_sub(y + clamped_h)?
    } else {
        y
    };
    if scissor_y.saturating_add(clamped_h) > height {
        return None;
    }
    Some([x, scissor_y, w, clamped_h])
}

fn make_quads(device: &wgpu::Device, n: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("image quads"),
        size: (n.max(1) * std::mem::size_of::<Quad>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_scissor_rect_flipped_underflow_prevention() {
        // Normal in-bounds
        assert_eq!(compute_scissor_rect([10, 20, 30, 40], true, 100), Some([10, 40, 30, 40]));
        assert_eq!(compute_scissor_rect([10, 20, 30, 40], false, 100), Some([10, 20, 30, 40]));

        // Rect partially exceeds target height (would underflow in naive height - y - h)
        assert_eq!(compute_scissor_rect([10, 80, 30, 50], true, 100), Some([10, 0, 30, 20]));
        assert_eq!(compute_scissor_rect([10, 80, 30, 50], false, 100), Some([10, 80, 30, 20]));

        // Completely outside target
        assert_eq!(compute_scissor_rect([10, 100, 30, 50], true, 100), None);
        assert_eq!(compute_scissor_rect([10, 150, 30, 50], true, 100), None);

        // Zero target height or zero rect dimensions
        assert_eq!(compute_scissor_rect([10, 0, 30, 50], true, 0), None);
        assert_eq!(compute_scissor_rect([10, 20, 0, 40], true, 100), None);
        assert_eq!(compute_scissor_rect([10, 20, 30, 0], true, 100), None);
    }
}
