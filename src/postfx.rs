//! `custom-shader`: Ghostty / Shadertoy-style GLSL post-processing passes.
//!
//! The cell pass renders into an offscreen texture instead of the window; each
//! shader then runs as a full-screen pass that samples the previous result as
//! `iChannel0` (`mainImage(out vec4 fragColor, in vec2 fragCoord)`), and the
//! last pass draws to the window. Textures are stored bottom row first, so
//! Shadertoy's bottom-left `fragCoord` and `texture(iChannel0, uv)` agree.
//!
//! Animation (`iTime`) means a redraw per frame; it only runs while a shader
//! uses `iTime` / `iFrame` and `custom-shader-animation` allows it.

use std::path::Path;

/// Uniforms every pass sees (std140, matches `PRELUDE`).
#[repr(C)]
#[derive(Copy, Clone, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    resolution: [f32; 4],
    mouse: [f32; 4],
    date: [f32; 4],
    time: f32,
    time_delta: f32,
    frame: i32,
    /// 1 when this pass draws to the window (flip `fragCoord` to bottom-left).
    to_screen: u32,
}

const _: () = assert!(std::mem::size_of::<Globals>() == 64);

/// Declarations Shadertoy shaders expect, then the user's code, then `main`.
const PRELUDE: &str = "#version 450
layout(set = 0, binding = 0, std140) uniform LaplaceGlobals {
    vec4 _laplace_resolution;
    vec4 iMouse;
    vec4 iDate;
    float iTime;
    float iTimeDelta;
    int iFrame;
    uint _laplace_to_screen;
};
layout(set = 0, binding = 1) uniform texture2D _laplace_tex;
layout(set = 0, binding = 2) uniform sampler _laplace_smp;
#define iResolution (_laplace_resolution.xyz)
#define iChannel0 sampler2D(_laplace_tex, _laplace_smp)
#define iChannelResolution (vec3[4](_laplace_resolution.xyz, vec3(0.0), vec3(0.0), vec3(0.0)))
layout(location = 0) out vec4 _laplace_out;
#line 1
";

const MAIN: &str = "
void main() {
    vec2 fc = gl_FragCoord.xy;
    // Texture rows are stored bottom first; only the window wants top first.
    if (_laplace_to_screen != 0u) {
        fc.y = _laplace_resolution.y - fc.y;
    }
    vec4 color = vec4(0.0);
    mainImage(color, fc);
    _laplace_out = color;
}
";

const VERTEX: &str = "
@vertex
fn main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering the whole target.
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
";

/// Full GLSL source for one user shader.
fn wrap(user: &str) -> String {
    let mut src = String::with_capacity(PRELUDE.len() + user.len() + MAIN.len());
    src.push_str(PRELUDE);
    src.push_str(user);
    src.push_str(MAIN);
    src
}

