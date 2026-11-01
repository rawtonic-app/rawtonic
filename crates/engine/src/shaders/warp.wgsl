// SPDX-License-Identifier: AGPL-3.0-or-later
// The geometry warp of `renderVersion` 7 (warp_gpu.rs): per output pixel
// the arithmetic of image_processing.rs `WarpKernel` with `f32_warp`
// (`source_point_f32`, `interpolate_pixel`, `interpolate_pixel_with_tca`,
// `shade`), in the same order. Every pixel is computed from its frame
// position alone, so a region, a band or every fourth row is bit-identical
// to the same pixels of the whole frame.

struct Params {
    origin: vec4<f32>,
    step_x: vec4<f32>,
    step_y: vec4<f32>,
    // The frame (source and warped: the warp keeps the size).
    width: u32,
    height: u32,
    flags: u32,
    table_len: u32,
    // The frame's pixels `src` holds: columns col0.., rows row0...
    src_col0: u32,
    src_row0: u32,
    src_cols: u32,
    src_rows: u32,
    // Output row r, pixel i is frame pixel (out_x0 + i, out_y0 + r × out_step).
    out_x0: u32,
    out_y0: u32,
    out_width: u32,
    out_step: u32,
    // This dispatch writes output rows band_row0.. (band_rows of them) into
    // `dst` from its row dst_row0.
    band_row0: u32,
    band_rows: u32,
    dst_row0: u32,
    _pad0: u32,
    cx: f32,
    cy: f32,
    hd: f32,
    max_radius_sq_inv: f32,
    auto_crop_scale: f32,
    k1: f32,
    k2: f32,
    k3: f32,
    lens_dist_amt: f32,
    k_distortion_c: f32,
    vr: f32,
    vb: f32,
    vr2: f32,
    vb2: f32,
    inv_hd2: f32,
    vk1: f32,
    vk2: f32,
    vk3: f32,
    lens_vig_amt: f32,
    table_step: f32,
    table_end_slope: f32,
    // Manual lens vignetting (manual_vignette.rs): EV at half the long
    // side, power of the radius, half-diagonals per half long side.
    mv_ev: f32,
    mv_power: f32,
    mv_per_hd: f32,
}

const LENS: u32 = 1u;
const PTLENS: u32 = 2u;
const TABLE: u32 = 4u;
const KNOTS: u32 = 8u;
const MANUAL: u32 = 16u;
const TCA: u32 = 32u;
const VIGNETTING: u32 = 64u;
const KEEP_NEGATIVE: u32 = 128u;
const MANUAL_VIGNETTING: u32 = 1024u;
// Write each pixel's source position (x, y, and z < 0 where it is left
// black) instead of sampling (tests of the warp's precision).
const POSITIONS: u32 = 256u;
// The source holds sRGB-encoded values: vignetting applies to the decoded
// values (`GeometryParams::srgb_base`).
const SRGB_BASE: u32 = 512u;

// The sRGB decode and its unclamped inverse, as `srgb_decode` and
// `srgb_encode` in image_processing.rs.
fn srgb_decode(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3<f32>(2.4)), c / 12.92, c <= vec3<f32>(0.04045));
}

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3<f32>(0.0031308));
}

@group(0) @binding(0) var<uniform> P: Params;
// Source pixels, RGB, rows of `src_cols` pixels.
@group(0) @binding(1) var<storage, read> src: array<f32>;
// Output rows, RGB, rows of `out_width` pixels.
@group(0) @binding(2) var<storage, read_write> dst: array<f32>;
// Lens table: values, tangents (slope × step), knot radii (model 3).
@group(0) @binding(3) var<storage, read> table: array<f32>;
// Set when a sample needs a pixel `src` does not hold.
@group(0) @binding(4) var<storage, read_write> missed: atomic<u32>;

// NaN whatever the compiler assumes about floats (Metal compiles with fast
// math).
fn is_nan(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7fffffffu) > 0x7f800000u;
}

fn table_at(r_in: f32) -> f32 {
    let r = max(r_in, 0.0);
    let n = P.table_len;
    let last = n - 1u;
    if (P.flags & KNOTS) != 0u {
        // `partition_point(|x| x <= r)`, clamped to 1..=last.
        var i = 0u;
        loop {
            if i >= n || table[2u * n + i] > r {
                break;
            }
            i += 1u;
        }
        i = clamp(i, 1u, last);
        let r0 = table[2u * n + i - 1u];
        let r1 = table[2u * n + i];
        let y0 = table[i - 1u];
        let y1 = table[i];
        let per_square = (y1 - y0) / (r1 * r1 - r0 * r0);
        return y0 + per_square * (r * r - r0 * r0);
    }
    let x = r / P.table_step;
    if x >= f32(last) {
        return table[last] + P.table_end_slope * (r - f32(last) * P.table_step);
    }
    let i = u32(x);
    let t = x - f32(i);
    let y0 = table[i];
    let y1 = table[i + 1u];
    let m0 = table[n + i];
    let m1 = table[n + i + 1u];
    let t2 = t * t;
    let t3 = t * t * t;
    return (2.0 * t3 - 3.0 * t2 + 1.0) * y0 + (t3 - 2.0 * t2 + t) * m0 + (-2.0 * t3 + 3.0 * t2) * y1
        + (t3 - t2) * m1;
}

