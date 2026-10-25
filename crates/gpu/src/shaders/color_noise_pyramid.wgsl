// SPDX-License-Identifier: AGPL-3.0-or-later
// The colour noise pyramid built, measured and prepared on the GPU
// (rawtonic_engine::color_noise, `colorNoiseVersion` 11,
// `Tuning::gpu_pyramid`): the same steps as `ChromaPyramid::build_with`
// (linear means) and `ChromaPyramid::prepare` on the CPU, from the
// renderer's input texture. Each level is an Rgba32Float texture: chroma
// (xy), square-root luminance (z), measurable (w, 1 or 0).
// - `first_level`: level 1 from the input (5 x 5 binomial taps, mirrored);
// - `reduce_level`: the next level from a level and its linear mean colour;
// - `block_stats`: per measured block of a level's detail, its mean squared
//   chroma and luminance detail, mean luminance and whether it is
//   measurable (`NoiseMeasurement::from_blocks` takes the smoothest share);
// - `coherent_gradient`, `prepare_level`: a coarse level's prepared detail,
//   local energy, chroma-edge ratio and luminance-following part
//   (`PreparedLevel`), which color_noise.wgsl `reconstruct_level` reads;
// - `finest_energy`: color_noise.wgsl `finest_energy` for whole photos;
// - `smooth_coarsest`: the coarsest level's bilateral smoothing above
//   Amount 25 (`ChromaPyramid::coarsest`).

struct PyramidParams {
    // Size of the level (or input) read and of the level written.
// Neighbourhoods are read from workgroup tiles, each sample converted once.
    fine_size: vec2<u32>,
    out_size: vec2<u32>,
    is_raw: u32,
    // Measured blocks: every `stride` blocks, `blocks` across and down.
    stride: u32,
    blocks: vec2<u32>,
    // 1: `block_stats` reads the input (level 0) as the finer level.
    from_source: u32,
    // Radius of the coarsest level's smoothing.
    radius: i32,
    // Squared luminance-guard threshold (`Tuning::luma_guard` times the
    // level's luminance noise; 0: off) and squared chroma-edge guard
    // threshold at Amount 25 (`Tuning::edge_guard` times its colour noise;
    // 0: off), the gain's steepness and the luminance regression's
    // regularisation (25 times the squared luminance noise).
    luma2: f32,
    edge2: f32,
    steepness: f32,
    regular: f32,
    // The coarsest level's smoothing: 1 / (2 sigma^2) in samples and
    // 1 / (2 range^2) in chroma.
    inv_s: f32,
    inv_r: f32,
    // The first output of a dispatch over part of an image (`finest_energy`
    // in bands).
    offset: vec2<u32>,
    _pad: vec2<u32>,
    // Colour noise by brightness relative to the photo's (8 bins of
    // square-root luminance, color_noise.rs `profile_at`).
    profile: array<vec4<f32>, 2>,
}

@group(0) @binding(0) var fine: texture_2d<f32>;
@group(0) @binding(1) var coarse: texture_2d<f32>;
@group(0) @binding(2) var aux: texture_2d<f32>;
@group(0) @binding(3) var<uniform> params: PyramidParams;
@group(0) @binding(4) var out_level: texture_storage_2d<rgba32float, write>;
@group(0) @binding(5) var out_aux: texture_storage_2d<rgba32float, write>;
@group(0) @binding(6) var out_scalar: texture_storage_2d<r32float, write>;
@group(0) @binding(7) var<storage, read_write> blocks: array<vec4<f32>>;

const LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);
const KERNEL = array<f32, 5>(0.0625, 0.25, 0.375, 0.25, 0.0625);
// color_noise.rs `MEASURABLE_BLACK`, `MEASURABLE_PEAK`, `BLOCK`.
const MEASURABLE_BLACK: f32 = 0.0005;
const MEASURABLE_PEAK: f32 = 0.9;
const BLOCK: i32 = 8;

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let cutoff = vec3<f32>(0.04045);
    let a = vec3<f32>(0.055);
    let higher = pow((c + a) / (1.0 + a), vec3<f32>(2.4));
    let lower = c / 12.92;
    return select(higher, lower, c <= cutoff);
}

fn encode(v: vec3<f32>) -> vec3<f32> {
    return sign(v) * sqrt(abs(v));
}

