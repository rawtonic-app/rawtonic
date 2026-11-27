// Clarity version 4's large-scale mean log luminance on a reduced grid
// (clarity_base.rs). Version 3 took it from the large blur (blur.wgsl, index
// 3: a Gaussian of 40 × short side / 1080 pixels radius) at full
// resolution, per tile, 297 taps a pass on a 24 MP photo. That field is
// smooth far below the grid's Nyquist, so version 4 averages the log
// luminance over F × F blocks of the photo, blurs the blocks with the full-
// resolution blur's taps summed over each block, and gives each tile's
// large blur its alpha by bilinear interpolation between the block centres.
// shader.wgsl reads that alpha exactly as it reads version 3's
// (`clarity3_base`, `clarity_base`). The grid is anchored at the photo's
// origin, so tiles and partial renders join exactly.
//
// Beyond the window's edges the full-resolution blur repeats the edge pixel
// (its horizontal pass clamps the column, its vertical pass the row of the
// horizontally blurred photo). The block texture therefore holds one more
// column on each side, the edge pixel's column averaged over each block's
// rows, and one more row on each side, the edge row averaged over each
// block's columns (its corners the corner pixels): the blur reads those for
// every block beyond the edge, so a thin dark border weighs as it does at
// full resolution.

struct ClarityBaseParams {
    // Block side F in photo pixels.
    step: u32,
    // Photo window the input texture holds (origin, size): coordinates are
    // photo pixels as in blur.wgsl.
    window_x: u32,
    window_y: u32,
    window_width: u32,
    window_height: u32,
    // First block of the field (window origin / F) and the field's size in
    // blocks; the block texture has one more column and row on each side.
    origin_x: u32,
    origin_y: u32,
    blocks_x: u32,
    blocks_y: u32,
    // Taps either side on the block grid (`weights` holds radius + 1).
    radius: u32,
    // Bit 1: the input holds sRGB-encoded values (not a RAW), as blur.wgsl.
    flags: u32,
    // The tile the upsampling pass fills: its first photo pixel; bit 0 of
    // `tile_flags`: take the colour from `colour_texture` (Dehaze or Glow
    // read the large blur's colour), else 0.
    tile_x: u32,
    tile_y: u32,
    tile_flags: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var reduced_out: texture_storage_2d<r32float, write>;
@group(0) @binding(2) var<uniform> params: ClarityBaseParams;
// The weight of the block `i` blocks from a block's centre: the full-
// resolution blur's taps that fall in it, summed (clarity_base.rs
// `block_weights`).
@group(0) @binding(5) var<storage, read> weights: array<f32>;

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

// First pixel and pixel count of block texture column (or row) `index`
// along one axis: the window's first pixel for index 0, its last for
// `blocks + 1`, else block index − 1 from the field's origin.
fn block_span(index: u32, blocks: u32, origin: u32, lo: i32, hi: i32) -> vec2<i32> {
    if (index == 0u) {
        return vec2<i32>(lo, 1);
    }
    if (index > blocks) {
        return vec2<i32>(hi, 1);
    }
    let f = i32(params.step);
    return vec2<i32>((i32(origin) + i32(index) - 1) * f, f);
}

// The mean log luminance of each block, and of the edge columns and rows;
// pixels beyond the window repeat its edge, as the full-resolution blur
// repeats the photo's.
@compute @workgroup_size(8, 8, 1)
fn reduce_log(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.blocks_x + 2u || id.y >= params.blocks_y + 2u) {
        return;
    }
    let lo = vec2<i32>(i32(params.window_x), i32(params.window_y));
    let hi = lo + vec2<i32>(i32(params.window_width), i32(params.window_height)) - vec2<i32>(1);
    let xs = block_span(id.x, params.blocks_x, params.origin_x, lo.x, hi.x);
    let ys = block_span(id.y, params.blocks_y, params.origin_y, lo.y, hi.y);
    var total = 0.0;
    for (var dy = 0; dy < ys.y; dy = dy + 1) {
        for (var dx = 0; dx < xs.y; dx = dx + 1) {
            let p = clamp(vec2<i32>(xs.x + dx, ys.x + dy), lo, hi);
            total += log_luma(sample_range(textureLoad(source, p - lo, 0).rgb));
        }
    }
    textureStore(reduced_out, id.xy, vec4<f32>(total / f32(xs.y * ys.y), 0.0, 0.0, 1.0));
}

// Along the rows of the block texture: every block column (output column x
// is block texture column x + 1) on every row, edge rows included; blocks
// beyond the edge read the edge column.
@compute @workgroup_size(8, 8, 1)
fn blur_horizontal(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.blocks_x || id.y >= params.blocks_y + 2u) {
        return;
    }
    let radius = i32(params.radius);
    let last = i32(params.blocks_x) + 1;
    var total = 0.0;
    var sum = 0.0;
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let x = clamp(i32(id.x) + 1 + offset, 0, last);
        let w = weights[abs(offset)];
        total += textureLoad(source, vec2<i32>(x, i32(id.y)), 0).r * w;
        sum += w;
    }
    textureStore(reduced_out, id.xy, vec4<f32>(total / sum, 0.0, 0.0, 1.0));
}

// Along the columns of the horizontal pass's output: every block (output
// row y is its row y + 1); blocks beyond the edge read the edge row.
@compute @workgroup_size(8, 8, 1)
fn blur_vertical(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.blocks_x || id.y >= params.blocks_y) {
        return;
    }
    let radius = i32(params.radius);
    let last = i32(params.blocks_y) + 1;
    var total = 0.0;
    var sum = 0.0;
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let y = clamp(i32(id.y) + 1 + offset, 0, last);
        let w = weights[abs(offset)];
        total += textureLoad(source, vec2<i32>(i32(id.x), y), 0).r * w;
        sum += w;
    }
    textureStore(reduced_out, id.xy, vec4<f32>(total / sum, 0.0, 0.0, 1.0));
}

@group(0) @binding(3) var colour_texture: texture_2d<f32>;
@group(0) @binding(4) var tile_out: texture_storage_2d<rgba16float, write>;

// The block field at a photo pixel: bilinear between the four nearest block
// centres (block b's centre is pixel b × F + (F − 1) / 2), clamped to the
// field.
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
