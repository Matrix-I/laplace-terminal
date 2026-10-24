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
    tab_info: u32,          // low 16 bits = tab_count (0 if single tab), high 16 bits = active_tab
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
    @location(4) @interpolate(flat) flags: u32, // bit 0 = underline, bit 1 = tab_bar_row, bits 16..31 = sxy.y
    @location(5) @interpolate(flat) col: u32,
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
    let tab_count = u.tab_info & 0xFFFFu;
    let is_tab_row = tab_count > 0u && row == 0u;

    var px: vec2<f32>;
    if (is_tab_row) {
        var x = (f32(col) + corner.x) * u.cell.x + u.offset.x;
        if (col == 0u && corner.x == 0.0) {
            x = 0.0;
        } else if (col == u.cols - 1u && corner.x == 1.0) {
            x = u.screen.x;
        }
        let total_top = u.cell.y * 2.0 + u.offset.y;
        px = vec2<f32>(x, corner.y * total_top);
    } else if (tab_count > 0u) {
        // Shift terminal rows down by 2 cell rows so there is a clean gap below the tab bar.
        px = vec2<f32>(
            (f32(col) + corner.x) * u.cell.x + u.offset.x,
            (f32(row + 1u) + corner.y) * u.cell.y + u.offset.y,
        );
    } else {
        px = (vec2<f32>(f32(col), f32(row)) + corner) * u.cell + u.offset;
    }

    var out: VsOut;
    out.pos = vec4<f32>(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0, 0.0, 1.0);
    let slot = glyph & 0xFFFFu;
    let sxy = vec2<f32>(f32(slot % u.slots_per_row), f32(slot / u.slots_per_row));
    out.uv = (sxy + corner) * u.cell / u.atlas_size;
    out.local = corner;

    var f = select(unpack(fg), unpack(u.default_fg), (fg & 0xFFu) == 0u);
    var b = select(unpack(bg), unpack(u.default_bg), (bg & 0x80u) == 0u);
    if (!is_tab_row && u.cursor_visible != 0u && col == u.cursor.x && row == u.cursor.y) {
        f = unpack(u.default_bg);
        b = vec3<f32>(0.44, 0.47, 0.59); // Ghostty slate-lavender block cursor
    }
    out.fg = f;
    out.bg = b;
    out.flags = (bg & 1u) | (select(0u, 2u, is_tab_row)) | (bg & 4u) | ((slot / u.slots_per_row) << 16u);
    out.col = col;
    return out;
}

