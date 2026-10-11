// SPDX-License-Identifier: AGPL-3.0-or-later
// X-Trans (any non-Bayer RGB pattern) demosaic on the GPU: the same steps
// and arithmetic as `directional_tile` in demosaic.rs, run over a band of
// rows of the padded, square-root encoded mosaic. Band coordinates: row r
// of the band is padded row `band_top + r`; columns span the padded width.
// The CPU path documents that pixels 20 or more from a tile edge do not
// depend on the tile's extent, so whole-width bands give the same values.

struct Params {
    padded_width: u32,
    band_top: u32,
    band_rows: u32,
    out_width: u32,
    out_rows: u32,
    pattern_width: u32,
    pattern_height: u32,
    pixels: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> mosaic: array<f32>;
@group(0) @binding(2) var<storage, read> pattern: array<u32>;
// Per phase and direction two entries: (has line, a, b, has own line) and
// (s1, s2, 0, 0).
@group(0) @binding(3) var<storage, read> green_lines: array<vec4<i32>>;
// Per phase, direction and slot (0 = red, 1 = blue): (first tap, count).
@group(0) @binding(4) var<storage, read> tap_index: array<vec2<u32>>;
// (row offset, column offset, geometric weight, 0).
@group(0) @binding(5) var<storage, read> taps: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read_write> greens: array<f32>;
@group(0) @binding(7) var<storage, read_write> cand: array<f32>;
@group(0) @binding(8) var<storage, read_write> rough: array<f32>;
@group(0) @binding(9) var<storage, read_write> out_rgb: array<f32>;

const EDGE_K: f32 = 0.2;
const CHROMA_A: f32 = 2.0;
const CHROMA_B: f32 = 1.0;
const BORDER: u32 = 20u;
const F32_MAX: f32 = 3.40282347e38;

fn direction(d: u32) -> vec2<i32> {
    // (row step, column step) as DIRECTIONS in demosaic.rs.
    switch d {
        case 0u: { return vec2<i32>(0, 1); }
        case 1u: { return vec2<i32>(1, 0); }
        case 2u: { return vec2<i32>(1, 1); }
        default: { return vec2<i32>(1, -1); }
    }
}

fn data_at(i: u32) -> f32 {
    return mosaic[params.band_top * params.padded_width + i];
}

// Pattern position of the band's (row 0, column 0), computed once per
// invocation; neighbours wrap without integer division.
fn pattern_origin(r: u32, c: u32) -> vec2<i32> {
    let ay = i32(params.band_top + r) - i32(BORDER);
    let ax = i32(c) - i32(BORDER);
    let ph = i32(params.pattern_height);
    let pw = i32(params.pattern_width);
    return vec2<i32>(((ay % ph) + ph) % ph, ((ax % pw) + pw) % pw);
}

fn wrap(v: i32, n: i32) -> i32 {
    var x = v;
    if (x < 0) { x = x + n; }
    if (x >= n) { x = x - n; }
    return x;
}

fn phase_at(origin: vec2<i32>, dy: i32, dx: i32) -> u32 {
    let ph = i32(params.pattern_height);
    let pw = i32(params.pattern_width);
    return u32(wrap(origin.x + dy, ph) * pw + wrap(origin.y + dx, pw));
}

fn phase_of(r: u32, c: u32) -> u32 {
    return phase_at(pattern_origin(r, c), 0, 0);
}

fn color_of(r: u32, c: u32) -> u32 {
    return pattern[phase_of(r, c)];
}

fn cand_at(d: u32, i: u32, k: u32) -> f32 {
    return cand[(d * params.pixels + i) * 3u + k];
}

// 1. Green along each direction.
@compute @workgroup_size(16, 16, 1)
fn green_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = params.padded_width;
    let h = params.band_rows;
    let c = id.x;
    let r = id.y;
    if (c >= w || r >= h) {
        return;
    }
    let i = r * w + c;
    let n = params.pixels;
    let v = data_at(i);
    for (var d = 0u; d < 4u; d = d + 1u) {
        greens[d * n + i] = v;
    }
    if (r < 3u || r >= h - 3u || c < 3u || c >= w - 3u || color_of(r, c) == 1u) {
        return;
    }
    var lo = F32_MAX;
    var hi = -F32_MAX;
    let origin = pattern_origin(r, c);
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let rr = u32(i32(r) + dy);
            let cc = u32(i32(c) + dx);
            if (pattern[phase_at(origin, dy, dx)] == 1u) {
                let g = data_at(rr * w + cc);
                lo = min(lo, g);
                hi = max(hi, g);
            }
        }
    }
    let phase = phase_at(origin, 0, 0);
    let inner = r >= 6u && r < h - 6u && c >= 6u && c < w - 6u;
    let fallback = select(0.0, 0.5 * (lo + hi), lo <= hi);
    for (var d = 0u; d < 4u; d = d + 1u) {
        let line = green_lines[(phase * 4u + d) * 2u];
        var value = fallback;
        if (line.x != 0 && inner) {
            let dir = direction(d);
            let step = dir.x * i32(w) + dir.y;
            let a = f32(line.y);
            let b = f32(line.z);
            let ga = data_at(u32(i32(i) - line.y * step));
            let gb = data_at(u32(i32(i) + line.z * step));
            var g = (b * ga + a * gb) / (a + b);
            if (line.w != 0) {
                let own = green_lines[(phase * 4u + d) * 2u + 1u];
                let f1 = f32(own.x);
                let f2 = f32(own.y);
                let own_interp = (f2 * data_at(u32(i32(i) - own.x * step)) + f1 * data_at(u32(i32(i) + own.y * step))) / (f1 + f2);
                g = g + a * b / (f1 * f2) * (v - own_interp);
            }
            value = select(max(g, 0.0), clamp(g, lo, hi), lo <= hi);
        }
        greens[d * n + i] = value;
    }
}

