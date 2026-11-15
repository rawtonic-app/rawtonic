struct BlurParams {
    radius: u32,
    tile_offset_x: u32,
    tile_offset_y: u32,
    input_width: u32,
    input_height: u32,
    // Origin of the photo window the input texture holds, and the photo's
    // size: coordinates are photo pixels (see `source_window` in shader.wgsl).
    source_x: u32,
    source_y: u32,
    full_width: u32,
    full_height: u32,
    // Rows the horizontal pass blurs above the input region, and all the
    // rows it blurs (the vertical pass's whole radius at the region's edges).
    rows_above: u32,
    blurred_rows: u32,
    // Bit 0: carry the mean log2 luminance in alpha (else 1). Bit 1: the input
    // is sRGB-encoded (not a RAW). Bit 2: store that mean minus the log2
    // luminance of the blurred colour (`log_luma` of it), a small number that
    // keeps its precision in 16 bits where the absolute value is large (deep
    // shadows: 0.008 EV); readers add `log_luma` of the colour back.
    flags: u32,
}

@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> params: BlurParams;

const F16_MAX = 65504.0;

// Negative channels are colours beyond sRGB (the working gamut is ProPhoto).
fn sample_range(c: vec3<f32>) -> vec3<f32> {
    return clamp(c, vec3(-F16_MAX), vec3(F16_MAX));
}

// log2 of a sample's linear luminance. The blurs carry its mean in alpha
// (Clarity's edge-aware base, shader.wgsl clarity_base); colour stays the
// plain mean.
fn log_luma(c: vec3<f32>) -> f32 {
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

@compute @workgroup_size(16, 16, 1)
fn horizontal_blur(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(output_texture));
    if (id.x >= u32(dims.x) || id.y >= params.blurred_rows) {
        return;
    }

    let radius = i32(params.radius);
    let sigma = f32(radius) / 2.0;

    let absolute_coord = vec2<u32>(id.x + params.tile_offset_x, id.y + params.tile_offset_y - params.rows_above);
    let full_dims = vec2<i32>(i32(params.full_width), i32(params.full_height));
    let window_origin = vec2<i32>(i32(params.source_x), i32(params.source_y));

    var total_color = vec3<f32>(0.0);
    var total_log = 0.0;
    var total_weight = 0.0;

    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let sample_x = clamp(i32(absolute_coord.x) + offset, 0, full_dims.x - 1);
        let sample_coord = vec2<i32>(sample_x, i32(absolute_coord.y));

        let sample_color = sample_range(textureLoad(input_texture, sample_coord - window_origin, 0).rgb);
        let weight = gaussian(f32(offset), sigma);

        total_color += sample_color * weight;
        if ((params.flags & 1u) != 0u) {
            total_log += log_luma(sample_color) * weight;
        }
        total_weight += weight;
    }

    let final_color = total_color / total_weight;
    let alpha = select(1.0, stored_log(total_log / total_weight, final_color), (params.flags & 1u) != 0u);
    textureStore(output_texture, id.xy, vec4<f32>(final_color, alpha));
}

// The mean log luminance as stored in alpha (absolute or relative, bit 2).
fn stored_log(mean_log: f32, color: vec3<f32>) -> f32 {
    if ((params.flags & 4u) != 0u) {
        return mean_log - log_luma(sample_range(color));
    }
    return mean_log;
}

// 16 × 16 groups: neighbouring pixels share the rows they read (a one-pixel
// wide group used a single texel of each cache line).
@compute @workgroup_size(16, 16, 1)
fn vertical_blur(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.y >= params.input_height || id.x >= params.input_width) {
        return;
    }

    let radius = i32(params.radius);
    let sigma = f32(radius) / 2.0;

    let local_coord = vec2<i32>(i32(id.x), i32(id.y + params.rows_above));
    let max_y = i32(params.blurred_rows) - 1;

    var total_color = vec3<f32>(0.0);
    var total_log = 0.0;
    var total_weight = 0.0;

    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let sample_y = clamp(local_coord.y + offset, 0, max_y);
        let sample_coord = vec2<i32>(local_coord.x, sample_y);

        let sample = textureLoad(input_texture, vec2<u32>(sample_coord), 0);
        let sample_color = sample_range(sample.rgb);
        let weight = gaussian(f32(offset), sigma);

        total_color += sample_color * weight;
        if ((params.flags & 1u) != 0u) {
            var mean_log = sample.a;
            if ((params.flags & 4u) != 0u) {
                mean_log += log_luma(sample_color);
            }
            total_log += mean_log * weight;
        }
        total_weight += weight;
    }

    let final_color = total_color / total_weight;
    let alpha = select(1.0, stored_log(total_log / total_weight, final_color), (params.flags & 1u) != 0u);
    textureStore(output_texture, id.xy, vec4<f32>(final_color, alpha));
}

