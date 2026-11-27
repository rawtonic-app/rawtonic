// Clarity version 4's large-scale mean log luminance on a reduced grid
// (clarity_base.rs). Version 3 took it from the large blur (blur.wgsl, index
// 3: a Gaussian of 40 × short side / 1080 pixels radius) at full
// resolution, per tile, 297 taps a pass on a 24 MP photo. That field is
// smooth far below the grid's Nyquist, so version 4 averages the log
// luminance over F × F blocks of the photo, blurs the blocks with the same
// Gaussian scaled by 1 / F, and gives each tile's large blur its alpha by
// bilinear interpolation between the block centres. shader.wgsl reads that
// alpha exactly as it reads version 3's (`clarity3_base`, `clarity_base`).
// The grid is anchored at the photo's origin, so tiles and partial renders
// join exactly.

struct ClarityBaseParams {
    // Block side F in photo pixels.
    step: u32,
    // Photo window the input texture holds (origin, size) and the photo's
    // size: coordinates are photo pixels as in blur.wgsl.
    window_x: u32,
    window_y: u32,
    window_width: u32,
    window_height: u32,
    // First block of the reduced texture (window origin / F) and its size.
    origin_x: u32,
    origin_y: u32,
    blocks_x: u32,
    blocks_y: u32,
    // The full-resolution blur's radius and sigma in pixels, and the
    // blocks either side that hold one of its taps.
    pixel_radius: u32,
    sigma: f32,
    radius: u32,
    // Bit 1: the input holds sRGB-encoded values (not a RAW), as blur.wgsl.
    flags: u32,
    // The tile the upsampling pass fills: its first photo pixel; bit 0 of
    // `tile_flags`: take the colour from `colour_texture` (Dehaze or Glow
    // read the large blur's colour), else 0.
    tile_x: u32,
    tile_y: u32,
    tile_flags: u32,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var reduced_out: texture_storage_2d<r32float, write>;
@group(0) @binding(2) var<uniform> params: ClarityBaseParams;

const F16_MAX = 65504.0;

// As blur.wgsl's `sample_range` and `log_luma`.
fn sample_range(c: vec3<f32>) -> vec3<f32> {
    return clamp(c, vec3(-F16_MAX), vec3(F16_MAX));
}

fn log_luma(c: vec3<f32>) -> f32 {
    var lin = c;
    if ((params.flags & 2u) != 0u) {
        let s = max(c, vec3<f32>(0.0));
        lin = select(pow((s + 0.055) / 1.055, vec3<f32>(2.4)), s / 12.92, s <= vec3<f32>(0.04045));
    }
    return log2(max(dot(lin, vec3<f32>(0.2126, 0.7152, 0.0722)), 0.0001));
}

// The mean log luminance of each block; pixels beyond the window repeat its
// edge, as the full-resolution blur repeats the photo's.
@compute @workgroup_size(8, 8, 1)
fn reduce_log(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.blocks_x || id.y >= params.blocks_y) {
        return;
    }
    let f = i32(params.step);
    let lo = vec2<i32>(i32(params.window_x), i32(params.window_y));
    let hi = lo + vec2<i32>(i32(params.window_width), i32(params.window_height)) - vec2<i32>(1);
    let first = (vec2<i32>(id.xy) + vec2<i32>(i32(params.origin_x), i32(params.origin_y))) * f;
    var total = 0.0;
    for (var dy = 0; dy < f; dy = dy + 1) {
        for (var dx = 0; dx < f; dx = dx + 1) {
            let p = clamp(first + vec2<i32>(dx, dy), lo, hi);
            total += log_luma(sample_range(textureLoad(source, p - lo, 0).rgb));
        }
    }
    textureStore(reduced_out, id.xy, vec4<f32>(total / f32(f * f), 0.0, 0.0, 1.0));
}

fn gaussian(x: f32, sigma: f32) -> f32 {
    return exp(-(x * x) / (2.0 * sigma * sigma));
}

