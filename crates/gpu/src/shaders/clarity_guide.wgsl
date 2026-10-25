// Clarity version 3's base (shader.wgsl `apply_band_contrast_robust`): a
// local mean of the input's log2 luminance L over the large blur's window
// that leaves out samples standing far above their own surroundings
// (narrow bright edges: slats, balusters, window frames, sky between
// branches). Measured against it, the darker pixels beside such an edge get
// no dark halo, while texture, which stays within a few tenths of an EV of
// its surroundings, keeps the plain mean's band. A pixel standing above its
// surroundings itself (the edge) is measured against the mean without the
// samples far below theirs instead, so the edge keeps its own shading
// rather than being lifted against the darker side.
//
// Four passes per tile, each over exactly the area the next one reads
// (photo pixels; r = radius):
// 1. `local_mean_horizontal`: L's horizontal Gaussian mean, over the tile
//    widened by r sideways and 2 r up and down;
// 2. `local_mean_vertical`: its vertical mean (the plain local mean) and the
//    pixel's deviation from it, over the tile widened by r;
// 3. `robust_horizontal`: the sums without the samples far above and far
//    below their plain local mean, over the tile's columns and the rows of
//    pass 2;
// 4. `robust_vertical`: the base, over the tile, written at the tile's place
//    in its input region.
// Areas end at the photo's edges, where each pass clamps as a blur of the
// whole photo would, so tiles join exactly.

struct GuideParams {
    // Origin and size of the area this pass writes, photo pixels.
    area: vec4<u32>,
    // Origin and size of the area the pass reads (its input texture).
    source: vec4<u32>,
    // Origin of the photo window the input texture of passes 1 and 2 holds,
    // and the photo's size.
    window_origin: vec2<u32>,
    full_size: vec2<u32>,
    // Photo pixel at the output texture's (0, 0), and at aux's (pass 4).
    output_origin: vec2<u32>,
    aux_origin: vec2<u32>,
    radius: u32,
    // Bit 1: the input is sRGB-encoded (not a RAW).
    flags: u32,
    _pad: vec2<u32>,
}

@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: GuideParams;
// Pass 2: the photo (the pixel's own L). Pass 4: pass 2's plain local mean
// and deviation at the pixel.
@group(0) @binding(2) var aux_texture: texture_2d<f32>;
// One output per format.
@group(0) @binding(3) var out_r: texture_storage_2d<r32float, write>;
@group(0) @binding(4) var out_rg: texture_storage_2d<rg32float, write>;
@group(0) @binding(5) var out_rgba: texture_storage_2d<rgba32float, write>;

const F16_MAX = 65504.0;
// Deviation from the plain local mean (EV) up to which a sample counts
// fully, and the Gaussian falloff beyond it (notes: parity-gaps/G06-presence.md).
const DEAD_ZONE: f32 = 0.25;
const FALLOFF: f32 = 0.25;

// log2 of a sample's linear luminance, as blur.wgsl `log_luma`.
fn log_luma(raw: vec3<f32>) -> f32 {
    let c = clamp(raw, vec3(-F16_MAX), vec3(F16_MAX));
    var lin = c;
    if ((params.flags & 2u) != 0u) {
        let s = max(c, vec3<f32>(0.0));
        lin = select(pow((s + 0.055) / 1.055, vec3<f32>(2.4)), s / 12.92, s <= vec3<f32>(0.04045));
    }
    return log2(max(dot(lin, vec3<f32>(0.2126, 0.7152, 0.0722)), 0.0001));
}

fn gaussian(x: f32, sigma: f32) -> f32 {
    return exp(-(x * x) / (2.0 * sigma * sigma));
}

// How much a sample standing `d` EV beyond its surroundings counts.
fn keep(d: f32) -> f32 {
    let t = max(d - DEAD_ZONE, 0.0) / FALLOFF;
    return exp(-0.5 * t * t);
}

fn in_area(id: vec3<u32>) -> bool {
    return id.x < params.area.z && id.y < params.area.w;
}