fn chroma(rgb: vec3<f32>) -> vec2<f32> {
    let p = encode(rgb);
    return vec2<f32>(p.r - p.g, p.b - p.g);
}

fn chroma_luma(rgb: vec3<f32>) -> vec3<f32> {
    let p = encode(rgb);
    return vec3<f32>(p.r - p.g, p.b - p.g, dot(p, LUMA));
}

fn measurable(rgb: vec3<f32>) -> bool {
    return dot(rgb, LUMA) >= MEASURABLE_BLACK && all(rgb < vec3<f32>(MEASURABLE_PEAK));
}

// A pixel of the input, linear.
fn source_rgb(p: vec2<i32>) -> vec3<f32> {
    let rgb = textureLoad(fine, p, 0).rgb;
    if (params.is_raw == 0u) { return srgb_to_linear(rgb); }
    return rgb;
}

// color_noise.rs `mirror`.
fn mirror(i: i32, n: i32) -> i32 {
    if (n == 1) { return 0; }
    var j = abs(i);
    if (j >= n) { j = 2 * n - 2 - j; }
    return clamp(j, 0, n - 1);
}

fn profile_bin(i: i32) -> f32 {
    return params.profile[i / 4][i % 4];
}

fn profile_at(y: f32) -> f32 {
    let t = clamp(y * 8.0 - 0.5, 0.0, 7.0);
    let i = min(i32(t), 6);
    let f = t - f32(i);
    let a = profile_bin(i);
    return a + f * (profile_bin(i + 1) - a);
}

// color_noise.rs `gain`.
fn gain(size2: f32, threshold2: f32, steepness: f32) -> f32 {
    if (threshold2 <= 0.0) { return 1.0; }
    if (size2 <= 0.0) { return 0.0; }
    let x = threshold2 / size2;
    var p = x * x * x;
    if (steepness != 3.0) { p = pow(x, steepness); }
    return 1.0 / (1.0 + p);
}

// Expansion of a coarser level at finer position `p` (color_noise.rs
// `expand_at`).
fn expand(tex: texture_2d<f32>, p: vec2<i32>) -> vec4<f32> {
    let n = vec2<i32>(textureDimensions(tex)) - vec2<i32>(1);
    let k = p / 2;
    var xs = vec3<i32>(k.x - 1, k.x, k.x + 1);
    var wx = vec3<f32>(0.125, 0.75, 0.125);
    if (p.x % 2 == 1) {
        xs = vec3<i32>(k.x, k.x + 1, k.x + 1);
        wx = vec3<f32>(0.5, 0.5, 0.0);
    }
    var ys = vec3<i32>(k.y - 1, k.y, k.y + 1);
    var wy = vec3<f32>(0.125, 0.75, 0.125);
    if (p.y % 2 == 1) {
        ys = vec3<i32>(k.y, k.y + 1, k.y + 1);
        wy = vec3<f32>(0.5, 0.5, 0.0);
    }
    xs = clamp(xs, vec3<i32>(0), vec3<i32>(n.x));
    ys = clamp(ys, vec3<i32>(0), vec3<i32>(n.y));
    var out = vec4<f32>(0.0);
    for (var j: i32 = 0; j < 3; j = j + 1) {
        if (wy[j] == 0.0) { continue; }
        var row = vec4<f32>(0.0);
        for (var i: i32 = 0; i < 3; i = i + 1) {
            if (wx[i] == 0.0) { continue; }
            row += wx[i] * textureLoad(tex, vec2<i32>(xs[i], ys[j]), 0);
        }
        out += wy[j] * row;
    }
    return out;
}

// Input pixels of a `first_level` workgroup (8 x 8 outputs and their taps),
// their chroma and luminance and their linear colour, each converted once.
const FIRST_OUT: i32 = 8;
const FIRST_SPAN: i32 = 19;
var<workgroup> first_chroma: array<vec3<f32>, 361>;
var<workgroup> first_rgb: array<vec3<f32>, 361>;

