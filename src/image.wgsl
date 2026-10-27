// Inline images (Kitty graphics, Sixel): one textured quad per placement,
// drawn after the cells.

struct ImageUniforms {
    screen: vec2<f32>,  // target size in px
    flip_y: u32,        // 1 = drawing into a custom-shader texture (bottom row first)
    pad: u32,
};

@group(0) @binding(0) var<uniform> u: ImageUniforms;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    @location(0) rect: vec4<f32>,  // x, y, width, height in px
    @location(1) uv: vec4<f32>,    // u0, v0, u1, v1
) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
    let px = rect.xy + corner * rect.zw;
    var out: VsOut;
    out.pos = vec4<f32>(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0, 0.0, 1.0);
    if (u.flip_y != 0u) {
        out.pos.y = -out.pos.y;
    }
    out.uv = mix(uv.xy, uv.zw, corner);
    return out;
}

@fragment
fn fs_main(v: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, v.uv);
}
