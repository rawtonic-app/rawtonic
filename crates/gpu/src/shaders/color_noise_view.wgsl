// The export's render input after its multi-scale colour noise reduction
// alone (color_noise_view.rs `reduced_render_input`): the colour noise functions are
// the image-processing shader's own (shader.wgsl, from "Multi-scale colour
// noise reduction" to the default noise reduction, and the helpers they
// call), appended to this file when the module is made; these bindings and
// `adjustments` stand in for the ones they read there.

struct ViewGlobal {
    color_noise: vec4<f32>,
    color_noise_window: vec4<u32>,
    color_noise_aware2: f32,
    color_noise_aware_far: f32,
    color_noise_guide: f32,
    color_noise_guide_k2: f32,
    color_noise_speck: f32,
    color_noise_reduction: f32,
    // 1: the filtered pixel, 2: the input pixel (Color 0).
    output_mode: u32,
    _pad1: f32,
}

struct ViewParams {
    global: ViewGlobal,
    source_window: vec4<u32>,
    tile_offset_x: u32,
    tile_offset_y: u32,
    is_raw: u32,
    // The first input row of this band.
    row_offset: u32,
}

@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var chroma_pyramid_texture: texture_2d<f32>;
@group(0) @binding(2) var chroma_energy_texture: texture_2d<f32>;
// Read only with luminance noise reduction, which this pass leaves out.
@group(0) @binding(3) var sharpness_blur_texture: texture_2d<f32>;
@group(0) @binding(4) var<uniform> adjustments: ViewParams;
@group(0) @binding(5) var output_delta: texture_storage_2d<rgba32float, write>;

// The filtered pixel (linear), for the band's rows, as the detail stage
// filters a RAW's input pixel without luminance noise reduction (`new_luma`
// its own luminance, `apply_noise_reduction_default`); the input pixel at
// Color 0.
@compute @workgroup_size(16, 16, 1)
fn color_noise_view_band(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = adjustments.source_window.zw;
    let band = textureDimensions(output_delta);
    let p = vec2<u32>(id.x, id.y + adjustments.row_offset);
    if (id.x >= band.x || id.y >= band.y || p.x >= size.x || p.y >= size.y) {
        return;
    }
    var c = load_input(vec2<i32>(p)).rgb;
    if (adjustments.is_raw == 0u) {
        c = srgb_to_linear(c);
    }
    if (adjustments.global.output_mode == 2u) {
        textureStore(output_delta, vec2<i32>(id.xy), vec4<f32>(c, 0.0));
        return;
    }
    let color_a = clamp(adjustments.global.color_noise_reduction, 0.0, 1.0);
    let filtered = apply_color_noise_multiscale(c, vec2<i32>(p), get_luma(gamut_floor(c)), 0.0, color_a, adjustments.is_raw);
    textureStore(output_delta, vec2<i32>(id.xy), vec4<f32>(filtered, 0.0));
}