// fine: the input. out_level: level 1; out_aux: its linear mean colour.
@compute @workgroup_size(8, 8, 1)
fn first_level(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
    @builtin(local_invocation_index) index: u32,
) {
    let n = vec2<i32>(params.fine_size);
    // Taps 2p - 2 ... 2p + 2 of the workgroup's outputs p, mirrored.
    let base = vec2<i32>(group.xy) * (2 * FIRST_OUT) - vec2<i32>(2);
    for (var i = i32(index); i < FIRST_SPAN * FIRST_SPAN; i = i + FIRST_OUT * FIRST_OUT) {
        let q = base + vec2<i32>(i % FIRST_SPAN, i / FIRST_SPAN);
        let rgb = source_rgb(vec2<i32>(mirror(q.x, n.x), mirror(q.y, n.y)));
        first_rgb[i] = rgb;
        first_chroma[i] = chroma_luma(rgb);
    }
    workgroupBarrier();
    let p = vec2<i32>(group.xy) * FIRST_OUT + vec2<i32>(local.xy);
    if (p.x >= i32(params.out_size.x) || p.y >= i32(params.out_size.y)) { return; }
    let t = 2 * vec2<i32>(local.xy);
    var level = vec3<f32>(0.0);
    var mean = vec3<f32>(0.0);
    for (var j: i32 = 0; j < 5; j = j + 1) {
        var row = vec3<f32>(0.0);
        var row_mean = vec3<f32>(0.0);
        for (var i: i32 = 0; i < 5; i = i + 1) {
            let k = (t.y + j) * FIRST_SPAN + t.x + i;
            row += KERNEL[i] * first_chroma[k];
            row_mean += KERNEL[i] * first_rgb[k];
        }
        level += KERNEL[j] * row;
        mean += KERNEL[j] * row_mean;
    }
    // Measurable when the four input pixels it covers are.
    var covered = true;
    for (var dy: i32 = 0; dy < 2; dy = dy + 1) {
        for (var dx: i32 = 0; dx < 2; dx = dx + 1) {
            let q = min(2 * p + vec2<i32>(dx, dy), n - vec2<i32>(1));
            covered = covered && measurable(source_rgb(q));
        }
    }
    // The chroma of the linear mean colour (`Tuning::linear_means`).
    textureStore(out_level, p, vec4<f32>(chroma(mean), level.z, select(0.0, 1.0, covered)));
    textureStore(out_aux, p, vec4<f32>(mean, 0.0));
}

// fine: a level; aux: its linear mean colour. out_level: the next level;
// out_aux: its linear mean colour.
@compute @workgroup_size(16, 16, 1)
fn reduce_level(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.out_size.x || id.y >= params.out_size.y) { return; }
    let p = vec2<i32>(id.xy);
    let n = vec2<i32>(params.fine_size);
    var luma = 0.0;
    var mean = vec3<f32>(0.0);
    for (var j: i32 = 0; j < 5; j = j + 1) {
        let y = mirror(2 * p.y + j - 2, n.y);
        var row = 0.0;
        var row_mean = vec3<f32>(0.0);
        for (var i: i32 = 0; i < 5; i = i + 1) {
            let q = vec2<i32>(mirror(2 * p.x + i - 2, n.x), y);
            row += KERNEL[i] * textureLoad(fine, q, 0).z;
            row_mean += KERNEL[i] * textureLoad(aux, q, 0).rgb;
        }
        luma += KERNEL[j] * row;
        mean += KERNEL[j] * row_mean;
    }
    // Measurable when the finer samples it covers are
    // (color_noise.rs `reduce_mask`).
    var covered = true;
    for (var dy: i32 = 0; dy < 2; dy = dy + 1) {
        for (var dx: i32 = 0; dx < 2; dx = dx + 1) {
            let q = min(2 * p + vec2<i32>(dx, dy), n - vec2<i32>(1));
            covered = covered && textureLoad(fine, q, 0).w > 0.5;
        }
    }
    textureStore(out_level, id.xy, vec4<f32>(chroma(mean), luma, select(0.0, 1.0, covered)));
    textureStore(out_aux, id.xy, vec4<f32>(mean, 0.0));
}

