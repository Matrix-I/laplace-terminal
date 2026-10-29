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
    cursor_style: u32,      // 0 = block, 1 = bar, 2 = underline, 3 = block_hollow
    default_fg: u32,
    default_bg: u32,
    tab_info: u32,          // low 16 bits = tab_count (0 if single tab), high 16 bits = active_tab
    bg_opacity: f32,
    underline_y: f32,       // underline top, px from the cell top
    strike_y: f32,          // strikethrough center, px from the cell top
    cursor_color: u32,      // OSC 12 color as 0xRRGGBB00 | 1, or 0 for the built-in color
    unfocused_dim: f32,     // 1 - unfocused-split-opacity
    unfocused_fill: u32,    // 0xRRGGBB00 | 1, or 0 for the default background
    min_contrast: f32,      // minimum-contrast ratio, 1 = off
    progress: u32,          // OSC 9;4 of the active pane: state << 8 | percent, 0 = none
    progress_row: u32,      // active pane's first instance row,
    progress_col: u32,      // first column
    progress_cols: u32,     // and width
    bell_border: u32,       // 1 = outline the window (bell-features = border)
    flip_y: u32,            // 1 = draw upside down (into a custom-shader texture)
    layer: u32,             // 0 = everything, 1 = backgrounds only, 2 = text only (images under the text)
    pad2: u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var color_atlas: texture_2d<f32>;  // premultiplied RGBA emoji, same slots

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) @interpolate(flat) fg: vec3<f32>,
    @location(3) @interpolate(flat) bg: vec3<f32>,
    // bits 0..3 = underline style, bit 3 = strikethrough, bits 4..7 = dividers, bit 7 = tab_bar_row,
    // bits 8..11 = cursor style, bit 11 = scrollbar thumb, bit 12 = unfocused split,
    // bit 13 = color glyph, bits 16..31 = sxy.y
    @location(4) @interpolate(flat) flags: u32,
    @location(5) @interpolate(flat) col: u32,
    @location(6) @interpolate(flat) ul: vec3<f32>,
    @location(7) @interpolate(flat) row: u32,
    // Unstretched top-left of the cell in px and its atlas slot origin in uv:
    // edge quads may be stretched over the padding (bell border).
    @location(8) @interpolate(flat) origin: vec2<f32>,
    @location(9) @interpolate(flat) uv0: vec2<f32>,
};

fn unpack(c: u32) -> vec3<f32> {
    return vec3<f32>(f32((c >> 24u) & 255u), f32((c >> 16u) & 255u), f32((c >> 8u) & 255u)) / 255.0;
}

// Cursor color: set by OSC 12, else Ghostty's slate-lavender.
fn cursor_rgb() -> vec3<f32> {
    return select(vec3<f32>(0.44, 0.47, 0.59), unpack(u.cursor_color), (u.cursor_color & 1u) != 0u);
}

// WCAG relative luminance and contrast ratio.
fn luminance(c: vec3<f32>) -> f32 {
    return dot(srgb_to_linear(c), vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn contrast(a: vec3<f32>, b: vec3<f32>) -> f32 {
    let la = luminance(a);
    let lb = luminance(b);
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05);
}

// Text color with at least `u.min_contrast` against `bg`: black or white,
// whichever contrasts more, when the color itself falls short.
fn ensure_contrast(fg: vec3<f32>, bg: vec3<f32>) -> vec3<f32> {
    if (u.min_contrast <= 1.0 || contrast(fg, bg) >= u.min_contrast) {
        return fg;
    }
    let white = contrast(vec3<f32>(1.0), bg);
    let black = contrast(vec3<f32>(0.0), bg);
    return select(vec3<f32>(0.0), vec3<f32>(1.0), white > black);
}

// Exact sRGB <-> Linear color conversions for gamma-correct alpha blending
// (matches Ghostty `alpha-blending = native/linear`, eliminates dark halos).
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let cutoff = c <= vec3<f32>(0.04045);
    let higher = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    let lower = c / 12.92;
    return select(higher, lower, cutoff);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let cutoff = c <= vec3<f32>(0.0031308);
    let higher = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    let lower = c * 12.92;
    return select(higher, lower, cutoff);
}

