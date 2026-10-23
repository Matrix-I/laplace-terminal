// Instanced cell renderer: one quad (triangle strip, 4 vertices) per cell,
// the whole screen in a single draw call. Cell position comes from
// instance_index; glyph pixels come from the R8 atlas.

struct Uniforms {
    screen: vec2<f32>,      // surface size in px
    cell: vec2<f32>,        // cell size in px
    atlas_size: vec2<f32>,  // atlas texture size in px
    offset: vec2<f32>,      // padding in px
    cols: u32,
    slots_per_row: u32,
    cursor: vec2<u32>,
    cursor_visible: u32,
    default_fg: u32,
    default_bg: u32,
    _pad: u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) @interpolate(flat) fg: vec3<f32>,
    @location(3) @interpolate(flat) bg: vec3<f32>,
    @location(4) @interpolate(flat) underline: u32,
};

fn unpack(c: u32) -> vec3<f32> {
    return vec3<f32>(f32((c >> 24u) & 255u), f32((c >> 16u) & 255u), f32((c >> 8u) & 255u)) / 255.0;
}

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    @builtin(instance_index) ii: u32,
    @location(0) glyph: u32,
    @location(1) fg: u32,
    @location(2) bg: u32,
) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
    let col = ii % u.cols;
    let row = ii / u.cols;
    let px = (vec2<f32>(f32(col), f32(row)) + corner) * u.cell + u.offset;

    var out: VsOut;
    out.pos = vec4<f32>(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0, 0.0, 1.0);
    let slot = glyph & 0xFFFFu;
    let sxy = vec2<f32>(f32(slot % u.slots_per_row), f32(slot / u.slots_per_row));
    out.uv = (sxy + corner) * u.cell / u.atlas_size;
    out.local = corner;

    var f = select(unpack(fg), unpack(u.default_fg), (fg & 0xFFu) == 0u);
    var b = select(unpack(bg), unpack(u.default_bg), (bg & 0x80u) == 0u);
    if (u.cursor_visible != 0u && col == u.cursor.x && row == u.cursor.y) {
        let t = f;
        f = b;
        b = t;
    }
    out.fg = f;
    out.bg = b;
    out.underline = bg & 1u;
    return out;
}

@fragment
fn fs_main(v: VsOut) -> @location(0) vec4<f32> {
    var a = textureSampleLevel(atlas, samp, v.uv, 0.0).r;
    let thickness = max(1.0, floor(u.cell.y / 14.0));
    if (v.underline != 0u && v.local.y * u.cell.y >= u.cell.y - thickness) {
        a = 1.0;
    }
    return vec4<f32>(mix(v.bg, v.fg, a), 1.0);
}