// fine: the finer level (the input with `from_source`); coarse: the level
// below it. One invocation per measured block (color_noise.rs
// `smoothest_rms`, `noise_profile`, `block_covered`).
@compute @workgroup_size(8, 8, 1)
fn block_stats(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.blocks.x || id.y >= params.blocks.y) { return; }
    let origin = vec2<i32>(id.xy * params.stride) * BLOCK;
    var chroma2 = 0.0;
    var luma2 = 0.0;
    var luma = 0.0;
    for (var y: i32 = 0; y < BLOCK; y = y + 1) {
        for (var x: i32 = 0; x < BLOCK; x = x + 1) {
            let q = origin + vec2<i32>(x, y);
            var c: vec3<f32>;
            if (params.from_source == 1u) {
                c = chroma_luma(source_rgb(q));
            } else {
                c = textureLoad(fine, q, 0).xyz;
            }
            let d = c - expand(coarse, q).xyz;
            chroma2 += d.x * d.x + d.y * d.y;
            luma2 += d.z * d.z;
            luma += c.z;
        }
    }
    let size = vec2<i32>(textureDimensions(coarse));
    let first = origin / 2;
    let end = min(first + vec2<i32>(BLOCK / 2), size);
    var covered = true;
    for (var y: i32 = first.y; y < end.y; y = y + 1) {
        for (var x: i32 = first.x; x < end.x; x = x + 1) {
            covered = covered && textureLoad(coarse, vec2<i32>(x, y), 0).w > 0.5;
        }
    }
    let n = f32(BLOCK * BLOCK);
    blocks[id.y * params.blocks.x + id.x] =
        vec4<f32>(chroma2 / n, luma2 / n, luma / n, select(0.0, 1.0, covered));
}

// The squared chroma gradient (central differences, borders clamped) of
// the level in `fine` at `q`: xx, xy, yy of the structure tensor.
fn tensor(q: vec2<i32>, last: vec2<i32>) -> vec3<f32> {
    let l = textureLoad(fine, vec2<i32>(max(q.x - 1, 0), q.y), 0).xy;
    let r = textureLoad(fine, vec2<i32>(min(q.x + 1, last.x), q.y), 0).xy;
    let u = textureLoad(fine, vec2<i32>(q.x, max(q.y - 1, 0)), 0).xy;
    let d = textureLoad(fine, vec2<i32>(q.x, min(q.y + 1, last.y)), 0).xy;
    let gx = 0.5 * (r - l);
    let gy = 0.5 * (d - u);
    return vec3<f32>(dot(gx, gx), dot(gx, gy), dot(gy, gy));
}

// 16 x 16 outputs and a margin of 2 on each side.
const TILE: i32 = 16;
const SPAN: i32 = 20;
var<workgroup> tile_tensor: array<vec3<f32>, 400>;
var<workgroup> tile_detail: array<vec4<f32>, 400>;

// fine: a coarse level. out_scalar: its squared chroma gradient times the
// coherence of the structure tensor over 5 x 5, cut at the borders
// (color_noise.rs `coherent_gradient`).
@compute @workgroup_size(16, 16, 1)
fn coherent_gradient(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
    @builtin(local_invocation_index) index: u32,
) {
    let size = vec2<i32>(textureDimensions(fine));
    let last = size - vec2<i32>(1);
    let origin = vec2<i32>(group.xy) * TILE - vec2<i32>(2);
    for (var i = i32(index); i < SPAN * SPAN; i = i + TILE * TILE) {
        let q = origin + vec2<i32>(i % SPAN, i / SPAN);
        // Outside the level: no part of the window.
        var t = vec3<f32>(0.0);
        if (all(q >= vec2<i32>(0)) && all(q <= last)) {
            t = tensor(q, last);
        }
        tile_tensor[i] = t;
    }
    workgroupBarrier();
    let p = vec2<i32>(group.xy) * TILE + vec2<i32>(local.xy);
    if (p.x >= size.x || p.y >= size.y) { return; }
    var sum = vec3<f32>(0.0);
    for (var y: i32 = 0; y < 5; y = y + 1) {
        for (var x: i32 = 0; x < 5; x = x + 1) {
            sum += tile_tensor[(i32(local.y) + y) * SPAN + i32(local.x) + x];
        }
    }
    let trace = sum.x + sum.z;
    var coherence = 0.0;
    if (trace > 0.0) {
        coherence = ((sum.x - sum.z) * (sum.x - sum.z) + 4.0 * sum.y * sum.y) / (trace * trace);
    }
    let own = tile_tensor[(i32(local.y) + 2) * SPAN + i32(local.x) + 2];
    textureStore(out_scalar, p, vec4<f32>((own.x + own.z) * coherence, 0.0, 0.0, 0.0));
}