// Coverage (0..1) of an underline of `style` (1 single, 2 double, 3 curly, 4 dotted,
// 5 dashed) at `px` inside the cell. Patterns repeat per cell so they tile seamlessly.
fn underline_coverage(style: u32, px: vec2<f32>, t: f32) -> f32 {
    if (style == 0u) {
        return 0.0;
    }
    // Room below the line: double needs two lines with a gap, curly needs its wave amplitude.
    let amp = max(1.0, floor(t * 1.25));
    let gap = max(2.0, 2.0 * t);
    var room = t;
    if (style == 2u) {
        room = 2.0 * t + gap;
    } else if (style == 3u) {
        room = 2.0 * amp + t;
    }
    let y0 = max(0.0, min(u.underline_y, u.cell.y - room));
    let in_line = px.y >= y0 && px.y < y0 + t;
    if (style == 2u) {
        let y1 = y0 + t + gap;
        return select(0.0, 1.0, in_line || (px.y >= y1 && px.y < y1 + t));
    }
    if (style == 3u) {
        // One sine period per cell; distance to the curve, corrected for its slope, gives AA.
        let k = 6.2831853 / u.cell.x;
        let center = y0 + amp + t * 0.5;
        let wave = center - amp * sin(px.x * k);
        let slope = amp * k * cos(px.x * k);
        let d = abs(px.y - wave) / sqrt(1.0 + slope * slope);
        return clamp(t * 0.5 + 0.5 - d, 0.0, 1.0);
    }
    if (!in_line) {
        return 0.0;
    }
    if (style == 4u) {
        // Dots as long as the line is thick, with equal gaps.
        return select(0.0, 1.0, fract(floor(px.x / t) * 0.5) < 0.25);
    }
    if (style == 5u) {
        // One dash per cell, centered, with a gap on either side.
        let x = px.x / u.cell.x;
        return select(0.0, 1.0, x >= 0.2 && x < 0.8);
    }
    return 1.0;
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
    let cols = max(u.cols, 1u);
    let col = ii % cols;
    let row = ii / cols;
    let tab_count = u.tab_info & 0xFFFFu;
    let is_tab_row = tab_count > 0u && row == 0u;
    let is_preview = (bg & 16u) != 0u;

    var px: vec2<f32>;
    if (is_tab_row) {
        var x = (f32(col) + corner.x) * u.cell.x + u.offset.x;
        if (col == 0u && corner.x == 0.0) {
            x = 0.0;
        } else if (u.cols > 0u && col == u.cols - 1u && corner.x == 1.0) {
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
    let origin = px - corner * u.cell;
    // Bell border: stretch the outermost cells over the padding so it can be drawn there.
    if (u.bell_border != 0u && !is_tab_row && !is_preview) {
        if (col == 0u && corner.x == 0.0) {
            px.x = 0.0;
        } else if (corner.x == 1.0 && px.x + u.cell.x > u.screen.x) {
            px.x = u.screen.x;
        }
        if (row == 0u && corner.y == 0.0) {
            px.y = 0.0;
        } else if (corner.y == 1.0 && px.y + u.cell.y > u.screen.y) {
            px.y = u.screen.y;
        }
    }

    var out: VsOut;
    out.pos = vec4<f32>(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0, 0.0, 1.0);
    if (u.flip_y != 0u) {
        out.pos.y = -out.pos.y;
    }
    let slot = glyph & 0xFFFFu;
    let sxy = vec2<f32>(f32(slot % u.slots_per_row), f32(slot / u.slots_per_row));
    out.uv = (sxy + corner) * u.cell / u.atlas_size;
    out.local = corner;
    out.origin = origin;
    out.uv0 = sxy * u.cell / u.atlas_size;

    // fg low byte: bit 7 = explicit color, bits 0-1 = bold / italic (atlas variants),
    // bit 2 = color glyph.
    var f = select(unpack(fg), unpack(u.default_fg), (fg & 0x80u) == 0u);
    var b = select(unpack(bg), unpack(u.default_bg), (bg & 0x80u) == 0u);
    let is_cursor = !is_tab_row && !is_preview && u.cursor_visible != 0u && col == u.cursor.x && row == u.cursor.y;
    if (is_cursor && u.cursor_style == 0u) {
        f = unpack(u.default_bg);
        b = cursor_rgb();
    }
    if (!is_tab_row && !is_preview) {
        f = ensure_contrast(f, b);
    }
    out.fg = f;
    out.bg = b;
    // Underline color: 4-bit RGB in glyph bits 19..31 when bit 31 is set, else the text color.
    let ul = (glyph >> 19u) & 0xFFFu;
    out.ul = select(f, vec3<f32>(f32(ul >> 8u), f32((ul >> 4u) & 15u), f32(ul & 15u)) / 15.0, (glyph >> 31u) != 0u);
    let cursor_flag = select(0u, (u.cursor_style + 1u) << 8u, is_cursor);
    let divider_flags = (glyph >> 16u) & 7u;
    out.flags = (bg & 15u) | (select(0u, 2048u, (bg & 32u) != 0u)) | (select(0u, 4096u, (bg & 64u) != 0u)) | (select(0u, 8192u, (fg & 4u) != 0u)) | (select(0u, 128u, is_tab_row)) | (divider_flags << 4u) | cursor_flag | ((slot / u.slots_per_row) << 16u);
    out.col = col;
    out.row = row;
    return out;
}

@fragment
fn fs_main(v: VsOut) -> @location(0) vec4<f32> {
    // Pixel position in screen space, top-left origin, even when drawing flipped.
    let fp = vec2<f32>(v.pos.x, select(v.pos.y, u.screen.y - v.pos.y, u.flip_y != 0u));
    // Unseen bell: an amber outline around the window.
    if (u.bell_border != 0u) {
        let w = max(2.0, floor(u.cell.y / 8.0));
        let p = fp;
        if (p.x < w || p.y < w || p.x >= u.screen.x - w || p.y >= u.screen.y - w) {
            return vec4<f32>(0.96, 0.69, 0.25, 1.0);
        }
    }

    if ((v.flags & 128u) != 0u) {
        if (u.layer == 2u) {
            discard;
        }
        let tab_count = u.tab_info & 0xFFFFu;
        let active_tab = u.tab_info >> 16u;
        let bar_h = floor(u.cell.y * 1.56);
        let term_bg = unpack(u.default_bg);

        // Clean breathing gap between the bottom of the tab bar and the first terminal line.
        if (fp.y >= bar_h) {
            return vec4<f32>(term_bg, 1.0);
        }

        let bar_bg = vec3<f32>(0.110, 0.114, 0.125); // #1c1d20 Ghostty tab bar backdrop
        var col_rgb = bar_bg;
        if (fp.y >= bar_h - 1.0) {
            return vec4<f32>(vec3<f32>(0.07, 0.075, 0.085), 1.0);
        }

        let cy = bar_h * 0.5;
        let half_h = floor(u.cell.y * 0.58);
        var tab_cols_total = 1u;
        if (u.cols > 2u) {
            tab_cols_total = u.cols - 2u;
        }
        let plus_cx = u.offset.x + (f32(tab_cols_total) + 1.0) * u.cell.x;

        // Right-side circular (+) button.
        let d_plus = length(fp - vec2<f32>(plus_cx, cy)) - half_h;
        if (d_plus < 1.0) {
            let fill_a = clamp(0.5 - d_plus, 0.0, 1.0);
            let border_a = clamp(1.0 - abs(d_plus + 0.6), 0.0, 1.0);
            let btn_fill = vec3<f32>(0.165, 0.169, 0.188);
            let btn_border = vec3<f32>(0.290, 0.298, 0.329);
            var btn_col = mix(btn_fill, btn_border, border_a);

            // Crisp '+' icon inside the circular button.
            let dp = abs(fp - vec2<f32>(plus_cx, cy));
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

            let q = abs(fp - vec2<f32>(pill_cx, cy)) - vec2<f32>(max(half_w - half_h, 0.0), 0.0);
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
            let gy = fp.y - text_y0;
            if (gy >= 0.0 && gy < u.cell.y && d_pill < -1.5) {
                let slot_y = f32(v.flags >> 16u);
                let uv = vec2<f32>(v.uv.x, (slot_y * u.cell.y + gy) / u.atlas_size.y);
                let ga = textureSampleLevel(atlas, samp, uv, 0.0).r;
                if (ga >= 1.0) {
                    pill_col = v.fg;
                } else if (ga > 0.0) {
                    let pill_lin = srgb_to_linear(pill_col);
                    let fg_lin = srgb_to_linear(v.fg);
                    pill_col = linear_to_srgb(mix(pill_lin, fg_lin, ga));
                }
            }

            col_rgb = mix(bar_bg, pill_col, fill_a);
        }
        return vec4<f32>(col_rgb, 1.0);
    }

    // Position inside the cell, from the fragment itself (edge quads may be stretched).
    let local = (fp - v.origin) / u.cell;
    if (any(local < vec2<f32>(0.0)) || any(local >= vec2<f32>(1.0))) {
        // Padding covered by a stretched edge cell (the clear color already fills it in layers).
        if (u.layer != 0u) {
            discard;
        }
        return vec4<f32>(unpack(u.default_bg), u.bg_opacity);
    }
    let glyph_uv = v.uv0 + local * u.cell / u.atlas_size;
    var a = textureSampleLevel(atlas, samp, glyph_uv, 0.0).r;
    let is_color = (v.flags & 8192u) != 0u;
    var emoji = vec4<f32>(0.0);
    if (is_color) {
        emoji = textureSampleLevel(color_atlas, samp, glyph_uv, 0.0);
        a = emoji.a;
    }
    let px = local * u.cell;
    let thickness = max(1.0, floor(u.cell.y / 14.0));
    if ((v.flags & 8u) != 0u && abs(px.y - u.strike_y) <= thickness * 0.5) {
        a = 1.0;
    }
    if (u.layer == 1u) {
        a = 0.0;
    }
    // Text-only layer: the glyph is blended over what is already drawn, not over v.bg.
    let base_bg = select(v.bg, v.fg, u.layer == 2u);
    // Set by overlays that cover the pixel completely (cursor, dividers, bars).
    var solid = false;
    var cell_color: vec3<f32>;
    if (a <= 0.0) {
        cell_color = base_bg;
    } else if (a >= 1.0) {
        cell_color = v.fg;
    } else {
        let fg_lin = srgb_to_linear(v.fg);
        let bg_lin = srgb_to_linear(base_bg);
        let cell_lin = mix(bg_lin, fg_lin, a);
        cell_color = linear_to_srgb(cell_lin);
    }
    if (is_color && a > 0.0) {
        // Premultiplied over the background; the text layer blends it itself.
        cell_color = select(emoji.rgb + base_bg * (1.0 - a), emoji.rgb / a, u.layer == 2u);
    }

    var ul_cov = 0.0;
    if (u.layer != 1u) {
        ul_cov = underline_coverage(v.flags & 7u, px, thickness);
    }
    if (ul_cov > 0.0) {
        cell_color = linear_to_srgb(mix(srgb_to_linear(cell_color), srgb_to_linear(v.ul), ul_cov));
        a = max(a, ul_cov);
    }

    // Non-block cursor styling (1=block handled in vs_main, 2=bar, 3=underline, 4=block_hollow)
    let c_style = (v.flags >> 8u) & 7u;
    if (c_style != 0u) {
        let cursor_color = cursor_rgb();
        if (c_style == 2u) {
            // Bar cursor: vertical beam on left edge of cell
            if (local.x * u.cell.x <= 2.0) {
                cell_color = cursor_color;
                solid = true;
            }
        } else if (c_style == 3u) {
            // Underline cursor: horizontal line at bottom of cell
            if (local.y * u.cell.y >= u.cell.y - 2.0) {
                cell_color = cursor_color;
                solid = true;
            }
        } else if (c_style == 4u) {
            // Block hollow: outline border
            let lx = local.x * u.cell.x;
            let ly = local.y * u.cell.y;
            if (lx <= 1.5 || lx >= u.cell.x - 1.5 || ly <= 1.5 || ly >= u.cell.y - 1.5) {
                cell_color = cursor_color;
                solid = true;
            }
        }
    }


    var on_divider = false;
    let is_div_r = (v.flags & 16u) != 0u;
    let is_div_b = (v.flags & 32u) != 0u;
    let is_div_act = (v.flags & 64u) != 0u;
    if (is_div_r || is_div_b) {
        let div_color = select(
            vec3<f32>(0.208, 0.212, 0.251), // #353640 subtle inactive divider
            vec3<f32>(0.420, 0.451, 0.549), // #6b738c active pane divider
            is_div_act,
        );
        let rx = local.x * u.cell.x;
        let ry = local.y * u.cell.y;
        if (is_div_r && rx >= u.cell.x - 1.0) {
            cell_color = div_color;
            on_divider = true;
        }
        if (is_div_b && ry >= u.cell.y - 1.0) {
            cell_color = div_color;
            on_divider = true;
        }
    }

    // Overlay scrollbar thumb: a 3px bar inset from the right edge.
    if ((v.flags & 2048u) != 0u) {
        let rx = local.x * u.cell.x;
        if (rx >= u.cell.x - 5.0 && rx < u.cell.x - 2.0) {
            cell_color = mix(cell_color, vec3<f32>(0.62, 0.64, 0.70), 0.75);
            solid = true;
        }
    }

    // OSC 9;4 progress: a thin bar along the top of the active pane.
    let p_state = u.progress >> 8u;
    if (p_state != 0u && (v.flags & 128u) == 0u && v.row == u.progress_row
        && u.progress_cols > 0u
        && v.col >= u.progress_col && v.col < u.progress_col + u.progress_cols
        && local.y * u.cell.y < max(2.0, floor(u.cell.y / 8.0))) {
        let x = (f32(v.col - u.progress_col) + local.x) / f32(max(u.progress_cols, 1u));
        // Indeterminate (3) has no percentage: a full, dimmer bar (no animation, 0% idle CPU).
        let filled = p_state == 3u || x * 100.0 < f32(u.progress & 255u);
        var bar = vec3<f32>(0.36, 0.56, 0.98);
        if (p_state == 2u) {
            bar = vec3<f32>(0.93, 0.33, 0.36);
        } else if (p_state == 4u) {
            bar = vec3<f32>(0.95, 0.76, 0.30);
        } else if (p_state == 3u) {
            bar = vec3<f32>(0.30, 0.42, 0.68);
        }
        if (filled) {
            cell_color = bar;
            solid = true;
        }
    }

    // Unfocused splits fade toward the fill color.
    if ((v.flags & 4096u) != 0u && u.unfocused_dim > 0.0) {
        let fill = select(unpack(u.default_bg), unpack(u.unfocused_fill), (u.unfocused_fill & 1u) != 0u);
        cell_color = mix(cell_color, fill, u.unfocused_dim);
    }

    var out_alpha = 1.0;
    let def_bg = unpack(u.default_bg);
    let default_cell = !on_divider && (v.flags & 128u) == 0u && c_style != 1u
        && abs(v.bg.r - def_bg.r) < 0.002 && abs(v.bg.g - def_bg.g) < 0.002 && abs(v.bg.b - def_bg.b) < 0.002;
    if (u.layer == 2u) {
        // Only ink: glyph coverage, or full for the overlays.
        out_alpha = select(a, 1.0, solid || on_divider);
        if (out_alpha <= 0.0) {
            discard;
        }
    } else if (default_cell) {
        if (u.layer == 1u && !solid) {
            // The clear color is the default background; images below may show here.
            discard;
        }
        out_alpha = mix(u.bg_opacity, 1.0, a);
    }

    return vec4<f32>(cell_color, out_alpha);
}
