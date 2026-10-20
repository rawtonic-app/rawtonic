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
    _pad3: u32,
}

@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> params: BlurParams;

const F16_MAX = 65504.0;

// Negative channels are colours beyond sRGB (the working gamut is ProPhoto).
fn sample_range(c: vec3<f32>) -> vec3<f32> {
    return clamp(c, vec3(-F16_MAX), vec3(F16_MAX));
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
    var total_weight = 0.0;

    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let sample_x = clamp(i32(absolute_coord.x) + offset, 0, full_dims.x - 1);
        let sample_coord = vec2<i32>(sample_x, i32(absolute_coord.y));

        let sample_color = sample_range(textureLoad(input_texture, sample_coord - window_origin, 0).rgb);
        let weight = gaussian(f32(offset), sigma);

        total_color += sample_color * weight;
        total_weight += weight;
    }

    let final_color = total_color / total_weight;
    textureStore(output_texture, id.xy, vec4<f32>(final_color, 1.0));
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
    var total_weight = 0.0;

    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let sample_y = clamp(local_coord.y + offset, 0, max_y);
        let sample_coord = vec2<i32>(local_coord.x, sample_y);

        let sample_color = sample_range(textureLoad(input_texture, vec2<u32>(sample_coord), 0).rgb);
        let weight = gaussian(f32(offset), sigma);

        total_color += sample_color * weight;
        total_weight += weight;
    }

    let final_color = total_color / total_weight;
    textureStore(output_texture, id.xy, vec4<f32>(final_color, 1.0));
}
