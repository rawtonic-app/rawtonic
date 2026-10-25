struct Transform {
    rect: vec4<f32>,
    clip: vec4<f32>,
    window: vec2<f32>,
    image_size: vec2<f32>,
    texture_size: vec2<f32>,
    pixelated: f32,
    // Clockwise image rotation about the rect's centre, radians.
    rotation: f32,
    bg_primary: vec4<f32>,
    bg_secondary: vec4<f32>,
    // Mask overlay: colour (rgb) and opacity (a); mode.x 0 none, 1 colour, 2 grayscale.
    overlay_color: vec4<f32>,
    overlay_mode: vec4<f32>,
    // Screen area that may show the image; zero width means rect only.
    box_rect: vec4<f32>,
    // Before/after: x enabled, y screen divider, z 1 for top/bottom.
    // Before occupies the left or top side; both use the same photo UV.
    split: vec4<f32>,
};

@group(0) @binding(0) var<uniform> transform: Transform;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
// The active mask layer at the image's size (1×1 and unused without an overlay).
@group(0) @binding(3) var overlay_tex: texture_2d<f32>;
// The split view's unedited render at its own resolution, covering the same
// photograph rect as `tex` (1×1 and unused without a split).
@group(1) @binding(0) var before_tex: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) screen: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) id: u32) -> VertexOutput {
    let uvs = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0)
    );
    let pos = uvs[id];

    let uv_x = transform.clip.x + pos.x * transform.clip.z;
    let uv_y = transform.clip.y + pos.y * transform.clip.w;

    let half_pixel_x = 0.5;
    let half_pixel_y = 0.5;
    let outset_x = (pos.x * 2.0 - 1.0) * half_pixel_x;
    let outset_y = (pos.y * 2.0 - 1.0) * half_pixel_y;

    let screen_x = uv_x + outset_x;
    let screen_y = uv_y + outset_y;

    let ndc_x = (screen_x / transform.window.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (screen_y / transform.window.y) * 2.0;

    var out: VertexOutput;
    out.pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);

    out.uv = vec2<f32>(
        (uv_x - transform.rect.x) / transform.rect.z,
        (uv_y - transform.rect.y) / transform.rect.w
    );
    out.screen = vec2<f32>(uv_x, uv_y);

    return out;
}

// The overlay is blended in display-encoded values, as the webview composited
// the earlier PNG overlay over the image: colour at coverage × opacity, or the
// mask itself as an opaque grayscale image.
fn with_overlay(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
    let mode = transform.overlay_mode.x;
    if (mode < 0.5) {
        return color;
    }
    let size = transform.image_size;
    var overlay_uv = clamp(uv, vec2<f32>(0.5) / size, vec2<f32>(1.0) - vec2<f32>(0.5) / size);
    if (transform.pixelated > 0.5) {
        overlay_uv = (floor(uv * size) + vec2<f32>(0.5)) / size;
    }
    let coverage = textureSampleLevel(overlay_tex, samp, overlay_uv, 0.0).r;
    if (mode > 1.5) {
        return vec4<f32>(coverage, coverage, coverage, 1.0);
    }
    let amount = coverage * transform.overlay_color.a;
    return vec4<f32>(mix(color.rgb, transform.overlay_color.rgb, amount), color.a);
}

// Crop editing: the image turns about the rect's centre (as the webview's
// rotate() did) and is clipped to its unrotated box.
fn crop_edit_uv(screen: vec2<f32>) -> vec2<f32> {
    let b = transform.box_rect;
    if (screen.x < b.x || screen.y < b.y || screen.x > b.x + b.z || screen.y > b.y + b.w) {
        return vec2<f32>(-1.0);
    }
    let half_size = transform.rect.zw * 0.5;
    let d = screen - (transform.rect.xy + half_size);
    let c = cos(transform.rotation);
    let s = sin(transform.rotation);
    let local = vec2<f32>(d.x * c + d.y * s, -d.x * s + d.y * c);
    return (local + half_size) / transform.rect.zw;
}

// The unedited render; it fills its texture, so uv maps onto it directly.
fn sample_before(uv: vec2<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(before_tex));
    var before_uv = clamp(uv, vec2<f32>(0.5) / size, vec2<f32>(1.0) - vec2<f32>(0.5) / size);
    if (transform.pixelated > 0.5) {
        before_uv = (floor(clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)) * size) + vec2<f32>(0.5)) / size;
        before_uv = min(before_uv, vec2<f32>(1.0) - vec2<f32>(0.5) / size);
    }
    return textureSampleLevel(before_tex, samp, before_uv, 0.0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var uv = in.uv;
    if (transform.box_rect.z > 0.0) {
        uv = crop_edit_uv(in.screen);
    }
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return transform.bg_secondary;
    }
    let split_axis = select(in.screen.x, in.screen.y, transform.split.z > 0.5);
    if (transform.split.x > 0.5 && split_axis < transform.split.y) {
        return sample_before(uv);
    }

    let adjusted_uv = uv * (transform.image_size / transform.texture_size);
    let half_texel = vec2<f32>(0.5, 0.5) / transform.texture_size;

    let min_uv = half_texel;
    let max_uv = (transform.image_size / transform.texture_size) - half_texel;

    if (transform.pixelated > 0.5) {
        let texel_coords = floor(adjusted_uv * transform.texture_size);
        let nearest_uv = (texel_coords + vec2<f32>(0.5, 0.5)) / transform.texture_size;

        let clamped_nearest = clamp(nearest_uv, min_uv, max_uv);
        return with_overlay(textureSample(tex, samp, clamped_nearest), uv);
    } else {
        let clamped_uv = clamp(adjusted_uv, min_uv, max_uv);
        return with_overlay(textureSample(tex, samp, clamped_uv), uv);
    }
}