// 2. Red and blue per direction from colour differences.
@compute @workgroup_size(16, 16, 1)
fn colour_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = params.padded_width;
    let h = params.band_rows;
    let c = id.x;
    let r = id.y;
    if (c >= w || r >= h) {
        return;
    }
    let i = r * w + c;
    let n = params.pixels;
    if (r < 2u || r >= h - 2u || c < 2u || c >= w - 2u) {
        for (var d = 0u; d < 4u; d = d + 1u) {
            cand[(d * n + i) * 3u] = 0.0;
            cand[(d * n + i) * 3u + 1u] = 0.0;
            cand[(d * n + i) * 3u + 2u] = 0.0;
        }
        return;
    }
    let own = color_of(r, c);
    let phase = phase_of(r, c);
    let v = data_at(i);
    for (var d = 0u; d < 4u; d = d + 1u) {
        let gi = greens[d * n + i];
        for (var slot = 0u; slot < 2u; slot = slot + 1u) {
            let x = slot * 2u;
            if (x == own) {
                cand[(d * n + i) * 3u + x] = v;
                continue;
            }
            let range = tap_index[(phase * 4u + d) * 2u + slot];
            var s = 0.0;
            var wt = 0.0;
            for (var t = 0u; t < range.y; t = t + 1u) {
                let tap = taps[range.x + t];
                let k = u32(i32(i) + i32(tap.x) * i32(w) + i32(tap.y));
                let gk = greens[d * n + k];
                let dk = data_at(k);
                let scale = EDGE_K * (gk + gi) + 1e-4;
                let dg = gk - gi;
                let wgt = tap.z * scale * scale / (scale * scale + dg * dg);
                s = s + wgt * (dk - gk);
                wt = wt + wgt;
            }
            cand[(d * n + i) * 3u + x] = select(gi, max(gi + s / wt, 0.0), wt > 0.0);
        }
        cand[(d * n + i) * 3u + 1u] = gi;
    }
}