// The horizontal passes of up to three blurs without the mean log luminance
// (flags bit 0 clear) over the same rows (`rows_above` 0) in one: each
// sample is loaded once for all of them. Blur k's sums take its own samples
// in its own order (`horizontal_blur`), so it stores the same values.
struct FusedBlurParams {
    blurs: array<BlurParams, 3>,
    // Bit k: blur k is computed (into `fused_output_k`).
    enabled: u32,
    widest: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(3) var<uniform> fused: FusedBlurParams;
@group(0) @binding(4) var fused_output_0: texture_storage_2d<rgba16float, write>;
@group(0) @binding(5) var fused_output_1: texture_storage_2d<rgba16float, write>;
@group(0) @binding(6) var fused_output_2: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16, 1)
fn horizontal_blurs(@builtin(global_invocation_id) id: vec3<u32>) {
    let first = fused.blurs[0];
    let dims = vec2<i32>(textureDimensions(fused_output_0));
    if (id.x >= u32(dims.x) || id.y >= first.blurred_rows) {
        return;
    }

    let absolute_coord = vec2<u32>(id.x + first.tile_offset_x, id.y + first.tile_offset_y);
    let full_dims = vec2<i32>(i32(first.full_width), i32(first.full_height));
    let window_origin = vec2<i32>(i32(first.source_x), i32(first.source_y));

    let radius0 = i32(fused.blurs[0].radius);
    let radius1 = i32(fused.blurs[1].radius);
    let radius2 = i32(fused.blurs[2].radius);
    let sigma0 = f32(radius0) / 2.0;
    let sigma1 = f32(radius1) / 2.0;
    let sigma2 = f32(radius2) / 2.0;
    let on0 = (fused.enabled & 1u) != 0u;
    let on1 = (fused.enabled & 2u) != 0u;
    let on2 = (fused.enabled & 4u) != 0u;

    var total0 = vec3<f32>(0.0);
    var total1 = vec3<f32>(0.0);
    var total2 = vec3<f32>(0.0);
    var weight0 = 0.0;
    var weight1 = 0.0;
    var weight2 = 0.0;

    let widest = i32(fused.widest);
    for (var offset = -widest; offset <= widest; offset = offset + 1) {
        let sample_x = clamp(i32(absolute_coord.x) + offset, 0, full_dims.x - 1);
        let sample_coord = vec2<i32>(sample_x, i32(absolute_coord.y));
        let sample_color = sample_range(textureLoad(input_texture, sample_coord - window_origin, 0).rgb);
        if (on0 && abs(offset) <= radius0) {
            let weight = gaussian(f32(offset), sigma0);
            total0 += sample_color * weight;
            weight0 += weight;
        }
        if (on1 && abs(offset) <= radius1) {
            let weight = gaussian(f32(offset), sigma1);
            total1 += sample_color * weight;
            weight1 += weight;
        }
        if (on2 && abs(offset) <= radius2) {
            let weight = gaussian(f32(offset), sigma2);
            total2 += sample_color * weight;
            weight2 += weight;
        }
    }

    if (on0) {
        textureStore(fused_output_0, id.xy, vec4<f32>(total0 / weight0, 1.0));
    }
    if (on1) {
        textureStore(fused_output_1, id.xy, vec4<f32>(total1 / weight1, 1.0));
    }
    if (on2) {
        textureStore(fused_output_2, id.xy, vec4<f32>(total2 / weight2, 1.0));
    }
}