// The full-resolution blur's taps (offsets up to its radius, blur.wgsl)
// that fall in the block `offset` blocks from a block's centre, summed: the
// field at block centres is that blur's wherever the luminance is even
// within a block, and its edge at the radius stays where it is.
fn block_weight(offset: i32) -> f32 {
    let f = i32(params.step);
    let half = f32(f - 1) * 0.5;
    let r = f32(params.pixel_radius);
    var w = 0.0;
    for (var i = 0; i < f; i = i + 1) {
        let d = f32(offset * f + i) - half;
        if (abs(d) <= r) {
            w += gaussian(d, params.sigma);
        }
    }
    return w;
}

fn blur_blocks(id: vec2<u32>, step: vec2<i32>) {
    if (id.x >= params.blocks_x || id.y >= params.blocks_y) {
        return;
    }
    let radius = i32(params.radius);
    let last = vec2<i32>(i32(params.blocks_x), i32(params.blocks_y)) - vec2<i32>(1);
    var total = 0.0;
    var weights = 0.0;
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let p = clamp(vec2<i32>(id) + step * offset, vec2<i32>(0), last);
        let w = block_weight(offset);
        total += textureLoad(source, p, 0).r * w;
        weights += w;
    }
    textureStore(reduced_out, id, vec4<f32>(total / weights, 0.0, 0.0, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn blur_horizontal(@builtin(global_invocation_id) id: vec3<u32>) {
    blur_blocks(id.xy, vec2<i32>(1, 0));
}

@compute @workgroup_size(8, 8, 1)
fn blur_vertical(@builtin(global_invocation_id) id: vec3<u32>) {
    blur_blocks(id.xy, vec2<i32>(0, 1));
}

@group(0) @binding(3) var colour_texture: texture_2d<f32>;
@group(0) @binding(4) var tile_out: texture_storage_2d<rgba16float, write>;

// The block field at a photo pixel: bilinear between the four nearest block
// centres (block b's centre is pixel b × F + (F − 1) / 2), clamped to the
// reduced texture.
fn field_at(p: vec2<i32>) -> f32 {
    let f = f32(params.step);
    let origin = vec2<f32>(f32(params.origin_x), f32(params.origin_y));
    let u = (vec2<f32>(p) + vec2<f32>(0.5)) / f - vec2<f32>(0.5) - origin;
    let b = floor(u);
    let t = u - b;
    let last = vec2<i32>(i32(params.blocks_x), i32(params.blocks_y)) - vec2<i32>(1);
    let b0 = clamp(vec2<i32>(b), vec2<i32>(0), last);
    let b1 = clamp(vec2<i32>(b) + vec2<i32>(1), vec2<i32>(0), last);
    let v00 = textureLoad(source, b0, 0).r;
    let v10 = textureLoad(source, vec2<i32>(b1.x, b0.y), 0).r;
    let v01 = textureLoad(source, vec2<i32>(b0.x, b1.y), 0).r;
    let v11 = textureLoad(source, b1, 0).r;
    return mix(mix(v00, v10, t.x), mix(v01, v11, t.x), t.y);
}

// One tile's large blur for version 4: the field in alpha (absolute mean
// log2 luminance, as blur.wgsl stores index 3's), the colour from the
// full-resolution blur when a tool reads it.
@compute @workgroup_size(16, 16, 1)
fn upsample(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(tile_out);
    if (id.x >= dims.x || id.y >= dims.y) {
        return;
    }
    let p = vec2<i32>(id.xy) + vec2<i32>(i32(params.tile_x), i32(params.tile_y));
    var colour = vec3<f32>(0.0);
    if ((params.tile_flags & 1u) != 0u) {
        colour = textureLoad(colour_texture, id.xy, 0).rgb;
    }
    textureStore(tile_out, id.xy, vec4<f32>(colour, field_at(p)));
}