// Green again from colour differences at the nearest greens.
@compute @workgroup_size(16, 16, 1)
fn refine_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = params.padded_width;
    let h = params.band_rows;
    let c = id.x;
    let r = id.y;
    if (c >= w || r >= h || r < 8u || r >= h - 8u || c < 8u || c >= w - 8u) {
        return;
    }
    let own = color_of(r, c);
    if (own == 1u) {
        return;
    }
    let i = r * w + c;
    let n = params.pixels;
    let phase = phase_of(r, c);
    let v = data_at(i);
    for (var d = 0u; d < 4u; d = d + 1u) {
        let line = green_lines[(phase * 4u + d) * 2u];
        if (line.x == 0) {
            continue;
        }
        let dir = direction(d);
        let step = dir.x * i32(w) + dir.y;
        let qa = u32(i32(i) - line.y * step);
        let qb = u32(i32(i) + line.z * step);
        let a = f32(line.y);
        let b = f32(line.z);
        let da = data_at(qa) - cand_at(d, qa, own);
        let db = data_at(qb) - cand_at(d, qb, own);
        greens[d * n + i] = max(v + (b * da + a * db) / (a + b), 0.0);
    }
}

fn to_lab(d: u32, i: u32) -> vec3<f32> {
    let pr = cand_at(d, i, 0u);
    let pg = cand_at(d, i, 1u);
    let pb = cand_at(d, i, 2u);
    return vec3<f32>(0.25 * pr + 0.5 * pg + 0.25 * pb, CHROMA_A * (pr - pg), CHROMA_B * (pg - pb));
}

// 3. Smoothness of each candidate along its direction.
@compute @workgroup_size(16, 16, 1)
fn roughness_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = params.padded_width;
    let h = params.band_rows;
    let c = id.x;
    let r = id.y;
    if (c >= w || r >= h) {
        return;
    }
    let i = r * w + c;
    let n = params.pixels;
    let inner = r >= 3u && r < h - 3u && c >= 3u && c < w - 3u;
    for (var d = 0u; d < 4u; d = d + 1u) {
        var value = 0.0;
        if (inner) {
            let dir = direction(d);
            let step = dir.x * i32(w) + dir.y;
            let a = to_lab(d, u32(i32(i) - step));
            let b = to_lab(d, u32(i32(i) + step));
            let m = to_lab(d, i);
            let dl = 2.0 * m.x - a.x - b.x;
            let da = 2.0 * m.y - a.y - b.y;
            let db = 2.0 * m.z - a.z - b.z;
            value = dl * dl + da * da + db * db;
        }
        rough[d * n + i] = value;
    }
}

fn triple(d: u32, j: u32) -> f32 {
    let base = d * params.pixels;
    return rough[base + j - 1u] + rough[base + j] + rough[base + j + 1u];
}

// Per pixel keep the smoothest directions; measured samples stay exact.
@compute @workgroup_size(16, 16, 1)
fn select_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if (x >= params.out_width || y >= params.out_rows) {
        return;
    }
    let w = params.padded_width;
    let r = y + BORDER;
    let c = x + BORDER;
    let i = r * w + c;
    var sums: array<f32, 4>;
    var best = F32_MAX;
    for (var d = 0u; d < 4u; d = d + 1u) {
        var s = 0.0;
        s = s + triple(d, i - w);
        s = s + triple(d, i);
        s = s + triple(d, i + w);
        sums[d] = s;
        best = min(best, s);
    }
    let limit = best * 1.5 + 1e-6;
    var acc = vec3<f32>(0.0);
    var count = 0.0;
    for (var d = 0u; d < 4u; d = d + 1u) {
        if (sums[d] <= limit) {
            acc = acc + vec3<f32>(cand_at(d, i, 0u), cand_at(d, i, 1u), cand_at(d, i, 2u));
            count = count + 1.0;
        }
    }
    var rgb = acc / count;
    let own = color_of(r, c);
    let v = data_at(i);
    if (own == 0u) { rgb.x = v; } else if (own == 1u) { rgb.y = v; } else { rgb.z = v; }
    let decoded = max(rgb, vec3<f32>(0.0));
    let o = (y * params.out_width + x) * 3u;
    out_rgb[o] = decoded.x * decoded.x;
    out_rgb[o + 1u] = decoded.y * decoded.y;
    out_rgb[o + 2u] = decoded.z * decoded.z;
}