// fine: a level; coarse: the level below it; aux: the coarser level's
// coherent gradient. out_level: the prepared level (detail xy, local energy
// z, chroma-edge ratio w); out_aux: the luminance-following part of the
// detail (xy). color_noise.rs `ChromaPyramid::prepare`, `local_energy`.
@compute @workgroup_size(16, 16, 1)
fn prepare_level(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
    @builtin(local_invocation_index) index: u32,
) {
    let size = vec2<i32>(textureDimensions(fine));
    let last = size - vec2<i32>(1);
    let origin = vec2<i32>(group.xy) * TILE - vec2<i32>(2);
    // Each sample's chroma detail (xy), luminance detail (z) and the
    // squared colour noise profile at the coarser level's brightness (w),
    // taken at the nearest sample inside the level.
    for (var i = i32(index); i < SPAN * SPAN; i = i + TILE * TILE) {
        let q = clamp(origin + vec2<i32>(i % SPAN, i / SPAN), vec2<i32>(0), last);
        let f = textureLoad(fine, q, 0).xyz;
        let c = expand(coarse, q).xyz;
        let profile = profile_at(c.z);
        tile_detail[i] = vec4<f32>(f.xy - c.xy, f.z - c.z, profile * profile);
    }
    workgroupBarrier();
    let p = vec2<i32>(group.xy) * TILE + vec2<i32>(local.xy);
    if (p.x >= size.x || p.y >= size.y) { return; }
    let centre = (i32(local.y) + 2) * SPAN + i32(local.x) + 2;
    let own = tile_detail[centre];
    // The detail's local energy (3 x 3, weights 1-2-1, borders clamped) in
    // units of the colour noise at the pixel's brightness.
    var energy = 0.0;
    for (var j: i32 = 0; j < 3; j = j + 1) {
        let wy = select(0.25, 0.5, j == 1);
        for (var i: i32 = 0; i < 3; i = i + 1) {
            let d = tile_detail[centre + (j - 1) * SPAN + i - 1].xy;
            energy += wy * select(0.25, 0.5, i == 1) * dot(d, d);
        }
    }
    // The chroma detail that follows the luminance detail: a regression
    // over 5 x 5 (cut at the borders), where the luminance detail stands
    // out of its noise.
    var follow = vec2<f32>(0.0);
    if (params.luma2 > 0.0) {
        var sab = vec2<f32>(0.0);
        var sl = 0.0;
        for (var y: i32 = -2; y <= 2; y = y + 1) {
            for (var x: i32 = -2; x <= 2; x = x + 1) {
                let q = p + vec2<i32>(x, y);
                if (any(q < vec2<i32>(0)) || any(q > last)) { continue; }
                let t = tile_detail[centre + y * SPAN + x];
                sab += t.xy * t.z;
                sl += t.z * t.z;
            }
        }
        let dl = own.z;
        let share = gain(dl * dl, params.luma2, params.steepness) * dl / max(sl + params.regular, 1e-30);
        follow = sab * share;
    }
    var edge = 0.0;
    if (params.edge2 > 0.0) {
        edge = expand(aux, p).x / params.edge2;
    }
    textureStore(out_level, p, vec4<f32>(own.xy, energy / own.w, edge));
    textureStore(out_aux, p, vec4<f32>(follow, 0.0, 0.0));
}

// The finest level's chroma detail around a workgroup's 16 x 16 input
// pixels (a margin of 1) and the level-1 samples their expansions read.
const ENERGY_SPAN: i32 = 18;
const LEVEL_SPAN: i32 = 12;
var<workgroup> energy_detail: array<vec2<f32>, 324>;
var<workgroup> energy_level: array<vec2<f32>, 144>;