// `source_point_f32`; z < 0 when the pixel is left black.
fn source_point(p: vec3<f32>) -> vec3<f32> {
    if abs(p.z) <= 1e-6 {
        return vec3<f32>(0.0, 0.0, -1.0);
    }
    let cx = P.cx;
    let cy = P.cy;
    let inv_z = 1.0 / p.z;
    var sx = p.x * inv_z;
    var sy = p.y * inv_z;
    if P.auto_crop_scale > 1.0 {
        sx = cx + (sx - cx) / P.auto_crop_scale;
        sy = cy + (sy - cy) / P.auto_crop_scale;
    }
    if (P.flags & LENS) != 0u {
        let dx = sx - cx;
        let dy = sy - cy;
        let ru = sqrt(dx * dx + dy * dy);
        if ru > 1e-6 {
            let ru_norm = ru / P.hd;
            let ru_norm2 = ru_norm * ru_norm;
            var rd_norm: f32;
            if (P.flags & TABLE) != 0u {
                rd_norm = ru_norm * table_at(ru_norm);
            } else if (P.flags & PTLENS) != 0u {
                let d = 1.0 - P.k1 - P.k2 - P.k3;
                rd_norm = ru_norm * (P.k1 * ru_norm2 * ru_norm + P.k2 * ru_norm2 + P.k3 * ru_norm + d);
            } else {
                rd_norm = ru_norm * (1.0 + P.k1 * ru_norm2 + P.k2 * (ru_norm2 * ru_norm2)
                    + P.k3 * (ru_norm2 * ru_norm2 * ru_norm2));
            }
            let effective = ru_norm + (rd_norm - ru_norm) * P.lens_dist_amt;
            let scale = effective / ru_norm;
            sx = cx + dx * scale;
            sy = cy + dy * scale;
        }
    }
    if (P.flags & MANUAL) != 0u {
        let dx = sx - cx;
        let dy = sy - cy;
        let r2_norm = (dx * dx + dy * dy) * P.max_radius_sq_inv;
        let f = 1.0 + P.k_distortion_c * (1.0 - r2_norm);
        sx = cx + dx * f;
        sy = cy + dy * f;
    }
    return vec3<f32>(sx, sy, 1.0);
}

// Channel `c` of frame pixel (x, y); 0 and `missed` set where `src` does
// not hold it.
fn at(x: u32, y: u32, c: u32) -> f32 {
    if x < P.src_col0 || y < P.src_row0 || x - P.src_col0 >= P.src_cols || y - P.src_row0 >= P.src_rows {
        atomicOr(&missed, 1u);
        return 0.0;
    }
    return src[((y - P.src_row0) * P.src_cols + (x - P.src_col0)) * 3u + c];
}

fn catmull_rom(t: f32) -> vec4<f32> {
    let t2 = t * t;
    let t3 = t2 * t;
    return vec4<f32>(
        0.5 * (-t3 + 2.0 * t2 - t),
        0.5 * (3.0 * t3 - 5.0 * t2 + 2.0),
        0.5 * (-3.0 * t3 + 4.0 * t2 + t),
        0.5 * (t3 - t2),
    );
}

// `bicubic_channel`'s area (false for NaN too).
fn in_bicubic(x: f32, y: f32) -> bool {
    if is_nan(x) || is_nan(y) {
        return false;
    }
    return x >= 1.0 && y >= 1.0 && x < f32(P.width) - 2.0 && y < f32(P.height) - 2.0;
}

// `bicubic_channel` inside its area.
fn bicubic(x: f32, y: f32, c: u32) -> f32 {
    let x0 = u32(floor(x));
    let y0 = u32(floor(y));
    let wx = catmull_rom(x - f32(x0));
    let wy = catmull_rom(y - f32(y0));
    var value = 0.0;
    var low = 0.0;
    for (var j = 0u; j < 4u; j++) {
        let row = y0 + j - 1u;
        let a = at(x0 - 1u, row, c);
        let b = at(x0, row, c);
        let d = at(x0 + 1u, row, c);
        let e = at(x0 + 2u, row, c);
        let line = a * wx.x + b * wx.y + d * wx.z + e * wx.w;
        value += line * wy[j];
        low = min(low, min(min(a, b), min(d, e)));
    }
    if value < 0.0 && (P.flags & KEEP_NEGATIVE) != 0u {
        return max(value, low);
    }
    return max(value, 0.0);
}