// The input texture at photo coordinate `p`, clamped to the area it holds
// along `axis` (0 = x, 1 = y).
fn read_clamped(p: vec2<i32>, axis: u32) -> vec4<f32> {
    let lo = vec2<i32>(params.source.xy);
    let hi = lo + vec2<i32>(params.source.zw) - vec2<i32>(1);
    var q = p;
    if (axis == 0u) {
        q.x = clamp(p.x, lo.x, hi.x);
    } else {
        q.y = clamp(p.y, lo.y, hi.y);
    }
    return textureLoad(input_texture, q - lo, 0);
}

@compute @workgroup_size(16, 16, 1)
fn local_mean_horizontal(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_area(id)) {
        return;
    }
    let p = vec2<i32>(params.area.xy + id.xy);
    let radius = i32(params.radius);
    let sigma = f32(radius) / 2.0;
    let window = vec2<i32>(params.window_origin);
    var sum = 0.0;
    var total = 0.0;
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let x = clamp(p.x + offset, 0, i32(params.full_size.x) - 1);
        let w = gaussian(f32(offset), sigma);
        sum += w * log_luma(textureLoad(input_texture, vec2<i32>(x, p.y) - window, 0).rgb);
        total += w;
    }
    textureStore(out_r, vec2<i32>(id.xy), vec4<f32>(sum / total, 0.0, 0.0, 1.0));
}

@compute @workgroup_size(16, 16, 1)
fn local_mean_vertical(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_area(id)) {
        return;
    }
    let p = vec2<i32>(params.area.xy + id.xy);
    let radius = i32(params.radius);
    let sigma = f32(radius) / 2.0;
    var sum = 0.0;
    var total = 0.0;
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let w = gaussian(f32(offset), sigma);
        sum += w * read_clamped(p + vec2<i32>(0, offset), 1u).r;
        total += w;
    }
    let mean = sum / total;
    let own = log_luma(textureLoad(aux_texture, p - vec2<i32>(params.window_origin), 0).rgb);
    textureStore(out_rg, vec2<i32>(id.xy), vec4<f32>(mean, own - mean, 0.0, 1.0));
}

// Sums of L and of the weights without the samples far above their plain
// local mean (xy) and without those far below it (zw), per unit of spatial
// weight.
@compute @workgroup_size(16, 16, 1)
fn robust_horizontal(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_area(id)) {
        return;
    }
    let p = vec2<i32>(params.area.xy + id.xy);
    let radius = i32(params.radius);
    let sigma = f32(radius) / 2.0;
    var acc = vec4<f32>(0.0);
    var total = 0.0;
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let s = read_clamped(p + vec2<i32>(offset, 0), 0u).rg;
        let l = s.x + s.y;
        let w = gaussian(f32(offset), sigma);
        let without_bright = keep(s.y);
        let without_dark = keep(-s.y);
        acc += w * vec4<f32>(without_bright * l, without_bright, without_dark * l, without_dark);
        total += w;
    }
    textureStore(out_rgba, vec2<i32>(id.xy), acc / total);
}

@compute @workgroup_size(16, 16, 1)
fn robust_vertical(@builtin(global_invocation_id) id: vec3<u32>) {
    if (!in_area(id)) {
        return;
    }
    let p = vec2<i32>(params.area.xy + id.xy);
    let radius = i32(params.radius);
    let sigma = f32(radius) / 2.0;
    var acc = vec4<f32>(0.0);
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        acc += gaussian(f32(offset), sigma) * read_clamped(p + vec2<i32>(0, offset), 1u);
    }
    // Where every sample of the window is left out, the plain mean.
    let own = textureLoad(aux_texture, p - vec2<i32>(params.aux_origin), 0).rg;
    let without_bright = select(own.x, acc.x / acc.y, acc.y > 1e-6);
    let without_dark = select(own.x, acc.z / acc.w, acc.w > 1e-6);
    let above = smoothstep(DEAD_ZONE, 2.0 * DEAD_ZONE, own.y);
    let base = mix(without_bright, without_dark, above);
    textureStore(out_r, p - vec2<i32>(params.output_origin), vec4<f32>(base, 0.0, 0.0, 1.0));
}