// fine: the input; coarse: the packed level 1 (xy). out_scalar: the local
// energy (3 x 3, weights 1-2-1, borders clamped) of the finest level's
// chroma detail in units of the colour noise at the pixel's brightness
// (color_noise.wgsl `finest_energy`, whole photos).
@compute @workgroup_size(16, 16, 1)
fn finest_energy(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
    @builtin(local_invocation_index) index: u32,
) {
    let size = vec2<i32>(params.fine_size);
    let last = size - vec2<i32>(1);
    let level_last = vec2<i32>(textureDimensions(coarse)) - vec2<i32>(1);
    let first = vec2<i32>(params.offset) + vec2<i32>(group.xy) * TILE;
    let origin = first - vec2<i32>(1);
    // The level-1 samples the expansions of the tile's pixels read.
    let level_origin = max(origin, vec2<i32>(0)) / 2 - vec2<i32>(1);
    for (var i = i32(index); i < LEVEL_SPAN * LEVEL_SPAN; i = i + TILE * TILE) {
        let q = clamp(level_origin + vec2<i32>(i % LEVEL_SPAN, i / LEVEL_SPAN), vec2<i32>(0), level_last);
        energy_level[i] = textureLoad(coarse, q, 0).xy;
    }
    workgroupBarrier();
    for (var i = i32(index); i < ENERGY_SPAN * ENERGY_SPAN; i = i + TILE * TILE) {
        let q = clamp(origin + vec2<i32>(i % ENERGY_SPAN, i / ENERGY_SPAN), vec2<i32>(0), last);
        // color_noise.rs `expand_at` from the level-1 tile.
        let k = q / 2;
        var xs = vec3<i32>(k.x - 1, k.x, k.x + 1);
        var wx = vec3<f32>(0.125, 0.75, 0.125);
        if (q.x % 2 == 1) {
            xs = vec3<i32>(k.x, k.x + 1, k.x + 1);
            wx = vec3<f32>(0.5, 0.5, 0.0);
        }
        var ys = vec3<i32>(k.y - 1, k.y, k.y + 1);
        var wy = vec3<f32>(0.125, 0.75, 0.125);
        if (q.y % 2 == 1) {
            ys = vec3<i32>(k.y, k.y + 1, k.y + 1);
            wy = vec3<f32>(0.5, 0.5, 0.0);
        }
        xs = clamp(xs, vec3<i32>(0), vec3<i32>(level_last.x)) - vec3<i32>(level_origin.x);
        ys = clamp(ys, vec3<i32>(0), vec3<i32>(level_last.y)) - vec3<i32>(level_origin.y);
        var e = vec2<f32>(0.0);
        for (var j: i32 = 0; j < 3; j = j + 1) {
            if (wy[j] == 0.0) { continue; }
            var row = vec2<f32>(0.0);
            for (var t: i32 = 0; t < 3; t = t + 1) {
                if (wx[t] == 0.0) { continue; }
                row += wx[t] * energy_level[ys[j] * LEVEL_SPAN + xs[t]];
            }
            e += wy[j] * row;
        }
        energy_detail[i] = chroma(source_rgb(q)) - e;
    }
    workgroupBarrier();
    let p = first + vec2<i32>(local.xy);
    if (p.x >= size.x || p.y >= size.y) { return; }
    let centre = (i32(local.y) + 1) * ENERGY_SPAN + i32(local.x) + 1;
    var energy = 0.0;
    for (var j: i32 = 0; j < 3; j = j + 1) {
        let wy = select(0.25, 0.5, j == 1);
        for (var i: i32 = 0; i < 3; i = i + 1) {
            let d = energy_detail[centre + (j - 1) * ENERGY_SPAN + i - 1];
            energy += wy * select(0.25, 0.5, i == 1) * dot(d, d);
        }
    }
    // color_noise.rs `finest_relative2`.
    let pc = encode(source_rgb(p));
    let relative = profile_at(dot(pc, LUMA));
    textureStore(out_scalar, p, vec4<f32>(energy / (relative * relative), 0.0, 0.0, 0.0));
}

// fine: the coarsest level. out_level: smoothed by a bilateral filter
// (color_noise.rs `ChromaPyramid::coarsest`), its luminance kept.
@compute @workgroup_size(16, 16, 1)
fn smooth_coarsest(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<i32>(textureDimensions(fine));
    let p = vec2<i32>(id.xy);
    if (p.x >= size.x || p.y >= size.y) { return; }
    let c = textureLoad(fine, p, 0);
    let r = params.radius;
    var sum = vec2<f32>(0.0);
    var sw = 0.0;
    for (var dy: i32 = -r; dy <= r; dy = dy + 1) {
        for (var dx: i32 = -r; dx <= r; dx = dx + 1) {
            let q = textureLoad(fine, vec2<i32>(mirror(p.x + dx, size.x), mirror(p.y + dy, size.y)), 0).xy;
            let d = q - c.xy;
            let w = exp(-f32(dx * dx + dy * dy) * params.inv_s - dot(d, d) * params.inv_r);
            sum += w * q;
            sw += w;
        }
    }
    textureStore(out_level, id.xy, vec4<f32>(sum / sw, c.z, c.w));
}
