// SPDX-License-Identifier: AGPL-3.0-or-later
// Multi-scale colour noise reduction (rawtonic_engine::color_noise), the
// passes outside the main shader:
// - `finest_energy`: the local energy (3 x 3, weights 1-2-1) of the finest
//   level's chroma detail, per input pixel. It depends on the source only,
//   so it is computed once per input texture (or photo window).
// - `reconstruct_level` / `pack_finest`: level 1 rebuilt from the coarse
//   levels prepared on the CPU (`ChromaPyramid::prepared`), for a new
//   threshold set while a slider moves (display frames).

struct ColorNoiseParams {
    // Photo window the input texture holds (origin) and the photo's size.
    source_x: u32,
    source_y: u32,
    full_width: u32,
    full_height: u32,
    // Level-1 window the packed texture holds (origin) and level 1's size.
    level_x: u32,
    level_y: u32,
    level_width: u32,
    level_height: u32,
    is_raw: u32,
    // Squared threshold and steepness of the level being rebuilt.
    threshold2: f32,
    steepness: f32,
    _pad: u32,
}

@group(0) @binding(0) var first_texture: texture_2d<f32>;
@group(0) @binding(1) var second_texture: texture_2d<f32>;
@group(0) @binding(2) var output_f32: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> params: ColorNoiseParams;

@group(1) @binding(0) var output_packed: texture_storage_2d<rgba16float, write>;
@group(1) @binding(1) var output_energy: texture_storage_2d<r32float, write>;

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let cutoff = vec3<f32>(0.04045);
    let a = vec3<f32>(0.055);
    let higher = pow((c + a) / (1.0 + a), vec3<f32>(2.4));
    let lower = c / 12.92;
    return select(higher, lower, c <= cutoff);
}

fn chroma(rgb: vec3<f32>) -> vec2<f32> {
    let p = sign(rgb) * sqrt(abs(rgb));
    return vec2<f32>(p.r - p.g, p.b - p.g);
}

fn gain(size2: f32, threshold2: f32, steepness: f32) -> f32 {
    if (threshold2 <= 0.0) { return 1.0; }
    if (size2 <= 0.0) { return 0.0; }
    return 1.0 / (1.0 + pow(threshold2 / size2, steepness));
}

// Expansion of a coarser level at finer position `p` (color_noise.rs
// `expand_at`): `n` is the coarser level's last sample, `origin` the first
// sample the texture holds.
fn expand(tex: texture_2d<f32>, p: vec2<i32>, n: vec2<i32>, origin: vec2<i32>) -> vec4<f32> {
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
    let last = vec2<i32>(textureDimensions(tex)) - vec2<i32>(1);
    var out = vec4<f32>(0.0);
    for (var j: i32 = 0; j < 3; j = j + 1) {
        if (wy[j] == 0.0) { continue; }
        var row = vec4<f32>(0.0);
        for (var i: i32 = 0; i < 3; i = i + 1) {
            if (wx[i] == 0.0) { continue; }
            let q = clamp(vec2<i32>(xs[i], ys[j]) - origin, vec2<i32>(0), last);
            row += wx[i] * textureLoad(tex, q, 0);
        }
        out += wy[j] * row;
    }
    return out;
}

// first_texture: the input (photo window); second_texture: the packed
// level 1 (xy) window.
@compute @workgroup_size(16, 16, 1)
fn finest_energy(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(first_texture));
    if (i32(id.x) >= dims.x || i32(id.y) >= dims.y) { return; }
    let source = vec2<i32>(i32(params.source_x), i32(params.source_y));
    let full_last = vec2<i32>(i32(params.full_width), i32(params.full_height)) - vec2<i32>(1);
    let level_n = vec2<i32>(i32(params.level_width), i32(params.level_height)) - vec2<i32>(1);
    let level_origin = vec2<i32>(i32(params.level_x), i32(params.level_y));
    let p = vec2<i32>(id.xy) + source;
    var energy: f32 = 0.0;
    for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
        for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
            let q = clamp(p + vec2<i32>(dx, dy), vec2<i32>(0), full_last);
            // Outside the window only at its edges, which are never rendered.
            var rgb = textureLoad(first_texture, clamp(q - source, vec2<i32>(0), dims - vec2<i32>(1)), 0).rgb;
            if (params.is_raw == 0u) { rgb = srgb_to_linear(rgb); }
            let d = chroma(rgb) - expand(second_texture, q, level_n, level_origin).xy;
            let w = select(0.25, 0.5, dx == 0) * select(0.25, 0.5, dy == 0);
            energy += w * dot(d, d);
        }
    }
    textureStore(output_energy, id.xy, vec4<f32>(energy, 0.0, 0.0, 0.0));
}

// first_texture: the level's prepared detail (xy), local energy (z) and
// guard (w); second_texture: the coarser level rebuilt (xy).
@compute @workgroup_size(16, 16, 1)
fn reconstruct_level(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(first_texture));
    if (i32(id.x) >= dims.x || i32(id.y) >= dims.y) { return; }
    let level = textureLoad(first_texture, vec2<i32>(id.xy), 0);
    let n = vec2<i32>(textureDimensions(second_texture)) - vec2<i32>(1);
    let r = expand(second_texture, vec2<i32>(id.xy), n, vec2<i32>(0));
    let g = max(gain(level.z, params.threshold2, params.steepness), level.w);
    textureStore(output_f32, id.xy, vec4<f32>(r.xy + g * level.xy, 0.0, 0.0));
}

// first_texture: level 1 (xy); second_texture: level 1 rebuilt (xy).
@compute @workgroup_size(16, 16, 1)
fn pack_finest(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(first_texture));
    if (i32(id.x) >= dims.x || i32(id.y) >= dims.y) { return; }
    let g = textureLoad(first_texture, vec2<i32>(id.xy), 0).xy;
    let r = textureLoad(second_texture, vec2<i32>(id.xy), 0).xy;
    textureStore(output_packed, id.xy, vec4<f32>(g, r));
}
