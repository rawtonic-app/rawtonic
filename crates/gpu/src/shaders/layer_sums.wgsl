// Display only: per pixel, the mask-weighted sums of the layers' slider
// values (shader.wgsl's first layer loop). They stay the same while a global
// slider moves, so a display frame reads them instead of looping over every
// layer (gpu_processing.rs `LayerSums`; notes: many-layer-performance).

struct SumsParams {
    width: u32,
    height: u32,
    // Layers summed (indices into the mask array in `layers`).
    layer_count: u32,
    // vec4s of sums per pixel, at most four.
    vec4_count: u32,
    // First vec4 of the pixel data in `sums` (after the header shader.wgsl reads).
    data_offset: u32,
    pixels: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var mask_textures: texture_2d_array<f32>;
@group(0) @binding(1) var<storage, read> params: SumsParams;
// Per summed layer: its mask index, then `vec4_count` vec4s of slider values.
@group(0) @binding(2) var<storage, read> layers: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> sums: array<vec4<f32>>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.width || id.y >= params.height) {
        return;
    }
    let stride = params.vec4_count + 1u;
    var s0 = vec4<f32>(0.0);
    var s1 = vec4<f32>(0.0);
    var s2 = vec4<f32>(0.0);
    var s3 = vec4<f32>(0.0);
    for (var n = 0u; n < params.layer_count; n = n + 1u) {
        let base = n * stride;
        let mask_index = bitcast<u32>(layers[base].x);
        let influence = textureLoad(mask_textures, vec2<i32>(id.xy), i32(mask_index), 0).r;
        // The same threshold as the per-pixel loop it replaces.
        if (influence > 0.001) {
            s0 += layers[base + 1u] * influence;
            if (params.vec4_count > 1u) { s1 += layers[base + 2u] * influence; }
            if (params.vec4_count > 2u) { s2 += layers[base + 3u] * influence; }
            if (params.vec4_count > 3u) { s3 += layers[base + 4u] * influence; }
        }
    }
    let pixel = id.y * params.width + id.x;
    sums[params.data_offset + pixel] = s0;
    if (params.vec4_count > 1u) { sums[params.data_offset + params.pixels + pixel] = s1; }
    if (params.vec4_count > 2u) { sums[params.data_offset + 2u * params.pixels + pixel] = s2; }
    if (params.vec4_count > 3u) { sums[params.data_offset + 3u * params.pixels + pixel] = s3; }
}