/// Parse and validate a wrapped shader, so a broken file is reported (and
/// skipped) instead of failing inside wgpu.
fn check(src: &str) -> Result<(), String> {
    use wgpu::naga;
    let mut frontend = naga::front::glsl::Frontend::default();
    let options = naga::front::glsl::Options::from(naga::ShaderStage::Fragment);
    let module = frontend.parse(&options, src).map_err(|e| e.emit_to_string(src))?;
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// Whether the shader changes over time.
fn uses_time(user: &str) -> bool {
    ["iTime", "iFrame", "iTimeDelta", "iDate"].iter().any(|n| user.contains(n))
}

/// Read, wrap and check every `custom-shader` file; broken ones are reported
/// and left out. Returns the sources and whether any of them animates.
pub fn load(paths: &[std::path::PathBuf]) -> (Vec<String>, bool) {
    let mut sources = Vec::new();
    let mut animated = false;
    for path in paths {
        match std::fs::read_to_string(path) {
            Ok(user) => {
                let src = wrap(&user);
                match check(&src) {
                    Ok(()) => {
                        animated |= uses_time(&user);
                        sources.push(src);
                    }
                    Err(e) => eprintln!("laplace: custom-shader {}: {e}", display(path)),
                }
            }
            Err(e) => eprintln!("laplace: custom-shader {}: {e}", display(path)),
        }
    }
    (sources, animated)
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

pub struct PostFx {
    pipelines: Vec<wgpu::RenderPipeline>,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// One uniform buffer per pass (`to_screen` differs).
    uniforms: Vec<wgpu::Buffer>,
    /// Ping-pong targets; `targets[0]` receives the cell pass.
    targets: Vec<wgpu::TextureView>,
    /// Cached bind groups per pass, created alongside targets when size changes.
    bind_groups: Vec<wgpu::BindGroup>,
    format: wgpu::TextureFormat,
    size: (u32, u32),
    start: std::time::Instant,
    last_frame: std::time::Instant,
    frame: i32,
}

impl PostFx {
    /// Build the passes for already checked `sources` (see [`load`]); `None`
    /// when there are none.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, sources: &[String]) -> Option<Self> {
        if sources.is_empty() {
            return None;
        }
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("postfx"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
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
            label: Some("postfx"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let vertex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("postfx vertex"),
            source: wgpu::ShaderSource::Wgsl(VERTEX.into()),
        });
        let pipelines = sources
            .iter()
            .map(|src| {
                let fragment = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("custom-shader"),
                    source: wgpu::ShaderSource::Glsl {
                        shader: src.as_str().into(),
                        stage: wgpu::naga::ShaderStage::Fragment,
                        defines: Default::default(),
                    },
                });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("custom-shader"),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &vertex,
                        entry_point: Some("main"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &fragment,
                        entry_point: Some("main"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview: None,
                    cache: None,
                })
            })
            .collect::<Vec<_>>();
        let uniforms = (0..pipelines.len())
            .map(|_| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("postfx globals"),
                    size: std::mem::size_of::<Globals>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let now = std::time::Instant::now();
        Some(PostFx {
            pipelines,
            bgl,
            sampler,
            uniforms,
            targets: Vec::new(),
            bind_groups: Vec::new(),
            format,
            size: (0, 0),
            start: now,
            last_frame: now,
            frame: 0,
        })
    }

    /// Discard offscreen ping-pong render targets and cached bind groups to release VRAM when window shrinks.
    pub fn shrink(&mut self) {
        self.targets.clear();
        self.bind_groups.clear();
        self.size = (0, 0);
    }

    /// The texture the cell pass draws into, (re)created at the window size.
    pub fn cell_target(&mut self, device: &wgpu::Device, width: u32, height: u32) -> &wgpu::TextureView {
        // The last pass draws to the window, so N passes need N targets at most 2.
        let wanted = self.pipelines.len().min(2);
        if self.size != (width, height) || self.targets.len() != wanted {
            self.size = (width, height);
            self.targets = (0..wanted)
                .map(|_| {
                    let texture = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("postfx target"),
                        size: wgpu::Extent3d { width: width.max(1), height: height.max(1), depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: self.format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    });
                    texture.create_view(&Default::default())
                })
                .collect();
            self.bind_groups = (0..self.pipelines.len())
                .map(|i| {
                    let source = &self.targets[i % 2];
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("postfx"),
                        layout: &self.bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: self.uniforms[i].as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(source) },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                        ],
                    })
                })
                .collect();
        }
        &self.targets[0]
    }

    /// Run every pass after the cell pass, ending on `screen`.
    pub fn run(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        screen: &wgpu::TextureView,
        mouse: (f32, f32),
    ) {
        let now = std::time::Instant::now();
        let (w, h) = self.size;
        let date = date_now();
        let mut globals = Globals {
            resolution: [w as f32, h as f32, 1.0, 0.0],
            mouse: [mouse.0, h as f32 - mouse.1, 0.0, 0.0],
            date,
            time: (now - self.start).as_secs_f32(),
            time_delta: (now - self.last_frame).as_secs_f32(),
            frame: self.frame,
            to_screen: 0,
        };
        self.last_frame = now;
        self.frame = self.frame.wrapping_add(1);

        let last = self.pipelines.len() - 1;
        for (i, pipeline) in self.pipelines.iter().enumerate() {
            globals.to_screen = (i == last) as u32;
            queue.write_buffer(&self.uniforms[i], 0, bytemuck::bytes_of(&globals));
            let dest = if i == last { screen } else { &self.targets[(i + 1) % 2] };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("custom-shader"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dest,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &self.bind_groups[i], &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

/// Shadertoy's `iDate`: year, month (0-based), day, seconds since midnight (UTC).
fn date_now() -> [f32; 4] {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    [year as f32, (month - 1) as f32, day as f32, (secs % 86_400) as f32]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadertoy_shaders_compile() {
        let invert = "void mainImage(out vec4 fragColor, in vec2 fragCoord) {
            vec2 uv = fragCoord / iResolution.xy;
            vec4 c = texture(iChannel0, uv);
            fragColor = vec4(1.0 - c.rgb, c.a);
        }";
        check(&wrap(invert)).unwrap();
        assert!(!uses_time(invert));

        let crt = "void mainImage(out vec4 fragColor, in vec2 fragCoord) {
            vec2 uv = fragCoord.xy / iResolution.xy;
            float scan = 0.9 + 0.1 * sin(uv.y * iResolution.y * 3.14159 + iTime);
            fragColor = texture(iChannel0, uv) * scan;
        }";
        check(&wrap(crt)).unwrap();
        assert!(uses_time(crt));

        assert!(check(&wrap("void mainImage(out vec4 c, in vec2 f) { c = nope; }")).is_err());
    }

    #[test]
    fn date_is_plausible() {
        let [year, month, day, secs] = date_now();
        assert!(year >= 2024.0 && (0.0..12.0).contains(&month) && (1.0..=31.0).contains(&day));
        assert!((0.0..86_400.0).contains(&secs));
    }
}