@fragment
fn fs_main(v: VsOut) -> @location(0) vec4<f32> {
    if ((v.flags & 2u) != 0u) {
        let tab_count = u.tab_info & 0xFFFFu;
        let active_tab = u.tab_info >> 16u;
        let bar_h = floor(u.cell.y * 1.56);
        let term_bg = unpack(u.default_bg);

        // Clean breathing gap between the bottom of the tab bar and the first terminal line.
        if (v.pos.y >= bar_h) {
            return vec4<f32>(term_bg, 1.0);
        }

        let bar_bg = vec3<f32>(0.110, 0.114, 0.125); // #1c1d20 Ghostty tab bar backdrop
        var col_rgb = bar_bg;
        if (v.pos.y >= bar_h - 1.0) {
            return vec4<f32>(vec3<f32>(0.07, 0.075, 0.085), 1.0);
        }

        let cy = bar_h * 0.5;
        let half_h = floor(u.cell.y * 0.58);
        let tab_cols_total = max(u.cols - 2u, 1u);
        let plus_cx = u.offset.x + (f32(tab_cols_total) + 1.0) * u.cell.x;

        // Right-side circular (+) button.
        let d_plus = length(v.pos.xy - vec2<f32>(plus_cx, cy)) - half_h;
        if (d_plus < 1.0) {
            let fill_a = clamp(0.5 - d_plus, 0.0, 1.0);
            let border_a = clamp(1.0 - abs(d_plus + 0.6), 0.0, 1.0);
            let btn_fill = vec3<f32>(0.165, 0.169, 0.188);
            let btn_border = vec3<f32>(0.290, 0.298, 0.329);
            var btn_col = mix(btn_fill, btn_border, border_a);

            // Crisp '+' icon inside the circular button.
            let dp = abs(v.pos.xy - vec2<f32>(plus_cx, cy));
            let arm = floor(u.cell.y * 0.22);
            let thick = max(1.0, round(u.cell.y / 16.0)) * 0.55;
            if ((dp.x <= arm && dp.y <= thick) || (dp.y <= arm && dp.x <= thick)) {
                btn_col = vec3<f32>(0.92, 0.92, 0.95);
            }
            col_rgb = mix(bar_bg, btn_col, fill_a);
            return vec4<f32>(col_rgb, 1.0);
        }

        // Equal-width rounded capsule pill tabs across columns 0..tab_cols_total.
        if (v.col < tab_cols_total) {
            // Find which tab owns `v.col`.
            var tab_i = (v.col * tab_count) / tab_cols_total;
            let next_start = ((tab_i + 1u) * tab_cols_total) / tab_count;
            if (v.col >= next_start && tab_i + 1u < tab_count) {
                tab_i = tab_i + 1u;
            }
            let c0 = (tab_i * tab_cols_total) / tab_count;
            let c1 = ((tab_i + 1u) * tab_cols_total) / tab_count;

            let x0 = u.offset.x + f32(c0) * u.cell.x + 3.0;
            let x1 = u.offset.x + f32(c1) * u.cell.x - 3.0;
            let pill_cx = (x0 + x1) * 0.5;
            let half_w = max((x1 - x0) * 0.5, half_h);

            let q = abs(v.pos.xy - vec2<f32>(pill_cx, cy)) - vec2<f32>(max(half_w - half_h, 0.0), 0.0);
            let d_pill = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - half_h;

            let is_active = tab_i == active_tab;
            let pill_fill = select(
                vec3<f32>(0.141, 0.145, 0.161), // #242529 inactive pill fill
                vec3<f32>(0.204, 0.208, 0.231), // #34353b active pill fill
                is_active,
            );
            let pill_border = select(
                vec3<f32>(0.208, 0.212, 0.235), // #35363c inactive subtle border
                vec3<f32>(0.365, 0.373, 0.412), // #5d5f69 active pill border
                is_active,
            );

            let fill_a = clamp(0.5 - d_pill, 0.0, 1.0);
            let border_a = clamp(1.0 - abs(d_pill + 0.65), 0.0, 1.0);
            var pill_col = mix(pill_fill, pill_border, border_a);

            // Sample centered 1:1 glyph vertically inside the pill.
            let text_y0 = floor(cy - u.cell.y * 0.5);
            let gy = v.pos.y - text_y0;
            if (gy >= 0.0 && gy < u.cell.y && d_pill < -1.5) {
                let slot_y = f32(v.flags >> 16u);
                let uv = vec2<f32>(v.uv.x, (slot_y * u.cell.y + gy) / u.atlas_size.y);
                let ga = textureSampleLevel(atlas, samp, uv, 0.0).r;
                pill_col = mix(pill_col, v.fg, ga);
            }

            col_rgb = mix(bar_bg, pill_col, fill_a);
        }
        return vec4<f32>(col_rgb, 1.0);
    }

    var a = textureSampleLevel(atlas, samp, v.uv, 0.0).r;
    let thickness = max(1.0, floor(u.cell.y / 14.0));
    if ((v.flags & 1u) != 0u && v.local.y * u.cell.y >= u.cell.y - thickness) {
        a = 1.0;
    }
    var cell_color = mix(v.bg, v.fg, a);

    if ((v.flags & 4u) != 0u) {
        // Ghostty bottom-left floating preview badge styling:
        // Top border 1px
        if (v.local.y * u.cell.y <= 1.0) {
            cell_color = vec3<f32>(0.32, 0.35, 0.46); // Crisp top border #525975
        }
    }

    return vec4<f32>(cell_color, 1.0);
}