// `interpolate_pixel_with_tca`'s channel sample.
fn sample_channel(tx: f32, ty: f32, c: u32) -> f32 {
    if is_nan(tx) || is_nan(ty) {
        return 0.0;
    }
    if in_bicubic(tx, ty) {
        return bicubic(tx, ty, c);
    }
    let xc = clamp(tx, 0.0, f32(P.width) - 1.0);
    let yc = clamp(ty, 0.0, f32(P.height) - 1.0);
    var x0 = u32(floor(xc));
    var y0 = u32(floor(yc));
    if x0 >= P.width - 1u {
        x0 = max(P.width, 2u) - 2u;
    }
    if y0 >= P.height - 1u {
        y0 = max(P.height, 2u) - 2u;
    }
    let wx = xc - f32(x0);
    let wy = yc - f32(y0);
    // A one-pixel-wide or -high frame has no neighbour there (its weight
    // is 0): the pixel itself.
    let x1 = min(x0 + 1u, P.width - 1u);
    let y1 = min(y0 + 1u, P.height - 1u);
    let top = at(x0, y0, c) * (1.0 - wx) + at(x1, y0, c) * wx;
    let bot = at(x0, y1, c) * (1.0 - wx) + at(x1, y1, c) * wx;
    return top * (1.0 - wy) + bot * wy;
}

@compute @workgroup_size(16, 16)
fn warp(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.out_width || id.y >= P.band_rows {
        return;
    }
    let x = P.out_x0 + id.x;
    let y = P.out_y0 + (P.band_row0 + id.y) * P.out_step;
    let o = ((P.dst_row0 + id.y) * P.out_width + id.x) * 3u;
    let p = P.origin.xyz + P.step_y.xyz * f32(y) + P.step_x.xyz * f32(x);
    let s = source_point(p);
    if (P.flags & POSITIONS) != 0u {
        dst[o] = s.x;
        dst[o + 1u] = s.y;
        dst[o + 2u] = s.z;
        return;
    }
    var out = vec3<f32>(0.0);
    if s.z > 0.0 {
        let sx = s.x;
        let sy = s.y;
        if (P.flags & TCA) != 0u {
            var vr = P.vr;
            var vb = P.vb;
            if P.vr2 != 0.0 || P.vb2 != 0.0 {
                let r2 = ((sx - P.cx) * (sx - P.cx) + (sy - P.cy) * (sy - P.cy)) * P.inv_hd2;
                vr = vr + P.vr2 * r2;
                vb = vb + P.vb2 * r2;
            }
            out = vec3<f32>(
                sample_channel(P.cx + (sx - P.cx) * vr, P.cy + (sy - P.cy) * vr, 0u),
                sample_channel(sx, sy, 1u),
                sample_channel(P.cx + (sx - P.cx) * vb, P.cy + (sy - P.cy) * vb, 2u),
            );
        } else if !is_nan(sx) && !is_nan(sy) && sx >= 0.0 && sy >= 0.0 && sx <= f32(P.width) - 1.0
            && sy <= f32(P.height) - 1.0 {
            if in_bicubic(sx, sy) {
                out = vec3<f32>(bicubic(sx, sy, 0u), bicubic(sx, sy, 1u), bicubic(sx, sy, 2u));
            } else {
                let x0 = u32(floor(sx));
                let y0 = u32(floor(sy));
                let wx = sx - f32(x0);
                let wy = sy - f32(y0);
                let x1 = min(x0 + 1u, P.width - 1u);
                let y1 = min(y0 + 1u, P.height - 1u);
                for (var c = 0u; c < 3u; c++) {
                    let top = at(x0, y0, c) * (1.0 - wx) + at(x1, y0, c) * wx;
                    let bot = at(x0, y1, c) * (1.0 - wx) + at(x1, y1, c) * wx;
                    out[c] = top * (1.0 - wy) + bot * wy;
                }
            }
        } else {
            // Outside the frame: black, and no shading.
            dst[o] = 0.0;
            dst[o + 1u] = 0.0;
            dst[o + 2u] = 0.0;
            return;
        }
        if (P.flags & VIGNETTING) != 0u {
            let dx = sx - P.cx;
            let dy = sy - P.cy;
            let ru_norm = sqrt(dx * dx + dy * dy) / P.hd;
            let ru_norm2 = ru_norm * ru_norm;
            let v = 1.0 + P.vk1 * ru_norm2 + P.vk2 * (ru_norm2 * ru_norm2) + P.vk3 * (ru_norm2 * ru_norm2 * ru_norm2);
            if v > 1e-6 {
                let gain = 1.0 + (1.0 / v - 1.0) * P.lens_vig_amt;
                if (P.flags & SRGB_BASE) != 0u {
                    out = srgb_encode(srgb_decode(out) * gain);
                } else {
                    out *= gain;
                }
            }
        }
        // Manual lens vignetting after the profile's (as image_processing.rs).
        if (P.flags & MANUAL_VIGNETTING) != 0u {
            let dx = sx - P.cx;
            let dy = sy - P.cy;
            let rho = sqrt(dx * dx + dy * dy) / P.hd * P.mv_per_hd;
            let gain = exp2(P.mv_ev * pow(max(rho, 0.0), P.mv_power));
            if (P.flags & SRGB_BASE) != 0u {
                out = srgb_encode(srgb_decode(out) * gain);
            } else {
                out *= gain;
            }
        }
    }
    dst[o] = out.x;
    dst[o + 1u] = out.y;
    dst[o + 2u] = out.z;
}
