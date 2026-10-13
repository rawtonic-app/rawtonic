// Specialize preview combinations without reserving the registers and local
// arrays needed by inactive neighborhood and mask operations.
override ENABLE_MASKS: bool = true;
override ENABLE_NOISE_REDUCTION: bool = true;
override ENABLE_SHARPEN: bool = true;
override ENABLE_HIGHLIGHTS: bool = true;
override ENABLE_CURVES: bool = true;
// Preview variants read curve control points directly from storage. Keep the
// original path as the default for export and as a pixel-parity reference.
override DIRECT_CURVES: bool = false;
// Prepare curve polynomials and active-layer flags once per preview recipe;
// read scalar and grading values without per-pixel mask-struct copies.
override PREPARED_LAYERS: bool = false;

struct CurveSegment {
    endpoints: vec4<f32>,
    coefficients: vec4<f32>,
}
struct PreparedCurve {
    segments: array<CurveSegment, 15>,
    endpoints: vec4<f32>,
    count: u32,
    bypass: u32,
    padding: vec2<u32>,
    segment_starts: array<u32, 16>,
}
struct PreparedLayer {
    curves: array<PreparedCurve, 4>,
    flags: vec4<u32>,
}
@group(0) @binding(12) var<storage, read> prepared_layers: array<PreparedLayer, 33>;
// Display-only sampled curves: 1,025 RGB entries per mask layer, used when the
// layer's flags contain 8u (built on the CPU within a quarter 8-bit level).
@group(0) @binding(13) var<storage, read> curve_luts: array<vec4<f32>>;

struct Point {
    x: f32,
    y: f32,
    _pad1: f32,
    _pad2: f32,
}

struct HslColor {
    hue: f32,
    saturation: f32,
    luminance: f32,
    _pad: f32,
}

struct ColorGradeSettings {
    hue: f32,
    saturation: f32,
    luminance: f32,
    _pad: f32,
}

struct ColorCalibrationSettings {
    shadows_tint: f32,
    red_hue: f32,
    red_saturation: f32,
    green_hue: f32,
    green_saturation: f32,
    blue_hue: f32,
    blue_saturation: f32,
    _pad1: f32,
}

struct GlobalAdjustments {
    exposure: f32,
    brightness: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
    vibrance: f32,
    hue: f32,
    _pad_color1: f32,
    _pad_color2: f32,
    _pad_color3: f32,

    sharpness: f32,
    luma_noise_reduction: f32,
    color_noise_reduction: f32,
    clarity: f32,
    dehaze: f32,
    structure: f32,
    centre: f32,
    vignette_amount: f32,
    vignette_midpoint: f32,
    vignette_roundness: f32,
    vignette_feather: f32,
    grain_amount: f32,
    grain_size: f32,
    grain_roughness: f32,

    chromatic_aberration_red_cyan: f32,
    chromatic_aberration_blue_yellow: f32,
    show_clipping: u32,
    is_raw_image: u32,
    grain_structure: u32,

    has_lut: u32,
    lut_intensity: f32,
    tonemapper_mode: u32,
    lut_is_scene_referred: u32,
    image_key: f32,
    image_key_low: f32,
    _pad_lut5: f32,

    baseline_exposure: f32,
    noise_level: f32,
    base_curve: f32,
    agx_pipe_to_rendering_matrix: mat3x3<f32>,
    agx_rendering_to_pipe_matrix: mat3x3<f32>,

    _pad_cg1: f32,
    _pad_cg2: f32,
    _pad_cg3: f32,
    _pad_cg4: f32,
    color_grading_shadows: ColorGradeSettings,
    color_grading_midtones: ColorGradeSettings,
    color_grading_highlights: ColorGradeSettings,
    color_grading_global: ColorGradeSettings,
    color_grading_blending: f32,
    color_grading_balance: f32,
    grading_active: u32,
    bw_enabled: u32,

    color_calibration: ColorCalibrationSettings,

    hsl: array<HslColor, 8>,
    luma_curve: array<Point, 16>,
    red_curve: array<Point, 16>,
    green_curve: array<Point, 16>,
    blue_curve: array<Point, 16>,
    luma_curve_count: u32,
    red_curve_count: u32,
    green_curve_count: u32,
    blue_curve_count: u32,
    detail_scale: f32,
    sharpen_radius_offset: f32,
    sharpen_detail_offset: f32,
    sharpen_masking: f32,

    glow_amount: f32,
    halation_amount: f32,
    flare_amount: f32,
    sharpness_threshold: f32,
    profile_curve: array<vec4<f32>, 16>,
    grading_lut: array<vec4<f32>, 32>,
    bw_mix: array<vec4<f32>, 2>,
    levels: array<vec4<f32>, 4>,
    levels_white: vec4<f32>,
    levels_active: vec4<u32>,
    defringe: array<vec4<f32>, 2>,
    color_ranges: array<vec4<f32>, 32>,
    color_range_count: vec4<u32>,
    region_curve: array<vec4<f32>, 64>,
    region_curve_active: vec4<u32>,
    calibration_matrix: mat3x3<f32>,
    calibration_tint: vec4<f32>,
    calibration_active: vec4<u32>,
    vignette_style: vec4<u32>,
    calibration_luma: vec4<f32>,
    sharpen_falloff: vec4<f32>,
    camera_profile: vec4<u32>,
    camera_profile_matrix: mat3x3<f32>,
    camera_profile_hue_sat: vec4<u32>,
    camera_profile_look: vec4<u32>,
    moire: vec4<f32>,
    white_balance: vec4<f32>,
    noise_detail: vec4<f32>,
    output_space: vec4<u32>,
    output_matrix: mat3x3<f32>,
    proof_matrix: mat3x3<f32>,
    proof_inverse: mat3x3<f32>,
    relight: array<vec4<f32>, 20>,
    relight_depth: vec4<u32>,
    relight_surface: vec4<f32>,
    relight_camera: vec4<f32>,
    relight_geometry: vec4<f32>,
}

struct MaskAdjustments {
    exposure: f32,
    brightness: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
    vibrance: f32,

    sharpness: f32,
    luma_noise_reduction: f32,
    color_noise_reduction: f32,
    clarity: f32,
    dehaze: f32,
    structure: f32,

    glow_amount: f32,
    halation_amount: f32,
    flare_amount: f32,
    sharpness_threshold: f32,

    hue: f32,
    defringe: f32,
    _pad_cg2: f32,
    color_grading_shadows: ColorGradeSettings,
    color_grading_midtones: ColorGradeSettings,
    color_grading_highlights: ColorGradeSettings,
    color_grading_global: ColorGradeSettings,
    color_grading_blending: f32,
    color_grading_balance: f32,
    grading_active: u32,
    color_range_count: u32,

    hsl: array<HslColor, 8>,
    luma_curve: array<Point, 16>,
    red_curve: array<Point, 16>,
    green_curve: array<Point, 16>,
    blue_curve: array<Point, 16>,
    luma_curve_count: u32,
    red_curve_count: u32,
    green_curve_count: u32,
    blue_curve_count: u32,
    _pad_end4: f32,
    _pad_end5: f32,
    _pad_end6: f32,
    _pad_end7: f32,
    grading_lut: array<vec4<f32>, 32>,
    color_ranges: array<vec4<f32>, 16>,
    moire: vec4<f32>,
}

struct AllAdjustments {
    global: GlobalAdjustments,
    mask_adjustments: array<MaskAdjustments, 32>,
    mask_count: u32,
    tile_offset_x: u32,
    tile_offset_y: u32,
    mask_atlas_cols: u32,
    render_region: vec4<u32>,
    // x != 0: detail_texture holds this tile's detail stage at the global
    // noise-reduction and sharpening amounts (display frames only).
    detail_cache: vec4<u32>,
}

@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<storage, read> adjustments: AllAdjustments;

@group(0) @binding(3) var mask_textures: texture_2d_array<f32>;

@group(0) @binding(4) var lut_texture: texture_3d<f32>;
@group(0) @binding(5) var lut_sampler: sampler;

@group(0) @binding(6) var sharpness_blur_texture: texture_2d<f32>;
@group(0) @binding(7) var tonal_blur_texture: texture_2d<f32>;
@group(0) @binding(8) var clarity_blur_texture: texture_2d<f32>;
@group(0) @binding(9) var structure_blur_texture: texture_2d<f32>;
// Blur of sigma 1 px for the luminance noise reduction's finest band.
@group(0) @binding(17) var nr_blur_texture: texture_2d<f32>;
@group(0) @binding(18) var relight_depth_texture: texture_2d<f32>;
@group(0) @binding(19) var relight_camera_texture: texture_2d<f32>;
@group(0) @binding(20) var relight_scene_texture: texture_2d<f32>;

@group(0) @binding(10) var flare_texture: texture_2d<f32>;
@group(0) @binding(11) var flare_sampler: sampler;

// The detail stage (noise reduction and sharpening of the input) depends only
// on the input and the global detail amounts, so display frames compute it
// once per image into a tile-sized cache (addressed like the blur textures).
@group(0) @binding(14) var detail_texture: texture_2d<f32>;
@group(0) @binding(15) var detail_output: texture_storage_2d<rgba32float, write>;

// x of element 0: the number of regions; then the tile-local regions (origin,
// size) one dispatch renders. Workgroups are numbered flat, row by row of a
// RENDER_REGION_GROUPS_X-wide grid, and assigned to the regions in order.
@group(0) @binding(16) var<storage, read> render_regions: array<vec4<u32>>;

override HIGH_PRECISION_OUTPUT: u32 = 0u;

const LUMA_COEFF = vec3<f32>(0.2126, 0.7152, 0.0722);

fn get_luma(c: vec3<f32>) -> f32 {
    return dot(c, LUMA_COEFF);
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let cutoff = vec3<f32>(0.04045);
    let a = vec3<f32>(0.055);
    let higher = pow((c + a) / (1.0 + a), vec3<f32>(2.4));
    let lower = c / 12.92;
    return select(higher, lower, c <= cutoff);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let c_clamped = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let cutoff = vec3<f32>(0.0031308);
    let a = vec3<f32>(0.055);
    let higher = (1.0 + a) * pow(c_clamped, vec3<f32>(1.0 / 2.4)) - a;
    let lower = c_clamped * 12.92;
    return select(higher, lower, c_clamped <= cutoff);
}

fn linear_to_srgb_extended(c: vec3<f32>) -> vec3<f32> {
    let safe_c = max(c, vec3<f32>(0.0));
    let cutoff = vec3<f32>(0.0031308);
    let a = vec3<f32>(0.055);
    let higher = (1.0 + a) * pow(safe_c, vec3<f32>(1.0 / 2.4)) - a;
    let lower = safe_c * 12.92;
    return select(higher, lower, safe_c <= cutoff);
}

// The working gamut (ADR 0010): colours are kept out to the ProPhoto gamut
// as linear sRGB with negative channels, and display values encode them
// with the sRGB curve mirrored below zero. Every output (sRGB, Display P3,
// Adobe RGB, ProPhoto, the display) is this one rendering in its primaries
// and curve, clipped only at the end (encode_output), as Camera Raw clips
// its ProPhoto rendering.
fn outside_unit_cube(c: vec3<f32>) -> bool {
    return any(c < vec3<f32>(0.0)) || any(c > vec3<f32>(1.0));
}

// Scene-linear colour clipped to the ProPhoto gamut from below.
fn gamut_floor(c: vec3<f32>) -> vec3<f32> {
    if (all(c >= vec3<f32>(0.0))) {
        return c;
    }
    return prophoto_to_srgb(max(srgb_to_prophoto(c), vec3<f32>(0.0)));
}

fn below_gamut(c: vec3<f32>) -> bool {
    let p = srgb_to_prophoto(c);
    return min(p.r, min(p.g, p.b)) < 0.0;
}

// Display-referred linear colour clipped to the ProPhoto unit cube.
fn gamut_unit(c: vec3<f32>) -> vec3<f32> {
    if (!outside_unit_cube(c)) {
        return c;
    }
    return prophoto_to_srgb(clamp(srgb_to_prophoto(c), vec3<f32>(0.0), vec3<f32>(1.0)));
}

fn encode_signed(c: vec3<f32>) -> vec3<f32> {
    return sign(c) * linear_to_srgb_extended(abs(c));
}

fn decode_signed(d: vec3<f32>) -> vec3<f32> {
    return sign(d) * srgb_to_linear(abs(d));
}

// Display values → linear, clipped to the gamut.
fn display_to_linear(d: vec3<f32>) -> vec3<f32> {
    return gamut_unit(decode_signed(d));
}

// Linear → display values, clipped to the gamut.
fn linear_to_display(c: vec3<f32>) -> vec3<f32> {
    return encode_signed(gamut_unit(c));
}

// Display values clipped to the gamut.
fn display_unit(d: vec3<f32>) -> vec3<f32> {
    if (!outside_unit_cube(d)) {
        return d;
    }
    return encode_signed(gamut_unit(decode_signed(d)));
}

// A per-channel operation (curves, levels, LUTs) acts on the colour clipped
// to the sRGB cube, `clipped` → `result`; the part of `original` beyond it
// is carried along at the operation's luminance ratio, so colours outside
// sRGB follow the tone change and keep their distance from it. All three
// are display values.
fn carry_beyond_srgb(original: vec3<f32>, clipped: vec3<f32>, result: vec3<f32>) -> vec3<f32> {
    // Nothing beyond the cube to carry (most pixels).
    if (all(original == clipped)) {
        return result;
    }
    let o = decode_signed(original);
    let s = srgb_to_linear(clipped);
    let r = decode_signed(result);
    let ratio = get_luma(max(r, vec3<f32>(0.0))) / max(get_luma(s), 1e-5);
    return display_unit(encode_signed(r + (o - s) * min(ratio, 4.0)));
}

// The same for scene-linear per-channel tone operations: `clipped` is the
// colour floored at zero.
fn carry_scene(original: vec3<f32>, clipped: vec3<f32>, result: vec3<f32>) -> vec3<f32> {
    let ratio = get_luma(max(result, vec3<f32>(0.0))) / max(get_luma(clipped), 1e-6);
    return gamut_floor(result + (original - clipped) * min(ratio, 4.0));
}

// Signed square-root encoding of the colour noise reduction.
fn nr_encode(c: vec3<f32>) -> vec3<f32> {
    return sign(c) * sqrt(abs(c));
}

fn nr_decode(p: vec3<f32>) -> vec3<f32> {
    return p * abs(p);
}

// Lightroom's destination gamut warning colour (linear sRGB).
const PROOF_WARNING_COLOUR: vec3<f32> = vec3<f32>(1.0, 0.0, 0.0);

// The last step: display values → the output space's primaries and curve
// (output_color_space.rs; sRGB for the display), clipped to its gamut.
fn encode_output(d: vec3<f32>) -> vec3<f32> {
    let proof = adjustments.global.output_space.y;
    // sRGB output of a colour inside the sRGB cube: already its encoding.
    if (adjustments.global.output_space.x == 0u && proof == 0u && !outside_unit_cube(d)) {
        return d;
    }
    var colour = decode_signed(d);
    // Soft proofing: the display shows the colour the proofed space keeps
    // (clipped to its gamut, as its export would be), optionally marking
    // colours outside it, as Lightroom's destination gamut warning does.
    if (proof != 0u) {
        let proofed = adjustments.global.proof_matrix * colour;
        let kept = clamp(proofed, vec3<f32>(0.0), vec3<f32>(1.0));
        if (adjustments.global.output_space.z != 0u && any(abs(proofed - kept) > vec3<f32>(1.0 / 255.0))) {
            colour = PROOF_WARNING_COLOUR;
        } else {
            colour = adjustments.global.proof_inverse * kept;
        }
    }
    let linear = clamp(adjustments.global.output_matrix * colour, vec3<f32>(0.0), vec3<f32>(1.0));
    switch (adjustments.global.output_space.x) {
        case 2u: { return pow(linear, vec3<f32>(256.0 / 563.0)); }
        case 3u: { return pow(linear, vec3<f32>(256.0 / 461.0)); }
        default: { return linear_to_srgb(linear); }
    }
}

fn linear_to_vlog(c: vec3<f32>) -> vec3<f32> {
    let safe_c = max(c, vec3<f32>(0.0));
    let low = 5.6 * safe_c + 0.125;
    let log10_val = log2(safe_c + 0.00873) * 0.30102999566398;
    let high = 0.241514 * log10_val + 0.598206;
    return select(high, low, safe_c <= vec3<f32>(0.01));
}

fn rgb_to_hsv(c: vec3<f32>) -> vec3<f32> {
    let c_max = max(c.r, max(c.g, c.b));
    let c_min = min(c.r, min(c.g, c.b));
    let delta = c_max - c_min;
    var h: f32 = 0.0;
    if (delta > 0.0) {
        if (c_max == c.r) { h = 60.0 * (((c.g - c.b) / delta) % 6.0); }
        else if (c_max == c.g) { h = 60.0 * (((c.b - c.r) / delta) + 2.0); }
        else { h = 60.0 * (((c.r - c.g) / delta) + 4.0); }
    }
    if (h < 0.0) { h += 360.0; }
    let s = select(0.0, delta / c_max, c_max > 0.0);
    return vec3<f32>(h, s, c_max);
}

fn hsv_to_rgb(c: vec3<f32>) -> vec3<f32> {
    let h = c.x; let s = c.y; let v = c.z;
    let C = v * s;
    let X = C * (1.0 - abs((h / 60.0) % 2.0 - 1.0));
    let m = v - C;
    var rgb_prime: vec3<f32>;
    if (h < 60.0) { rgb_prime = vec3<f32>(C, X, 0.0); }
    else if (h < 120.0) { rgb_prime = vec3<f32>(X, C, 0.0); }
    else if (h < 180.0) { rgb_prime = vec3<f32>(0.0, C, X); }
    else if (h < 240.0) { rgb_prime = vec3<f32>(0.0, X, C); }
    else if (h < 300.0) { rgb_prime = vec3<f32>(X, 0.0, C); }
    else { rgb_prime = vec3<f32>(C, 0.0, X); }
    return rgb_prime + vec3<f32>(m, m, m);
}

// A layer's Hue (Lightroom's local Hue): turns the hue in Oklab LCh by
// `shift_degrees`, keeping lightness and chroma; a colour that leaves the
// gamut keeps its hue and lightness and loses only the chroma it must.
// Camera Raw's turns in an RGB-like space and changes chroma (×1.5 at 63°)
// and lightness on the way (notes: reference/local-hue.md).
fn apply_hue_shift(color: vec3<f32>, shift_degrees: f32) -> vec3<f32> {
    if (abs(shift_degrees) < 0.01) {
        return color;
    }
    let lab = linear_srgb_to_oklab(color);
    let angle = radians(shift_degrees);
    let c = cos(angle);
    let s = sin(angle);
    let ab = vec2<f32>(lab.y * c - lab.z * s, lab.y * s + lab.z * c);
    var turned = oklab_to_linear_srgb(vec3<f32>(lab.x, ab));
    if (below_gamut(turned)) {
        // Largest chroma scale that stays in gamut (bisection).
        var lo = 0.0;
        var hi = 1.0;
        for (var i = 0; i < 12; i = i + 1) {
            let mid = 0.5 * (lo + hi);
            let candidate = oklab_to_linear_srgb(vec3<f32>(lab.x, ab * mid));
            if (below_gamut(candidate)) { hi = mid; } else { lo = mid; }
        }
        turned = oklab_to_linear_srgb(vec3<f32>(lab.x, ab * lo));
    }
    return gamut_floor(turned);
}

fn hash(p: vec2<f32>) -> f32 {
    var p3  = fract(vec3<f32>(p.xyx) * .1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn gradient_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);

    let ga = vec2<f32>(hash(i + vec2(0.0, 0.0)), hash(i + vec2(0.0, 0.0) + vec2(11.0, 37.0))) * 2.0 - 1.0;
    let gb = vec2<f32>(hash(i + vec2(1.0, 0.0)), hash(i + vec2(1.0, 0.0) + vec2(11.0, 37.0))) * 2.0 - 1.0;
    let gc = vec2<f32>(hash(i + vec2(0.0, 1.0)), hash(i + vec2(0.0, 1.0) + vec2(11.0, 37.0))) * 2.0 - 1.0;
    let gd = vec2<f32>(hash(i + vec2(1.0, 1.0)), hash(i + vec2(1.0, 1.0) + vec2(11.0, 37.0))) * 2.0 - 1.0;

    let dot_00 = dot(ga, f - vec2(0.0, 0.0));
    let dot_10 = dot(gb, f - vec2(1.0, 0.0));
    let dot_01 = dot(gc, f - vec2(0.0, 1.0));
    let dot_11 = dot(gd, f - vec2(1.0, 1.0));

    let bottom_interp = mix(dot_00, dot_10, u.x);
    let top_interp = mix(dot_01, dot_11, u.x);

    return mix(bottom_interp, top_interp, u.y);
}

// Grain structures (recipe `grainStructure`, notes: reference/grain.md).
// Even is the original grain: evenly spread, the same in every tone. The
// others are Rawtonic's own procedural grains, measured against the grain
// characters other editors offer: Soft has rounded grains with irregular
// clusters, Clumped gathers fine grain into clumps (Roughness sets how
// strongly, from even to strongly mottled), Speckled scatters bright specks
// over a fine grain, Gritty is a coarse, hard-edged grain. Soft, Speckled
// and Gritty are strongest in the midtones and fade towards black and
// white as film grain does; Even and Clumped stay even across the tones.
// Each is scaled to Even's strength at Roughness 50 (Gritty 1.4x), so
// Amount means the same across structures.
const GRAIN_EVEN: u32 = 0u;
const GRAIN_SOFT: u32 = 1u;
const GRAIN_CLUMPED: u32 = 2u;
const GRAIN_SPECKLED: u32 = 3u;
const GRAIN_GRITTY: u32 = 4u;

// Gradient noise for the grain structures: the same as gradient_noise, kept
// separate so the Even grain compiles exactly as before (byte-identical
// exports of recipes without a structure).
fn grain_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let ga = vec2<f32>(hash(i), hash(i + vec2(11.0, 37.0))) * 2.0 - 1.0;
    let gb = vec2<f32>(hash(i + vec2(1.0, 0.0)), hash(i + vec2(12.0, 37.0))) * 2.0 - 1.0;
    let gc = vec2<f32>(hash(i + vec2(0.0, 1.0)), hash(i + vec2(11.0, 38.0))) * 2.0 - 1.0;
    let gd = vec2<f32>(hash(i + vec2(1.0, 1.0)), hash(i + vec2(12.0, 38.0))) * 2.0 - 1.0;
    let bottom = mix(dot(ga, f), dot(gb, f - vec2(1.0, 0.0)), u.x);
    let top = mix(dot(gc, f - vec2(0.0, 1.0)), dot(gd, f - vec2(1.0, 1.0)), u.x);
    return mix(bottom, top, u.y);
}

fn grain_structure_noise(structure: u32, p: vec2<f32>, roughness: f32) -> f32 {
    if (structure == GRAIN_SOFT) {
        let n = grain_noise(p * 0.8 + vec2<f32>(3.1, 7.7));
        let c = grain_noise(p * 0.3 + vec2<f32>(17.0, 9.0));
        let w = 0.35 + 0.65 * roughness;
        return 0.71 * (n + w * c) / sqrt(1.0 + w * w);
    }
    if (structure == GRAIN_CLUMPED) {
        let fine = grain_noise(p);
        let c = 0.6 * grain_noise(p * 0.4 + vec2<f32>(41.0, 3.0))
            + 0.4 * grain_noise(p * 0.13 + vec2<f32>(13.0, 29.0));
        let k = 1.6 * roughness;
        return 0.71 * (fine * (1.0 + 0.8 * k * c) + 0.9 * k * c) / sqrt(1.0 + 0.43 * k * k);
    }
    if (structure == GRAIN_SPECKLED) {
        let base = grain_noise(p * 0.9 + vec2<f32>(1.7, 4.4));
        let s = grain_noise(p * 0.7 + vec2<f32>(23.0, 31.0));
        let c = grain_noise(p * 0.25 + vec2<f32>(7.0, 2.0));
        let speck = max(s - 0.12, 0.0) * (1.6 + 6.0 * roughness * max(c + 0.1, 0.0));
        // The specks' mean is taken off so the grain does not brighten.
        return 0.575 * (base + speck - (0.0418 + 0.0206 * roughness));
    }
    // Gritty
    let n = grain_noise(p * 0.55 + vec2<f32>(2.3, 8.1))
        + 0.45 * grain_noise(p + vec2<f32>(6.6, 1.9))
        + (0.5 + 0.8 * roughness) * grain_noise(p * 0.2 + vec2<f32>(19.0, 5.0));
    return 0.524 * tanh(1.5 * n);
}

// The grain with a structure other than Even (same strength and tone mask
// as Even, see the grain block in main).
fn apply_grain_structure(rgb: vec3<f32>, coord: vec2<f32>, scale: f32) -> vec3<f32> {
    let structure = adjustments.global.grain_structure;
    var amount = adjustments.global.grain_amount * 0.5;
    let grain_frequency = (1.0 / max(adjustments.global.grain_size, 0.1)) / scale;
    let luma = max(0.0, get_luma(rgb));
    var luma_mask = smoothstep(0.0, 0.15, luma) * (1.0 - smoothstep(0.6, 1.0, luma));
    if (adjustments.global.tonemapper_mode >= 2u) {
        amount *= GRAIN_GAIN * (1.0 + GRAIN_FINE_BOOST * max(0.0, 1.0 - adjustments.global.grain_size * 2.0));
        luma_mask = smoothstep(0.0, 0.03, luma) * (1.0 - smoothstep(0.97, 1.0, luma));
    }
    let noise = grain_structure_noise(structure, coord * grain_frequency, adjustments.global.grain_roughness);
    return rgb + vec3<f32>(noise) * amount * luma_mask * grain_tone_weight(structure, luma);
}

// Film-like tone response: full strength around display level 0.45,
// falling off towards black and white (Even and Clumped: 1 everywhere).
fn grain_tone_weight(structure: u32, luma: f32) -> f32 {
    if (structure == GRAIN_CLUMPED) { return 1.0; }
    let d = luma - 0.45;
    if (structure == GRAIN_SOFT) { return max(1.0 - 3.0 * d * d, 0.35); }
    return max(1.0 - 4.0 * d * d, 0.3);
}

fn dither(coords: vec2<u32>) -> f32 {
    let p = vec2<f32>(coords);
    return fract(sin(dot(p, vec2<f32>(12.9898, 78.233))) * 43758.5453) - 0.5;
}

fn interpolate_cubic_hermite(x: f32, p1: Point, p2: Point, m1: f32, m2: f32) -> f32 {
    let dx = p2.x - p1.x;
    if (dx <= 0.0) { return p1.y; }
    let t = (x - p1.x) / dx;
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    return h00 * p1.y + h10 * m1 * dx + h01 * p2.y + h11 * m2 * dx;
}

fn apply_curve(val: f32, points: array<Point, 16>, count: u32) -> f32 {
    if (count < 2u) { return val; }
    var local_points = points;
    let x = val * 255.0;
    if (x <= local_points[0].x) { return local_points[0].y / 255.0; }
    if (x >= local_points[count - 1u].x) { return local_points[count - 1u].y / 255.0; }
    for (var i = 0u; i < 15u; i = i + 1u) {
        if (i >= count - 1u) { break; }
        let p1 = local_points[i];
        let p2 = local_points[i + 1u];
        if (x <= p2.x) {
            let p0 = local_points[max(0u, i - 1u)];
            let p3 = local_points[min(count - 1u, i + 2u)];
            let delta_before = (p1.y - p0.y) / max(0.001, p1.x - p0.x);
            let delta_current = (p2.y - p1.y) / max(0.001, p2.x - p1.x);
            let delta_after = (p3.y - p2.y) / max(0.001, p3.x - p2.x);
            var tangent_at_p1: f32;
            var tangent_at_p2: f32;
            if (i == 0u) { tangent_at_p1 = delta_current; } else {
                if (delta_before * delta_current <= 0.0) { tangent_at_p1 = 0.0; } else { tangent_at_p1 = (delta_before + delta_current) / 2.0; }
            }
            if (i + 1u == count - 1u) { tangent_at_p2 = delta_current; } else {
                if (delta_current * delta_after <= 0.0) { tangent_at_p2 = 0.0; } else { tangent_at_p2 = (delta_current + delta_after) / 2.0; }
            }
            if (delta_current != 0.0) {
                let alpha = tangent_at_p1 / delta_current;
                let beta = tangent_at_p2 / delta_current;
                if (alpha * alpha + beta * beta > 9.0) {
                    let tau = 3.0 / sqrt(alpha * alpha + beta * beta);
                    tangent_at_p1 = tangent_at_p1 * tau;
                    tangent_at_p2 = tangent_at_p2 * tau;
                }
            }
            let result_y = interpolate_cubic_hermite(x, p1, p2, tangent_at_p1, tangent_at_p2);
            return clamp(result_y / 255.0, 0.0, 1.0);
        }
    }
    return local_points[count - 1u].y / 255.0;
}

// Whites: the renderer receives slider / 30; calibrated against Capture One 16
// and Lightroom Classic (notes: reference/tone-controls.md).
// Tone sliders under the Standard / Camera Raw base looks (modes 2 and 3),
// whose curves compress the upper tones more than the Classic curve the
// sliders were first calibrated on (notes: reference/tone-controls.md).
const STANDARD_CONTRAST_PIVOT: f32 = 0.31;
const STANDARD_HIGHLIGHTS_PIVOT: f32 = 0.006;
const STANDARD_HIGHLIGHTS_LOWER: f32 = 1.6;
const STANDARD_HIGHLIGHTS_RAISE: f32 = 1.5;
// Camera Raw look: lowering reaches Capture One's Highlight response on
// reconstructed highlights (notes: reference/highlight-recovery-gfx.md).
const CR_HIGHLIGHTS_LOWER: f32 = 2.5;
const CR_HIGHLIGHTS_SHAPE: f32 = 0.3;
const CR_HIGHLIGHTS_BLEND: f32 = 0.26;
const CR_HIGHLIGHTS_DESAT: f32 = 0.5;
const CR_HIGHLIGHTS_DESAT_START: f32 = 0.9;
const CR_HIGHLIGHTS_DESAT_END: f32 = 2.5;
const STANDARD_WHITES_RAISE: f32 = 1.7;
const STANDARD_WHITES_LOWER: f32 = 1.9;
const STANDARD_CONTRAST_RAISE: f32 = 1.35;

fn base_look_is_standard() -> bool {
    return adjustments.global.tonemapper_mode >= 2u;
}

fn base_look_is_camera_raw() -> bool {
    return adjustments.global.tonemapper_mode == 3u;
}

// Tone sliders under the Camera Raw look follow Lightroom Classic's measured
// display-EV change by display level (ILCE-7M3 sweeps, notes: reference/
// tone-controls.md), on one grid: −10…−3 EV in 1 EV steps, then 0.5 EV.
// Shadows and Blacks act on the blurred luminance (detail is kept), with the
// ±50 response as an exponent on the amount; Whites and Contrast act per
// channel like a tone curve, linear between their ±50 and ±100 responses.
const CR_SHADOWS_RAISE = array<f32, 14>(1.16, 1.16, 1.81, 2.46, 1.9, 1.34, 0.89, 0.44, 0.28, 0.12, 0.075, 0.03, 0.01, 0.0);
const CR_SHADOWS_LOWER = array<f32, 14>(-3.09, -3.09, -2.615, -2.14, -1.82, -1.5, -0.975, -0.45, -0.29, -0.13, -0.085, -0.04, -0.01, 0.0);
const CR_BLACKS_RAISE = array<f32, 14>(1.14, 1.14, 1.22, 1.3, 1.16, 1.02, 0.81, 0.6, 0.465, 0.33, 0.205, 0.08, 0.02, 0.0);
const CR_BLACKS_LOWER = array<f32, 14>(-10.0, -10.0, -10.0, -10.0, -8.33, -6.66, -4.02, -1.38, -1.025, -0.67, -0.415, -0.16, -0.04, 0.0);
const CR_SHADOWS_RAISE_EXPONENT: f32 = 1.1;
const CR_SHADOWS_LOWER_EXPONENT: f32 = 0.95;
const CR_BLACKS_RAISE_EXPONENT: f32 = 1.0;
const CR_BLACKS_LOWER_EXPONENT: f32 = 2.0;
const CR_WHITES_RAISE_50 = array<f32, 14>(0.01, 0.01, 0.01, 0.026, 0.051, 0.08, 0.115, 0.186, 0.226, 0.245, 0.266, 0.291, 0.5, 0.0);
const CR_WHITES_RAISE_100 = array<f32, 14>(0.03, 0.05, 0.1, 0.17, 0.31, 0.467, 0.682, 1.005, 1.067, 1.112, 1.126, 1.331, 0.562, 0.0);
const CR_WHITES_LOWER_50 = array<f32, 14>(0.0, 0.0, -0.01, -0.02, -0.036, -0.055, -0.082, -0.124, -0.159, -0.178, -0.192, -0.208, -0.164, -0.2);
const CR_WHITES_LOWER_100 = array<f32, 14>(0.0, 0.0, -0.01, -0.02, -0.036, -0.06, -0.094, -0.146, -0.189, -0.223, -0.251, -0.295, -0.278, -0.45);
const CR_CONTRAST_RAISE_50 = array<f32, 14>(-1.33, -1.27, -1.18, -1.129, -0.995, -0.802, -0.581, -0.369, -0.01, 0.099, 0.199, 0.266, 0.187, 0.0);
const CR_CONTRAST_RAISE_100 = array<f32, 14>(-3.67, -3.42, -3.12, -2.813, -2.496, -1.881, -1.268, -0.77, -0.02, 0.195, 0.399, 0.542, 0.354, 0.0);
const CR_CONTRAST_LOWER_50 = array<f32, 14>(0.97, 0.92, 0.88, 0.797, 0.734, 0.607, 0.438, 0.28, 0.01, -0.076, -0.169, -0.243, -0.187, 0.0);
const CR_CONTRAST_LOWER_100 = array<f32, 14>(1.9, 1.8, 1.68, 1.508, 1.382, 1.12, 0.789, 0.492, 0.01, -0.135, -0.302, -0.444, -0.435, 0.0);
const CR_DEHAZE_RAISE: f32 = 0.7;
const CR_DEHAZE_LOWER: f32 = 0.9;
const CR_DEHAZE_CHROMA: f32 = 1.9;
const CR_CONTRAST_CHANNEL_MIX: f32 = 0.7;
const CR_ROLLOFF_KNEE: f32 = 0.55;
const CR_ROLLOFF_LIMIT: f32 = 1.02;
// Share of the smallest channel taken from Camera Raw's curve ratio rather
// than from scaling with the largest channel's roll-off.
const CR_ROLLOFF_CONVERGE: f32 = 0.5;

// The neutral look's highlight roll-off of the largest channel (ramped
// linear).
fn cr_rolloff(x: f32) -> f32 {
    if (x <= CR_ROLLOFF_KNEE) {
        return x;
    }
    let span = CR_ROLLOFF_LIMIT - CR_ROLLOFF_KNEE;
    return CR_ROLLOFF_KNEE + span * (1.0 - exp(-(x - CR_ROLLOFF_KNEE) / span));
}

// Position on the tone grid: index and fraction.
fn cr_grid(level: f32) -> vec2<f32> {
    var f: f32;
    if (level < -3.0) {
        f = level + 10.0;
    } else {
        f = 7.0 + (level + 3.0) * 2.0;
    }
    f = clamp(f, 0.0, 13.0);
    let i = min(floor(f), 12.0);
    return vec2<f32>(i, f - i);
}

// The slope below the black ramp: infinite, so tone gains there are 1.
const CR_SLOPE_CRUSHED: f32 = 1e30;

// Slope (display EV per scene EV) of the curve segment `segment` (0..47);
// −1 is the straight extension below the first point.
fn camera_raw_curve_segment_slope(segment: i32, own_curve: bool) -> f32 {
    if (segment < 0) {
        return 1.0;
    }
    let i = u32(min(segment, 47));
    var a = DEFAULT_TONE_CURVE[i];
    var b = DEFAULT_TONE_CURVE[i + 1u];
    if (own_curve) {
        a = profile_curve_value(i);
        b = profile_curve_value(i + 1u);
    }
    return (b - a) * 4.0;
}

// Display EV and its slope against scene EV for a scene luminance under the
// Camera Raw look (black ramp and curve evaluated once, slope analytically).
//
// Both must be continuous in the luminance: the tone sliders turn a
// display-EV change into a gain by dividing by the slope, and Shadows and
// Blacks take it from the blurred luminance, so any step shows as contour
// lines along that blur. Below the black ramp (crushed to black) the slope
// is infinite, which leaves those pixels unchanged as the ramp's toe does
// next to it. A slope of 0 there gave them the largest gain (×14.6 at
// Shadows +100, ×80 per channel at Contrast −100) and lifted near-black
// shadows into grey blobs and speckles.
fn camera_raw_level_slope(luma: f32) -> vec2<f32> {
    let black = camera_raw_black();
    let x_in = max(luma, 0.0) * exp2(adjustments.global.baseline_exposure);
    let ramp_slope = 1.0 / (1.0 - black);
    let radius = min(0.5 * black, 0.0625 / ramp_slope);
    var x: f32;
    var dx: f32;
    // The toe's EV slope x_in · dx / x is 2 · x_in / y; taken directly where
    // x falls below the 1e-9 floor of the level, which made the slope
    // collapse right above the toe's bottom (Contrast −100 lifted such a
    // pixel ×80).
    var toe_ev_slope = 0.0;
    if (x_in <= black - radius) {
        return vec2<f32>(-20.0, CR_SLOPE_CRUSHED);
    } else if (x_in >= black + radius) {
        x = (x_in - black) * ramp_slope;
        dx = ramp_slope;
    } else {
        let y = x_in - (black - radius);
        x = ramp_slope / (4.0 * radius) * y * y;
        dx = ramp_slope / (2.0 * radius) * y;
        if (x < 1e-9) {
            toe_ev_slope = 2.0 * x_in / y;
        }
    }
    if (adjustments.global.profile_curve[15].w > 1.5 && x > CR_ROLLOFF_KNEE) {
        let span = CR_ROLLOFF_LIMIT - CR_ROLLOFF_KNEE;
        let falloff = exp(-(x - CR_ROLLOFF_KNEE) / span);
        dx *= falloff;
        x = CR_ROLLOFF_KNEE + span * (1.0 - falloff);
    }
    let flag = adjustments.global.profile_curve[15].w;
    // 1: the file's own curve; 3: an own curve with neutral colour.
    // 4: a camera profile's curve (5: the profile uses the default curve).
    let own_curve = (flag > 0.5 && flag < 1.5) || (flag > 2.5 && flag < 4.5);
    if (x >= 1.0) {
        // White: the curve's last segment, so the slope (and with it the
        // tone gains) stays continuous where a look without the roll-off
        // clips. A slope of 0 here stepped Whites −100 down by 0.24 EV at
        // white under the file's own curve.
        return vec2<f32>(0.0, camera_raw_curve_segment_slope(47, own_curve) * x_in * dx / x);
    }
    let ev = log2(max(x, 1e-9));
    let position = (ev - DEFAULT_TONE_CURVE_START_EV) * 4.0;
    var level: f32;
    var curve_slope = 1.0;
    if (position <= 0.0) {
        let first = select(DEFAULT_TONE_CURVE[0], profile_curve_value(0u), own_curve);
        level = ev + first - DEFAULT_TONE_CURVE_START_EV;
    } else {
        let index = min(u32(position), 47u);
        let t = position - f32(index);
        var a = DEFAULT_TONE_CURVE[index];
        var b = DEFAULT_TONE_CURVE[index + 1u];
        if (own_curve) {
            a = profile_curve_value(index);
            b = profile_curve_value(index + 1u);
        }
        level = mix(a, b, t);
        // The slope at each curve point is the mean of its two segments,
        // interpolated between the points (a per-segment slope stepped by up
        // to 0.3 EV of gain every 0.25 EV).
        let s = i32(index);
        let before = camera_raw_curve_segment_slope(s - 1, own_curve);
        let segment = (b - a) * 4.0;
        let after = camera_raw_curve_segment_slope(s + 1, own_curve);
        curve_slope = mix(0.5 * (before + segment), 0.5 * (segment + after), t);
    }
    if (toe_ev_slope > 0.0) {
        return vec2<f32>(level, curve_slope * toe_ev_slope);
    }
    return vec2<f32>(level, curve_slope * x_in * dx / max(x, 1e-9));
}

fn cr_gain(delta: f32, slope: f32) -> f32 {
    return exp2(delta / max(slope, 0.3));
}

// The toe: Shadows +, Blacks + and Contrast − around and below the black
// point. Turning a display-EV change into a gain through the slope gives
// ≈ ×1 there (the black ramp's slope in EV grows without bound towards
// black), so the deepest tones stayed black with a steep, darker core next
// to lifted mid-shadows. Camera Raw instead lifts them by a scene gain that
// barely depends on the level in the toe and reveals detail inside crushed
// black. Measured per Camera Raw neutral-L* bin as the scene gain that
// gives Camera Raw's result through our neutral look (notes: reference/
// tone-controls.md, 2026-10-10): first on five cameras against Adobe
// Standard renders, refitted on 60 cameras against Camera Raw's actual
// default (Adobe Standard with the Adobe Color look, darker in the
// shadows), which lowered Blacks + and Contrast − by 0.2–0.3 EV around
// the black point. Log2 gain at ±50 and ±100 by log2(scene / black point)
// from −1 to 5 in 0.5 steps, constant below. It only adds to the
// slope-based lift where that is weaker, so mid-shadows keep their fit.
const CR_TOE_SHADOWS_50 = array<f32, 13>(1.1, 1.07, 0.93, 0.84, 0.73, 0.64, 0.54, 0.48, 0.36, 0.23, 0.14, 0.08, 0.0);
const CR_TOE_SHADOWS_100 = array<f32, 13>(1.6, 1.52, 1.55, 1.46, 1.33, 1.18, 1.0, 0.88, 0.65, 0.46, 0.32, 0.22, 0.0);
const CR_TOE_BLACKS_50 = array<f32, 13>(1.54, 1.24, 0.94, 0.72, 0.63, 0.53, 0.46, 0.42, 0.34, 0.26, 0.19, 0.16, 0.0);
const CR_TOE_BLACKS_100 = array<f32, 13>(1.73, 1.47, 1.17, 1.01, 0.96, 0.89, 0.84, 0.78, 0.68, 0.58, 0.49, 0.42, 0.0);
const CR_TOE_CONTRAST_50 = array<f32, 13>(1.16, 0.98, 0.79, 0.72, 0.59, 0.51, 0.44, 0.42, 0.33, 0.21, 0.08, 0.0, 0.0);
const CR_TOE_CONTRAST_100 = array<f32, 13>(1.56, 1.36, 1.18, 1.11, 0.95, 0.86, 0.78, 0.71, 0.57, 0.38, 0.18, 0.0, 0.0);

// Position on the toe grid for a (blurred or channel) scene luminance.
// Without a black point (DefaultBlackRender = None) there is no toe.
fn cr_toe_position(luma: f32) -> vec2<f32> {
    let black = camera_raw_black();
    var f = 12.0;
    if (black > 0.0) {
        let x_in = max(luma, 0.0) * exp2(adjustments.global.baseline_exposure);
        f = clamp((log2(max(x_in, 1e-12) / black) + 1.0) * 2.0, 0.0, 12.0);
    }
    let i = min(floor(f), 11.0);
    return vec2<f32>(i, f - i);
}

fn cr_toe_shadows(p: vec2<f32>, amount: f32) -> f32 {
    let i = u32(p.x);
    return cr_two_point(
        mix(CR_TOE_SHADOWS_50[i], CR_TOE_SHADOWS_50[i + 1u], p.y),
        mix(CR_TOE_SHADOWS_100[i], CR_TOE_SHADOWS_100[i + 1u], p.y), amount);
}

fn cr_toe_blacks(p: vec2<f32>, amount: f32) -> f32 {
    let i = u32(p.x);
    return cr_two_point(
        mix(CR_TOE_BLACKS_50[i], CR_TOE_BLACKS_50[i + 1u], p.y),
        mix(CR_TOE_BLACKS_100[i], CR_TOE_BLACKS_100[i + 1u], p.y), amount);
}

fn cr_toe_contrast(p: vec2<f32>, amount: f32) -> f32 {
    let i = u32(p.x);
    return cr_two_point(
        mix(CR_TOE_CONTRAST_50[i], CR_TOE_CONTRAST_50[i + 1u], p.y),
        mix(CR_TOE_CONTRAST_100[i], CR_TOE_CONTRAST_100[i + 1u], p.y), amount);
}

// The toe lift added to a log2 gain: up to the toe's gain where the
// slope-based lift is smaller, nothing where it is larger. Continuous in
// both (also where the slope-based gain is negative, e.g. Contrast − above
// its pivot, the toe is 0 there).
fn cr_with_toe(log_gain: f32, toe: f32) -> f32 {
    return log_gain + max(toe - max(log_gain, 0.0), 0.0);
}

// Camera Raw's Shadows adapts to the photograph (Blacks and Contrast do
// not). Fitted per log2(scene / black point) band on 60 raw.pixls.us
// cameras and checked on 29 held-out ones (notes: reference/
// tone-controls.md, 2026-10-10), with K = log2(90th percentile of scene
// luminance with the baseline / 0.006) and R = log2(p90 / p10) of the
// decode (image_key.rs; 0 = unknown: no adaptation):
// - raising lifts more on brighter (K) and wider-ranged (R) frames,
//   ×amount^1.3 and ×amount^0.4 (±50 adapted 0.4 and 0.75 as much); not
//   below the black point, where adapting did worse on held-out cameras;
// - lowering darkens more on brighter frames (K, ×amount^0.8);
// - corrections at the reference key, at ±50 and ±100 (references:
//   Camera Raw's default Adobe Color): lowering (Lightroom's −100 table
//   darkened u 0–1.5 up to 0.45 EV more than Camera Raw) and raising
//   (u 1.5–2 lifted 0.12 EV, ±50 u 0–1.5 0.15 EV too much).
// Tables per unit of K − K0 or R − R0 or of log2 gain, log2 u from −1 to
// 7 in 0.5 steps.
const CR_KEY0: f32 = 5.94;
const CR_KEY_RANGE: f32 = 2.0;
const CR_RANGE0: f32 = 4.1;
const CR_RANGE_RANGE: f32 = 3.0;
// Narrow frames lose at most one range EV of lift: an α7R III frame with a
// narrow range needed more lift than any global statistic predicts (a local
// effect), and the limit cost held-out cameras almost nothing.
const CR_RANGE_LOW: f32 = 1.0;
const CR_RANGE_FLOOR: f32 = 0.000244140625;
const CR_KEY_RAISE_EXPONENT: f32 = 1.3;
const CR_RANGE_RAISE_EXPONENT: f32 = 0.4;
const CR_ADAPT_KEEP: f32 = 0.25;
const CR_SHADOWS_KEY_RAISE = array<f32, 17>(0.0, 0.0, 0.13, 0.26, 0.28, 0.29, 0.31, 0.33, 0.28, 0.27, 0.23, 0.18, 0.1, 0.09, 0.03, 0.0, 0.0);
const CR_SHADOWS_RANGE_RAISE = array<f32, 17>(0.0, 0.0, 0.1, 0.2, 0.16, 0.12, 0.11, 0.09, 0.1, 0.07, 0.04, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
const CR_SHADOWS_KEY_LOWER = array<f32, 17>(0.12, 0.17, 0.22, 0.2, 0.19, 0.19, 0.27, 0.34, 0.38, 0.33, 0.26, 0.18, 0.12, 0.08, 0.03, 0.0, 0.0);
const CR_SHADOWS_RAISE_FIX_50 = array<f32, 17>(0.15, 0.02, -0.14, -0.15, -0.18, -0.17, -0.11, -0.06, -0.02, 0.01, 0.03, 0.05, 0.04, -0.03, 0.0, 0.0, 0.0);
const CR_SHADOWS_RAISE_FIX_100 = array<f32, 17>(0.19, 0.14, -0.08, 0.03, -0.01, -0.12, -0.12, -0.04, -0.01, 0.02, 0.06, 0.06, 0.06, -0.02, 0.0, 0.0, 0.0);
const CR_SHADOWS_LOWER_FIX_50 = array<f32, 17>(0.0, 0.0, 0.0, 0.14, 0.2, 0.15, 0.06, -0.01, -0.03, 0.0, -0.03, -0.02, -0.02, -0.04, 0.0, 0.0, 0.0);
const CR_SHADOWS_LOWER_FIX_100 = array<f32, 17>(0.0, 0.0, 0.0, 0.15, 0.35, 0.36, 0.24, 0.12, 0.03, 0.0, -0.05, -0.05, -0.04, -0.04, 0.0, 0.0, 0.0);
const CR_KEY_LOWER_EXPONENT: f32 = 0.8;

// (K − K0, R − R0), limited; 0 without a key.
fn cr_key_offset() -> vec2<f32> {
    let key = adjustments.global.image_key;
    if (key <= 0.0) {
        return vec2<f32>(0.0);
    }
    // Without Exposure: Camera Raw's lift at Exposure +1 rose by what the
    // display-level response already gives (20 cameras); adding Exposure
    // to the key doubled that.
    let k = log2(key) + adjustments.global.baseline_exposure - log2(CAMERA_RAW_BLACK_NEUTRAL);
    let r = log2(key / max(adjustments.global.image_key_low, CR_RANGE_FLOOR));
    return vec2<f32>(
        clamp(k - CR_KEY0, -CR_KEY_RANGE, CR_KEY_RANGE),
        clamp(r - CR_RANGE0, -CR_RANGE_LOW, CR_RANGE_RANGE));
}

// Position on the wide grid (log2(scene / black point) −1…7).
fn cr_wide_position(luma: f32) -> vec2<f32> {
    let black = camera_raw_black();
    var f = 16.0;
    if (black > 0.0) {
        let x_in = max(luma, 0.0) * exp2(adjustments.global.baseline_exposure);
        f = clamp((log2(max(x_in, 1e-12) / black) + 1.0) * 2.0, 0.0, 16.0);
    }
    let i = min(floor(f), 15.0);
    return vec2<f32>(i, f - i);
}

// 0: raising per key EV, 1: lowering per key EV, 4: raising per range EV;
// 5/6: raising correction at 50/100, 7/8: lowering correction at 50/100.
fn cr_wide(p: vec2<f32>, table: u32) -> f32 {
    let i = u32(p.x);
    if (table == 0u) {
        return mix(CR_SHADOWS_KEY_RAISE[i], CR_SHADOWS_KEY_RAISE[i + 1u], p.y);
    }
    if (table == 1u) {
        return mix(CR_SHADOWS_KEY_LOWER[i], CR_SHADOWS_KEY_LOWER[i + 1u], p.y);
    }
    if (table == 4u) {
        return mix(CR_SHADOWS_RANGE_RAISE[i], CR_SHADOWS_RANGE_RAISE[i + 1u], p.y);
    }
    if (table == 5u) {
        return mix(CR_SHADOWS_RAISE_FIX_50[i], CR_SHADOWS_RAISE_FIX_50[i + 1u], p.y);
    }
    if (table == 6u) {
        return mix(CR_SHADOWS_RAISE_FIX_100[i], CR_SHADOWS_RAISE_FIX_100[i + 1u], p.y);
    }
    if (table == 7u) {
        return mix(CR_SHADOWS_LOWER_FIX_50[i], CR_SHADOWS_LOWER_FIX_50[i + 1u], p.y);
    }
    return mix(CR_SHADOWS_LOWER_FIX_100[i], CR_SHADOWS_LOWER_FIX_100[i + 1u], p.y);
}

// Display-EV change linear between the ±50 and ±100 responses.
fn cr_two_point(half: f32, full: f32, amount: f32) -> f32 {
    let a = min(abs(amount), 1.0);
    if (a <= 0.5) {
        return half * a * 2.0;
    }
    return mix(half, full, (a - 0.5) * 2.0);
}

fn cr_whites_delta(amount: f32, g: vec2<f32>) -> f32 {
    let i = u32(g.x);
    if (amount > 0.0) {
        return cr_two_point(
            mix(CR_WHITES_RAISE_50[i], CR_WHITES_RAISE_50[i + 1u], g.y),
            mix(CR_WHITES_RAISE_100[i], CR_WHITES_RAISE_100[i + 1u], g.y), amount);
    }
    return cr_two_point(
        mix(CR_WHITES_LOWER_50[i], CR_WHITES_LOWER_50[i + 1u], g.y),
        mix(CR_WHITES_LOWER_100[i], CR_WHITES_LOWER_100[i + 1u], g.y), amount);
}

fn cr_contrast_delta(amount: f32, g: vec2<f32>) -> f32 {
    let i = u32(g.x);
    if (amount > 0.0) {
        return cr_two_point(
            mix(CR_CONTRAST_RAISE_50[i], CR_CONTRAST_RAISE_50[i + 1u], g.y),
            mix(CR_CONTRAST_RAISE_100[i], CR_CONTRAST_RAISE_100[i + 1u], g.y), amount);
    }
    return cr_two_point(
        mix(CR_CONTRAST_LOWER_50[i], CR_CONTRAST_LOWER_50[i + 1u], g.y),
        mix(CR_CONTRAST_LOWER_100[i], CR_CONTRAST_LOWER_100[i + 1u], g.y), amount);
}

// Tone order. The Camera Raw-look sliders turn a display-EV change into a
// scene gain by dividing it by the slope of the display response; where
// that slope (or a lift table) changes fast, the gain changed faster than
// the level rose and a brighter tone came out darker. On a grey ramp (GPU
// test `camera_raw_tone_sliders_keep_tones_in_order`) this starts at fixed
// levels:
// - lowering, from the black ramp's toe top, where its slope
//   (∝ x / (x − black)) falls fastest (Blacks −50 fell 0.07 EV, Contrast
//   +100 likewise near black), and inside the toe from its bottom (Blacks
//   −100 reversed below black, which Contrast −100 then lifted into view);
// - lowering, from the highlight roll-off's knee, where the slope falls to
//   the 0.3 floor (Whites −100 fell 0.22 EV from scene −0.85 to −0.35 EV);
// - Shadows + and Blacks + together, from the toe tables' last segment,
//   where both lifts end at once (−1.28 EV per EV), and from the Shadows
//   table's peak at display −7 EV, where both lifts fall together.
// From each such level a floor keeps the result rising at least `rise` EV
// per EV from that level's own result. Where the response was in order the
// floor stays below it and the measured response is unchanged. The levels
// are fixed: a tone curve of another shape can reverse elsewhere (a
// straight log-log curve still does above the knee).
const CR_ORDER_RISE_TOE: f32 = 0.1;
// Lowered highlights keep a quarter of their scene gradient. Whites −100
// then changes display −0.3 / −0.2 / −0.1 EV by −0.42 / −0.47 / −0.50 EV
// (it was −0.48 / −0.70 / −0.69), continuing Lightroom's measured −0.38 /
// −0.44 at −1 / −0.5 EV (notes: reference/tone-controls.md).
const CR_ORDER_RISE_SHOULDER: f32 = 0.25;
// The toe tables' last segment starts at 2^4.5 × the black point.
const CR_TOE_LAST_SEGMENT: f32 = 22.627417;

// The gain that keeps `level` rising at least `rise` EV per EV from
// `anchor`'s result (`gain_anchor` there); 0 at and below the anchor.
fn cr_order_gain(level: f32, anchor: f32, gain_anchor: f32, rise: f32) -> f32 {
    if (anchor <= 0.0 || level <= anchor) {
        return 0.0;
    }
    return gain_anchor * pow(anchor / level, 1.0 - rise);
}

// Levels (inputs of `camera_raw_level_slope`) of the black ramp's toe
// bottom and top (0 without a black point) and of the roll-off knee (the
// same ramped level where a look has no roll-off).
fn cr_toe_bottom_level() -> f32 {
    let black = camera_raw_black();
    let radius = min(0.5 * black, 0.0625 * (1.0 - black));
    return max(black - radius, 0.0) / exp2(adjustments.global.baseline_exposure);
}

fn cr_toe_top_level() -> f32 {
    let black = camera_raw_black();
    if (black <= 0.0) {
        return 0.0;
    }
    let radius = min(0.5 * black, 0.0625 * (1.0 - black));
    return (black + radius) / exp2(adjustments.global.baseline_exposure);
}

fn cr_knee_level() -> f32 {
    let black = camera_raw_black();
    return (black + CR_ROLLOFF_KNEE * (1.0 - black)) / exp2(adjustments.global.baseline_exposure);
}

fn cr_whites_gain_unordered(x: f32, amount: f32) -> f32 {
    let ls = camera_raw_level_slope(x);
    return cr_gain(cr_whites_delta(amount, cr_grid(ls.x)), ls.y);
}

fn cr_whites_gain(x: f32, amount: f32) -> f32 {
    let gain = cr_whites_gain_unordered(x, amount);
    let knee = cr_knee_level();
    if (amount >= 0.0 || knee <= 0.0 || x <= knee) {
        return gain;
    }
    return max(gain, cr_order_gain(x, knee, cr_whites_gain_unordered(knee, amount), CR_ORDER_RISE_SHOULDER));
}

fn cr_whites(rgb: vec3<f32>, amount: f32) -> vec3<f32> {
    var out = rgb;
    for (var c = 0u; c < 3u; c = c + 1u) {
        out[c] = rgb[c] * cr_whites_gain(rgb[c], amount);
    }
    return out;
}

fn cr_contrast_gain_unordered(x: f32, amount: f32) -> f32 {
    let ls = camera_raw_level_slope(x);
    if (amount < 0.0) {
        // Lowering contrast lifts the toe like Camera Raw's.
        let log_gain = cr_contrast_delta(amount, cr_grid(ls.x)) / max(ls.y, 0.3);
        let toe = cr_toe_contrast(cr_toe_position(x), amount);
        return exp2(cr_with_toe(log_gain, toe));
    }
    return cr_gain(cr_contrast_delta(amount, cr_grid(ls.x)), ls.y);
}

// Raising darkens the deep tones (toe), lowering the highlights (shoulder).
fn cr_contrast_gain(x: f32, amount: f32) -> f32 {
    var gain = cr_contrast_gain_unordered(x, amount);
    if (amount > 0.0) {
        let bottom = cr_toe_bottom_level();
        if (bottom > 0.0 && x > bottom) {
            gain = max(gain, cr_order_gain(x, bottom, cr_contrast_gain_unordered(bottom, amount), 0.0));
            let top = cr_toe_top_level();
            gain = max(gain, cr_order_gain(x, top, cr_contrast_gain_unordered(top, amount), CR_ORDER_RISE_TOE));
        }
    } else {
        let knee = cr_knee_level();
        if (knee > 0.0 && x > knee) {
            gain = max(gain, cr_order_gain(x, knee, cr_contrast_gain_unordered(knee, amount), CR_ORDER_RISE_SHOULDER));
        }
    }
    return gain;
}

fn cr_contrast(rgb: vec3<f32>, amount: f32) -> vec3<f32> {
    var out = rgb;
    for (var c = 0u; c < 3u; c = c + 1u) {
        out[c] = rgb[c] * cr_contrast_gain(rgb[c], amount);
    }
    return out;
}

fn camera_raw_shadows_blacks_gain(level_luma: f32, sh: f32, bl: f32) -> f32 {
    var gain = camera_raw_shadows_blacks_gain_unordered(level_luma, sh, bl);
    let bottom = cr_toe_bottom_level();
    if (bottom <= 0.0 || level_luma <= bottom) {
        return gain;
    }
    if (sh < 0.0 || bl < 0.0) {
        gain = max(gain, cr_order_gain(level_luma, bottom,
            camera_raw_shadows_blacks_gain_unordered(bottom, sh, bl), 0.0));
        let top = cr_toe_top_level();
        gain = max(gain, cr_order_gain(level_luma, top,
            camera_raw_shadows_blacks_gain_unordered(top, sh, bl), CR_ORDER_RISE_TOE));
    }
    if (sh > 0.0 && bl > 0.0) {
        let last = camera_raw_black() * CR_TOE_LAST_SEGMENT / exp2(adjustments.global.baseline_exposure);
        gain = max(gain, cr_order_gain(level_luma, last,
            camera_raw_shadows_blacks_gain_unordered(last, sh, bl), CR_ORDER_RISE_TOE));
        let peak = cr_curve_level(CR_RAISE_PEAK_DISPLAY);
        gain = max(gain, cr_order_gain(level_luma, peak,
            camera_raw_shadows_blacks_gain_unordered(peak, sh, bl), CR_ORDER_RISE_TOE));
    }
    return gain;
}

// Display level of the Shadows + table's peak (`CR_SHADOWS_RAISE`, grid 3):
// above it the lift falls 0.56 EV per EV, with Blacks + 0.7.
const CR_RAISE_PEAK_DISPLAY: f32 = -7.0;

// The level (input of `camera_raw_level_slope`) that the look's tone curve
// (default or own) shows at `display` EV, for levels above the black ramp's
// toe and below the roll-off.
fn cr_curve_level(display: f32) -> f32 {
    let flag = adjustments.global.profile_curve[15].w;
    let own_curve = (flag > 0.5 && flag < 1.5) || (flag > 2.5 && flag < 4.5);
    var ev = default_tone_curve_inverse(display);
    if (own_curve) {
        var i = 0u;
        for (var k = 0u; k < 48u; k = k + 1u) {
            if (profile_curve_value(k + 1u) <= display) {
                i = k + 1u;
            }
        }
        i = min(i, 47u);
        let a = profile_curve_value(i);
        let b = profile_curve_value(i + 1u);
        ev = DEFAULT_TONE_CURVE_START_EV + (f32(i) + clamp((display - a) / max(b - a, 1e-6), 0.0, 1.0)) * 0.25;
    }
    let black = camera_raw_black();
    return (exp2(ev) * (1.0 - black) + black) / exp2(adjustments.global.baseline_exposure);
}

fn camera_raw_shadows_blacks_gain_unordered(level_luma: f32, sh: f32, bl: f32) -> f32 {
    // Renderer values are slider / 120 (Shadows) and slider / 40 (Blacks).
    let shadows = clamp(sh * 1.2, -1.0, 1.0);
    let blacks = clamp(bl * 0.4, -1.0, 1.0);
    let ls = camera_raw_level_slope(level_luma);
    let g = cr_grid(ls.x);
    let i = u32(g.x);
    var delta = 0.0;
    var raise = 0.0;
    if (shadows > 0.0) {
        raise += mix(CR_SHADOWS_RAISE[i], CR_SHADOWS_RAISE[i + 1u], g.y) * pow(shadows, CR_SHADOWS_RAISE_EXPONENT);
    } else if (shadows < 0.0) {
        delta += mix(CR_SHADOWS_LOWER[i], CR_SHADOWS_LOWER[i + 1u], g.y) * pow(-shadows, CR_SHADOWS_LOWER_EXPONENT);
    }
    if (blacks > 0.0) {
        raise += mix(CR_BLACKS_RAISE[i], CR_BLACKS_RAISE[i + 1u], g.y) * pow(blacks, CR_BLACKS_RAISE_EXPONENT);
    } else if (blacks < 0.0) {
        delta += mix(CR_BLACKS_LOWER[i], CR_BLACKS_LOWER[i + 1u], g.y) * pow(-blacks, CR_BLACKS_LOWER_EXPONENT);
    }
    if (shadows == 0.0 && blacks <= 0.0) {
        return cr_gain(delta, ls.y);
    }
    let slope = max(ls.y, 0.3);
    if (shadows < 0.0) {
        // Lowering Shadows: the measured response, made image-adaptive,
        // and corrected where it darkened more than Camera Raw.
        let q = cr_wide_position(level_luma);
        let adapted = -cr_wide(q, 1u) * pow(-shadows, CR_KEY_LOWER_EXPONENT) * cr_key_offset().x
            + cr_two_point(cr_wide(q, 7u), cr_wide(q, 8u), shadows);
        // Lowering keeps at least a quarter of its darkening (a low-key
        // frame would otherwise turn it into a lift).
        if (blacks <= 0.0) {
            return exp2(min(delta / slope + adapted, CR_ADAPT_KEEP * delta / slope));
        }
        // Blacks raising with Shadows lowering: as below, plus the change.
        let p = cr_toe_position(level_luma);
        let fade = 1.0 + shadows;
        let toe = cr_toe_blacks(p, blacks) * fade * fade * fade;
        return exp2(cr_with_toe(raise / slope, toe)
            + min(delta / slope + adapted, CR_ADAPT_KEEP * delta / slope));
    }
    // Raising: the slope-based lift, then the toe where that is weaker;
    // a lowering slider adds its own change on top.
    let p = cr_toe_position(level_luma);
    var toe = 0.0;
    if (shadows > 0.0) {
        toe += cr_toe_shadows(p, shadows);
    }
    if (blacks > 0.0) {
        toe += cr_toe_blacks(p, blacks);
    }
    // The other slider lowering fades the toe out (cubically): its
    // darkening grows out of the toe faster than the toe's lift falls,
    // which would reorder tones there (Shadows +100 with Blacks −50 fell
    // 0.013 EV between neighbouring levels with a linear fade; with Blacks
    // −100 the slope-based sum is left as before).
    let fade = 1.0 - max(max(-shadows, -blacks), 0.0);
    toe *= fade * fade * fade;
    var lift = cr_with_toe(raise / slope, toe);
    if (shadows > 0.0) {
        // Raising keeps at least a quarter of its lift on low-key frames.
        let q = cr_wide_position(level_luma);
        let key = cr_key_offset();
        let adapted = cr_wide(q, 0u) * pow(shadows, CR_KEY_RAISE_EXPONENT) * key.x
            + cr_wide(q, 4u) * pow(shadows, CR_RANGE_RAISE_EXPONENT) * key.y
            + cr_two_point(cr_wide(q, 5u), cr_wide(q, 6u), shadows);
        lift = max(lift + adapted, CR_ADAPT_KEEP * lift);
    }
    return exp2(lift + delta / slope);
}

const WHITES_SCALE: f32 = 3.3333333;
const WHITES_RAISE: f32 = 0.5;
const WHITES_LOWER: f32 = 0.2;
const WHITES_REACH_RAISE: f32 = 1.3;
const WHITES_REACH_LOWER: f32 = 2.5;
// Shadows (renderer value slider / 120) against the same references.
const SHADOWS_RAISE: f32 = 1.8;
const SHADOWS_LOWER: f32 = 1.2;

fn apply_tonal_adjustments(
    color: vec3<f32>,
    blurred_color_input_space: vec3<f32>,
    is_raw: u32,
    con: f32,
    sh: f32,
    wh: f32,
    bl: f32,
    exposure_gain: f32
) -> vec3<f32> {
    var rgb = color;

    var blurred_linear: vec3<f32>;
    if (is_raw == 1u) {
        blurred_linear = blurred_color_input_space;
    } else {
        blurred_linear = srgb_to_linear(blurred_color_input_space);
    }

    if (wh != 0.0 && base_look_is_camera_raw()) {
        // Renderer value is slider / 30.
        let amount = clamp(wh * 0.3, -1.0, 1.0);
        var whitened = cr_whites(rgb, amount);
        if (any(rgb < vec3<f32>(0.0))) {
            let floored = max(rgb, vec3<f32>(0.0));
            whitened = carry_scene(rgb, floored, cr_whites(floored, amount));
        }
        let luma_before = max(get_luma(max(rgb, vec3<f32>(0.0))), 1e-6);
        let ratio = max(get_luma(max(whitened, vec3<f32>(0.0))), 0.0) / luma_before;
        rgb = whitened;
        blurred_linear *= ratio;
    } else if (wh != 0.0) {
        // Whites moves the upper tones, as Capture One's and Lightroom's White
        // sliders do (it used to scale every tone like a white-point gain).
        // Raising reaches further down than lowering, as in both references.
        var w = wh / WHITES_SCALE;
        if (base_look_is_standard()) {
            w *= select(STANDARD_WHITES_LOWER, STANDARD_WHITES_RAISE, w > 0.0);
        }
        let t = clamp(pow(max(get_luma(max(rgb, vec3<f32>(0.0))), 0.0), 0.4545), 0.0, 1.0);
        let gain = select(WHITES_LOWER, WHITES_RAISE, w > 0.0);
        let reach = select(WHITES_REACH_LOWER, WHITES_REACH_RAISE, w > 0.0);
        let ratio = pow(max(1.0 + gain * w * pow(t, reach), 0.05), 2.2);
        rgb *= ratio;
        blurred_linear *= ratio;
    }

    let pixel_luma = get_luma(max(rgb, vec3<f32>(0.0)));
    let blurred_luma = get_luma(max(blurred_linear, vec3<f32>(0.0)));

    let safe_pixel_luma = max(pixel_luma, 0.0001);
    let safe_blurred_luma = max(blurred_luma, 0.0001);

    if ((sh != 0.0 || bl != 0.0) && base_look_is_camera_raw()) {
        // Lightroom's measured response by display level, applied as a
        // gain from the blurred (exposed) luminance so detail is kept.
        let gain = camera_raw_shadows_blacks_gain(safe_blurred_luma * exposure_gain, sh, bl);
        rgb *= gain;
    } else if (sh != 0.0 || bl != 0.0) {
        let t_pixel = pow(safe_pixel_luma, 0.4545);
        let t_blurred = pow(safe_blurred_luma, 0.4545);

        let shadow_gain = select(SHADOWS_LOWER, SHADOWS_RAISE, sh > 0.0);
        let shadow_lift = sh * shadow_gain * t_pixel * pow(max(1.0 - t_pixel, 0.0), 4.5);
        let black_lift = bl * t_pixel * pow(max(1.0 - t_pixel, 0.0), 12.0);
        let lift_amount = max(shadow_lift + black_lift, 0.0);

        let t_pixel_curved = max(t_pixel + shadow_lift + black_lift, 0.0);

        let shadow_pivot = 0.2;
        let stretch_factor = 1.0 + (lift_amount * 1.3);
        let contrasted_t = shadow_pivot + (t_pixel_curved - shadow_pivot) * stretch_factor;

        let final_t = max(mix(t_pixel_curved, contrasted_t, 0.85), 0.0);
        let curved_luma = pow(final_t, 2.2);

        let luma_ratio = curved_luma / safe_pixel_luma;
        rgb *= luma_ratio;

        let detail = t_pixel / max(t_blurred, 0.0001);
        let safe_detail = clamp(detail, 0.8, 1.25);

        let noise_protection = smoothstep(0.0, 0.1, t_blurred);

        let detail_amp = 1.0 + (lift_amount * 1.2 * noise_protection);

        let enhanced_detail = pow(safe_detail, detail_amp);
        let detail_correction = enhanced_detail / safe_detail;

        let linear_correction = pow(detail_correction, 2.2);
        rgb *= linear_correction;

        if (luma_ratio > 1.0) {
            let recovered_luma = get_luma(rgb);
            let boost_amount = clamp((luma_ratio - 1.0) * 0.15, 0.0, 0.4);
            rgb = mix(rgb, vec3<f32>(recovered_luma), boost_amount);
        }
    }

    if (con != 0.0 && base_look_is_camera_raw()) {
        // Renderer value is slider / 100. Per channel alone over-shoots
        // Lightroom's saturation change (×1.76 against ×1.49 at +100), so
        // it is mixed with the same luminance change applied neutrally.
        let safe = max(rgb, vec3<f32>(0.0));
        let per_channel = cr_contrast(safe, clamp(con, -1.0, 1.0));
        let neutral = safe * (get_luma(per_channel) / max(get_luma(safe), 1e-6));
        let contrasted = mix(neutral, per_channel, CR_CONTRAST_CHANNEL_MIX);
        if (any(rgb < vec3<f32>(0.0))) {
            rgb = carry_scene(rgb, safe, contrasted);
        } else {
            rgb = contrasted;
        }
    } else if (con != 0.0) {
        let safe_rgb = max(rgb, vec3<f32>(0.0));
        let g = 2.2;
        let perceptual = pow(safe_rgb, vec3<f32>(1.0 / g));
        let clamped_perceptual = clamp(perceptual, vec3<f32>(0.0), vec3<f32>(1.0));
        // Half the inherited slope: ±100 now matches Lightroom's ±100
        // (the inherited ±50 did); notes: reference/tone-controls.md.
        var strength = pow(2.0, con * 0.625);
        if (base_look_is_standard() && con > 0.0) {
            strength = pow(2.0, con * 0.625 * STANDARD_CONTRAST_RAISE);
        }
        var curved_perceptual: vec3<f32>;
        if (base_look_is_standard()) {
            let pivot = STANDARD_CONTRAST_PIVOT;
            let condition = clamped_perceptual < vec3<f32>(pivot);
            let high_part = 1.0 - (1.0 - pivot) * pow((1.0 - clamped_perceptual) / (1.0 - pivot), vec3<f32>(strength));
            let low_part = pivot * pow(clamped_perceptual / pivot, vec3<f32>(strength));
            curved_perceptual = select(high_part, low_part, condition);
        } else {
            let condition = clamped_perceptual < vec3<f32>(0.5);
            let high_part = 1.0 - 0.5 * pow(2.0 * (1.0 - clamped_perceptual), vec3<f32>(strength));
            let low_part = 0.5 * pow(2.0 * clamped_perceptual, vec3<f32>(strength));
            curved_perceptual = select(high_part, low_part, condition);
        }
        let contrast_adjusted_rgb = pow(curved_perceptual, vec3<f32>(g));
        let mix_factor = smoothstep(vec3<f32>(1.0), vec3<f32>(1.01), safe_rgb);
        if (any(rgb < vec3<f32>(0.0))) {
            rgb = carry_scene(rgb, safe_rgb, mix(contrast_adjusted_rgb, safe_rgb, mix_factor));
        } else {
            rgb = mix(contrast_adjusted_rgb, rgb, mix_factor);
        }
    }
    return rgb;
}

fn apply_highlights_adjustment(
    color_in: vec3<f32>,
    coords_i: vec2<i32>,
    scale: f32,
    is_raw: u32,
    highlights_adj: f32
) -> vec3<f32> {
    if (abs(highlights_adj) < 0.001) {
        return color_in;
    }

    let pixel_luma = get_luma(max(color_in, vec3<f32>(0.0)));
    if (pixel_luma < 0.0001) {
        return color_in;
    }

    let standard_look = base_look_is_standard();
    let l_pivot = select(0.10, STANDARD_HIGHLIGHTS_PIVOT, standard_look);
    let lower_gain = select(STANDARD_HIGHLIGHTS_LOWER, CR_HIGHLIGHTS_LOWER, base_look_is_camera_raw());
    let highlights_gain = select(
        1.0,
        select(STANDARD_HIGHLIGHTS_RAISE, lower_gain, highlights_adj < 0.0),
        standard_look
    );
    if (pixel_luma <= l_pivot) {
        return color_in;
    }

    let dims = vec2<i32>(textureDimensions(input_texture));
    let max_idx = dims - vec2<i32>(1);

    var center_tex = textureLoad(input_texture, clamp(coords_i, vec2<i32>(0), max_idx), 0).rgb;
    if (is_raw == 0u) {
        center_tex = srgb_to_linear(center_tex);
    }
    let center_tex_luma = max(get_luma(center_tex), 1e-4);

    let r_inner = max(1, i32(round(3.5 * scale)));
    let r_outer = max(2, i32(round(7.5 * scale)));
    let r_diag  = max(1, i32(round(f32(r_outer) * 0.7071)));

    let offsets = array<vec2<i32>, 12>(
        vec2<i32>( r_inner,        0), vec2<i32>(-r_inner,        0),
        vec2<i32>(       0,  r_inner), vec2<i32>(       0, -r_inner),
        vec2<i32>( r_outer,        0), vec2<i32>(-r_outer,        0),
        vec2<i32>(       0,  r_outer), vec2<i32>(       0, -r_outer),
        vec2<i32>(  r_diag,   r_diag), vec2<i32>( -r_diag,   r_diag),
        vec2<i32>(  r_diag,  -r_diag), vec2<i32>( -r_diag,  -r_diag)
    );

    let spatial_weights = array<f32, 12>(
        0.85, 0.85, 0.85, 0.85,
        0.50, 0.50, 0.50, 0.50,
        0.50, 0.50, 0.50, 0.50
    );

    let range_tol = max(pixel_luma * 0.22, 0.03);
    let inv_two_range_sq = 1.0 / (2.0 * range_tol * range_tol);

    var sum_luma: f32 = pixel_luma;
    var sum_w: f32 = 1.0;

    for (var i = 0u; i < 12u; i = i + 1u) {
        let coord = clamp(coords_i + offsets[i], vec2<i32>(0), max_idx);
        var s_rgb = textureLoad(input_texture, coord, 0).rgb;
        if (is_raw == 0u) {
            s_rgb = srgb_to_linear(s_rgb);
        }
        let s_luma_raw = max(get_luma(s_rgb), 0.0);
        let s_luma = pixel_luma * (s_luma_raw / center_tex_luma);

        let diff = abs(s_luma - pixel_luma);
        let w = exp(- (diff * diff) * inv_two_range_sq) * spatial_weights[i];

        sum_luma += s_luma * w;
        sum_w += w;
    }

    let luma_base = sum_luma / sum_w;
    let detail_ratio = pixel_luma / max(luma_base, 1e-4);

    let safe_detail = clamp(detail_ratio, 0.65, 1.55);

    let log_detail = log2(safe_detail);
    let soft_log_detail = log_detail / (1.0 + abs(log_detail) * 0.40);

    var target_luma: f32 = pixel_luma;

    if (highlights_adj < 0.0) {
        let k = -highlights_adj;
        let delta_base = max(luma_base - l_pivot, 0.0);

        let compression_strength = k * 2.2 * highlights_gain;
        let shape = select(0.35, CR_HIGHLIGHTS_SHAPE, base_look_is_camera_raw());
        let compressed_delta = delta_base / (1.0 + compression_strength * (delta_base / (1.0 + delta_base * shape)));
        let target_base = l_pivot + compressed_delta;

        let restoration_gain = 1.0 + k * 0.55;
        let recovered_detail = exp2(soft_log_detail * restoration_gain);

        let recovered_target = target_base * recovered_detail;

        let blend_width = select(0.35, CR_HIGHLIGHTS_BLEND, base_look_is_camera_raw());
        let blend = smoothstep(l_pivot, l_pivot + blend_width, pixel_luma);
        target_luma = mix(pixel_luma, recovered_target, blend);

    } else {
        let boost = highlights_adj * 0.70 * highlights_gain;
        let delta_base = max(luma_base - l_pivot, 0.0);
        let boosted_delta = delta_base * (1.0 + boost / (1.0 + delta_base * 0.25));
        let target_base = l_pivot + boosted_delta;

        let detail_boost = exp2(soft_log_detail * (1.0 + highlights_adj * 0.20));
        let boosted_target = target_base * detail_boost;

        let blend = smoothstep(l_pivot, l_pivot + 0.35, pixel_luma);
        target_luma = mix(pixel_luma, boosted_target, blend);
    }

    let luma_ratio = target_luma / pixel_luma;
    var final_color = color_in * luma_ratio;

    // Compressed highlights lose colour as in Capture One: pixels brought
    // down from above white keep only part of their chroma (their colour
    // there is largely reconstructed from clipped channels).
    let blowout_start = select(1.0, CR_HIGHLIGHTS_DESAT_START, base_look_is_camera_raw());
    if (highlights_adj < 0.0 && pixel_luma > blowout_start) {
        let blowout_end = select(3.5, CR_HIGHLIGHTS_DESAT_END, base_look_is_camera_raw());
        let blowout_gain = select(0.40, CR_HIGHLIGHTS_DESAT, base_look_is_camera_raw());
        let blowout = smoothstep(blowout_start, blowout_end, pixel_luma) * (-highlights_adj) * blowout_gain;
        final_color = mix(final_color, vec3<f32>(target_luma), blowout);
    }

    return final_color;
}

fn apply_linear_exposure(color_in: vec3<f32>, exposure_adj: f32) -> vec3<f32> {
    if (exposure_adj == 0.0) {
        return color_in;
    }
    return color_in * pow(2.0, exposure_adj);
}

fn apply_filmic_exposure(color_in: vec3<f32>, brightness_adj: f32) -> vec3<f32> {
    if (brightness_adj == 0.0) {
        return color_in;
    }
    const RATIONAL_CURVE_MIX: f32 = 1.0;
    const MIDTONE_STRENGTH: f32 = 1.2;
    const TOP_ANCHOR: f32 = 1.0;

    let original_luma = get_luma(color_in);
    if (abs(original_luma) < 0.00001) {
        return color_in;
    }

    let direct_adj = brightness_adj * (1.0 - RATIONAL_CURVE_MIX);
    let rational_adj = brightness_adj * RATIONAL_CURVE_MIX;
    let scale = pow(2.0, direct_adj);
    let k = pow(2.0, -rational_adj * MIDTONE_STRENGTH);
    let luma_abs = abs(original_luma);

    var shaped_luma_abs: f32;
    if (luma_abs <= TOP_ANCHOR) {
        let luma_norm = luma_abs / TOP_ANCHOR;
        let shaped_norm = luma_norm / (luma_norm + (1.0 - luma_norm) * k);
        shaped_luma_abs = shaped_norm * TOP_ANCHOR;
    } else {
        shaped_luma_abs = TOP_ANCHOR + (luma_abs - TOP_ANCHOR) * k;
    }

    let new_luma_abs = shaped_luma_abs * scale;
    let new_luma = sign(original_luma) * new_luma_abs;
    let chroma = color_in - vec3<f32>(original_luma);

    let total_luma_scale = new_luma_abs / luma_abs;
    let luma_weight = clamp(new_luma_abs, 0.0, 2.0) * 0.5;
    let dynamic_exp = mix(0.95, 0.65, luma_weight);
    let base_chroma_scale = pow(total_luma_scale, dynamic_exp);

    let highlight_rolloff = clamp((1.05 - new_luma_abs) / max(1.05 - original_luma, 1e-4), 0.0, 1.0);
    let chroma_scale = base_chroma_scale * mix(1.0, highlight_rolloff, clamp(abs(brightness_adj), 0.0, 1.0));

    return vec3<f32>(new_luma) + chroma * chroma_scale;
}

fn apply_color_calibration(color: vec3<f32>, cal: ColorCalibrationSettings) -> vec3<f32> {
    let h_r = cal.red_hue;
    let h_g = cal.green_hue;
    let h_b = cal.blue_hue;
    let r_prime = vec3<f32>(1.0 - abs(h_r), max(0.0, h_r), max(0.0, -h_r));
    let g_prime = vec3<f32>(max(0.0, -h_g), 1.0 - abs(h_g), max(0.0, h_g));
    let b_prime = vec3<f32>(max(0.0, h_b), max(0.0, -h_b), 1.0 - abs(h_b));
    let hue_matrix = mat3x3<f32>(r_prime, g_prime, b_prime);
    var c = hue_matrix * color;

    let luma = get_luma(max(vec3(0.0), c));
    let desaturated_color = vec3<f32>(luma);
    let sat_vector = c - desaturated_color;

    let color_sum = c.r + c.g + c.b;
    var masks = vec3<f32>(0.0);
    if (color_sum > 0.001) {
        masks = c / color_sum;
    }

    let total_sat_adjustment =
        masks.r * cal.red_saturation +
        masks.g * cal.green_saturation +
        masks.b * cal.blue_saturation;

    c += sat_vector * total_sat_adjustment;

    let st = cal.shadows_tint;
    if (abs(st) > 0.001) {
        let shadow_luma = get_luma(max(vec3(0.0), c));
        let mask = 1.0 - smoothstep(0.0, 0.3, shadow_luma);
        let tint_mult = vec3<f32>(1.0 + st * 0.25, 1.0 - st * 0.25, 1.0 + st * 0.25);
        c = mix(c, c * tint_mult, mask);
    }

    return c;
}

fn apply_white_balance(color: vec3<f32>, temp: f32, tnt: f32) -> vec3<f32> {
    var rgb = color;
    let temp_kelvin_mult = vec3<f32>(1.0 + temp * 0.2, 1.0 + temp * 0.05, 1.0 - temp * 0.2);
    let tint_mult = vec3<f32>(1.0 + tnt * 0.25, 1.0 - tnt * 0.25, 1.0 + tnt * 0.25);
    // Keep neutral luminance: Capture One's ±2000 K moves brightness by
    // ±0.05 EV, the unnormalised gains here by ±0.5 EV at ±100.
    let mult = temp_kelvin_mult * tint_mult;
    let inverse_luma = 1.0 / max(dot(mult, LUMA_COEFF), 0.0001);
    rgb *= mult * inverse_luma;
    return rgb;
}

fn apply_creative_color(color: vec3<f32>, sat: f32, vib: f32) -> vec3<f32> {
    if (sat == 0.0 && vib == 0.0) {
        return color;
    }

    var processed = color;
    let luma = get_luma(gamut_floor(processed));

    let srgb = linear_to_srgb_extended(max(processed, vec3<f32>(0.0)));
    let hsv = rgb_to_hsv(srgb);
    let current_sat = hsv.y;
    let hue = hsv.x;

    var vib_factor: f32 = 0.0;
    if (vib != 0.0) {
        if (vib > 0.0) {
            // Rounding can put the saturation of a colour with a zero
            // channel just above 1; pow of a negative base is NaN, which
            // turned such reds grey.
            let sat_weight = pow(max(1.0 - current_sat, 0.0), 1.25);
            let skin_center = 25.0;
            let hue_dist = min(abs(hue - skin_center), 360.0 - abs(hue - skin_center));
            let is_skin = 1.0 - smoothstep(12.0, 38.0, hue_dist);
            let skin_dampener = mix(1.0, 0.50, is_skin);

            vib_factor = vib * sat_weight * skin_dampener * 1.5;
        } else {
            // Lowering affects every colour and saturated ones a little more:
            // Lightroom's Vibrance -50 removes about half the chroma (the
            // inherited weighting spared typical colours almost entirely).
            let desat_weight = 0.85 + 0.4 * smoothstep(0.0, 0.6, current_sat);
            vib_factor = vib * desat_weight;
        }
    }

    let sat_scale = max(1.0 + sat, 0.0);
    let vib_scale = max(1.0 + vib_factor, 0.0);
    let final_mult = sat_scale * vib_scale;

    processed = mix(vec3<f32>(luma), processed, final_mult);

    return gamut_floor(processed);
}

fn linear_srgb_to_oklab(c: vec3<f32>) -> vec3<f32> {
    let lms = vec3<f32>(
        0.4122214708 * c.r + 0.5363325363 * c.g + 0.0514459929 * c.b,
        0.2119034982 * c.r + 0.6806995451 * c.g + 0.1073969566 * c.b,
        0.0883024619 * c.r + 0.2817188376 * c.g + 0.6299787005 * c.b,
    );
    let l = sign(lms) * pow(abs(lms), vec3<f32>(1.0 / 3.0));
    return vec3<f32>(
        0.2104542553 * l.x + 0.7936177850 * l.y - 0.0040720468 * l.z,
        1.9779984951 * l.x - 2.4285922050 * l.y + 0.4505937099 * l.z,
        0.0259040371 * l.x + 0.7827717662 * l.y - 0.8086757660 * l.z,
    );
}

fn oklab_to_linear_srgb(lab: vec3<f32>) -> vec3<f32> {
    let l = vec3<f32>(
        lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z,
        lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z,
        lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z,
    );
    let lms = l * l * l;
    return vec3<f32>(
        4.0767416621 * lms.x - 3.3077115913 * lms.y + 0.2309699292 * lms.z,
        -1.2684380046 * lms.x + 2.6097574011 * lms.y - 0.3413193965 * lms.z,
        -0.0041960863 * lms.x - 0.7034186147 * lms.y + 1.7076147010 * lms.z,
    );
}

// The colour mixer follows perceived hue, as Lightroom's channels do: colours
// are selected by Oklab hue with centres where Lightroom's eight channels act
// and adjusted in Oklab LCh (notes: reference/hsl.md). The inherited panel
// selected and rotated HSV hue of sRGB values, which misplaced orange and
// yellow and lost chroma while rotating.
const HSL_OKLAB_CENTRES = array<f32, 8>(15.0, 52.0, 105.0, 145.0, 195.0, 262.0, 305.0, 340.0);
const HSL_OKLAB_REACH = array<f32, 8>(32.0, 38.0, 50.0, 55.0, 55.0, 50.0, 40.0, 38.0);
// Renderer values: hue = slider × 0.3, saturation and luminance = slider / 100.
// Lightroom turns each channel roughly in proportion to the gap to its
// neighbours, so blue moves furthest; aqua, purple and magenta were not
// populated in the reference photograph and are interpolated.
const HSL_HUE_DEGREES = array<f32, 8>(1.0, 1.2, 1.15, 1.15, 1.5, 2.6, 1.7, 1.4);
const HSL_CHROMA_GAIN: f32 = 0.9;
const HSL_LIGHTNESS_GAIN: f32 = 0.16;

fn apply_hsl_panel(color: vec3<f32>, hsl_adjustments: array<HslColor, 8>, coords_i: vec2<i32>) -> vec3<f32> {
    let safe_color = gamut_floor(color);
    let lab = linear_srgb_to_oklab(safe_color);
    let chroma = length(lab.yz);
    let colourful = smoothstep(0.0, 0.03, chroma);
    if (colourful < 0.001) {
        return safe_color;
    }
    let hue = degrees(atan2(lab.z, lab.y));

    var influences: array<f32, 8>;
    var total: f32 = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let distance_deg = abs(((hue - HSL_OKLAB_CENTRES[i]) % 360.0 + 540.0) % 360.0 - 180.0);
        let x = clamp(distance_deg / HSL_OKLAB_REACH[i], 0.0, 1.0);
        let influence = 0.5 + 0.5 * cos(x * 3.14159265);
        influences[i] = influence;
        total += influence;
    }

    var hue_shift: f32 = 0.0;
    var chroma_change: f32 = 0.0;
    var lightness_change: f32 = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let weight = influences[i] / max(total, 0.0001) * colourful;
        hue_shift += hsl_adjustments[i].hue * HSL_HUE_DEGREES[i] * weight;
        chroma_change += hsl_adjustments[i].saturation * HSL_CHROMA_GAIN * weight;
        lightness_change += hsl_adjustments[i].luminance * HSL_LIGHTNESS_GAIN * weight;
    }
    if (hue_shift == 0.0 && chroma_change == 0.0 && lightness_change == 0.0) {
        return safe_color;
    }

    let new_hue = radians(hue + hue_shift);
    let new_chroma = chroma * max(1.0 + chroma_change, 0.0);
    // Lightness moves with the colour's saturation, so near-greys shift less.
    let new_lightness = max(lab.x + lightness_change * smoothstep(0.0, 0.05, chroma), 0.0);
    let result = oklab_to_linear_srgb(vec3<f32>(new_lightness, new_chroma * cos(new_hue), new_chroma * sin(new_hue)));
    return gamut_floor(result);
}

fn apply_color_grading(color: vec3<f32>, shadows: ColorGradeSettings, midtones: ColorGradeSettings, highlights: ColorGradeSettings, global: ColorGradeSettings, blending: f32, balance: f32) -> vec3<f32> {
    let luma = get_luma(max(vec3(0.0), color));
    let base_shadow_crossover = 0.1;
    let base_highlight_crossover = 0.5;
    let balance_range = 0.5;
    let shadow_crossover = base_shadow_crossover + max(0.0, -balance) * balance_range;
    let highlight_crossover = base_highlight_crossover - max(0.0, balance) * balance_range;
    let feather = 0.2 * blending;
    let final_shadow_crossover = min(shadow_crossover, highlight_crossover - 0.01);
    let shadow_mask = 1.0 - smoothstep(final_shadow_crossover - feather, final_shadow_crossover + feather, luma);
    let highlight_mask = smoothstep(highlight_crossover - feather, highlight_crossover + feather, luma);
    let midtone_mask = max(0.0, 1.0 - shadow_mask - highlight_mask);
    let global_mask = 1.0;
    var graded_color = color;
    let shadow_sat_strength = 0.1;
    let shadow_lum_strength = 0.5;
    let midtone_sat_strength = 0.6;
    let midtone_lum_strength = 0.8;
    let highlight_sat_strength = 0.8;
    let highlight_lum_strength = 1.0;
    let global_sat_strength = 1.0;
    let global_lum_strength = 1.0;
    if (shadows.saturation > 0.001) { let tint_rgb = hsv_to_rgb(vec3<f32>(shadows.hue, 1.0, 1.0)); graded_color += (tint_rgb - 0.5) * shadows.saturation * shadow_mask * shadow_sat_strength; }
    graded_color += shadows.luminance * shadow_mask * shadow_lum_strength;
    if (midtones.saturation > 0.001) { let tint_rgb = hsv_to_rgb(vec3<f32>(midtones.hue, 1.0, 1.0)); graded_color += (tint_rgb - 0.5) * midtones.saturation * midtone_mask * midtone_sat_strength; }
    graded_color += midtones.luminance * midtone_mask * midtone_lum_strength;
    if (highlights.saturation > 0.001) { let tint_rgb = hsv_to_rgb(vec3<f32>(highlights.hue, 1.0, 1.0)); graded_color += (tint_rgb - 0.5) * highlights.saturation * highlight_mask * highlight_sat_strength; }
    graded_color += highlights.luminance * highlight_mask * highlight_lum_strength;
    if (global.saturation > 0.001) { let tint_rgb = hsv_to_rgb(vec3<f32>(global.hue, 1.0, 1.0)); graded_color += (tint_rgb - 0.5) * global.saturation * global_mask * global_sat_strength; }
    graded_color += global.luminance * global_mask * global_lum_strength;
    return graded_color;
}

// Lightroom-style colour grading on display values: Oklab (dL, da, db) by
// display lightness from a 32-entry table the CPU builds per recipe and
// layer (color_grading.rs, fitted to Camera Raw). Layer 0 is the photo,
// layer i the mask layer i - 1.
fn grading_shift(layer: u32, lightness: f32) -> vec3<f32> {
    let x = clamp(lightness, 0.0, 1.0) * 31.0;
    let i = min(u32(x), 30u);
    let f = x - f32(i);
    var a: vec4<f32>;
    var b: vec4<f32>;
    if (layer == 0u) {
        a = adjustments.global.grading_lut[i];
        b = adjustments.global.grading_lut[i + 1u];
    } else {
        a = adjustments.mask_adjustments[layer - 1u].grading_lut[i];
        b = adjustments.mask_adjustments[layer - 1u].grading_lut[i + 1u];
    }
    return mix(a.xyz, b.xyz, f);
}

fn apply_grading(display: vec3<f32>, layer: u32) -> vec3<f32> {
    let lab = linear_srgb_to_oklab(display_to_linear(display));
    let graded = oklab_to_linear_srgb(lab + grading_shift(layer, lab.x));
    return linear_to_display(graded);
}

// Black & white after Lightroom's B&W mix, on display values (Oklab of the
// rendered colour): each channel lightens or darkens the colours of its hue
// range in proportion to chroma / lightness, lowering more strongly than
// raising; the neutral conversion adds a small hue-dependent offset (reds
// and yellows a little darker, blues lighter). Fitted per pixel to Camera
// Raw's colour and B&W renders (notes: reference/black-and-white.md).
const BW_CENTRES = array<f32, 8>(18.6, 55.2, 94.3, 134.9, 181.9, 236.0, 312.1, 348.0);
const BW_REACH = array<f32, 8>(33.9, 41.8, 38.7, 41.4, 42.1, 84.8, 60.6, 31.1);
const BW_RAISE = array<f32, 8>(1.986, 3.3, 2.106, 2.203, 2.27, 4.216, 2.117, 1.248);
const BW_LOWER = array<f32, 8>(3.117, 5.741, 7.863, 4.676, 2.995, 6.053, 2.099, 1.677);
// Neutral offset every 15 degrees of Oklab hue from 0.
const BW_BASE = array<f32, 24>(-0.071, -0.13, -0.194, -0.144, -0.058, -0.006, 0.021, 0.004, 0.061, 0.115, 0.209, 0.238, 0.133, 0.045, 0.031, 0.033, -0.042, 0.068, 0.169, 0.158, 0.153, 0.058, -0.119, -0.129);
const BW_CHROMA_POWER: f32 = 1.077;
const BW_MIN_LIGHTNESS: f32 = 0.075;

fn apply_black_and_white(display: vec3<f32>) -> vec3<f32> {
    let lab = linear_srgb_to_oklab(display_to_linear(display));
    let chroma = length(lab.yz);
    let hue = (degrees(atan2(lab.z, lab.y)) + 360.0) % 360.0;
    let position = hue / 15.0;
    let index = u32(position) % 24u;
    var k = mix(BW_BASE[index], BW_BASE[(index + 1u) % 24u], fract(position));
    for (var i = 0u; i < 8u; i = i + 1u) {
        let m = adjustments.global.bw_mix[i / 4u][i % 4u];
        if (m == 0.0) { continue; }
        let distance_deg = abs(((hue - BW_CENTRES[i]) % 360.0 + 540.0) % 360.0 - 180.0);
        let w = 0.5 + 0.5 * cos(clamp(distance_deg / BW_REACH[i], 0.0, 1.0) * 3.14159265);
        k += w * m * select(BW_LOWER[i], BW_RAISE[i], m > 0.0);
    }
    let lightness = lab.x * exp(k * pow(chroma, BW_CHROMA_POWER) / max(lab.x, BW_MIN_LIGHTNESS));
    let grey = clamp(lightness * lightness * lightness, 0.0, 1.0);
    return linear_to_srgb(vec3<f32>(grey));
}

// Capture One's Levels (Photoshop semantics) on display values: the RGB
// composite first, then each channel.
fn level(v: f32, p: vec4<f32>, white: f32) -> f32 {
    let x = clamp((v - p.x) / max(p.y - p.x, 1.0e-4), 0.0, 1.0);
    return p.w + (white - p.w) * pow(x, p.z);
}

fn apply_levels(c: vec3<f32>) -> vec3<f32> {
    let l = adjustments.global.levels;
    let w = adjustments.global.levels_white;
    let o = vec3<f32>(level(c.r, l[0], w.x), level(c.g, l[0], w.x), level(c.b, l[0], w.x));
    return vec3<f32>(level(o.r, l[1], w.y), level(o.g, l[2], w.z), level(o.b, l[3], w.w));
}

// Lightroom's Defringe, fitted to Camera Raw (notes: reference/defringe.md):
// near luminance edges (within 2 original pixels at about one stop of
// contrast, within 10 at 1.5 stops), colours whose
// Oklab hue lies in the purple or green range lose chroma, at amount 20
// about half of it. Lightness is kept.
fn in_hue_range(hue: f32, lo: f32, hi: f32) -> f32 {
    // Soft shoulders of about 15 degrees each side (Camera Raw's ranges
    // reach that far past the slider values); ranges may wrap past 360.
    let centre = 0.5 * (lo + hi);
    let half = 0.5 * (hi - lo);
    let d = abs(((hue - centre) % 360.0 + 540.0) % 360.0 - 180.0);
    return 1.0 - smoothstep(half - 5.0, half + 15.0, d);
}

// Camera Raw's Defringe removes about as much at amount 10 as at 20 (chroma
// kept at edges, per 30° hue bin, X-T3/X-T5/X100V: 0.17–0.89 at 10 and
// 0.24–0.88 at 20): the share rises quickly to its full strength by about
// amount 7 (notes: reference/defringe.md).
const DEFRINGE_STRENGTH: f32 = 0.5;
const DEFRINGE_RAMP: f32 = 0.35;
// A layer's Defringe (Lightroom's local Defringe, measured in Camera Raw):
// colour of any hue along edges loses chroma, up to this share at +100.
const LOCAL_DEFRINGE_STRENGTH: f32 = 0.6;

fn apply_defringe(color: vec3<f32>, coord: vec2<i32>, local: f32) -> vec3<f32> {
    // A negative local amount holds the photo's Defringe back (−100: none).
    let protect = select(1.0, max(0.0, 1.0 + local), local < 0.0);
    let purple = adjustments.global.defringe[0];
    let green = adjustments.global.defringe[1];
    let lab = linear_srgb_to_oklab(gamut_floor(color));
    let chroma = length(lab.yz);
    if (chroma < 0.01) { return color; }
    let hue = (degrees(atan2(lab.z, lab.y)) + 360.0) % 360.0;
    let selected = protect * max(purple.x * in_hue_range(hue, purple.y, purple.z),
        green.x * in_hue_range(hue, green.y, green.z));
    let local_amount = max(local, 0.0) * LOCAL_DEFRINGE_STRENGTH;
    if (selected <= 0.0 && local_amount <= 0.0) { return color; }
    // Fringes run along edges, axial ones on out-of-focus edges up to
    // about 20 original pixels wide: the contrast is taken on rings at 2
    // and 10 pixels (following the preview scale; detail_scale 0 = full
    // resolution).
    let pixel_scale = select(1.0, adjustments.global.detail_scale, adjustments.global.detail_scale > 0.0);
    let dims = vec2<i32>(textureDimensions(input_texture));
    var stops = 0.0;
    for (var ring = 0; ring < 2; ring = ring + 1) {
        let radius = max(1, i32(round(select(10.0, 2.0, ring == 0) * pixel_scale)));
        var lo = 1.0e9;
        var hi = 0.0;
        for (var dy = -radius; dy <= radius; dy = dy + radius) {
            for (var dx = -radius; dx <= radius; dx = dx + radius) {
                let p = clamp(coord + vec2<i32>(dx, dy), vec2<i32>(0), dims - 1);
                let y = get_luma(max(textureLoad(input_texture, vec2<u32>(p), 0).rgb, vec3<f32>(0.0)));
                lo = min(lo, y);
                hi = max(hi, y);
            }
        }
        // The wide ring needs a stronger edge.
        stops = max(stops, log2((hi + 0.002) / (lo + 0.002)) - f32(ring) * 0.5);
    }
    let edge = smoothstep(0.75, 1.75, stops);
    // The local Defringe takes only fringes: colour that neither side of
    // the edge has (sampled across it, 10 and 18 original pixels away). A
    // coloured object keeps its edge, as in Camera Raw.
    var fringe = 0.0;
    if (local_amount > 0.0 && edge > 0.0) {
        let step = max(1, i32(round(2.0 * pixel_scale)));
        let gx = get_luma(max(textureLoad(input_texture, vec2<u32>(clamp(coord + vec2<i32>(step, 0), vec2<i32>(0), dims - 1)), 0).rgb, vec3<f32>(0.0)))
            - get_luma(max(textureLoad(input_texture, vec2<u32>(clamp(coord - vec2<i32>(step, 0), vec2<i32>(0), dims - 1)), 0).rgb, vec3<f32>(0.0)));
        let gy = get_luma(max(textureLoad(input_texture, vec2<u32>(clamp(coord + vec2<i32>(0, step), vec2<i32>(0), dims - 1)), 0).rgb, vec3<f32>(0.0)))
            - get_luma(max(textureLoad(input_texture, vec2<u32>(clamp(coord - vec2<i32>(0, step), vec2<i32>(0), dims - 1)), 0).rgb, vec3<f32>(0.0)));
        let g = length(vec2<f32>(gx, gy));
        let dir = select(vec2<f32>(1.0, 0.0), vec2<f32>(gx, gy) / g, g > 1.0e-6);
        // A blend of the two sides (green leaf on blue sky gives cyan) is
        // not a fringe: the colour is explained when it lies near the mix of
        // the sides' colours (their ab segment); a purple seam between grey
        // sides is far from it.
        var nearest = 1.0e9;
        for (var k = 0; k < 2; k = k + 1) {
            let distance = select(10.0, 18.0, k == 1) * pixel_scale;
            let p1 = clamp(coord + vec2<i32>(round(dir * distance)), vec2<i32>(0), dims - 1);
            let p2 = clamp(coord - vec2<i32>(round(dir * distance)), vec2<i32>(0), dims - 1);
            let a1 = linear_srgb_to_oklab(max(textureLoad(input_texture, vec2<u32>(p1), 0).rgb, vec3<f32>(0.0))).yz;
            let a2 = linear_srgb_to_oklab(max(textureLoad(input_texture, vec2<u32>(p2), 0).rgb, vec3<f32>(0.0))).yz;
            let seg = a2 - a1;
            let t = clamp(dot(lab.yz - a1, seg) / max(dot(seg, seg), 1.0e-8), 0.0, 1.0);
            nearest = min(nearest, length(lab.yz - (a1 + seg * t)));
        }
        fringe = smoothstep(0.3, 0.7, nearest / chroma);
    }
    let keep = (1.0 - clamp(smoothstep(0.0, DEFRINGE_RAMP, selected) * edge * DEFRINGE_STRENGTH, 0.0, 1.0))
        * (1.0 - clamp(local_amount * edge * fringe, 0.0, 1.0));
    return gamut_floor(oklab_to_linear_srgb(vec3<f32>(lab.x, lab.yz * keep)));
}

// Capture One's Advanced Color Editor / Lightroom's Point Color on display
// values: every range selects colours around a sampled Oklab colour (hue
// window, chroma and lightness band, soft edges), evens them out towards
// the sample (uniformity) and shifts hue, chroma and lightness. Weights come
// from the colour before any range, so ranges do not cascade.
fn band_weight(x: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    let feather = 0.5 * soft * max(hi - lo, 0.0) + 0.004;
    return smoothstep(lo - feather, lo + feather, x) * (1.0 - smoothstep(hi - feather, hi + feather, x));
}

struct ColorRange {
    a: vec4<f32>,
    b: vec4<f32>,
    s: vec4<f32>,
    u: vec4<f32>,
}

// Range r of the photo (layer 0) or of mask layer `layer - 1`.
// Indexed in place: binding the uniform array to a local first copied all
// of it (512 bytes) for every range and pixel, tripling the frame time.
fn color_range(layer: u32, r: u32) -> ColorRange {
    let i = r * 4u;
    if (layer == 0u) {
        return ColorRange(
            adjustments.global.color_ranges[i],
            adjustments.global.color_ranges[i + 1u],
            adjustments.global.color_ranges[i + 2u],
            adjustments.global.color_ranges[i + 3u],
        );
    }
    let l = layer - 1u;
    return ColorRange(
        adjustments.mask_adjustments[l].color_ranges[i],
        adjustments.mask_adjustments[l].color_ranges[i + 1u],
        adjustments.mask_adjustments[l].color_ranges[i + 2u],
        adjustments.mask_adjustments[l].color_ranges[i + 3u],
    );
}

fn color_range_weight(range: ColorRange, lab: vec3<f32>, chroma: f32, hue: f32) -> f32 {
    let a = range.a;
    let b = range.b;
    let soft = b.w;
    let d = abs(((hue - a.x) % 360.0 + 540.0) % 360.0 - 180.0);
    let wh = 1.0 - smoothstep(b.x * (1.0 - soft), b.x * (1.0 + soft), d);
    let wc = band_weight(chroma, b.y, b.z, soft);
    var wl = 1.0;
    if (a.w < 1.0) {
        wl = band_weight(lab.x, a.z - a.w, a.z + a.w, soft);
    }
    // Hue is undefined for greys.
    return wh * wc * wl * smoothstep(0.004, 0.015, chroma);
}

fn apply_color_ranges(display: vec3<f32>, layer: u32, count: u32) -> vec3<f32> {
    let lab = linear_srgb_to_oklab(display_to_linear(display));
    let chroma = length(lab.yz);
    let hue = (degrees(atan2(lab.z, lab.y)) + 360.0) % 360.0;
    var dh = 0.0;
    var chroma_scale = 1.0;
    var dc = 0.0;
    var dl = 0.0;
    for (var r = 0u; r < count; r = r + 1u) {
        let range = color_range(layer, r);
        let w = color_range_weight(range, lab, chroma, hue);
        if (w <= 0.0) { continue; }
        let to_sample = ((range.a.x - hue) % 360.0 + 540.0) % 360.0 - 180.0;
        dh += w * (range.u.x * to_sample + range.s.x);
        dc += w * range.u.y * (range.a.y - chroma);
        // Lightness scales (s.z at L 0.5): dark tones of the range are
        // lifted less and keep their depth, as in Lightroom's Point Color.
        dl += w * (range.u.z * (range.a.z - lab.x) + range.s.z * 2.0 * lab.x);
        chroma_scale *= max(1.0 + w * range.s.y, 0.0);
    }
    let new_chroma = max(chroma + dc, 0.0) * chroma_scale;
    let new_hue = radians(hue + dh);
    let out = vec3<f32>(clamp(lab.x + dl, 0.0, 1.0), new_chroma * cos(new_hue), new_chroma * sin(new_hue));
    return linear_to_display(oklab_to_linear_srgb(out));
}

fn color_range_view_weight(display: vec3<f32>, r: u32) -> f32 {
    let lab = linear_srgb_to_oklab(srgb_to_linear(clamp(display, vec3<f32>(0.0), vec3<f32>(1.0))));
    let chroma = length(lab.yz);
    let hue = (degrees(atan2(lab.z, lab.y)) + 360.0) % 360.0;
    return color_range_weight(color_range(0u, r), lab, chroma, hue);
}

// Lightroom's region tone curve, a 256-entry table per channel
// (region_curve.rs), on display values before the point curve.
fn region_curve_value(v: f32) -> f32 {
    let x = clamp(v, 0.0, 1.0) * 255.0;
    let i = min(u32(x), 254u);
    let f = x - f32(i);
    let a = adjustments.global.region_curve[i / 4u][i % 4u];
    let b = adjustments.global.region_curve[(i + 1u) / 4u][(i + 1u) % 4u];
    return mix(a, b, f);
}

fn apply_region_curve(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(region_curve_value(c.r), region_curve_value(c.g), region_curve_value(c.b));
}

// Lightroom's Calibration as Camera Raw renders it (calibration.rs): a
// white-keeping matrix on linear ProPhoto that turns and scales the
// primaries, then the shadows tint (less green in the shadows for magenta,
// less red and blue for green).
fn apply_camera_raw_calibration(color: vec3<f32>) -> vec3<f32> {
    let before = srgb_to_prophoto(color);
    var pro = adjustments.global.calibration_matrix * before;
    let keep = adjustments.global.calibration_luma.x;
    if (keep != 0.0) {
        let y_weights = vec3<f32>(0.2880402, 0.7118741, 0.0000857);
        let y0 = dot(before, y_weights);
        let y1 = dot(pro, y_weights);
        if (y1 > 1.0e-6) {
            pro *= max(y1 + keep * (y0 - y1), 0.0) / y1;
        }
    }
    let tint = adjustments.global.calibration_tint;
    if (tint.x != 0.0) {
        let y = max(dot(pro, vec3<f32>(0.2880402, 0.7118741, 0.0000857)), 0.0);
        let shadow = exp(-pow(y / tint.z, tint.w));
        let cut = tint.y * abs(tint.x) * shadow;
        if (tint.x > 0.0) {
            pro.y *= 1.0 - cut;
        } else {
            pro.x *= 1.0 - cut;
            pro.z *= 1.0 - cut;
        }
    }
    return prophoto_to_srgb(pro);
}

// Lightroom's post-crop vignette (Highlight Priority) on display values,
// fitted to Camera Raw (notes: reference/vignette.md): a smooth ramp over the
// frame-normalised elliptical distance d (roundness +1 turns it into a
// circle, negative roundness pushes it into the corners), darkening by
// multiplication or lightening towards white.
const VIGNETTE_GAIN: f32 = 1.32;
const VIGNETTE_AMOUNT_POWER: f32 = 0.731;
const VIGNETTE_LIGHTEN: f32 = 0.9;
const VIGNETTE_CENTRE: f32 = 0.958;
const VIGNETTE_CENTRE_PER_MIDPOINT: f32 = 0.677;
const VIGNETTE_WIDTH: f32 = 0.02;
const VIGNETTE_WIDTH_PER_FEATHER: f32 = 1.313;
const VIGNETTE_RAMP_POWER: f32 = 1.02;
const VIGNETTE_CIRCLE_SCALE: f32 = 0.864;
const VIGNETTE_CORNER_SCALE: f32 = 0.315;
const VIGNETTE_PROTECT_PIVOT: f32 = 0.6;
const VIGNETTE_PROTECT_POWER: f32 = 0.6;

fn apply_vignette(display: vec3<f32>, coord: vec2<f32>, dims: vec2<f32>) -> vec3<f32> {
    let a = adjustments.global.vignette_amount;
    let midpoint = adjustments.global.vignette_midpoint;
    let feather = adjustments.global.vignette_feather;
    let roundness = adjustments.global.vignette_roundness;
    let uv = (coord / dims - 0.5) * 2.0;
    let ellipse = length(uv);
    var d = ellipse;
    if (roundness >= 0.0) {
        let circle = length(vec2<f32>(uv.x * dims.x / dims.y, uv.y)) * VIGNETTE_CIRCLE_SCALE;
        d = mix(ellipse, circle, roundness);
    } else {
        d = ellipse * (1.0 + roundness * VIGNETTE_CORNER_SCALE);
    }
    let centre = VIGNETTE_CENTRE + VIGNETTE_CENTRE_PER_MIDPOINT * (midpoint - 0.5);
    let width = VIGNETTE_WIDTH + VIGNETTE_WIDTH_PER_FEATHER * feather;
    let t = clamp((d - centre) / width + 0.5, 0.0, 1.0);
    let ramp = pow(t * t * (3.0 - 2.0 * t), VIGNETTE_RAMP_POWER);
    let strength = VIGNETTE_GAIN * pow(abs(a), VIGNETTE_AMOUNT_POWER) * ramp;
    let c = display_unit(display);
    let style = adjustments.global.vignette_style.x;
    let luma = get_luma(c);
    if (a < 0.0) {
        // Highlight Priority: bright areas darken less (Camera Raw at the
        // frame edge, Amount −50: ×0.61 for dark tones, ×0.88 near white);
        // normalised to mid-tones, where the strength was fitted. Colour
        // Priority protects less; Paint Overlay darkens all tones alike and
        // a little less. Highlights spares bright areas further.
        var protect = 0.86;
        if (style < 2u) {
            let power = select(VIGNETTE_PROTECT_POWER, 0.5 * VIGNETTE_PROTECT_POWER, style == 1u);
            protect = pow(max(1.0 - luma, 0.0) / VIGNETTE_PROTECT_PIVOT, power);
            let highlights = f32(adjustments.global.vignette_style.y) / 1000.0;
            protect *= 1.0 - highlights * 0.65 * smoothstep(0.55, 1.0, luma);
        }
        return c * (1.0 - min(strength * protect, 1.0));
    }
    let lighten = VIGNETTE_LIGHTEN * select(select(1.0, 0.55, style == 1u), 0.33, style == 2u);
    return c + (vec3<f32>(1.0) - c) * min(strength * lighten, 1.0);
}

const GRAIN_GAIN: f32 = 1.93;
const GRAIN_FINE_BOOST: f32 = 2.0;

fn apply_local_contrast(
    processed_color_linear: vec3<f32>,
    blurred_color_input_space: vec3<f32>,
    amount: f32,
    is_raw: u32,
    mode: u32,
    threshold: f32
) -> vec3<f32> {
    if (amount == 0.0) {
        return processed_color_linear;
    }

    var blurred_color_linear: vec3<f32>;
    if (is_raw == 1u) {
        blurred_color_linear = blurred_color_input_space;
    } else {
        blurred_color_linear = srgb_to_linear(blurred_color_input_space);
    }

    if (amount < 0.0) {
        var blur_amount = -amount;
        if (mode == 0u) {
            blur_amount = blur_amount * 0.5;
        }
        return mix(processed_color_linear, blurred_color_linear, blur_amount);
    }

    let center_luma = get_luma(processed_color_linear);

    let shadow_threshold = select(0.03, 0.1, is_raw == 1u);
    let shadow_protection = smoothstep(0.0, shadow_threshold, center_luma);
    let highlight_protection = 1.0 - smoothstep(0.9, 1.0, center_luma);
    let midtone_mask = shadow_protection * highlight_protection;

    if (midtone_mask < 0.001) {
        return processed_color_linear;
    }

    let blurred_luma = get_luma(blurred_color_linear);
    let safe_center_luma = max(center_luma, 0.0001);
    let safe_blurred_luma = max(blurred_luma, 0.0001);

    let log_ratio = log2(safe_center_luma / safe_blurred_luma);
    var effective_amount = amount;

    if (mode == 0u) {
        let edge_magnitude = abs(log_ratio);
        let normalized_edge = clamp(edge_magnitude / 3.0, 0.0, 1.0);
        let edge_dampener = 1.0 - pow(normalized_edge, 0.5);
        let edge_mask = smoothstep(threshold * 0.5, threshold * 1.5, edge_magnitude);
        effective_amount = amount * edge_dampener * edge_mask * 0.8;
    } else {
        effective_amount = amount;
    }

    let contrast_factor = exp2(log_ratio * effective_amount);
    let final_color = processed_color_linear * contrast_factor;

    return mix(processed_color_linear, final_color, midtone_mask);
}

// Clarity and Structure as in Capture One and Lightroom: the luminance ratio
// to a blurred copy (the band above that blur's scale) is raised or lowered in
// log space, the same model in both directions. Protection is judged after
// exposure, so it does not depend on how the RAW was exposed: only the
// noise floor is spared. Large edges are soft-limited to keep halos down.
// Gains calibrated against Capture One 16 and Lightroom Classic exports at ±50
// (notes: reference/tone-controls.md): both lower local contrast less than they
// raise it; Structure sits between Lightroom's Texture and Capture One's
// Structure.
const CLARITY_GAIN_RAISE: f32 = 1.0;
const CLARITY_GAIN_LOWER: f32 = 0.25;
const STRUCTURE_GAIN_RAISE: f32 = 1.5;
const STRUCTURE_GAIN_LOWER: f32 = 0.8;

fn band_amount(value: f32, raise: f32, lower: f32) -> f32 {
    return value * select(lower, raise, value > 0.0);
}

fn apply_band_contrast(
    color_linear: vec3<f32>,
    blurred_input_space: vec3<f32>,
    amount: f32,
    is_raw: u32,
    exposure_gain: f32
) -> vec3<f32> {
    if (amount == 0.0) {
        return color_linear;
    }
    var blurred_linear = blurred_input_space;
    if (is_raw != 1u) {
        blurred_linear = srgb_to_linear(blurred_input_space);
    }
    let luma = max(get_luma(color_linear), 0.0001);
    let blurred_luma = max(get_luma(blurred_linear), 0.0001);
    let band = 2.0 * tanh(log2(luma / blurred_luma) * 0.5);
    let noise_floor = smoothstep(0.0, 0.004, luma * exposure_gain);
    return mix(color_linear, color_linear * exp2(band * amount), noise_floor);
}

fn sharpen_perc(c: vec3<f32>, is_raw: u32) -> f32 {
    let y = max(get_luma(c), 0.0);
    if (is_raw == 1u) { return sqrt(y); }
    return y;
}

// Set by the luminance noise reduction for the sharpening that follows it
// at the same pixel: the input before noise reduction and the share of
// its finest band that stood out of the noise. Sharpening takes its
// high-pass from that input, weighted by the share; with its blur of the
// (noisy) input subtracted from the denoised pixel it put the removed
// noise back, more the stronger the noise reduction.
var<private> nr_active: bool = false;
var<private> nr_input: vec3<f32> = vec3<f32>(0.0);
var<private> nr_fine_keep: f32 = 1.0;

/// The sharpening high-pass of `l` against `blur`, after noise reduction
/// from the input it started from.
fn sharpen_detail(l: f32, blur: f32, is_raw: u32) -> f32 {
    if (!nr_active) {
        return l - blur;
    }
    var l_in: f32;
    if (is_raw == 1u) {
        l_in = sqrt(max(get_luma(nr_input), 0.0));
    } else {
        l_in = get_luma(linear_to_srgb_extended(nr_input));
    }
    return (l_in - blur) * nr_fine_keep;
}

const SHARPEN_GAIN: f32 = 1.2;
const FALLOFF_GAIN: f32 = 0.22;
const SHARPEN_RADIUS: f32 = 0.6;
const SHARPEN_PROGRESSION: f32 = 0.8;
const SHARPEN_HALO_LIMIT: f32 = 0.08;
const SHARPEN_THRESHOLD_SCALE: f32 = 0.01;
// Detail -> exp2 exponent per 25 units for the halo limit and the gain,
// and Masking -> neighbourhood spread threshold per unit (fitted to Camera
// Raw, notes: reference/detail.md).
const SHARPEN_DETAIL_HALO_LOW: f32 = 0.5;
const SHARPEN_DETAIL_HALO_HIGH: f32 = -0.3;
const SHARPEN_DETAIL_GAIN_LOW: f32 = 1.18;
const SHARPEN_DETAIL_GAIN_HIGH: f32 = 0.4;
const SHARPEN_MASK_SCALE: f32 = 0.0004;
// Detail above 25 tilts the response towards the finest band, as Camera
// Raw's does (Detail 100: fine ×1.81, coarse ×1.18 of its default gain;
// more gain alone lifted all bands ×1.52): with d = (Detail − 25) / 25 the
// band between the σ and 2σ blurs is taken out of the signal at tilt and
// the gain rises by (1 + d)^fine, where tilt = scale·d^exponent, growing
// with the amount by (amount / 0.8)^amount_exponent (Camera Raw's coarse
// band barely grows from Amount 40 to 80 at Detail 100). A narrower kernel cannot do it: below
// about 0.5 px a sampled Gaussian is nearly the pixel itself.
const SHARPEN_DETAIL_TILT: f32 = 0.096;
const SHARPEN_DETAIL_TILT_EXPONENT: f32 = 0.53;
const SHARPEN_DETAIL_FINE: f32 = 0.15;
const SHARPEN_DETAIL_TILT_AMOUNT: f32 = 0.87;
// Below 25 the band is added instead (Camera Raw's Detail 0 keeps relatively
// more of the coarse bands: 0.43 / 0.48 / 0.57), at low·(−d).
const SHARPEN_DETAIL_TILT_LOW: f32 = 0.21;
// At high Detail the halo limit also tightens with the amount by
// (amount / 0.8)^(−halo_amount·d/3) (Camera Raw's edge overshoot grows only
// ×1.15 from Amount 40 to 80 at Detail 100).
const SHARPEN_DETAIL_HALO_AMOUNT: f32 = 0.6;
// Above Detail 25 the halo limiter's knee hardens towards this exponent
// (d / (1 + (|d| / limit)^n)^(1/n), n from 1 at Detail 25): fine texture
// passes almost unchanged while strong edges saturate, so the limit can
// keep Camera Raw's small edge overshoot without costing texture.
const SHARPEN_DETAIL_KNEE: f32 = 3.0;
// Above Detail 25 the gain also drops where the neighbourhood's spread
// (perceptual levels) exceeds `edge_spread` (0.03: medium edges carry
// halos too): by 1 / (1 + edge·d/3·(amount /
// 0.8)^a·(spread / edge_spread − 1)). Strong edges, where halos form, get
// less; fine texture, whose spread is small, keeps its gain. The amount
// enters as (amount / 0.8)^edge_amount.
const SHARPEN_EDGE_SPREAD: f32 = 0.03;
const SHARPEN_EDGE_DAMP: f32 = 1.0;
const SHARPEN_EDGE_AMOUNT: f32 = 2.0;

fn apply_sharpen(
    color: vec3<f32>,
    b1_in: vec3<f32>,
    b2_in: vec3<f32>,
    coords_i: vec2<i32>,
    amount: f32,
    threshold: f32,
    is_raw: u32
) -> vec3<f32> {
    if (abs(amount) < 0.0005) {
        return color;
    }

    if (amount < 0.0) {
        var b1_lin = b1_in;
        if (is_raw == 0u) { b1_lin = srgb_to_linear(b1_in); }
        return mix(color, b1_lin, clamp(-amount * 0.5, 0.0, 1.0));
    }

    // Lightroom's Radius / Detail / Masking away from their defaults
    // (1.0 / 25 / 0) take the extended path; the default path below is the
    // calibrated one, unchanged, so default exports stay bit-identical.
    if (adjustments.global.sharpen_radius_offset != 0.0
        || adjustments.global.sharpen_detail_offset != 0.0
        || adjustments.global.sharpen_masking > 0.0) {
        return apply_sharpen_extended(color, b1_in, b2_in, coords_i, amount, threshold, is_raw);
    }
    var color_enc = color;
    if (is_raw == 0u) {
        color_enc = linear_to_srgb_extended(color);
    }
    let l = select(get_luma(color_enc), sqrt(max(get_luma(color), 0.0)), is_raw == 1u);

    // Lightroom-like capture sharpening (notes: reference/detail.md): an
    // unsharp mask of perceptual luminance with a 0.6 px Gaussian, which
    // matches Lightroom's radius 1.0 and Detail 25 on its band gains; large
    // differences are limited softly to Lightroom's halo strength, and
    // Threshold keeps noise from being sharpened. The radius is in original
    // pixels, so scaled-down previews show it smaller, as in Lightroom.
    let detail_scale = select(1.0, adjustments.global.detail_scale, adjustments.global.detail_scale > 0.0);
    let sigma = max(SHARPEN_RADIUS * detail_scale, 0.25);
    let dims_s = vec2<i32>(textureDimensions(input_texture)) - vec2<i32>(1);
    var blur_sum = 0.0;
    var blur_w = 0.0;
    for (var sy = -2; sy <= 2; sy = sy + 1) {
        for (var sx = -2; sx <= 2; sx = sx + 1) {
            let w = exp(-f32(sx * sx + sy * sy) / (2.0 * sigma * sigma));
            let c = clamp(coords_i + vec2<i32>(sx, sy), vec2<i32>(0), dims_s);
            blur_sum += sharpen_perc(textureLoad(input_texture, vec2<u32>(c), 0).rgb, is_raw) * w;
            blur_w += w;
        }
    }
    let d = sharpen_detail(l, blur_sum / blur_w, is_raw);
    let t_gate = max(threshold * SHARPEN_THRESHOLD_SCALE, 1e-4);
    let gate = smoothstep(t_gate * 0.2, t_gate, abs(d));
    // Halos grow linearly with the amount in Lightroom while fine
    // detail grows faster, so the limit shrinks with the progression.
    let halo_limit = SHARPEN_HALO_LIMIT / (1.0 + SHARPEN_PROGRESSION * amount);
    let limited = d / (1.0 + abs(d) / halo_limit);
    // Lightroom's response grows faster than the amount (80 gives three
    // times the fine-detail gain of 40).
    let strength = amount * (1.0 + SHARPEN_PROGRESSION * amount) * SHARPEN_GAIN;
    let l_sharp = max(l + limited * gate * strength, l * 0.4);
    let shadow_floor_lr = select(0.03, 0.10, is_raw == 1u);
    let protect = smoothstep(0.0, shadow_floor_lr, l) * (1.0 - smoothstep(0.92, 1.0, l));
    let l_out = max(mix(l, l_sharp, protect), 0.0);
    let ratio_lr = l_out / max(l, 1e-4);
    if (is_raw == 1u) {
        return color * (ratio_lr * ratio_lr);
    }
    return srgb_to_linear(max(color_enc * ratio_lr, vec3<f32>(0.0)));
}

fn apply_sharpen_extended(
    color: vec3<f32>,
    b1_in: vec3<f32>,
    b2_in: vec3<f32>,
    coords_i: vec2<i32>,
    amount: f32,
    threshold: f32,
    is_raw: u32
) -> vec3<f32> {
    if (abs(amount) < 0.0005) {
        return color;
    }

    if (amount < 0.0) {
        var b1_lin = b1_in;
        if (is_raw == 0u) { b1_lin = srgb_to_linear(b1_in); }
        return mix(color, b1_lin, clamp(-amount * 0.5, 0.0, 1.0));
    }

    var color_enc = color;
    if (is_raw == 0u) {
        color_enc = linear_to_srgb_extended(color);
    }
    let l = select(get_luma(color_enc), sqrt(max(get_luma(color), 0.0)), is_raw == 1u);

    // Lightroom-like capture sharpening (notes: reference/detail.md): an
    // unsharp mask of perceptual luminance with a 0.6 px Gaussian, which
    // matches Lightroom's radius 1.0 and Detail 25 on its band gains; large
    // differences are limited softly to Lightroom's halo strength, and
    // Threshold keeps noise from being sharpened. The radius is in original
    // pixels, so scaled-down previews show it smaller, as in Lightroom.
    // Lightroom's Radius (0.5-3), Detail (0-100) and Masking (0-100), kept
    // as offsets from its defaults 1.0 / 25 / 0 (fitted to Camera Raw,
    // notes: reference/detail.md).
    let radius = 1.0 + adjustments.global.sharpen_radius_offset;
    let detail = (adjustments.global.sharpen_detail_offset) / 25.0;
    let masking = adjustments.global.sharpen_masking;
    let detail_scale = select(1.0, adjustments.global.detail_scale, adjustments.global.detail_scale > 0.0);
    // Below 1 the kernel narrows with the square root (a linear one left
    // almost nothing at 0.5); the gain is normalised so that, as in Camera
    // Raw, the radius moves the response between bands rather than scaling it.
    let radius_shape = select(sqrt(radius), radius, radius >= 1.0);
    let radius_gain = select(pow(radius, -0.75), pow(radius, -0.85), radius >= 1.0);
    let sigma = max(SHARPEN_RADIUS * detail_scale * radius_shape, 0.25);
    let tilt = select(
        SHARPEN_DETAIL_TILT_LOW * detail,
        SHARPEN_DETAIL_TILT * pow(max(detail, 1e-6), SHARPEN_DETAIL_TILT_EXPONENT)
            * pow(amount / 0.8, SHARPEN_DETAIL_TILT_AMOUNT),
        detail > 0.0,
    );
    let reach = select(
        select(2, 3, tilt != 0.0),
        min(i32(ceil(select(2.5, 5.0, tilt != 0.0) * sigma)), 6),
        radius > 1.0,
    );
    var wide_sum = 0.0;
    var wide_w = 0.0;

    let dims_s = vec2<i32>(textureDimensions(input_texture)) - vec2<i32>(1);
    var blur_sum = 0.0;
    var blur_sq = 0.0;
    var blur_w = 0.0;
    for (var sy = -reach; sy <= reach; sy = sy + 1) {
        for (var sx = -reach; sx <= reach; sx = sx + 1) {
            let w = exp(-f32(sx * sx + sy * sy) / (2.0 * sigma * sigma));
            let c = clamp(coords_i + vec2<i32>(sx, sy), vec2<i32>(0), dims_s);
            let p = sharpen_perc(textureLoad(input_texture, vec2<u32>(c), 0).rgb, is_raw);
            blur_sum += p * w;
            blur_sq += p * p * w;
            blur_w += w;
            if (tilt != 0.0) {
                let w2 = exp(-f32(sx * sx + sy * sy) / (8.0 * sigma * sigma));
                wide_sum += p * w2;
                wide_w += w2;
            }
        }
    }
    var d = sharpen_detail(l, blur_sum / blur_w, is_raw);
    if (tilt != 0.0) {
        // The band of the input around the pixel, kept where noise
        // reduction kept the detail.
        let band = (blur_sum / blur_w - wide_sum / wide_w) * select(1.0, nr_fine_keep, nr_active);
        d -= tilt * band;
    }
    let t_gate = max(threshold * SHARPEN_THRESHOLD_SCALE, 1e-4);
    var gate = smoothstep(t_gate * 0.2, t_gate, abs(d));
    // Masking: sharpen only where the neighbourhood varies more than a
    // threshold that rises with the slider (edges, not flat areas or noise).
    if (masking > 0.0) {
        let mean = blur_sum / blur_w;
        let spread = sqrt(max(blur_sq / blur_w - mean * mean, 0.0));
        let t_mask = masking * sqrt(masking / 50.0) * SHARPEN_MASK_SCALE;
        gate *= smoothstep(0.5 * t_mask, 1.5 * t_mask, spread);
    }
    if (detail > 0.0) {
        let mean_e = blur_sum / blur_w;
        let spread_e = sqrt(max(blur_sq / blur_w - mean_e * mean_e, 0.0));
        let excess = max(spread_e / SHARPEN_EDGE_SPREAD - 1.0, 0.0);
        gate /= 1.0 + SHARPEN_EDGE_DAMP * detail / 3.0 * pow(amount / 0.8, SHARPEN_EDGE_AMOUNT) * excess;
    }
    // Halos grow linearly with the amount in Lightroom while fine
    // detail grows faster, so the limit shrinks with the progression;
    // Detail lifts the limit and the gain (Lightroom's halo suppression).
    let halo_limit = SHARPEN_HALO_LIMIT / (1.0 + SHARPEN_PROGRESSION * amount)
        * exp2(select(SHARPEN_DETAIL_HALO_LOW, SHARPEN_DETAIL_HALO_HIGH, detail > 0.0) * detail)
        * pow(amount / 0.8, -SHARPEN_DETAIL_HALO_AMOUNT * max(detail, 0.0) / 3.0);
    let knee = 1.0 + (SHARPEN_DETAIL_KNEE - 1.0) * clamp(detail / 3.0, 0.0, 1.0);
    let limited = d / pow(1.0 + pow(abs(d) / halo_limit, knee), 1.0 / knee);
    // Lightroom's response grows faster than the amount (80 gives three
    // times the fine-detail gain of 40).
    let detail_gain = select(exp2(SHARPEN_DETAIL_GAIN_LOW * detail), pow(1.0 + detail, SHARPEN_DETAIL_GAIN_HIGH), detail > 0.0);
    let strength = amount * (1.0 + SHARPEN_PROGRESSION * amount) * SHARPEN_GAIN
        * detail_gain * radius_gain * pow(1.0 + max(detail, 0.0), SHARPEN_DETAIL_FINE);
    let l_sharp = max(l + limited * gate * strength, l * 0.4);
    let shadow_floor_lr = select(0.03, 0.10, is_raw == 1u);
    let protect = smoothstep(0.0, shadow_floor_lr, l) * (1.0 - smoothstep(0.92, 1.0, l));
    let l_out = max(mix(l, l_sharp, protect), 0.0);
    let ratio_lr = l_out / max(l, 1e-4);
    if (is_raw == 1u) {
        return color * (ratio_lr * ratio_lr);
    }
    return srgb_to_linear(max(color_enc * ratio_lr, vec3<f32>(0.0)));
}

fn apply_centre_local_contrast(
    color_in: vec3<f32>,
    centre_amount: f32,
    coords_i: vec2<i32>,
    blurred_color_srgb: vec3<f32>,
    is_raw: u32
) -> vec3<f32> {
    if (centre_amount == 0.0) {
        return color_in;
    }
    let full_dims_f = vec2<f32>(textureDimensions(input_texture));
    let coord_f = vec2<f32>(coords_i);
    let midpoint = 0.4;
    let feather = 0.375;
    let aspect = full_dims_f.y / full_dims_f.x;
    let uv_centered = (coord_f / full_dims_f - 0.5) * 2.0;
    let d = length(uv_centered * vec2<f32>(1.0, aspect)) * 0.5;
    let vignette_mask = smoothstep(midpoint - feather, midpoint + feather, d);
    let centre_mask = 1.0 - vignette_mask;

    const CLARITY_SCALE: f32 = 0.9;
    var processed_color = color_in;
    let clarity_strength = centre_amount * (2.0 * centre_mask - 1.0) * CLARITY_SCALE;

    if (abs(clarity_strength) > 0.001) {
        processed_color = apply_local_contrast(processed_color, blurred_color_srgb, clarity_strength, is_raw, 1u, 0.0);
    }

    return processed_color;
}

fn apply_centre_tonal_and_color(
    color_in: vec3<f32>,
    centre_amount: f32,
    coords_i: vec2<i32>
) -> vec3<f32> {
    if (centre_amount == 0.0) {
        return color_in;
    }
    let full_dims_f = vec2<f32>(textureDimensions(input_texture));
    let coord_f = vec2<f32>(coords_i);
    let midpoint = 0.4;
    let feather = 0.375;
    let aspect = full_dims_f.y / full_dims_f.x;
    let uv_centered = (coord_f / full_dims_f - 0.5) * 2.0;
    let d = length(uv_centered * vec2<f32>(1.0, aspect)) * 0.5;
    let vignette_mask = smoothstep(midpoint - feather, midpoint + feather, d);
    let centre_mask = 1.0 - vignette_mask;

    const EXPOSURE_SCALE: f32 = 0.5;
    const VIBRANCE_SCALE: f32 = 0.4;
    const SATURATION_CENTER_SCALE: f32 = 0.3;
    const SATURATION_EDGE_SCALE: f32 = 0.8;

    var processed_color = color_in;

    let exposure_boost = centre_mask * centre_amount * EXPOSURE_SCALE;
    processed_color = apply_linear_exposure(processed_color, exposure_boost);

    let vibrance_center_boost = centre_mask * centre_amount * VIBRANCE_SCALE;
    let saturation_center_boost = centre_mask * centre_amount * SATURATION_CENTER_SCALE;
    let saturation_edge_effect = -(1.0 - centre_mask) * centre_amount * SATURATION_EDGE_SCALE;
    let total_saturation_effect = saturation_center_boost + saturation_edge_effect;
    processed_color = apply_creative_color(processed_color, total_saturation_effect, vibrance_center_boost);

    return processed_color;
}

// Dehaze, modelled on Capture One's and Lightroom's responses
// (notes: reference/tone-controls.md); the renderer receives slider / 750. Both
// darken shadows and midtones by an almost even amount in EV that fades out
// towards white, raise local contrast and chroma; lowering lifts the
// midtones while sparing the blacks. A linear veil subtraction would crush
// the deepest shadows instead. Regional haze (the dark channel of the large
// blur) strengthens the effect where there is haze.
const DEHAZE_SCALE: f32 = 7.5;
const DEHAZE_LOCAL_HAZE: f32 = 0.5;
const DEHAZE_DARKEN_EV: f32 = 1.25;
const DEHAZE_LIFT: f32 = 0.46;
const DEHAZE_LOCAL_CONTRAST: f32 = 0.75;
const DEHAZE_CHROMA: f32 = 0.75;

fn apply_dehaze(color: vec3<f32>, blurred_color_input_space: vec3<f32>, is_raw: u32, amount: f32) -> vec3<f32> {
    if (amount == 0.0) { return color; }

    var blurred_linear: vec3<f32>;
    if (is_raw == 1u) {
        blurred_linear = blurred_color_input_space;
    } else {
        blurred_linear = srgb_to_linear(blurred_color_input_space);
    }
    // The Camera Raw look's curve steepens the lower tones, so Dehaze is
    // scaled to Lightroom's response there.
    let look_gain = select(1.0, select(CR_DEHAZE_LOWER, CR_DEHAZE_RAISE, amount > 0.0), base_look_is_camera_raw());
    let a = clamp(amount * DEHAZE_SCALE * look_gain, -1.0, 1.0);
    let safe = gamut_floor(color);
    let luma = max(get_luma(safe), 0.0001);
    let blurred_luma = max(get_luma(max(blurred_linear, vec3<f32>(0.0))), 0.0001);
    let regional_dark = max(min(blurred_linear.r, min(blurred_linear.g, blurred_linear.b)), 0.0);
    let haze = clamp(regional_dark / 0.95, 0.0, 1.0);
    let w = a * (1.0 + DEHAZE_LOCAL_HAZE * haze);
    // Perceptual tone; scene values may exceed 1 before tone mapping.
    let t = pow(luma, 0.4545);
    let band = 2.0 * tanh(log2(luma / blurred_luma) * 0.5);

    var ev: f32;
    var contrast: f32;
    var chroma: f32;
    if (w > 0.0) {
        ev = -DEHAZE_DARKEN_EV * w * (1.0 - smoothstep(0.3, 1.4, t));
        contrast = w * DEHAZE_LOCAL_CONTRAST;
        chroma = 1.0 + w * DEHAZE_CHROMA * select(1.0, CR_DEHAZE_CHROMA, base_look_is_camera_raw());
    } else {
        // A monotone lift towards white in the perceptual domain (its slope
        // stays above 1 − DEHAZE_LIFT), so tones never swap order; blacks
        // are spared. An EV bump on the midtones inverted tones at -100.
        // Adding haze is uniform (no regional modulation) and its contrast
        // reduction is capped: pulling pixels further towards the large blur
        // smeared the image into blotches.
        let p = min(t, 1.0);
        let lifted = p + DEHAZE_LIFT * -a * smoothstep(0.0, 0.5, p) * (1.0 - p);
        ev = 2.2 * log2(max(lifted, 0.0001) / max(p, 0.0001));
        contrast = max(a * DEHAZE_LOCAL_CONTRAST * 0.8, -0.3);
        chroma = max(1.0 + a * 0.75, 0.0);
    }
    var result = safe * exp2(ev + band * contrast);
    let result_luma = get_luma(result);
    result = mix(vec3<f32>(result_luma), result, chroma);
    return gamut_floor(result);
}

// Noise level assumed when the photo's own was not measured.
const LUMA_NR_DEFAULT_NOISE: f32 = 0.004;
const COLOR_NR_STRIDE_MIN: f32 = 3.0;
const COLOR_NR_STRIDE_MAX: f32 = 8.0;
const COLOR_NR_RESPONSE: f32 = 16.0;
const COLOR_NR_JITTER: f32 = 0.0;
const COLOR_NR_SPATIAL_SIGMA: f32 = 2.0;
const COLOR_NR_LUMA_TOL: f32 = 0.10;
const COLOR_NR_CHROMA_TOL_MIN: f32 = 0.05;
const COLOR_NR_CHROMA_TOL_MAX: f32 = 0.16;
const COLOR_NR_GATE_NOISE: f32 = 0.008;
const COLOR_NR_OWN_TOL: f32 = 5.0;
const COLOR_NR_SELF_WEIGHT: f32 = 0.2;

// Noise of white noise (unit spread) in the two bands of the blur
// kernels (blur.wgsl: radius 2 and 4 px, sigma half the radius).
const LUMA_NR_BAND_NOISE_0: f32 = 0.873;
const LUMA_NR_BAND_NOISE_1: f32 = 0.192;
// Relative thresholds of the bands.
const LUMA_NR_BAND_WEIGHT_0: f32 = 1.4;
const LUMA_NR_BAND_WEIGHT_1: f32 = 0.7;
// Spacing of the finest band's 5 x 5 energy grid (the other band's 3 x 3
// grid is spaced 4 px).
const LUMA_NR_FINE_SPACING: i32 = 2;
// Share of each band's grain that stays everywhere.
const LUMA_NR_RESIDUAL: vec2<f32> = vec2<f32>(0.15, 0.25);
// How much the coarser band's signal share (squared) also keeps the finest
// band.
const LUMA_NR_CROSS_SCALE: f32 = 1.0;
// Threshold in noise levels at the default amount (15 %).
const LUMA_NR_DEFAULT_THRESHOLD: f32 = 2.2;
// Lightroom's refinements (noise_detail, 0–1), fitted to Camera Raw's
// response relative to its defaults (notes: reference/detail.md):
// - Detail scales the luminance threshold by 2^((0.5 − d)·range).
// - Contrast keeps this much more of the coarser band (more of the finest
//   raised the grain in flat areas far beyond Camera Raw's).
// - Colour Detail below 50 widens the colour tolerances by
//   2^((0.5 − d)·lower); above 50 it narrows them by 2^(−(d − 0.5)·raise)
//   and returns this share of the finest colour detail (pixel minus its
//   1 px blur) times ((d − 0.5)·2)^exponent: Camera Raw's Detail 100 keeps
//   the fine colour of the pixel, its smoothing of blotches stays.
// - Smoothness scales the colour stride by 2^((s − 0.5)·lower) below 50
//   and 2^((s − 0.5)·raise) above.
const LUMA_NR_DETAIL_RANGE: f32 = 0.5;
const LUMA_NR_CONTRAST_RESIDUAL: vec2<f32> = vec2<f32>(0.0, 0.5);
const COLOR_NR_DETAIL_LOWER: f32 = 2.5;
const COLOR_NR_DETAIL_RAISE: f32 = 0.5;
const COLOR_NR_DETAIL_RETURN: f32 = 1.5;
const COLOR_NR_DETAIL_SHARE: f32 = 0.5;
const COLOR_NR_SMOOTHNESS_LOWER: f32 = 2.1;
const COLOR_NR_SMOOTHNESS_RAISE: f32 = 0.8;

fn luma_nr_threshold(a: f32) -> f32 {
    return LUMA_NR_DEFAULT_THRESHOLD * sqrt(a / 0.15);
}

fn apply_noise_reduction_default(
    center_linear: vec3<f32>,
    coords_i: vec2<i32>,
    luma_amount: f32,
    color_amount: f32,
    scale: f32,
    is_raw: u32
) -> vec3<f32> {
    let luma_a  = clamp(luma_amount,  0.0, 1.0);
    let color_a = clamp(color_amount, 0.0, 1.0);
    if (luma_a < 0.001 && color_a < 0.001) {
        return center_linear;
    }

    let dims = vec2<i32>(textureDimensions(input_texture));
    let max_idx = dims - vec2<i32>(1);
    let center_safe   = gamut_floor(center_linear);
    let center_luma   = get_luma(center_safe);
    let center_chroma = center_linear - vec3<f32>(center_luma);

    let res_factor = clamp(sqrt(scale), 0.5, 2.0);

    var new_luma   = center_luma;
    var new_chroma = center_chroma;

    // --- LUMA NOISE REDUCTION ---
    // A local Wiener filter on two bands of the luminance: input minus its
    // 1 px blur, and 1 px minus 2 px blur. Shot noise grows with the square
    // root of the brightness, so the bands are measured in units of
    // 2 sqrt(local brightness), the square-root encoding linearised around
    // the smooth 2 px level (encoding single noisy pixels turned those near
    // black into dark specks). Each band keeps the share of its local energy
    // (5 x 5 grid spaced 2 px, 3 x 3 spaced 4 px) that exceeds the noise expected in it
    // from the photo's measured noise level (noise_level.rs, scaled to the
    // preview size), plus a uniform residual: texture and edges stay where
    // they stand out of the noise, flat areas lose most of their grain.
    // Denser grids decided too locally and left smooth patches next to
    // grainy ones. A third band (2 - 4 px) added 0.45 s of blurring to a
    // 100 MP export for little gain. Measured against Capture One's and
    // Camera Raw's defaults: notes: benchmark-results/noise-reduction-2026-10-07.json.
    if (luma_a > 0.001) {
        let detail = select(1.0, adjustments.global.detail_scale, adjustments.global.detail_scale > 0.0);
        let measured = select(LUMA_NR_DEFAULT_NOISE, adjustments.global.noise_level, adjustments.global.noise_level > 0.0);
        let sigma_n = max(measured * clamp(detail, 0.05, 1.0), 1e-4);
        let k = luma_nr_threshold(luma_a) * sigma_n;
        let noise = vec2<f32>(
            k * LUMA_NR_BAND_NOISE_0 * LUMA_NR_BAND_WEIGHT_0,
            k * LUMA_NR_BAND_NOISE_1 * LUMA_NR_BAND_WEIGHT_1,
        );
        // Below this level read noise, not shot noise, dominates.
        let floor = 4.0 * sigma_n * sigma_n;
        let tile_origin = vec2<i32>(i32(adjustments.tile_offset_x), i32(adjustments.tile_offset_y));
        let blur_max = vec2<i32>(textureDimensions(nr_blur_texture)) - vec2<i32>(1);
        let qc = vec2<u32>(clamp(coords_i - tile_origin, vec2<i32>(0), blur_max));
        var v_c = textureLoad(input_texture, vec2<u32>(clamp(coords_i, vec2<i32>(0), max_idx)), 0).rgb;
        var g1_c = textureLoad(nr_blur_texture, qc, 0).rgb;
        var g2_c = textureLoad(tonal_blur_texture, qc, 0).rgb;
        if (is_raw == 0u) {
            v_c = srgb_to_linear(v_c);
            g1_c = srgb_to_linear(g1_c);
            g2_c = srgb_to_linear(g2_c);
        }
        let l1_c = get_luma(g1_c);
        let l2_c = get_luma(g2_c);
        let c_bands = vec2<f32>(get_luma(v_c) - l1_c, l1_c - l2_c);
        // One brightness unit for the whole neighbourhood.
        let unit = 2.0 * sqrt(max(l2_c, floor));
        var e0 = vec2<f32>(0.0);
        for (var dy: i32 = -2; dy <= 2; dy = dy + 1) {
            for (var dx: i32 = -2; dx <= 2; dx = dx + 1) {
                let p = coords_i + vec2<i32>(dx, dy) * LUMA_NR_FINE_SPACING;
                let q = vec2<u32>(clamp(p - tile_origin, vec2<i32>(0), blur_max));
                var fine = textureLoad(input_texture, vec2<u32>(clamp(p, vec2<i32>(0), max_idx)), 0).rgb;
                var coarse = textureLoad(nr_blur_texture, q, 0).rgb;
                if (is_raw == 0u) {
                    fine = srgb_to_linear(fine);
                    coarse = srgb_to_linear(coarse);
                }
                let d = (get_luma(fine) - get_luma(coarse)) / unit;
                e0.x += d * d;
            }
        }
        for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
            for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
                let q = vec2<u32>(clamp(coords_i + vec2<i32>(dx, dy) * 4 - tile_origin, vec2<i32>(0), blur_max));
                var fine = textureLoad(nr_blur_texture, q, 0).rgb;
                var coarse = textureLoad(tonal_blur_texture, q, 0).rgb;
                if (is_raw == 0u) {
                    fine = srgb_to_linear(fine);
                    coarse = srgb_to_linear(coarse);
                }
                let d = (get_luma(fine) - get_luma(coarse)) / unit;
                e0.y += d * d;
            }
        }
        let energy = e0 / vec2<f32>(25.0, 9.0);
        let noise2 = noise * noise;
        let signal = max(energy - noise2, vec2<f32>(0.0));
        let keep_own = signal / (signal + noise2 + vec2<f32>(1e-12));
        // Edges carry energy at every scale, noise mostly at the finest:
        // the finest band also stays where the coarser one stood out.
        let keep_edges = vec2<f32>(max(keep_own.x, keep_own.y * keep_own.y * LUMA_NR_CROSS_SCALE), keep_own.y);
        // A uniform share of every band stays: switching between kept and
        // removed grain left smooth patches next to grainy ones.
        let keep = LUMA_NR_RESIDUAL + (vec2<f32>(1.0) - LUMA_NR_RESIDUAL) * keep_edges;
        // The bands describe the input; the centre passed in may already
        // differ (no earlier stage changes it today).
        let removed = dot(c_bands, vec2<f32>(1.0) - keep);
        new_luma = max(center_luma - removed, 0.0);
        nr_active = true;
        nr_input = v_c;
        // Sharpening follows the detail decision, not the residual grain.
        nr_fine_keep = keep_edges.x;
    }

    if (color_a > 0.001) {
        // Chroma is smoothed in a perceptual (square-root) encoding, where
        // colour noise in the shadows is as large as it looks, over a sparse
        // 5x5 grid reaching up to ~12 px (colour noise forms blotches of
        // several pixels at high ISO). Samples are weighted by luminance and
        // chroma similarity so colour edges stay; luminance is untouched.
        let c_curve = sqrt(color_a);
        let stride_f = mix(COLOR_NR_STRIDE_MIN, COLOR_NR_STRIDE_MAX, c_curve) * res_factor;
        let c_spat_n = -1.0 / (2.0 * COLOR_NR_SPATIAL_SIGMA * COLOR_NR_SPATIAL_SIGMA);
        let luma_n = -1.0 / (2.0 * COLOR_NR_LUMA_TOL * COLOR_NR_LUMA_TOL);
        let c_measured = select(LUMA_NR_DEFAULT_NOISE, adjustments.global.noise_level, adjustments.global.noise_level > 0.0);
        let c_detail = select(1.0, adjustments.global.detail_scale, adjustments.global.detail_scale > 0.0);
        let chroma_tol = mix(COLOR_NR_CHROMA_TOL_MIN, COLOR_NR_CHROMA_TOL_MAX, c_curve);
        // At high ISO colour noise is far above any colour texture: the
        // gate opens and the pixel's own (noisy) colour loses its weight.
        let c_sigma = max(c_measured * clamp(c_detail, 0.05, 1.0), 1e-4);
        let gate_open = 1.0 + (c_sigma / COLOR_NR_GATE_NOISE) * (c_sigma / COLOR_NR_GATE_NOISE);
        let own_c_tol = c_sigma * COLOR_NR_OWN_TOL * gate_open;
        let own_c_n = -1.0 / (2.0 * own_c_tol * own_c_tol);
        let chroma_n = -1.0 / (2.0 * chroma_tol * chroma_tol);

        let center_p = nr_encode(center_safe);
        let center_pl = get_luma(center_p);
        let center_pc = center_p - vec3<f32>(center_pl);

        let jh1 = hash(vec2<f32>(coords_i) + vec2<f32>(43.7, 91.1));
        let jh2 = hash(vec2<f32>(coords_i) + vec2<f32>(73.3, 17.9));
        let jx = (jh1 - 0.5) * stride_f * COLOR_NR_JITTER;
        let jy = (jh2 - 0.5) * stride_f * COLOR_NR_JITTER;

        let tile_origin = vec2<i32>(i32(adjustments.tile_offset_x), i32(adjustments.tile_offset_y));
        let blur_max = vec2<i32>(textureDimensions(sharpness_blur_texture)) - vec2<i32>(1);
        // Samples are compared with the pre-blurred centre: a noisy centre
        // pixel would otherwise push its neighbours' weights down and keep
        // its own colour noise.
        var guide_rgb = textureLoad(sharpness_blur_texture, vec2<u32>(clamp(coords_i - tile_origin, vec2<i32>(0), blur_max)), 0).rgb;
        if (is_raw == 0u) { guide_rgb = srgb_to_linear(guide_rgb); }
        let guide_p = nr_encode(guide_rgb);
        let guide_pl = get_luma(guide_p);
        let guide_pc = guide_p - vec3<f32>(guide_pl);

        // The pixel's own colour starts the average; samples whose colour
        // differs from it by more than a few noise levels (real colour
        // texture) do not count.
        let self_c = COLOR_NR_SELF_WEIGHT / gate_open;
        var sum_c = center_pc * self_c;
        var w_sum: f32 = self_c;
        let d_gc = guide_pc - center_pc;
        let w_gc = exp(dot(d_gc, d_gc) * own_c_n);
        sum_c += guide_pc * w_gc;
        w_sum += w_gc;
        for (var dy: i32 = -2; dy <= 2; dy = dy + 1) {
            for (var dx: i32 = -2; dx <= 2; dx = dx + 1) {
                if (dx == 0 && dy == 0) { continue; }
                let off_f = vec2<f32>(f32(dx) * stride_f + jx, f32(dy) * stride_f + jy);
                let off = vec2<i32>(i32(round(off_f.x)), i32(round(off_f.y)));
                // Samples come from the 1 px blur: single noisy pixels would
                // leave a sampling floor (~0.4 of the noise at ISO 12800).
                // Blur textures are addressed in tile coordinates.
                let coord = clamp(coords_i + off - tile_origin, vec2<i32>(0), blur_max);
                // Colour comes from the 3.5 px blur (colour needs little
                // resolution; single 1 px samples left their own noise), the
                // luminance guard from the 1 px blur.
                var s = textureLoad(tonal_blur_texture, vec2<u32>(coord), 0).rgb;
                var s_fine = textureLoad(sharpness_blur_texture, vec2<u32>(coord), 0).rgb;
                if (is_raw == 0u) { s = srgb_to_linear(s); s_fine = srgb_to_linear(s_fine); }
                let sp = nr_encode(s);
                let spl = get_luma(nr_encode(s_fine));
                let spc = sp - vec3<f32>(get_luma(sp));
                let w_s = exp(f32(dx * dx + dy * dy) * c_spat_n);
                let dl = spl - guide_pl;
                let dc = spc - guide_pc;
                let d_own = spc - center_pc;
                let w = w_s * exp(dl * dl * luma_n) * exp(dot(dc, dc) * chroma_n) * exp(dot(d_own, d_own) * own_c_n);
                sum_c += spc * w;
                w_sum += w;
            }
        }
        // Lightroom's Color slider reaches nearly its full effect by 25.
        let strength = 1.0 - pow(1.0 - color_a, COLOR_NR_RESPONSE);
        let filtered_pc = mix(center_pc, sum_c / max(w_sum, 1e-6), strength);
        // The filtered colour is built on the (noise-reduced) luminance of
        // the result: on the noisy one, pixels near black turned into
        // saturated specks (the colour difference outweighed their level).
        // In square-root encoding a colour's luminance is below the square
        // root of its linear luminance; the smoothed neighbourhood gives
        // that ratio without the pixel's noise (sqrt alone desaturated).
        let guide_ratio = clamp(guide_pl / sqrt(max(get_luma(max(guide_rgb, vec3<f32>(0.0))), 1e-10)), 0.3, 1.0);
        let base_pl = select(center_pl, sqrt(max(new_luma, 0.0)) * guide_ratio, luma_a > 0.001);
        var rgb_new = nr_decode(vec3<f32>(base_pl) + filtered_pc);
        // Keep the linear luminance exactly.
        let l_new = get_luma(rgb_new);
        if (l_new > 1e-6) {
            rgb_new *= new_luma / l_new;
        }
        return rgb_new;
    }

    // Luminance only: the colour difference follows a darkening (added
    // unscaled to a darkened noisy pixel it pushed channels below zero).
    let luma_ratio = select(1.0, clamp(new_luma / center_luma, 0.0, 1.0), center_luma > 1e-5);
    return vec3<f32>(new_luma) + new_chroma * luma_ratio;
}

fn apply_noise_reduction_refined(
    center_linear: vec3<f32>,
    coords_i: vec2<i32>,
    luma_amount: f32,
    color_amount: f32,
    scale: f32,
    is_raw: u32
) -> vec3<f32> {
    let luma_a  = clamp(luma_amount,  0.0, 1.0);
    let color_a = clamp(color_amount, 0.0, 1.0);
    if (luma_a < 0.001 && color_a < 0.001) {
        return center_linear;
    }

    let dims = vec2<i32>(textureDimensions(input_texture));
    let max_idx = dims - vec2<i32>(1);
    let center_safe   = gamut_floor(center_linear);
    let center_luma   = get_luma(center_safe);
    let center_chroma = center_linear - vec3<f32>(center_luma);

    let res_factor = clamp(sqrt(scale), 0.5, 2.0);

    var new_luma   = center_luma;
    var new_chroma = center_chroma;

    // --- LUMA NOISE REDUCTION ---
    // A local Wiener filter on two bands of the luminance: input minus its
    // 1 px blur, and 1 px minus 2 px blur. Shot noise grows with the square
    // root of the brightness, so the bands are measured in units of
    // 2 sqrt(local brightness), the square-root encoding linearised around
    // the smooth 2 px level (encoding single noisy pixels turned those near
    // black into dark specks). Each band keeps the share of its local energy
    // (5 x 5 grid spaced 2 px, 3 x 3 spaced 4 px) that exceeds the noise expected in it
    // from the photo's measured noise level (noise_level.rs, scaled to the
    // preview size), plus a uniform residual: texture and edges stay where
    // they stand out of the noise, flat areas lose most of their grain.
    // Denser grids decided too locally and left smooth patches next to
    // grainy ones. A third band (2 - 4 px) added 0.45 s of blurring to a
    // 100 MP export for little gain. Measured against Capture One's and
    // Camera Raw's defaults: notes: benchmark-results/noise-reduction-2026-10-07.json.
    if (luma_a > 0.001) {
        let detail = select(1.0, adjustments.global.detail_scale, adjustments.global.detail_scale > 0.0);
        let measured = select(LUMA_NR_DEFAULT_NOISE, adjustments.global.noise_level, adjustments.global.noise_level > 0.0);
        let sigma_n = max(measured * clamp(detail, 0.05, 1.0), 1e-4);
        let refine = adjustments.global.noise_detail;
        // Factors only away from the defaults: exp2(0) is not exactly 1 on
        // every GPU, and default edits must render as before.
        var k = luma_nr_threshold(luma_a) * sigma_n;
        if (refine.x != 0.5) {
            k *= exp2((0.5 - refine.x) * LUMA_NR_DETAIL_RANGE);
        }
        let noise = vec2<f32>(
            k * LUMA_NR_BAND_NOISE_0 * LUMA_NR_BAND_WEIGHT_0,
            k * LUMA_NR_BAND_NOISE_1 * LUMA_NR_BAND_WEIGHT_1,
        );
        // Below this level read noise, not shot noise, dominates.
        let floor = 4.0 * sigma_n * sigma_n;
        let tile_origin = vec2<i32>(i32(adjustments.tile_offset_x), i32(adjustments.tile_offset_y));
        let blur_max = vec2<i32>(textureDimensions(nr_blur_texture)) - vec2<i32>(1);
        let qc = vec2<u32>(clamp(coords_i - tile_origin, vec2<i32>(0), blur_max));
        var v_c = textureLoad(input_texture, vec2<u32>(clamp(coords_i, vec2<i32>(0), max_idx)), 0).rgb;
        var g1_c = textureLoad(nr_blur_texture, qc, 0).rgb;
        var g2_c = textureLoad(tonal_blur_texture, qc, 0).rgb;
        if (is_raw == 0u) {
            v_c = srgb_to_linear(v_c);
            g1_c = srgb_to_linear(g1_c);
            g2_c = srgb_to_linear(g2_c);
        }
        let l1_c = get_luma(g1_c);
        let l2_c = get_luma(g2_c);
        let c_bands = vec2<f32>(get_luma(v_c) - l1_c, l1_c - l2_c);
        // One brightness unit for the whole neighbourhood.
        let unit = 2.0 * sqrt(max(l2_c, floor));
        var e0 = vec2<f32>(0.0);
        for (var dy: i32 = -2; dy <= 2; dy = dy + 1) {
            for (var dx: i32 = -2; dx <= 2; dx = dx + 1) {
                let p = coords_i + vec2<i32>(dx, dy) * LUMA_NR_FINE_SPACING;
                let q = vec2<u32>(clamp(p - tile_origin, vec2<i32>(0), blur_max));
                var fine = textureLoad(input_texture, vec2<u32>(clamp(p, vec2<i32>(0), max_idx)), 0).rgb;
                var coarse = textureLoad(nr_blur_texture, q, 0).rgb;
                if (is_raw == 0u) {
                    fine = srgb_to_linear(fine);
                    coarse = srgb_to_linear(coarse);
                }
                let d = (get_luma(fine) - get_luma(coarse)) / unit;
                e0.x += d * d;
            }
        }
        for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
            for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
                let q = vec2<u32>(clamp(coords_i + vec2<i32>(dx, dy) * 4 - tile_origin, vec2<i32>(0), blur_max));
                var fine = textureLoad(nr_blur_texture, q, 0).rgb;
                var coarse = textureLoad(tonal_blur_texture, q, 0).rgb;
                if (is_raw == 0u) {
                    fine = srgb_to_linear(fine);
                    coarse = srgb_to_linear(coarse);
                }
                let d = (get_luma(fine) - get_luma(coarse)) / unit;
                e0.y += d * d;
            }
        }
        let energy = e0 / vec2<f32>(25.0, 9.0);
        let noise2 = noise * noise;
        let signal = max(energy - noise2, vec2<f32>(0.0));
        let keep_own = signal / (signal + noise2 + vec2<f32>(1e-12));
        // Edges carry energy at every scale, noise mostly at the finest:
        // the finest band also stays where the coarser one stood out.
        let keep_edges = vec2<f32>(max(keep_own.x, keep_own.y * keep_own.y * LUMA_NR_CROSS_SCALE), keep_own.y);
        // A uniform share of every band stays: switching between kept and
        // removed grain left smooth patches next to grainy ones.
        var residual = LUMA_NR_RESIDUAL;
        if (refine.y > 0.0) {
            residual += refine.y * LUMA_NR_CONTRAST_RESIDUAL;
        }
        let keep = residual + (vec2<f32>(1.0) - residual) * keep_edges;
        // The bands describe the input; the centre passed in may already
        // differ (no earlier stage changes it today).
        let removed = dot(c_bands, vec2<f32>(1.0) - keep);
        new_luma = max(center_luma - removed, 0.0);
        nr_active = true;
        nr_input = v_c;
        // Sharpening follows the detail decision, not the residual grain.
        nr_fine_keep = keep_edges.x;
    }

    if (color_a > 0.001) {
        // Chroma is smoothed in a perceptual (square-root) encoding, where
        // colour noise in the shadows is as large as it looks, over a sparse
        // 5x5 grid reaching up to ~12 px (colour noise forms blotches of
        // several pixels at high ISO). Samples are weighted by luminance and
        // chroma similarity so colour edges stay; luminance is untouched.
        let c_curve = sqrt(color_a);
        let c_refine = adjustments.global.noise_detail;
        var stride_f = mix(COLOR_NR_STRIDE_MIN, COLOR_NR_STRIDE_MAX, c_curve) * res_factor;
        if (c_refine.w != 0.5) {
            stride_f *= exp2((c_refine.w - 0.5) * select(COLOR_NR_SMOOTHNESS_LOWER, COLOR_NR_SMOOTHNESS_RAISE, c_refine.w > 0.5));
        }
        var c_tol_scale = 1.0;
        if (c_refine.z != 0.5) {
            c_tol_scale = select(
                exp2((0.5 - c_refine.z) * COLOR_NR_DETAIL_LOWER),
                exp2(-(c_refine.z - 0.5) * COLOR_NR_DETAIL_RAISE),
                c_refine.z > 0.5,
            );
        }
        let c_spat_n = -1.0 / (2.0 * COLOR_NR_SPATIAL_SIGMA * COLOR_NR_SPATIAL_SIGMA);
        let luma_n = -1.0 / (2.0 * COLOR_NR_LUMA_TOL * COLOR_NR_LUMA_TOL);
        let c_measured = select(LUMA_NR_DEFAULT_NOISE, adjustments.global.noise_level, adjustments.global.noise_level > 0.0);
        let c_detail = select(1.0, adjustments.global.detail_scale, adjustments.global.detail_scale > 0.0);
        let chroma_tol = mix(COLOR_NR_CHROMA_TOL_MIN, COLOR_NR_CHROMA_TOL_MAX, c_curve) * c_tol_scale;
        // At high ISO colour noise is far above any colour texture: the
        // gate opens and the pixel's own (noisy) colour loses its weight.
        let c_sigma = max(c_measured * clamp(c_detail, 0.05, 1.0), 1e-4);
        let gate_open = 1.0 + (c_sigma / COLOR_NR_GATE_NOISE) * (c_sigma / COLOR_NR_GATE_NOISE);
        let own_c_tol = c_sigma * COLOR_NR_OWN_TOL * gate_open * c_tol_scale;
        let own_c_n = -1.0 / (2.0 * own_c_tol * own_c_tol);
        let chroma_n = -1.0 / (2.0 * chroma_tol * chroma_tol);

        let center_p = nr_encode(center_safe);
        let center_pl = get_luma(center_p);
        let center_pc = center_p - vec3<f32>(center_pl);

        let jh1 = hash(vec2<f32>(coords_i) + vec2<f32>(43.7, 91.1));
        let jh2 = hash(vec2<f32>(coords_i) + vec2<f32>(73.3, 17.9));
        let jx = (jh1 - 0.5) * stride_f * COLOR_NR_JITTER;
        let jy = (jh2 - 0.5) * stride_f * COLOR_NR_JITTER;

        let tile_origin = vec2<i32>(i32(adjustments.tile_offset_x), i32(adjustments.tile_offset_y));
        let blur_max = vec2<i32>(textureDimensions(sharpness_blur_texture)) - vec2<i32>(1);
        // Samples are compared with the pre-blurred centre: a noisy centre
        // pixel would otherwise push its neighbours' weights down and keep
        // its own colour noise.
        var guide_rgb = textureLoad(sharpness_blur_texture, vec2<u32>(clamp(coords_i - tile_origin, vec2<i32>(0), blur_max)), 0).rgb;
        if (is_raw == 0u) { guide_rgb = srgb_to_linear(guide_rgb); }
        let guide_p = nr_encode(guide_rgb);
        let guide_pl = get_luma(guide_p);
        let guide_pc = guide_p - vec3<f32>(guide_pl);

        // The pixel's own colour starts the average; samples whose colour
        // differs from it by more than a few noise levels (real colour
        // texture) do not count.
        let self_c = COLOR_NR_SELF_WEIGHT / gate_open;
        var sum_c = center_pc * self_c;
        var w_sum: f32 = self_c;
        let d_gc = guide_pc - center_pc;
        let w_gc = exp(dot(d_gc, d_gc) * own_c_n);
        sum_c += guide_pc * w_gc;
        w_sum += w_gc;
        for (var dy: i32 = -2; dy <= 2; dy = dy + 1) {
            for (var dx: i32 = -2; dx <= 2; dx = dx + 1) {
                if (dx == 0 && dy == 0) { continue; }
                let off_f = vec2<f32>(f32(dx) * stride_f + jx, f32(dy) * stride_f + jy);
                let off = vec2<i32>(i32(round(off_f.x)), i32(round(off_f.y)));
                // Samples come from the 1 px blur: single noisy pixels would
                // leave a sampling floor (~0.4 of the noise at ISO 12800).
                // Blur textures are addressed in tile coordinates.
                let coord = clamp(coords_i + off - tile_origin, vec2<i32>(0), blur_max);
                // Colour comes from the 3.5 px blur (colour needs little
                // resolution; single 1 px samples left their own noise), the
                // luminance guard from the 1 px blur.
                var s = textureLoad(tonal_blur_texture, vec2<u32>(coord), 0).rgb;
                var s_fine = textureLoad(sharpness_blur_texture, vec2<u32>(coord), 0).rgb;
                if (is_raw == 0u) { s = srgb_to_linear(s); s_fine = srgb_to_linear(s_fine); }
                let sp = nr_encode(s);
                let spl = get_luma(nr_encode(s_fine));
                let spc = sp - vec3<f32>(get_luma(sp));
                let w_s = exp(f32(dx * dx + dy * dy) * c_spat_n);
                let dl = spl - guide_pl;
                let dc = spc - guide_pc;
                let d_own = spc - center_pc;
                let w = w_s * exp(dl * dl * luma_n) * exp(dot(dc, dc) * chroma_n) * exp(dot(d_own, d_own) * own_c_n);
                sum_c += spc * w;
                w_sum += w;
            }
        }
        // Lightroom's Color slider reaches nearly its full effect by 25.
        let strength = 1.0 - pow(1.0 - color_a, COLOR_NR_RESPONSE);
        var filtered_pc = mix(center_pc, sum_c / max(w_sum, 1e-6), strength);
        if (c_refine.z > 0.5) {
            let fine_return = COLOR_NR_DETAIL_SHARE * pow((c_refine.z - 0.5) * 2.0, COLOR_NR_DETAIL_RETURN);
            filtered_pc += (center_pc - guide_pc) * fine_return * strength;
        }
        // The filtered colour is built on the (noise-reduced) luminance of
        // the result: on the noisy one, pixels near black turned into
        // saturated specks (the colour difference outweighed their level).
        // In square-root encoding a colour's luminance is below the square
        // root of its linear luminance; the smoothed neighbourhood gives
        // that ratio without the pixel's noise (sqrt alone desaturated).
        let guide_ratio = clamp(guide_pl / sqrt(max(get_luma(max(guide_rgb, vec3<f32>(0.0))), 1e-10)), 0.3, 1.0);
        let base_pl = select(center_pl, sqrt(max(new_luma, 0.0)) * guide_ratio, luma_a > 0.001);
        var rgb_new = nr_decode(vec3<f32>(base_pl) + filtered_pc);
        // Keep the linear luminance exactly.
        let l_new = get_luma(rgb_new);
        if (l_new > 1e-6) {
            rgb_new *= new_luma / l_new;
        }
        return rgb_new;
    }

    // Luminance only: the colour difference follows a darkening (added
    // unscaled to a darkened noisy pixel it pushed channels below zero).
    let luma_ratio = select(1.0, clamp(new_luma / center_luma, 0.0, 1.0), center_luma > 1e-5);
    return vec3<f32>(new_luma) + new_chroma * luma_ratio;
}

// Default refinements take the unchanged filter (default edits render
// exactly as before); others the refined copy.
fn apply_noise_reduction(
    center_linear: vec3<f32>,
    coords_i: vec2<i32>,
    luma_amount: f32,
    color_amount: f32,
    scale: f32,
    is_raw: u32
) -> vec3<f32> {
    let r = adjustments.global.noise_detail;
    if (r.x == 0.5 && r.y == 0.0 && r.z == 0.5 && r.w == 0.5) {
        return apply_noise_reduction_default(center_linear, coords_i, luma_amount, color_amount, scale, is_raw);
    }
    return apply_noise_reduction_refined(center_linear, coords_i, luma_amount, color_amount, scale, is_raw);
}

fn apply_ca_correction(coords: vec2<u32>, ca_rc: f32, ca_by: f32) -> vec3<f32> {
    let dims = vec2<f32>(textureDimensions(input_texture));
    let center = dims / 2.0;
    let current_pos = vec2<f32>(coords);

    let to_center = current_pos - center;
    let dist = length(to_center);

    if (dist == 0.0) {
        return textureLoad(input_texture, coords, 0).rgb;
    }

    let dir = to_center / dist;

    let red_shift = dir * dist * ca_rc;
    let blue_shift = dir * dist * ca_by;

    let red_coords = vec2<i32>(round(current_pos - red_shift));
    let blue_coords = vec2<i32>(round(current_pos - blue_shift));
    let green_coords = vec2<i32>(current_pos);

    let max_coords = vec2<i32>(dims - 1.0);

    let r = textureLoad(input_texture, vec2<u32>(clamp(red_coords, vec2<i32>(0), max_coords)), 0).r;
    let g = textureLoad(input_texture, vec2<u32>(clamp(green_coords, vec2<i32>(0), max_coords)), 0).g;
    let b = textureLoad(input_texture, vec2<u32>(clamp(blue_coords, vec2<i32>(0), max_coords)), 0).b;

    return vec3<f32>(r, g, b);
}

const AGX_EPSILON: f32 = 1.0e-6;
const AGX_MIN_EV: f32 = -15.2;
const AGX_MAX_EV: f32 = 5.0;
const AGX_RANGE_EV: f32 = AGX_MAX_EV - AGX_MIN_EV;
const AGX_GAMMA: f32 = 2.4;
const AGX_SLOPE: f32 = 2.3843;
const AGX_TOE_POWER: f32 = 1.5;
const AGX_SHOULDER_POWER: f32 = 1.5;
const AGX_TOE_TRANSITION_X: f32 = 0.6060606;
const AGX_TOE_TRANSITION_Y: f32 = 0.43446;
const AGX_SHOULDER_TRANSITION_X: f32 = 0.6060606;
const AGX_SHOULDER_TRANSITION_Y: f32 = 0.43446;
const AGX_INTERCEPT: f32 = -1.0112;
const AGX_TOE_SCALE: f32 = -1.0359;
const AGX_SHOULDER_SCALE: f32 = 1.3475;
const AGX_TARGET_BLACK_PRE_GAMMA: f32 = 0.0;
const AGX_TARGET_WHITE_PRE_GAMMA: f32 = 1.0;

fn agx_sigmoid(x: f32, power: f32) -> f32 {
    return x / pow(1.0 + pow(x, power), 1.0 / power);
}

fn agx_scaled_sigmoid(x: f32, scale: f32, slope: f32, power: f32, transition_x: f32, transition_y: f32) -> f32 {
    return scale * agx_sigmoid(slope * (x - transition_x) / scale, power) + transition_y;
}

fn agx_apply_curve_channel(x: f32) -> f32 {
    var result: f32 = 0.0;
    if (x < AGX_TOE_TRANSITION_X) {
        result = agx_scaled_sigmoid(x, AGX_TOE_SCALE, AGX_SLOPE, AGX_TOE_POWER, AGX_TOE_TRANSITION_X, AGX_TOE_TRANSITION_Y);
    } else if (x <= AGX_SHOULDER_TRANSITION_X) {
        result = AGX_SLOPE * x + AGX_INTERCEPT;
    } else {
        result = agx_scaled_sigmoid(x, AGX_SHOULDER_SCALE, AGX_SLOPE, AGX_SHOULDER_POWER, AGX_SHOULDER_TRANSITION_X, AGX_SHOULDER_TRANSITION_Y);
    }
    return clamp(result, AGX_TARGET_BLACK_PRE_GAMMA, AGX_TARGET_WHITE_PRE_GAMMA);
}

fn agx_compress_gamut(c: vec3<f32>) -> vec3<f32> {
    // Colours keep the ProPhoto gamut (the inherited version lifted
    // negative channels with grey).
    return gamut_floor(c);
}

// Curve choices on top of the base look (RAW only), as a change of display
// luminance (EV) and chroma by display level: 1 High contrast (darker
// shadows, brighter upper midtones around mid grey), 2 Extra shadow (shadows
// lifted up to 0.8 EV), 3 Linear response (the scene's own luminance: the
// default tone curve undone). Rawtonic's own shapes (notes: reference/
// own-base-look.md); `with_base_curve` in auto_adjust.rs mirrors them.
const CURVE_CHOICE_CONTRAST: f32 = 0.15;
const CURVE_CHOICE_PIVOT_EV: f32 = -2.5;
const CURVE_CHOICE_CONTRAST_CHROMA: f32 = 0.06;
const CURVE_CHOICE_SHADOW_LIFT: f32 = 0.8;
const CURVE_CHOICE_SHADOW_CHROMA: f32 = 0.05;

// Scene EV that the default tone curve maps to display EV `level`.
fn default_tone_curve_inverse(level: f32) -> f32 {
    if (level >= 0.0) {
        return 0.0;
    }
    if (level <= DEFAULT_TONE_CURVE[0]) {
        return level - DEFAULT_TONE_CURVE[0] + DEFAULT_TONE_CURVE_START_EV;
    }
    var i = 0u;
    for (var k = 0u; k < 48u; k = k + 1u) {
        if (DEFAULT_TONE_CURVE[k + 1u] <= level) {
            i = k + 1u;
        }
    }
    i = min(i, 47u);
    let a = DEFAULT_TONE_CURVE[i];
    let b = DEFAULT_TONE_CURVE[i + 1u];
    let t = clamp((level - a) / max(b - a, 1e-6), 0.0, 1.0);
    return DEFAULT_TONE_CURVE_START_EV + (f32(i) + t) * 0.25;
}

fn base_curve_lookup(curve: u32, level: f32) -> vec2<f32> {
    if (curve == 1u) {
        let w = smoothstep(-10.0, -6.0, level) * (1.0 - smoothstep(-1.5, 0.0, level));
        return vec2<f32>(CURVE_CHOICE_CONTRAST * (level - CURVE_CHOICE_PIVOT_EV) * w, 1.0 + CURVE_CHOICE_CONTRAST_CHROMA * w);
    }
    if (curve == 2u) {
        let s = 1.0 - smoothstep(-6.5, -1.5, level);
        return vec2<f32>(CURVE_CHOICE_SHADOW_LIFT * s, 1.0 - CURVE_CHOICE_SHADOW_CHROMA * s);
    }
    if (curve == 3u) {
        return vec2<f32>(default_tone_curve_inverse(level) - level, 1.0);
    }
    return vec2<f32>(0.0, 1.0);
}

/// Applies the chosen base curve to display-encoded (sRGB) values.
fn apply_base_curve(display: vec3<f32>, curve: u32) -> vec3<f32> {
    if (curve == 0u) {
        return display;
    }
    let linear = display_to_linear(display);
    let luma = get_luma(linear);
    if (luma <= 1e-6) {
        return display;
    }
    let change = base_curve_lookup(curve, log2(luma));
    let new_luma = min(luma * exp2(change.x), 1.0);
    let scaled = linear * (new_luma / luma);
    let toned = vec3<f32>(new_luma) + (scaled - vec3<f32>(new_luma)) * change.y;
    return linear_to_display(toned);
}

// Neutral base look (notes: reference/own-base-look.md): linear ProPhoto
// RGB, the default exposure-ramp black (0.5 % with a quadratic toe), then
// Rawtonic's own tone curve applied with a hue-preserving RGB method. The
// curve is a monotone cubic (PCHIP) through six points in log-log, scene EV
// -8, -6.5, -5, -3, -1.5, -0.5 to display EV
// -8.758, -7.378, -5.325, -2.438, -0.827, -0.198, with white at 0 EV and slope 1 below (a seventh
// point 6 EV under the first), sampled every 0.25 EV from -12 EV
// (`DEFAULT_TONE_CURVE` in auto_adjust.rs; a test rebuilds both from the
// points). The points were fitted to the lightness of Camera Raw's default
// renders of public raw.pixls.us files (281 camera models), then corrected
// by the measured difference per tone level on Rawtonic's own exports.
const DEFAULT_TONE_CURVE_START_EV: f32 = -12.0;
const DEFAULT_TONE_CURVE = array<f32, 49>(-12.6791, -12.4256, -12.1734, -11.9225, -11.6728, -11.4243, -11.1769, -10.9307, -10.6855, -10.4413, -10.1982, -9.9560, -9.7147, -9.4743, -9.2347, -8.9960, -8.7580, -8.5290, -8.3113, -8.0961, -7.8747, -7.6383, -7.3780, -7.0836, -6.7571, -6.4083, -6.0468, -5.6825, -5.3250, -4.9658, -4.5939, -4.2154, -3.8364, -3.4631, -3.1015, -2.7578, -2.4380, -2.1334, -1.8352, -1.5494, -1.2821, -1.0392, -0.8270, -0.6413, -0.4743, -0.3264, -0.1980, -0.0893, 0.0000);
const CAMERA_RAW_BLACK: f32 = 0.005;
// Neutral-colour Camera Raw look (flag 2): black ramp scaled with Exposure.
// A lighter 0.25 % matched Lightroom on the test set before the per-camera
// exposure tables; with them the deep shadows were 0.2-0.5 EV lighter than
// Camera Raw at display -6..-8 EV, and 0.5 % (Adobe's Shadows 5) left them
// 0.19 / 0.18 / 0.12 EV lighter at -8 / -7 / -6 EV over 900 camera models
// once their levels matched Adobe's; 0.6 % fits that profile
// (notes: reference/tone-controls.md, 2026-10-05).
const CAMERA_RAW_BLACK_NEUTRAL: f32 = 0.006;
const CAMERA_RAW_SATURATION: f32 = 1.15;
const CAMERA_RAW_BLUE_HUE: f32 = 265.0;
const CAMERA_RAW_BLUE_REACH: f32 = 45.0;
const CAMERA_RAW_BLUE_TURN: f32 = -7.0;

fn srgb_to_prophoto(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(vec3<f32>(0.5293458, 0.3300728, 0.1405812), c),
        dot(vec3<f32>(0.0983744, 0.8734611, 0.0281647), c),
        dot(vec3<f32>(0.0168832, 0.1176725, 0.8654443), c),
    );
}

fn prophoto_to_srgb(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(vec3<f32>(2.0340763, -0.7273343, -0.3067417), c),
        dot(vec3<f32>(-0.2288135, 1.2317302, -0.0029169), c),
        dot(vec3<f32>(-0.0085698, -0.1532866, 1.1618565), c),
    );
}

fn profile_curve_value(index: u32) -> f32 {
    return adjustments.global.profile_curve[index / 4u][index % 4u];
}

fn camera_raw_curve(x: f32) -> f32 {
    if (x >= 1.0) {
        return 1.0;
    }
    if (x <= 0.0) {
        return 0.0;
    }
    let ev = log2(x);
    let position = (ev - DEFAULT_TONE_CURVE_START_EV) * 4.0;
    // A curve embedded in the file (e.g. Apple ProRAW, flag 1), an own
    // curve with neutral colour (flag 3) or a camera profile's curve
    // (flag 4) replaces the default tone curve.
    let flag = adjustments.global.profile_curve[15].w;
    if ((flag > 0.5 && flag < 1.5) || (flag > 2.5 && flag < 4.5)) {
        if (position <= 0.0) {
            return exp2(ev + profile_curve_value(0u) - DEFAULT_TONE_CURVE_START_EV);
        }
        let index = min(u32(position), 47u);
        let t = position - f32(index);
        return exp2(mix(profile_curve_value(index), profile_curve_value(index + 1u), t));
    }
    if (position <= 0.0) {
        return exp2(ev + DEFAULT_TONE_CURVE[0] - DEFAULT_TONE_CURVE_START_EV);
    }
    let index = min(u32(position), 47u);
    let t = position - f32(index);
    return exp2(mix(DEFAULT_TONE_CURVE[index], DEFAULT_TONE_CURVE[index + 1u], t));
}

// The neutral look's black point scales with Exposure, so Exposure acts
// 1:1 in the deepest tones as in Lightroom.
fn camera_raw_black() -> f32 {
    // DNG DefaultBlackRender = None: no black subtraction.
    if (adjustments.global.profile_curve[15].z > 0.5) {
        return 0.0;
    }
    if (adjustments.global.profile_curve[15].w > 1.5) {
        return min(CAMERA_RAW_BLACK_NEUTRAL * exp2(adjustments.global.exposure), 0.05);
    }
    return CAMERA_RAW_BLACK;
}

fn camera_raw_black_ramp(x: f32) -> f32 {
    let black = camera_raw_black();
    let slope = 1.0 / (1.0 - black);
    let radius = min(0.5 * black, 0.0625 / slope);
    if (x <= black - radius) {
        return 0.0;
    }
    if (x >= black + radius) {
        return (x - black) * slope;
    }
    let y = x - (black - radius);
    return slope / (4.0 * radius) * y * y;
}

// Rawtonic's colour rendition for the neutral look (all RAWs except files
// rendered with their own embedded profile): smooth functions of Oklab hue h
// instead of tables. Log chroma scale and hue turn (degrees) are Fourier
// series up to 3h, the lightness offset (Oklab L / 100) up to 2h; chroma
// also follows lightness a little, and colours near white lose chroma
// towards white. 22 numbers in all, fitted to the colour of Camera Raw's
// default renders of 135 public raw.pixls.us files and checked on the other
// 135 (notes: reference/own-base-look.md, 2026-10-10). Near-neutral
// colours keep their place.
const OWN_LOOK_CHROMA = array<f32, 7>(-0.0567, 0.0364, 0.0765, -0.0374, -0.0460, -0.0043, 0.0442);
const OWN_LOOK_TURN = array<f32, 7>(0.1394, -2.1784, 0.7715, 3.7421, 0.8807, 2.1434, -0.9362);
const OWN_LOOK_LIGHT = array<f32, 5>(0.2358, -0.6643, -0.7239, 0.0696, 0.5470);
const OWN_LOOK_CHROMA_BY_LIGHTNESS: f32 = 0.0449;
const OWN_LOOK_HIGHLIGHT_START: f32 = 0.8971;
const OWN_LOOK_HIGHLIGHT_LOSS: f32 = 0.3966;

fn own_look_series(c: ptr<function, array<f32, 7>>, h: f32, harmonics: u32) -> f32 {
    var v = (*c)[0];
    for (var k = 1u; k <= harmonics; k = k + 1u) {
        let a = f32(k) * h;
        v += (*c)[2u * k - 1u] * cos(a) + (*c)[2u * k] * sin(a);
    }
    return v;
}

fn own_colour_look(lab: vec3<f32>, chroma: f32) -> vec3<f32> {
    let h = atan2(lab.z, lab.y);
    var chroma_terms = OWN_LOOK_CHROMA;
    var turn_terms = OWN_LOOK_TURN;
    var light_terms = array<f32, 7>(OWN_LOOK_LIGHT[0], OWN_LOOK_LIGHT[1], OWN_LOOK_LIGHT[2], OWN_LOOK_LIGHT[3], OWN_LOOK_LIGHT[4], 0.0, 0.0);
    let amount = smoothstep(0.0, 0.04, chroma);
    let log_scale = own_look_series(&chroma_terms, h, 3u) + OWN_LOOK_CHROMA_BY_LIGHTNESS * (lab.x - 0.6);
    let scale = exp(amount * log_scale) * (1.0 - OWN_LOOK_HIGHLIGHT_LOSS * smoothstep(OWN_LOOK_HIGHLIGHT_START, 1.0, lab.x));
    let angle = h + radians(amount * own_look_series(&turn_terms, h, 3u));
    let light = amount * own_look_series(&light_terms, h, 2u) * 0.01;
    let c = chroma * scale;
    return vec3<f32>(lab.x + light, c * cos(angle), c * sin(angle));
}

// Camera profiles (camera_profile.rs): HSV of linear ProPhoto and the
// DNG SDK's HueSatMap / LookTable lookup. Tables follow the mask layers'
// curve tables in `curve_luts`, HueSatMap first; `dims` = (hue, saturation, value
// divisions, sRGB-encoded value), `base` = entry offset of the table.
const DCP_TABLE_BASE: u32 = 1025u * 33u;

fn dcp_rgb_to_hsv(c: vec3<f32>) -> vec3<f32> {
    let v = max(c.r, max(c.g, c.b));
    let gap = v - min(c.r, min(c.g, c.b));
    if (gap <= 0.0 || v <= 0.0) {
        return vec3<f32>(0.0, 0.0, v);
    }
    var h: f32;
    if (c.r == v) {
        h = (c.g - c.b) / gap;
        if (h < 0.0) { h += 6.0; }
    } else if (c.g == v) {
        h = 2.0 + (c.b - c.r) / gap;
    } else {
        h = 4.0 + (c.r - c.g) / gap;
    }
    return vec3<f32>(h, gap / v, v);
}

fn dcp_hsv_to_rgb(hsv: vec3<f32>) -> vec3<f32> {
    let s = hsv.y;
    let v = hsv.z;
    if (s <= 0.0) {
        return vec3<f32>(v);
    }
    let h = hsv.x - 6.0 * floor(hsv.x / 6.0);
    let i = min(u32(h), 5u);
    let f = h - f32(i);
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    switch (i) {
        case 0u: { return vec3<f32>(v, t, p); }
        case 1u: { return vec3<f32>(q, v, p); }
        case 2u: { return vec3<f32>(p, v, t); }
        case 3u: { return vec3<f32>(p, q, v); }
        case 4u: { return vec3<f32>(t, p, v); }
        default: { return vec3<f32>(v, p, q); }
    }
}

fn dcp_entry(base: u32, dims: vec4<u32>, v: u32, h: u32, s: u32) -> vec3<f32> {
    return curve_luts[DCP_TABLE_BASE + base + (v * dims.x + h) * dims.y + s].xyz;
}

fn dcp_srgb_encode(v: f32) -> f32 {
    if (v <= 0.0031308) { return v * 12.92; }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn dcp_srgb_decode(v: f32) -> f32 {
    if (v <= 0.04045) { return v / 12.92; }
    return pow((v + 0.055) / 1.055, 2.4);
}

// Mirrors HsvTable::apply.
fn dcp_table(prophoto: vec3<f32>, dims: vec4<u32>, base: u32) -> vec3<f32> {
    let hsv = dcp_rgb_to_hsv(max(prophoto, vec3<f32>(0.0)));
    var h_scaled = 0.0;
    if (dims.x > 1u) {
        h_scaled = hsv.x * f32(dims.x) / 6.0;
    }
    let s_scaled = hsv.y * f32(dims.y - 1u);
    var h0 = u32(h_scaled);
    let s0 = min(u32(s_scaled), dims.y - 2u);
    let h_fract = h_scaled - f32(h0);
    let s_fract = s_scaled - f32(s0);
    var h1 = h0 + 1u;
    if (h0 >= dims.x - 1u) {
        h0 = dims.x - 1u;
        h1 = 0u;
    }
    let srgb_value = dims.w != 0u;
    var v_encoded = min(hsv.z, 1.0);
    if (srgb_value) {
        v_encoded = dcp_srgb_encode(v_encoded);
    }
    var v0 = 0u;
    var v1 = 0u;
    var v_fract = 0.0;
    if (dims.z > 1u) {
        let v_scaled = v_encoded * f32(dims.z - 1u);
        v0 = min(u32(v_scaled), dims.z - 2u);
        v1 = v0 + 1u;
        v_fract = v_scaled - f32(v0);
    }
    let lower = mix(
        mix(dcp_entry(base, dims, v0, h0, s0), dcp_entry(base, dims, v0, h1, s0), h_fract),
        mix(dcp_entry(base, dims, v0, h0, s0 + 1u), dcp_entry(base, dims, v0, h1, s0 + 1u), h_fract),
        s_fract);
    var shift = lower;
    if (dims.z > 1u) {
        let upper = mix(
            mix(dcp_entry(base, dims, v1, h0, s0), dcp_entry(base, dims, v1, h1, s0), h_fract),
            mix(dcp_entry(base, dims, v1, h0, s0 + 1u), dcp_entry(base, dims, v1, h1, s0 + 1u), h_fract),
            s_fract);
        shift = mix(lower, upper, v_fract);
    }
    let h = hsv.x + shift.x * (6.0 / 360.0);
    let s = min(hsv.y * shift.y, 1.0);
    var v = hsv.z * shift.z;
    if (srgb_value && hsv.z <= 1.0) {
        v = dcp_srgb_decode(clamp(v_encoded * shift.z, 0.0, 1.0));
    }
    return dcp_hsv_to_rgb(vec3<f32>(h, s, v));
}

// Camera profile before exposure: its matrix (from the decode's linear
// sRGB), then its HueSatMap on linear ProPhoto.
fn apply_camera_profile(c: vec3<f32>) -> vec3<f32> {
    var rgb = adjustments.global.camera_profile_matrix * c;
    if (adjustments.global.camera_profile.y != 0u && adjustments.global.camera_profile_hue_sat.x != 0u) {
        let pro = dcp_table(srgb_to_prophoto(rgb), adjustments.global.camera_profile_hue_sat, 0u);
        rgb = prophoto_to_srgb(pro);
    }
    return rgb;
}

fn camera_raw_tonemap(scene: vec3<f32>) -> vec3<f32> {
    let p = max(srgb_to_prophoto(scene), vec3<f32>(0.0));
    var ramped = vec3<f32>(camera_raw_black_ramp(p.r), camera_raw_black_ramp(p.g), camera_raw_black_ramp(p.b));
    // A camera profile's LookTable acts after exposure, before the curve.
    if (adjustments.global.camera_profile.y != 0u && adjustments.global.camera_profile_look.x != 0u) {
        ramped = dcp_table(ramped, adjustments.global.camera_profile_look, adjustments.global.camera_profile.z);
    }
    // RGB tone: the curve sets the largest and smallest channel; the middle
    // one keeps its relative position, which preserves hue. In the neutral
    // look the largest channel first rolls off above the knee towards a
    // limit just above white instead of clipping at it (Lightroom keeps
    // detail in over-exposed frames that clipped here), and the smallest
    // keeps the ratio Camera Raw's curve gives the two, which clips at
    // white: near-white colours converge on white as Camera Raw's do, and
    // bright saturated ones keep its saturation. Scaling every channel by
    // the roll-off of the largest kept near-white colours tinted (an α7R IV
    // dusk sky turned lavender).
    let hi = max(ramped.r, max(ramped.g, ramped.b));
    let lo = min(ramped.r, min(ramped.g, ramped.b));
    let roll = adjustments.global.profile_curve[15].w > 1.5 && hi > CR_ROLLOFF_KNEE;
    let hi_t = camera_raw_curve(select(hi, cr_rolloff(hi), roll));
    var lo_t = camera_raw_curve(lo);
    if (roll) {
        let scaled = camera_raw_curve(lo * cr_rolloff(hi) / hi);
        let converged = hi_t * lo_t / max(camera_raw_curve(hi), 1e-6);
        lo_t = mix(scaled, converged, CR_ROLLOFF_CONVERGE);
    }
    var toned = vec3<f32>(hi_t);
    if (hi > lo) {
        toned = lo_t + (ramped - vec3<f32>(lo)) * ((hi_t - lo_t) / (hi - lo));
    }
    var rgb = prophoto_to_srgb(toned);
    // The saturation lift and blue turn stood in for Adobe Standard's
    // tables on DJI files (flag 0; base_look no longer selects it). A file's own embedded curve (ProRAW,
    // flag 1) renders at its own saturation; neutral colour (flag 2, all
    // other RAWs) skips both.
    let flag = adjustments.global.profile_curve[15].w;
    let saturation = select(CAMERA_RAW_SATURATION, 1.0, flag > 0.5);
    rgb = mix(vec3<f32>(get_luma(rgb)), rgb, saturation);
    rgb = gamut_unit(rgb);
    var lab = linear_srgb_to_oklab(rgb);
    let chroma = length(lab.yz);
    // The neutral look's own colour rendition; a chosen camera profile
    // (flags 4, 5) brings its own.
    if (chroma > 1e-4 && flag > 1.5 && flag < 3.5) {
        lab = own_colour_look(lab, chroma);
        rgb = gamut_unit(oklab_to_linear_srgb(lab));
    }
    if (chroma > 1e-4 && flag < 1.5) {
        let hue = degrees(atan2(lab.z, lab.y));
        let distance = abs(((hue - CAMERA_RAW_BLUE_HUE) % 360.0 + 540.0) % 360.0 - 180.0);
        let weight = (1.0 - smoothstep(0.0, CAMERA_RAW_BLUE_REACH, distance)) * smoothstep(0.0, 0.03, chroma);
        let turn = radians(CAMERA_RAW_BLUE_TURN * weight);
        let c = cos(turn);
        let s = sin(turn);
        lab = vec3<f32>(lab.x, lab.y * c - lab.z * s, lab.y * s + lab.z * c);
        rgb = gamut_unit(oklab_to_linear_srgb(lab));
    }
    return linear_to_display(rgb);
}

fn agx_tonemap(c: vec3<f32>) -> vec3<f32> {
    let x_relative = max(c / 0.18, vec3<f32>(AGX_EPSILON));
    let log_encoded = (log2(x_relative) - AGX_MIN_EV) / AGX_RANGE_EV;
    let mapped = clamp(log_encoded, vec3<f32>(0.0), vec3<f32>(1.0));

    var curved: vec3<f32>;
    curved.r = agx_apply_curve_channel(mapped.r);
    curved.g = agx_apply_curve_channel(mapped.g);
    curved.b = agx_apply_curve_channel(mapped.b);

    let final_color = pow(max(curved, vec3<f32>(0.0)), vec3<f32>(AGX_GAMMA));

    return final_color;
}

fn agx_full_transform(color_in: vec3<f32>) -> vec3<f32> {
    let compressed_color = agx_compress_gamut(color_in);
    let color_in_agx_space = adjustments.global.agx_pipe_to_rendering_matrix * compressed_color;
    let tonemapped_agx = agx_tonemap(color_in_agx_space);
    let final_color = adjustments.global.agx_rendering_to_pipe_matrix * tonemapped_agx;
    return final_color;
}

fn legacy_tonemap(c: vec3<f32>) -> vec3<f32> {
    const a: f32 = 2.51;
    const b: f32 = 0.03;
    const c_const: f32 = 2.43;
    const d: f32 = 0.59;
    const e: f32 = 0.14;

    let x = max(c, vec3<f32>(0.0));

    let numerator = x * (a * x + b);
    let denominator = x * (c_const * x + d) + e;

    let tonemapped = select(vec3<f32>(0.0), numerator / denominator, denominator > vec3<f32>(0.00001));

    return clamp(tonemapped, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn no_tonemap(c: vec3<f32>) -> vec3<f32> {
    return c;
}

fn is_default_curve(points: array<Point, 16>, count: u32) -> bool {
    if (count < 2u) {
        return false;
    }

    var is_identity = true;
    for (var i = 0u; i < count; i = i + 1u) {
        if (abs(points[i].x - points[i].y) > 0.5) {
            is_identity = false;
            break;
        }
    }

    let p0 = points[0];
    let p_last = points[count - 1u];
    let p0_is_origin = abs(p0.x - 0.0) < 0.1 && abs(p0.y - 0.0) < 0.1;
    let p_last_is_end = abs(p_last.x - 255.0) < 0.1 && abs(p_last.y - 255.0) < 0.1;

    return is_identity && p0_is_origin && p_last_is_end;
}

fn apply_all_curves(
    color: vec3<f32>,
    luma_curve: array<Point, 16>, luma_curve_count: u32,
    red_curve: array<Point, 16>, red_curve_count: u32,
    green_curve: array<Point, 16>, green_curve_count: u32,
    blue_curve: array<Point, 16>, blue_curve_count: u32
) -> vec3<f32> {
    var r = apply_curve(color.r, luma_curve, luma_curve_count);
    var g = apply_curve(color.g, luma_curve, luma_curve_count);
    var b = apply_curve(color.b, luma_curve, luma_curve_count);

    if (!is_default_curve(red_curve, red_curve_count)) {
        r = apply_curve(r, red_curve, red_curve_count);
    }
    if (!is_default_curve(green_curve, green_curve_count)) {
        g = apply_curve(g, green_curve, green_curve_count);
    }
    if (!is_default_curve(blue_curve, blue_curve_count)) {
        b = apply_curve(b, blue_curve, blue_curve_count);
    }

    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

// Layer zero is global; the following layers are mask indices plus one. Read
// individual points so curve evaluation does not copy four 16-point arrays (or
// a complete MaskAdjustments value) into each pixel invocation's local memory.
fn curve_point(layer: u32, channel: u32, index: u32) -> Point {
    if (layer == 0u) {
        switch channel {
            case 0u: { return adjustments.global.luma_curve[index]; }
            case 1u: { return adjustments.global.red_curve[index]; }
            case 2u: { return adjustments.global.green_curve[index]; }
            default: { return adjustments.global.blue_curve[index]; }
        }
    }
    switch channel {
        case 0u: { return adjustments.mask_adjustments[layer - 1u].luma_curve[index]; }
        case 1u: { return adjustments.mask_adjustments[layer - 1u].red_curve[index]; }
        case 2u: { return adjustments.mask_adjustments[layer - 1u].green_curve[index]; }
        default: { return adjustments.mask_adjustments[layer - 1u].blue_curve[index]; }
    }
}

fn curve_count(layer: u32, channel: u32) -> u32 {
    if (layer == 0u) {
        switch channel {
            case 0u: { return adjustments.global.luma_curve_count; }
            case 1u: { return adjustments.global.red_curve_count; }
            case 2u: { return adjustments.global.green_curve_count; }
            default: { return adjustments.global.blue_curve_count; }
        }
    }
    switch channel {
        case 0u: { return adjustments.mask_adjustments[layer - 1u].luma_curve_count; }
        case 1u: { return adjustments.mask_adjustments[layer - 1u].red_curve_count; }
        case 2u: { return adjustments.mask_adjustments[layer - 1u].green_curve_count; }
        default: { return adjustments.mask_adjustments[layer - 1u].blue_curve_count; }
    }
}

fn apply_curve_direct(val: f32, layer: u32, channel: u32, count: u32) -> f32 {
    if (count < 2u) { return val; }
    let x = val * 255.0;
    let first = curve_point(layer, channel, 0u);
    let last = curve_point(layer, channel, count - 1u);
    if (x <= first.x) { return first.y / 255.0; }
    if (x >= last.x) { return last.y / 255.0; }
    for (var i = 0u; i < 15u; i = i + 1u) {
        if (i >= count - 1u) { break; }
        let p1 = curve_point(layer, channel, i);
        let p2 = curve_point(layer, channel, i + 1u);
        if (x <= p2.x) {
            let p0 = curve_point(layer, channel, max(1u, i) - 1u);
            let p3 = curve_point(layer, channel, min(count - 1u, i + 2u));
            let delta_before = (p1.y - p0.y) / max(0.001, p1.x - p0.x);
            let delta_current = (p2.y - p1.y) / max(0.001, p2.x - p1.x);
            let delta_after = (p3.y - p2.y) / max(0.001, p3.x - p2.x);
            var tangent_at_p1: f32;
            var tangent_at_p2: f32;
            if (i == 0u) { tangent_at_p1 = delta_current; } else {
                if (delta_before * delta_current <= 0.0) { tangent_at_p1 = 0.0; } else { tangent_at_p1 = (delta_before + delta_current) / 2.0; }
            }
            if (i + 1u == count - 1u) { tangent_at_p2 = delta_current; } else {
                if (delta_current * delta_after <= 0.0) { tangent_at_p2 = 0.0; } else { tangent_at_p2 = (delta_current + delta_after) / 2.0; }
            }
            if (delta_current != 0.0) {
                let alpha = tangent_at_p1 / delta_current;
                let beta = tangent_at_p2 / delta_current;
                if (alpha * alpha + beta * beta > 9.0) {
                    let tau = 3.0 / sqrt(alpha * alpha + beta * beta);
                    tangent_at_p1 = tangent_at_p1 * tau;
                    tangent_at_p2 = tangent_at_p2 * tau;
                }
            }
            let result_y = interpolate_cubic_hermite(x, p1, p2, tangent_at_p1, tangent_at_p2);
            return clamp(result_y / 255.0, 0.0, 1.0);
        }
    }
    return last.y / 255.0;
}

fn is_default_curve_direct(layer: u32, channel: u32, count: u32) -> bool {
    if (count < 2u) {
        return false;
    }
    var is_identity = true;
    for (var i = 0u; i < count; i = i + 1u) {
        let point = curve_point(layer, channel, i);
        if (abs(point.x - point.y) > 0.5) {
            is_identity = false;
            break;
        }
    }
    let p0 = curve_point(layer, channel, 0u);
    let p_last = curve_point(layer, channel, count - 1u);
    let p0_is_origin = abs(p0.x - 0.0) < 0.1 && abs(p0.y - 0.0) < 0.1;
    let p_last_is_end = abs(p_last.x - 255.0) < 0.1 && abs(p_last.y - 255.0) < 0.1;
    return is_identity && p0_is_origin && p_last_is_end;
}

fn apply_all_curves_direct(color: vec3<f32>, layer: u32) -> vec3<f32> {
    let luma_count = curve_count(layer, 0u);
    let red_count = curve_count(layer, 1u);
    let green_count = curve_count(layer, 2u);
    let blue_count = curve_count(layer, 3u);
    var r = apply_curve_direct(color.r, layer, 0u, luma_count);
    var g = apply_curve_direct(color.g, layer, 0u, luma_count);
    var b = apply_curve_direct(color.b, layer, 0u, luma_count);
    if (!is_default_curve_direct(layer, 1u, red_count)) {
        r = apply_curve_direct(r, layer, 1u, red_count);
    }
    if (!is_default_curve_direct(layer, 2u, green_count)) {
        g = apply_curve_direct(g, layer, 2u, green_count);
    }
    if (!is_default_curve_direct(layer, 3u, blue_count)) {
        b = apply_curve_direct(b, layer, 3u, blue_count);
    }
    return clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
}

// Endpoint and tangent work is invariant across pixels. Preserve the exact
// Hermite polynomial, including duplicate/unsorted knots and endpoint rules.
fn apply_curve_prepared(value: f32, layer: u32, channel: u32) -> f32 {
    let bypass = prepared_layers[layer].curves[channel].bypass;
    if (bypass == 1u) { return value; }
    if (bypass == 2u) { return clamp(value, 0.0, 1.0); }
    let count = prepared_layers[layer].curves[channel].count;
    let ends = prepared_layers[layer].curves[channel].endpoints;
    let x = value * 255.0;
    if (x <= ends.x) { return ends.y / 255.0; }
    if (x >= ends.z) { return ends.w / 255.0; }
    var start = 0u;
    if (count > 4u) {
        let bin = u32(clamp(value * 64.0, 0.0, 63.0));
        let packed_starts = prepared_layers[layer].curves[channel].segment_starts[bin / 4u];
        start = (packed_starts >> ((bin % 4u) * 8u)) & 255u;
    }
    for (var i = start; i + 1u < count; i += 1u) {
        let ends = prepared_layers[layer].curves[channel].segments[i].endpoints;
        if (x <= ends.z) {
            if (ends.z <= ends.x) { return clamp(ends.y / 255.0, 0.0, 1.0); }
            let coefficients = prepared_layers[layer].curves[channel].segments[i].coefficients;
            let t = (x - ends.x) / (ends.z - ends.x);
            let result = ((coefficients.x * t + coefficients.y) * t + coefficients.z) * t + coefficients.w;
            return clamp(result / 255.0, 0.0, 1.0);
        }
    }
    return ends.w / 255.0;
}
// The luma curve applies to all three channels. Locate its segments once and
// evaluate the channels together, sharing uniform storage reads across RGB.
fn apply_luma_prepared(color: vec3<f32>, layer: u32) -> vec3<f32> {
    let bypass = prepared_layers[layer].curves[0].bypass;
    if (bypass == 1u) { return color; }
    if (bypass == 2u) { return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)); }
    let count = prepared_layers[layer].curves[0].count;
    let endpoints = prepared_layers[layer].curves[0].endpoints;
    let x = color * 255.0;
    var result = select(vec3<f32>(endpoints.w / 255.0), vec3<f32>(endpoints.y / 255.0), x <= vec3<f32>(endpoints.x));
    var pending = (x > vec3<f32>(endpoints.x)) & (x < vec3<f32>(endpoints.z));
    var start = 0u;
    if (count > 4u) {
        let bin = u32(clamp(min(color.r, min(color.g, color.b)) * 64.0, 0.0, 63.0));
        let packed_starts = prepared_layers[layer].curves[0].segment_starts[bin / 4u];
        start = (packed_starts >> ((bin % 4u) * 8u)) & 255u;
    }
    for (var i = start; i + 1u < count && any(pending); i += 1u) {
        let ends = prepared_layers[layer].curves[0].segments[i].endpoints;
        let applies = pending & (x <= vec3<f32>(ends.z));
        if (any(applies)) {
            var curved = vec3<f32>(ends.y / 255.0);
            if (ends.z > ends.x) {
                let c = prepared_layers[layer].curves[0].segments[i].coefficients;
                let t = (x - ends.x) / (ends.z - ends.x);
                curved = (((c.x * t + c.y) * t + c.z) * t + c.w) / 255.0;
            }
            result = select(result, clamp(curved, vec3<f32>(0.0), vec3<f32>(1.0)), applies);
            pending = pending & !applies;
        }
    }
    return result;
}
fn apply_curves_prepared(color: vec3<f32>, layer: u32) -> vec3<f32> {
    let luma = apply_luma_prepared(color, layer);
    let r = luma.r;
    let g = luma.g;
    let b = luma.b;
    return clamp(vec3<f32>(apply_curve_prepared(r, layer, 1u), apply_curve_prepared(g, layer, 2u), apply_curve_prepared(b, layer, 3u)), vec3<f32>(0.0), vec3<f32>(1.0));
}

// Mask-layer curve inputs are already within [0, 1]; each channel reads its
// own sampled composite of the luma and channel curves.
fn apply_curve_lut(color: vec3<f32>, layer: u32) -> vec3<f32> {
    let position = clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)) * 1024.0;
    let index = min(vec3<u32>(position), vec3<u32>(1023u));
    let t = position - vec3<f32>(index);
    let base = layer * 1025u;
    let r = mix(curve_luts[base + index.x].x, curve_luts[base + index.x + 1u].x, t.x);
    let g = mix(curve_luts[base + index.y].y, curve_luts[base + index.y + 1u].y, t.y);
    let b = mix(curve_luts[base + index.z].z, curve_luts[base + index.z + 1u].z, t.z);
    return vec3<f32>(r, g, b);
}

fn get_mask_influence(mask_index: u32, coords: vec2<u32>) -> f32 {
    return textureLoad(mask_textures, vec2<i32>(coords), i32(mask_index), 0).r;
}

fn sample_lut_tetrahedral(uv: vec3<f32>) -> vec3<f32> {
    let dims = vec3<f32>(textureDimensions(lut_texture));
    let size = dims - vec3<f32>(1.0);
    let scaled = clamp(uv, vec3<f32>(0.0), vec3<f32>(1.0)) * size;
    let i_base = floor(scaled);
    let f = scaled - i_base;
    let coord0 = vec3<i32>(i_base);
    let coord1 = min(coord0 + vec3<i32>(1), vec3<i32>(dims) - vec3<i32>(1));
    let c000 = textureLoad(lut_texture, coord0, 0).rgb;
    let c111 = textureLoad(lut_texture, coord1, 0).rgb;

    var res = vec3<f32>(0.0);

    if (f.r > f.g) {
        if (f.g > f.b) {
            let c100 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord0.y, coord0.z), 0).rgb;
            let c110 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord1.y, coord0.z), 0).rgb;

            res = c000 * (1.0 - f.r) +
                  c100 * (f.r - f.g) +
                  c110 * (f.g - f.b) +
                  c111 * (f.b);
        } else if (f.r > f.b) {
            let c100 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord0.y, coord0.z), 0).rgb;
            let c101 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord0.y, coord1.z), 0).rgb;

            res = c000 * (1.0 - f.r) +
                  c100 * (f.r - f.b) +
                  c101 * (f.b - f.g) +
                  c111 * (f.g);
        } else {
            let c001 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord0.y, coord1.z), 0).rgb;
            let c101 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord0.y, coord1.z), 0).rgb;

            res = c000 * (1.0 - f.b) +
                  c001 * (f.b - f.r) +
                  c101 * (f.r - f.g) +
                  c111 * (f.g);
        }
    } else {
        if (f.b > f.g) {
            let c001 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord0.y, coord1.z), 0).rgb;
            let c011 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord1.y, coord1.z), 0).rgb;

            res = c000 * (1.0 - f.b) +
                  c001 * (f.b - f.g) +
                  c011 * (f.g - f.r) +
                  c111 * (f.r);
        } else if (f.b > f.r) {
            let c010 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord1.y, coord0.z), 0).rgb;
            let c011 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord1.y, coord1.z), 0).rgb;

            res = c000 * (1.0 - f.g) +
                  c010 * (f.g - f.b) +
                  c011 * (f.b - f.r) +
                  c111 * (f.r);
        } else {
            let c010 = textureLoad(lut_texture, vec3<i32>(coord0.x, coord1.y, coord0.z), 0).rgb;
            let c110 = textureLoad(lut_texture, vec3<i32>(coord1.x, coord1.y, coord0.z), 0).rgb;

            res = c000 * (1.0 - f.g) +
                  c010 * (f.g - f.r) +
                  c110 * (f.r - f.b) +
                  c111 * (f.b);
        }
    }

    return res;
}

fn apply_glow_bloom(
    color: vec3<f32>,
    blurred_color_input_space: vec3<f32>,
    amount: f32,
    is_raw: u32,
    exp: f32, bright: f32, con: f32, wh: f32
) -> vec3<f32> {
    if (amount <= 0.0) {
        return color;
    }

    var blurred_linear: vec3<f32>;
    if (is_raw == 1u) {
        blurred_linear = blurred_color_input_space;
    } else {
        blurred_linear = srgb_to_linear(blurred_color_input_space);
    }

    blurred_linear = apply_linear_exposure(blurred_linear, exp);
    blurred_linear = apply_filmic_exposure(blurred_linear, bright);
    blurred_linear = apply_tonal_adjustments(blurred_linear, blurred_color_input_space, is_raw, 0.0, 0.0, wh, 0.0, exp2(exp));

    let linear_luma = get_luma(max(blurred_linear, vec3<f32>(0.0)));

    var perceptual_luma: f32;
    if (linear_luma <= 1.0) {
        perceptual_luma = pow(max(linear_luma, 0.0), 1.0 / 2.2);
    } else {
        perceptual_luma = 1.0 + pow(linear_luma - 1.0, 1.0 / 2.2);
    }

    let luma_cutoff = mix(0.75, 0.08, clamp(amount, 0.0, 1.0));

    let cutoff_fade = smoothstep(
        luma_cutoff,
        luma_cutoff + 0.15,
        perceptual_luma
    );

    let excess = max(perceptual_luma - luma_cutoff, 0.0);

    let falloff_range = 5.5;
    let normalized = excess / falloff_range;

    let bloom_intensity =
        pow(smoothstep(0.0, 1.0, normalized), 0.45);

    var bloom_color: vec3<f32>;
    if (linear_luma > 0.01) {
        let color_ratio = blurred_linear / linear_luma;
        let warm_tint = vec3<f32>(1.03, 1.0, 0.97);
        bloom_color = color_ratio * warm_tint;
    } else {
        bloom_color = vec3<f32>(1.0, 0.99, 0.98);
    }

    let luma_factor = pow(linear_luma, 0.6);

    let black_gate_width = 0.5;
    let black_gate_raw = smoothstep(0.0, black_gate_width, linear_luma);
    let black_gate = pow(black_gate_raw, 0.5);

    bloom_color *= bloom_intensity * luma_factor * cutoff_fade * black_gate;

    let current_luma = get_luma(max(color, vec3<f32>(0.0)));
    let protection = 1.0 - smoothstep(1.0, 2.2, current_luma);

    return color + bloom_color * amount * 3.8 * protection;
}

// Projected light masks, evaluated in photograph coordinates for matching
// full-frame, tiled export and preview. Own analytic patterns, no bitmap assets.
fn relight_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    return length(p - a - ab * clamp(dot(p - a, ab) / dot(ab, ab), 0.0, 1.0));
}

fn relight_pattern(p: vec2<f32>, pattern: u32, softness: f32, pixel: f32) -> f32 {
    let edge = max(0.002 + softness * 0.22, pixel);
    let cell = abs(fract(p) - vec2<f32>(0.5));
    switch pattern {
        case 1u: { return 1.0 - smoothstep(0.23 - edge, 0.23 + edge, cell.y); }
        case 2u: {
            let box = 1.0 - smoothstep(0.47 - edge, 0.47 + edge, max(abs(p.x), abs(p.y)));
            let bars = smoothstep(0.025 - edge, 0.025 + edge, min(abs(p.x), abs(p.y)));
            return box * bars;
        }
        case 3u: { return 1.0 - smoothstep(0.15 - edge, 0.15 + edge, abs(p.y)); }
        case 4u: {
            // Smooth multiscale canopy shapes, deterministic at every resolution.
            let canopy = sin(p.x * 17.0 + sin(p.y * 11.0)) * sin(p.y * 19.0 - cos(p.x * 7.0))
                + 0.35 * sin(p.x * 39.0 + p.y * 29.0);
            return smoothstep(-0.12 - edge * 4.0, 0.12 + edge * 4.0, canopy);
        }
        case 5u: { return smoothstep(0.07 - edge, 0.07 + edge, min(cell.x, cell.y)); }
        case 6u: {
            // An original repeated frond silhouette: curved ribs and leaflets.
            let q = fract(p) - vec2<f32>(0.5);
            let rib = q.x - 0.22 * sin(q.y * 3.0);
            let stem = abs(rib);
            let leaflet = abs(fract(q.y * 7.0 + abs(rib) * 2.5) - 0.5);
            let canopy = (1.0 - smoothstep(0.32, 0.46, abs(rib)))
                * (1.0 - smoothstep(0.12 - edge, 0.12 + edge, leaflet));
            return 1.0 - max(canopy, 1.0 - smoothstep(0.015, 0.015 + edge, stem));
        }
        case 7u: {
            let q = fract(p) - vec2<f32>(0.5);
            var d = relight_segment(q, vec2<f32>(-0.1, 0.5), vec2<f32>(0.05, -0.5));
            for (var j = 0; j < 4; j++) {
                let y = -0.35 + f32(j) * 0.22;
                let side = select(-1.0, 1.0, j % 2 == 0);
                d = min(d, relight_segment(q, vec2<f32>(-0.1 * y, y + 0.15), vec2<f32>(side * 0.45, y - 0.2)));
            }
            return smoothstep(0.012, 0.028 + edge, d);
        }
        case 8u: {
            let a = smoothstep(0.25 - edge, 0.25 + edge, cell.x);
            let b = smoothstep(0.25 - edge, 0.25 + edge, cell.y);
            return a + b - 2.0 * a * b;
        }
        case 9u: { return 1.0 - smoothstep(0.22 - edge, 0.22 + edge, length(cell)); }
        case 10u: { return 1.0 - smoothstep(0.07 - edge, 0.07 + edge, cell.y); }
        case 11u: { return 1.0 - smoothstep(0.1 - edge, 0.1 + edge, abs(fract(p.x + p.y) - 0.5)); }
        case 12u: {
            let horizontal = smoothstep(0.12 - edge, 0.12 + edge, cell.y);
            let vertical = smoothstep(0.12 - edge, 0.12 + edge, cell.x);
            let over = step(0.5, fract((floor(p.x) + floor(p.y)) * 0.5));
            return mix(horizontal, vertical, over) * 0.8 + horizontal * vertical * 0.2;
        }
        case 13u: {
            let n = sin(p.x * 37.0 + sin(p.y * 43.0)) * sin(p.y * 31.0 - cos(p.x * 29.0));
            return smoothstep(-0.15 - edge * 3.0, 0.15 + edge * 3.0, n);
        }
        case 14u: {
            let a = abs(fract(p.x + p.y) - 0.5);
            let b = abs(fract(p.x - p.y) - 0.5);
            return smoothstep(0.07 - edge, 0.07 + edge, min(a, b));
        }
        default: { return 1.0; }
    }
}

// Visibility inside confident facial relief, with bounded work per light.
// Coarse scene silhouettes and missing/off-screen geometry are too uncertain
// for cast shadows. Fine bounded steps cannot jump over narrow nose/eye relief.
// Blended mesh boundaries are not occluders: their height includes coarse scene
// depth and would invent a raised rim around the face.
fn relight_visibility(uv: vec2<f32>, height: f32, direction: vec3<f32>, reach: f32, height_scale: f32, softness: f32, dims: vec2<f32>) -> f32 {
    let receiver = textureSampleLevel(relight_depth_texture, flare_sampler, uv, 0.0).a;
    if (receiver < 0.99) { return 1.0; }
    let resolution = vec2<f32>(textureDimensions(relight_depth_texture));
    let max_step = min(2.0 / min(resolution.x, resolution.y), 1.0 / 512.0);
    var step = min(0.75 / min(resolution.x, resolution.y), max_step);
    var travel = 1.25 / min(resolution.x, resolution.y);
    var visibility = 1.0;
    for (var sample = 0u; sample < 256u; sample++) {
        if (travel >= reach) { break; }
        let q = uv + direction.xy * travel * min(dims.x, dims.y) / dims;
        if (any(q <= vec2<f32>(0.0)) || any(q >= vec2<f32>(1.0))) { break; }
        let surface = textureSampleLevel(relight_depth_texture, flare_sampler, q, 0.0);
        if (surface.a < 0.995) { break; }
        let occluder = surface.z * height_scale;
        let clearance = height + direction.z * travel - occluder;
        let penumbra = 0.0008 + softness * travel * 0.18;
        visibility = min(visibility, smoothstep(-penumbra, penumbra, clearance + 0.001));
        if (visibility <= 0.0) { break; }
        step = min(step * 1.03, max_step);
        travel += step;
    }
    return mix(1.0, visibility, smoothstep(0.99, 1.0, receiver));
}

// Preserve inverse depth through storage and interpolation. Half-float texels
// shift projected patterns even when interpolated in f32. The unfilterable
// f32 camera texture uses explicit loads; legacy filtering stays unchanged.
fn relight_camera_sample(uv: vec2<f32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(relight_camera_texture));
    let pixel = clamp(uv * vec2<f32>(size) - vec2<f32>(0.5), vec2<f32>(0.0), vec2<f32>(size - vec2<i32>(1)));
    let a = vec2<i32>(floor(pixel));
    let b = min(a + vec2<i32>(1), size - vec2<i32>(1));
    let fraction = fract(pixel);
    let top = mix(textureLoad(relight_camera_texture, a, 0), textureLoad(relight_camera_texture, vec2<i32>(b.x, a.y), 0), fraction.x);
    let bottom = mix(textureLoad(relight_camera_texture, vec2<i32>(a.x, b.y), 0), textureLoad(relight_camera_texture, b, 0), fraction.x);
    return mix(top, bottom, fraction.y);
}

// Version-four geometry uses inverse depth and normalized camera intrinsics.
fn relight_position(uv: vec2<f32>, inverse: f32, strength: f32) -> vec3<f32> {
    let c = adjustments.global.relight_camera;
    let g = adjustments.global.relight_geometry;
    let distance = g.x + (1.0 / max(inverse, 0.000001) - g.x) * strength;
    return vec3<f32>((uv - c.zw) * distance / c.xy, g.x - distance) / g.y;
}

fn relight_source(uv: vec2<f32>, distance: f32, strength: f32) -> vec3<f32> {
    let c = adjustments.global.relight_camera;
    let g = adjustments.global.relight_geometry;
    return vec3<f32>((uv - c.zw) * g.x / c.xy / g.y, (g.x - g.z) * strength / g.y + distance);
}

fn relight_camera_normal(uv: vec2<f32>, surface: vec4<f32>, strength: f32) -> vec3<f32> {
    let c = adjustments.global.relight_camera;
    let original_distance = 1.0 / max(surface.z, 0.000001);
    let distance = adjustments.global.relight_geometry.x + (original_distance - adjustments.global.relight_geometry.x) * strength;
    let derivative = surface.xy * strength * original_distance * original_distance / (distance * distance);
    return normalize(vec3<f32>(-c.xy * derivative, 1.0 / distance - dot(derivative, uv - c.zw)));
}

fn relight_camera_visibility(uv: vec2<f32>, origin: vec3<f32>, direction: vec3<f32>, reach: f32, strength: f32, softness: f32) -> f32 {
    let receiver = relight_camera_sample(uv).a;
    if (receiver < 0.99) { return 1.0; }
    let c = adjustments.global.relight_camera;
    let g = adjustments.global.relight_geometry;
    let resolution = vec2<f32>(textureDimensions(relight_camera_texture));
    let depth_ratio = (g.x - origin.z * g.y) / g.x;
    var travel = 1.25 * depth_ratio / min(resolution.x, resolution.y);
    var step = min(0.75 / min(resolution.x, resolution.y), 1.0 / 512.0) * depth_ratio;
    var visibility = 1.0;
    for (var index = 0u; index < 256u; index++) {
        if (travel >= reach) { break; }
        let ray = origin + direction * travel;
        let distance = g.x - ray.z * g.y;
        if (distance <= g.x * 0.0001) { break; }
        let q = ray.xy * g.y * c.xy / distance + c.zw;
        if (any(q <= vec2<f32>(0.0)) || any(q >= vec2<f32>(1.0))) { break; }
        let surface = relight_camera_sample(q);
        if (surface.a < 0.995) { break; }
        let occluder = relight_position(q, surface.z, strength).z;
        let penumbra = 0.0008 + softness * travel * 0.18;
        visibility = min(visibility, smoothstep(-penumbra, penumbra, ray.z - occluder + 0.001));
        if (visibility <= 0.0) { break; }
        let max_step = min(2.0 / min(resolution.x, resolution.y), 1.0 / 512.0) * distance / g.x;
        step = min(step * 1.03, max_step);
        travel += step;
    }
    return mix(1.0, visibility, smoothstep(0.99, 1.0, receiver));
}

// Dielectric skin surface: GGX distribution, separable Smith masking and
// Schlick Fresnel (IOR 1.4 -> F0=1/36). This models an added light's reflection;
// it does not estimate or remove the photograph's original specular highlights.
// Return diffuse energy fraction and reflected radiance for unit illumination.
fn relight_skin(normal: vec3<f32>, light: vec3<f32>, view: vec3<f32>, softness: f32, amount: f32) -> vec2<f32> {
    let nl = max(dot(normal, light), 0.0);
    let nv = max(dot(normal, view), 0.0);
    if (amount <= 0.0 || nl <= 0.0 || nv <= 0.0) { return vec2<f32>(1.0, 0.0); }
    let half_vector = normalize(light + view);
    let nh = max(dot(normal, half_vector), 0.0);
    let vh = clamp(dot(half_vector, view), 0.0, 1.0);
    // Broaden the lobe for softer sources; the skin roughness is a modest
    // default, not a recovered material parameter.
    let roughness = 0.5 + 0.25 * softness;
    let alpha2 = roughness * roughness * roughness * roughness;
    let denominator = nh * nh * (alpha2 - 1.0) + 1.0;
    let distribution = alpha2 / (3.14159265 * denominator * denominator);
    let masking_v = 2.0 * nv / (nv + sqrt(alpha2 + (1.0 - alpha2) * nv * nv));
    let masking_l = 2.0 * nl / (nl + sqrt(alpha2 + (1.0 - alpha2) * nl * nl));
    let fresnel = (1.0 / 36.0 + (35.0 / 36.0) * pow(1.0 - vh, 5.0)) * amount;
    // The diffuse lighting convention absorbs 1/pi; use the same incident
    // energy here. N.L cancels the microfacet BRDF's denominator.
    let reflected = 3.14159265 * fresnel * distribution * masking_v * masking_l / (4.0 * nv);
    return vec2<f32>(1.0 - fresnel, reflected);
}

struct RelightEnergy {
    diffuse: vec3<f32>,
    reflected: vec3<f32>,
    shadow_ev: f32,
    has_light: bool,
}

fn relight_energy(surface: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, scene_only: bool) -> RelightEnergy {
    var normal = vec3<f32>(0.0, 0.0, 1.0);
    let camera_mode = scene_only || adjustments.global.relight_geometry.w > 0.5;
    let strength = adjustments.global.relight_surface.x;
    let has_depth = scene_only || any(adjustments.global.relight_depth.xy != vec2<u32>(0u)) && (strength > 0.0 || camera_mode);
    let camera_lighting = has_depth && camera_mode;
    var world_position = vec3<f32>(0.0);
    var view = vec3<f32>(0.0, 0.0, 1.0);
    let surface_lighting = scene_only || has_depth && adjustments.global.relight_surface.z > 0.5;
    var depth_value = 0.0;
    var skin = 0.0;
    // Height and its derivatives must use the same physical scale.
    let height_scale = adjustments.global.relight_surface.x * 0.25;
    if (has_depth) {
        let slope = surface.xy * min(dims.x, dims.y) / dims;
        depth_value = surface.z;
        if (surface_lighting && !scene_only) { skin = smoothstep(0.5, 1.0, surface.a) * adjustments.global.relight_surface.w; }
        normal = normalize(vec3<f32>(-slope * height_scale, 1.0));
        if (camera_lighting) {
            world_position = relight_position(uv, surface.z, strength);
            normal = relight_camera_normal(uv, surface, strength);
            view = normalize(vec3<f32>(-(uv - adjustments.global.relight_camera.zw) / adjustments.global.relight_camera.xy, 1.0));
        }
    }
    var light_gain = vec3<f32>(0.0);
    var reflected_light = vec3<f32>(0.0);
    var shadow_ev = 0.0;
    var has_light = false;
    for (var i = 0u; i < 4u; i++) {
        let color = adjustments.global.relight[i * 5u];
        if (color.w <= 0.0) { continue; }
        has_light = true;
        let placement = adjustments.global.relight[i * 5u + 1u];
        let shape = adjustments.global.relight[i * 5u + 2u];
        let settings = adjustments.global.relight[i * 5u + 3u];
        let controls = adjustments.global.relight[i * 5u + 4u];
        let source_delta = (uv - controls.xy) * dims / min(dims.x, dims.y);
        let delta = (uv - placement.xy) * dims / min(dims.x, dims.y);
        let c = cos(shape.x);
        let s = sin(shape.x);
        // Project onto the inferred height field, so gobo stripes bend over
        // faces/objects instead of remaining a flat overlay. No extra samples.
        var projected = delta;
        if (has_depth) {
            let height = depth_value * height_scale;
            let travel = min(cos(settings.w) / max(sin(settings.w), 0.15), 3.0);
            projected += vec2<f32>(-s, c) * height * travel;
            if (surface_lighting) {
                projected = delta - vec2<f32>(-s, c) * height * travel;
                if (placement.w > 0.5) {
                    let source_z = height_scale + controls.w;
                    projected = delta + source_delta * height / max(source_z - height, 0.05);
                }
            }
        }
        if (camera_lighting) {
            let c_camera = adjustments.global.relight_camera;
            let g_camera = adjustments.global.relight_geometry;
            let pattern_center = (placement.xy - c_camera.zw) * g_camera.x / c_camera.xy / g_camera.y;
            let sun_direction = vec3<f32>(-s * cos(settings.w), c * cos(settings.w), sin(settings.w));
            projected = world_position.xy - sun_direction.xy * world_position.z / max(sun_direction.z, 0.0001) - pattern_center;
            if (placement.w > 0.5) {
                let source = relight_source(controls.xy, controls.w, strength);
                projected = world_position.xy + (world_position.xy - source.xy) * world_position.z / max(source.z - world_position.z, 0.0001) - pattern_center;
            }
        }
        let pc = cos(controls.z);
        let ps = sin(controls.z);
        let p = vec2<f32>(pc * projected.x + ps * projected.y, -ps * projected.x + pc * projected.y) / shape.y;
        let pixel = 1.0 / (min(dims.x, dims.y) * shape.y);
        var mask = relight_pattern(p, u32(settings.x), shape.z, pixel);
        if (settings.y > 0.5) { mask = 1.0 - mask; }
        let radius = length(source_delta) / placement.z;
        let spot = 1.0 - smoothstep(max(0.0, 0.95 - shape.z * 0.9), 1.0, radius);
        var footprint = select(1.0, spot, placement.w > 0.5);
        if (camera_lighting && placement.w > 0.5) {
            // Cone radius is specified at the reference plane. Its physical
            // footprint expands with distance from the source.
            let source = relight_source(controls.xy, controls.w, strength);
            let gap = source.z - world_position.z;
            let cone_radius = length(world_position.xy - source.xy) * source.z / (max(gap, 0.0001) * placement.z);
            footprint = (1.0 - smoothstep(max(0.0, 0.95 - shape.z * 0.9), 1.0, cone_radius)) * select(0.0, 1.0, gap > 0.0);
        }
        var response = 1.0;
        var attenuation = 1.0;
        var specular = 0.0;
        if (has_depth) {
            let sun_direction = vec3<f32>(-s * cos(settings.w), c * cos(settings.w), sin(settings.w));
            let spot_direction = normalize(vec3<f32>(-source_delta, sin(settings.w) + depth_value * 0.4));
            var direction = select(sun_direction, spot_direction, placement.w > 0.5);
            response = mix(max(dot(normal, direction), 0.0), 1.0, shape.w);
            if (surface_lighting) {
                var reach = 2.0;
                if (placement.w > 0.5) {
                    // Scene height grows towards the camera. A foreground
                    // surface is closer to the source, not farther from it.
                    let source_z = height_scale + controls.w;
                    let to_light = vec3<f32>(-source_delta, source_z - depth_value * height_scale);
                    let distance2 = max(dot(to_light, to_light), 0.0025);
                    reach = sqrt(distance2);
                    direction = to_light / reach;
                    // Intensity is referenced to one short-side unit. A
                    // finite emitter radius avoids a singular near-field gain.
                    attenuation = 0.36 / (distance2 + 0.01);
                }
                if (camera_lighting && placement.w > 0.5) {
                    let to_light = relight_source(controls.xy, controls.w, strength) - world_position;
                    let distance2 = max(dot(to_light, to_light), 0.0025);
                    reach = sqrt(distance2);
                    direction = to_light / reach;
                    attenuation = 0.36 / (distance2 + 0.01);
                }
                let wrap = shape.w * 0.2;
                response = max(dot(normal, direction) + wrap, 0.0) / ((1.0 + wrap) * (1.0 + wrap));
                let material = relight_skin(normal, direction, view, shape.z, skin);
                response *= material.x;
                specular = material.y;
                if (!scene_only && response > 0.0 && settings.z > 0.0) {
                    var visibility = 1.0;
                    if (camera_lighting) {
                        visibility = relight_camera_visibility(uv, world_position, direction, reach, strength, shape.z);
                    } else {
                        visibility = relight_visibility(uv, depth_value * height_scale, direction, reach, height_scale, shape.z, dims);
                    }
                    let shadow = mix(1.0, mix(visibility, 1.0, shape.w * 0.08), settings.z);
                    response *= shadow;
                    specular *= shadow;
                }
            }
        }
        let transmission = select(mix(mask, 1.0, shape.w), mix(1.0, mask, settings.z), surface_lighting);
        let illumination = transmission * footprint * response * attenuation;
        light_gain += color.rgb * color.w * illumination;
        reflected_light += color.rgb * color.w * transmission * footprint * attenuation * specular;
        if (!surface_lighting) {
            shadow_ev += (1.0 - mask) * footprint * settings.z * color.w * (1.0 - shape.w);
        }
    }
    return RelightEnergy(light_gain, reflected_light, shadow_ev, has_light);
}

fn apply_relight(rgb: vec3<f32>, uv: vec2<f32>, dims: vec2<f32>, exposure_gain: f32) -> vec3<f32> {
    let camera_mode = adjustments.global.relight_geometry.w > 0.5;
    let strength = adjustments.global.relight_surface.x;
    let has_depth = any(adjustments.global.relight_depth.xy != vec2<u32>(0u)) && (strength > 0.0 || camera_mode);
    let surface_lighting = has_depth && adjustments.global.relight_surface.z > 0.5;
    var surface = vec4<f32>(0.0);
    if (has_depth) {
        if (camera_mode) { surface = relight_camera_sample(uv); }
        else { surface = textureSampleLevel(relight_depth_texture, flare_sampler, uv, 0.0); }
    }
    var confidence = 0.0;
    var planes: array<vec4<f32>, 4>;
    var weights: array<f32, 4>;
    if (camera_mode && has_depth && strength > 0.0 && surface.a < 0.2) {
        // Tangent planes coincide on a continuous planar surface. Where they
        // disagree, interpolate incident light, never an invented wall joining
        // disconnected depths. Use the original scene grid, before face-atlas
        // upsampling can blend those depths irreversibly.
        let size = vec2<i32>(textureDimensions(relight_scene_texture));
        let pixel = clamp(uv * vec2<f32>(size) - vec2<f32>(0.5), vec2<f32>(0.0), vec2<f32>(size - vec2<i32>(1)));
        let a = vec2<i32>(floor(pixel));
        let b = min(a + vec2<i32>(1), size - vec2<i32>(1));
        let f = fract(pixel);
        let coords = array<vec2<i32>, 4>(a, vec2<i32>(b.x, a.y), vec2<i32>(a.x, b.y), b);
        weights = array<f32, 4>((1.0-f.x)*(1.0-f.y), f.x*(1.0-f.y), (1.0-f.x)*f.y, f.x*f.y);
        var scene_surface = vec4<f32>(0.0);
        var low = 1e10;
        var high = 0.0;
        for (var j = 0u; j < 4u; j++) {
            let tap = textureLoad(relight_scene_texture, coords[j], 0);
            scene_surface += weights[j] * tap;
            let offset = uv - (vec2<f32>(coords[j]) + vec2<f32>(0.5)) / vec2<f32>(size);
            let inverse = max(tap.z + dot(tap.xy, offset), tap.z * 0.1);
            planes[j] = vec4<f32>(tap.xy, inverse, 0.0);
            low = min(low, inverse);
            high = max(high, inverse);
        }
        confidence = smoothstep(0.005, 0.025, (high-low) / max(surface.z, 0.000001)) * (1.0 - smoothstep(0.0, 0.2, surface.a));
        let face_weight = smoothstep(0.0, 0.2, surface.a);
        surface = vec4<f32>(mix(scene_surface.xyz, surface.xyz, face_weight), surface.a);
    }
    let energy = relight_energy(surface, uv, dims, false);
    var light_gain = energy.diffuse;
    // Incident radiance must receive the same exposure as the diffuse photo.
    // Otherwise reducing exposure leaves newly added dielectric highlights fixed.
    let reflected_light = energy.reflected * exposure_gain;
    let shadow_ev = energy.shadow_ev;
    let has_light = energy.has_light;
    if (confidence > 0.0 && has_light) {
        var separated = vec3<f32>(0.0);
        for (var j = 0u; j < 4u; j++) {
            separated += weights[j] * relight_energy(planes[j], uv, dims, true).diffuse;
        }
        light_gain = mix(light_gain, separated, confidence);
    }
    // Multiplicative illumination preserves photographic texture and black;
    // accumulate lights commutatively so reordering does not change the result.
    let original = select(1.0, adjustments.global.relight_surface.y, has_light);
    let gain = (vec3<f32>(original) + light_gain) * exp2(-shadow_ev);
    if (adjustments.global.is_raw_image == 0u && has_light) {
        // Already-rendered photos have no scene highlight headroom. Roll off
        // added light continuously, preserving texture instead of clipping it.
        let base = rgb * min(gain, vec3<f32>(1.0));
        let extra = rgb * max(gain - vec3<f32>(1.0), vec3<f32>(0.0)) + reflected_light;
        let room = max(vec3<f32>(1.0) - base, vec3<f32>(0.00001));
        if (surface_lighting) {
            // One scale for the added RGB energy: per-channel rolloff would
            // change the light's hue as intensity increases.
            let relative = extra / room;
            let peak = max(relative.x, max(relative.y, relative.z));
            let scale = select(1.0, (1.0 - exp(-peak)) / max(peak, 0.00001), peak > 0.00001);
            return base + extra * scale;
        }
        return base + room * (vec3<f32>(1.0) - exp(-extra / room));
    }
    return rgb * gain + reflected_light;
}

fn apply_halation(
    color: vec3<f32>,
    blurred_color_input_space: vec3<f32>,
    amount: f32,
    is_raw: u32,
    exp: f32, bright: f32, con: f32, wh: f32
) -> vec3<f32> {
    if (amount <= 0.0) { return color; }

    var blurred_linear: vec3<f32>;
    if (is_raw == 1u) {
        blurred_linear = blurred_color_input_space;
    } else {
        blurred_linear = srgb_to_linear(blurred_color_input_space);
    }

    blurred_linear = apply_linear_exposure(blurred_linear, exp);
    blurred_linear = apply_filmic_exposure(blurred_linear, bright);
    blurred_linear = apply_tonal_adjustments(blurred_linear, blurred_color_input_space, is_raw, 0.0, 0.0, wh, 0.0, exp2(exp));

    let linear_luma = get_luma(max(blurred_linear, vec3<f32>(0.0)));

    var perceptual_luma: f32;
    if (linear_luma <= 1.0) {
        perceptual_luma = pow(max(linear_luma, 0.0), 1.0 / 2.2);
    } else {
        perceptual_luma = 1.0 + pow(linear_luma - 1.0, 1.0 / 2.2);
    }

    let luma_cutoff = mix(0.85, 0.1, clamp(amount, 0.0, 1.0));

    if (perceptual_luma <= luma_cutoff) { return color; }

    let excess = perceptual_luma - luma_cutoff;
    let range = max(1.5 - luma_cutoff, 0.1);
    let halation_mask = smoothstep(0.0, range * 0.6, excess);

    let halation_core = vec3<f32>(1.0, 0.15, 0.03);
    let halation_fringe = vec3<f32>(1.0, 0.32, 0.10);

    let intensity_blend = smoothstep(0.0, 0.7, halation_mask);
    let halation_tint = mix(halation_fringe, halation_core, intensity_blend);

    let glow_intensity = halation_mask * linear_luma;
    let halation_glow = halation_tint * glow_intensity;

    let color_luma = get_luma(max(color, vec3<f32>(0.0)));
    let desat_strength = halation_mask * 0.12;
    let affected_color = mix(color, vec3<f32>(color_luma), desat_strength);

    let contrast_reduced = mix(vec3<f32>(0.5), affected_color, 1.0 - halation_mask * 0.06);

    return contrast_reduced + halation_glow * amount * 2.5;
}

/// Noise reduction, then sharpening, of the input at one pixel.
// Moiré (Capture One's tool; Lightroom's is a brush setting): colour moiré
// is false colour whose hue swings around neutral from one pattern period
// to the next. On a 5 × 5 grid of the input (step from Pattern), the mean
// relative chroma m and its energy e give the swing 1 − |m| / sqrt(e): about
// 1 for moiré, at most 0.5 for colour edges and thin coloured lines and 0
// for even colour. Where it swings, the chroma moves towards the mean;
// lightness is kept (notes: reference/moire.md).
fn moire_chroma(c: vec3<f32>) -> vec3<f32> {
    let l = max(get_luma(c), 1e-4);
    return c / l - vec3<f32>(1.0);
}

fn apply_moire(color: vec3<f32>, coords_i: vec2<i32>, amount: f32, pattern_step: f32) -> vec3<f32> {
    let detail_scale = select(1.0, adjustments.global.detail_scale, adjustments.global.detail_scale > 0.0);
    let step = max(pattern_step * detail_scale, 0.5);
    let dims = vec2<i32>(textureDimensions(input_texture)) - vec2<i32>(1);
    var mean = vec3<f32>(0.0);
    var energy = 0.0;
    var weight = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let offset = vec2<f32>(f32(dx), f32(dy)) * step;
            let p = clamp(coords_i + vec2<i32>(round(offset)), vec2<i32>(0), dims);
            let q = moire_chroma(max(textureLoad(input_texture, vec2<u32>(p), 0).rgb, vec3<f32>(0.0)));
            let w = exp(-f32(dx * dx + dy * dy) / 4.5);
            mean += q * w;
            energy += dot(q, q) * w;
            weight += w;
        }
    }
    mean /= weight;
    energy /= weight;
    if (energy < 1e-6) {
        return color;
    }
    let swing = 1.0 - length(mean) / sqrt(energy);
    let w = amount * smoothstep(MOIRE_SWING.x, MOIRE_SWING.y, swing);
    if (w <= 0.0) {
        return color;
    }
    let floored = gamut_floor(color);
    let l = get_luma(max(color, vec3<f32>(0.0)));
    let q = moire_chroma(floored);
    return gamut_floor(vec3<f32>(l) * (vec3<f32>(1.0) + mix(q, mean, w)));
}

const MOIRE_SWING: vec2<f32> = vec2<f32>(0.45, 0.8);

// Diffraction correction (diffraction.rs): an unsharp mask of perceptual
// luminance whose radius (original pixels, `sharpen_falloff.y`) and amount
// (`.z`) follow the Airy disk of the shot's f-number and pixel pitch.
fn apply_diffraction_correction(color: vec3<f32>, coords_i: vec2<i32>, is_raw: u32) -> vec3<f32> {
    let detail_scale = select(1.0, adjustments.global.detail_scale, adjustments.global.detail_scale > 0.0);
    let sigma = adjustments.global.sharpen_falloff.y * detail_scale;
    if (sigma < 0.3) {
        return color;
    }
    let reach = i32(min(ceil(2.0 * sigma), 3.0));
    let dims = vec2<i32>(textureDimensions(input_texture)) - vec2<i32>(1);
    var sum = 0.0;
    var weight = 0.0;
    for (var sy = -reach; sy <= reach; sy = sy + 1) {
        for (var sx = -reach; sx <= reach; sx = sx + 1) {
            let w = exp(-f32(sx * sx + sy * sy) / (2.0 * sigma * sigma));
            let c = clamp(coords_i + vec2<i32>(sx, sy), vec2<i32>(0), dims);
            sum += sharpen_perc(textureLoad(input_texture, vec2<u32>(c), 0).rgb, is_raw) * w;
            weight += w;
        }
    }
    let l = sharpen_perc(color, is_raw);
    if (l <= 1e-4) {
        return color;
    }
    // As the sharpening: with noise reduction on, the detail comes from
    // the input and keeps only its share above the noise.
    let detail = sharpen_detail(l, sum / weight, is_raw);
    let l_sharp = max(l + adjustments.global.sharpen_falloff.z * detail, l * 0.5);
    // Like the sharpening: deep shadows and near-white tones are left
    // alone, so noise is not lifted and highlights are not pushed to clip.
    let floor = select(0.03, 0.10, is_raw == 1u);
    let protect = smoothstep(0.0, floor, l) * (1.0 - smoothstep(0.92, 1.0, l));
    let ratio = mix(l, l_sharp, protect) / l;
    if (is_raw == 1u) {
        return color * (ratio * ratio);
    }
    return color * ratio;
}

fn detail_stage(
    initial_linear_rgb: vec3<f32>,
    tile_coord: vec2<u32>,
    absolute_coord_i: vec2<i32>,
    luma_nr: f32,
    color_nr: f32,
    sharpness: f32,
    sharp_thresh: f32,
    scale: f32,
    is_raw: u32,
    moire: vec2<f32>
) -> vec3<f32> {
    var rgb = initial_linear_rgb;
    if (ENABLE_NOISE_REDUCTION) {
        rgb = apply_noise_reduction(rgb, absolute_coord_i, luma_nr, color_nr, scale, is_raw);
    }
    if (moire.x > 0.0) {
        rgb = apply_moire(rgb, absolute_coord_i, moire.x, moire.y);
    }
    if (ENABLE_SHARPEN) {
        let sharpness_blurred = textureLoad(sharpness_blur_texture, tile_coord, 0).rgb;
        let tonal_blurred = textureLoad(tonal_blur_texture, tile_coord, 0).rgb;
        var amount = sharpness;
        let falloff = adjustments.global.sharpen_falloff.x;
        if (falloff > 0.0) {
            // Capture One's Sharpness Falloff: more sharpening towards the
            // corners, where lenses are softer; fitted to its frequency gain
            // (notes: reference/diffraction.md): quadratic in the slider,
            // ×1.44 at a corner at 100.
            let dims = vec2<f32>(textureDimensions(input_texture));
            let uv = (vec2<f32>(absolute_coord_i) / dims - 0.5) * 2.0;
            amount *= 1.0 + FALLOFF_GAIN * falloff * falloff * dot(uv, uv);
        }
        rgb = apply_sharpen(rgb, sharpness_blurred, tonal_blurred, absolute_coord_i, amount, sharp_thresh, is_raw);
    }
    if (adjustments.global.sharpen_falloff.z > 0.0) {
        rgb = apply_diffraction_correction(rgb, absolute_coord_i, is_raw);
    }
    return rgb;
}

/// The input at one pixel, chromatic aberration corrected, in linear light.
fn initial_linear_at(absolute_coord: vec2<u32>, is_raw: u32) -> vec3<f32> {
    let ca_rc = adjustments.global.chromatic_aberration_red_cyan;
    let ca_by = adjustments.global.chromatic_aberration_blue_yellow;
    var color_from_texture = textureLoad(input_texture, absolute_coord, 0).rgb;
    if (abs(ca_rc) > 0.000001 || abs(ca_by) > 0.000001) {
        color_from_texture = apply_ca_correction(absolute_coord, ca_rc, ca_by);
    }
    if (is_raw == 0u) {
        return srgb_to_linear(color_from_texture);
    }
    return color_from_texture;
}

/// Fills the detail cache for the render region at the global amounts.
@compute @workgroup_size(8, 8, 1)
fn detail_main(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if (invocation.x >= adjustments.render_region.z || invocation.y >= adjustments.render_region.w) { return; }
    let id = invocation.xy + adjustments.render_region.xy;
    let out_dims = vec2<u32>(textureDimensions(detail_output));
    if (id.x >= out_dims.x || id.y >= out_dims.y) { return; }

    const REFERENCE_DIMENSION: f32 = 1080.0;
    let full_dims = vec2<f32>(textureDimensions(input_texture));
    let scale = max(0.1, min(full_dims.x, full_dims.y) / REFERENCE_DIMENSION);
    let absolute_coord = id + vec2<u32>(adjustments.tile_offset_x, adjustments.tile_offset_y);
    let is_raw = adjustments.global.is_raw_image;
    let g = adjustments.global;
    let rgb = detail_stage(
        initial_linear_at(absolute_coord, is_raw), id, vec2<i32>(absolute_coord),
        g.luma_noise_reduction, g.color_noise_reduction, g.sharpness, g.sharpness_threshold,
        scale, is_raw, g.moire.xy
    );
    textureStore(detail_output, id, vec4<f32>(rgb, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn main(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>
) {
    // Find this workgroup's region; the dispatch rounds the grid up.
    var flat = group.x + group.y * groups.x;
    var region = vec4<u32>(0u);
    var found = false;
    let region_count = render_regions[0].x;
    for (var r = 1u; r <= region_count; r = r + 1u) {
        let candidate = render_regions[r];
        let count = ((candidate.z + 7u) / 8u) * ((candidate.w + 7u) / 8u);
        if (flat < count) {
            region = candidate;
            found = true;
            break;
        }
        flat = flat - count;
    }
    if (!found) { return; }
    let columns = (region.z + 7u) / 8u;
    let invocation = vec2<u32>((flat % columns) * 8u + local.x, (flat / columns) * 8u + local.y);
    if (invocation.x >= region.z || invocation.y >= region.w) { return; }
    // Blurs still include the surrounding halo. Only evaluate the expensive
    // adjustment stack for retained pixels; keep its original tile coordinates.
    let id = vec3<u32>(invocation + region.xy, 0u);
    let out_dims = vec2<u32>(textureDimensions(output_texture));
    if (id.x >= out_dims.x || id.y >= out_dims.y) { return; }

    const REFERENCE_DIMENSION: f32 = 1080.0;
    let full_dims = vec2<f32>(textureDimensions(input_texture));
    let current_ref_dim = min(full_dims.x, full_dims.y);
    let scale = max(0.1, current_ref_dim / REFERENCE_DIMENSION);

    let absolute_coord = id.xy + vec2<u32>(adjustments.tile_offset_x, adjustments.tile_offset_y);
    let absolute_coord_i = vec2<i32>(absolute_coord);

    let original_alpha = textureLoad(input_texture, absolute_coord, 0).a;
    let is_raw = adjustments.global.is_raw_image;
    let initial_linear_rgb = initial_linear_at(absolute_coord, is_raw);

    var t_exposure = adjustments.global.exposure;
    var t_brightness = adjustments.global.brightness;
    var t_contrast = adjustments.global.contrast;
    var t_highlights = adjustments.global.highlights;
    var t_shadows = adjustments.global.shadows;
    var t_whites = adjustments.global.whites;
    var t_blacks = adjustments.global.blacks;
    var t_saturation = adjustments.global.saturation;
    var t_temperature = adjustments.global.temperature;
    var t_tint = adjustments.global.tint;
    var t_vibrance = adjustments.global.vibrance;
    var t_luma_nr = adjustments.global.luma_noise_reduction;
    var t_color_nr = adjustments.global.color_noise_reduction;
    var t_clarity = adjustments.global.clarity;
    var t_dehaze = adjustments.global.dehaze;
    var t_structure = adjustments.global.structure;
    var t_glow = adjustments.global.glow_amount;
    var t_halation = adjustments.global.halation_amount;
    var t_flare = adjustments.global.flare_amount;
    var t_sharpness = adjustments.global.sharpness;
    var t_sharp_thresh = adjustments.global.sharpness_threshold;
    var t_hue = adjustments.global.hue;
    var t_defringe = 0.0;

    var h0_h = adjustments.global.hsl[0].hue; var h0_s = adjustments.global.hsl[0].saturation; var h0_l = adjustments.global.hsl[0].luminance;
    var h1_h = adjustments.global.hsl[1].hue; var h1_s = adjustments.global.hsl[1].saturation; var h1_l = adjustments.global.hsl[1].luminance;
    var h2_h = adjustments.global.hsl[2].hue; var h2_s = adjustments.global.hsl[2].saturation; var h2_l = adjustments.global.hsl[2].luminance;
    var h3_h = adjustments.global.hsl[3].hue; var h3_s = adjustments.global.hsl[3].saturation; var h3_l = adjustments.global.hsl[3].luminance;
    var h4_h = adjustments.global.hsl[4].hue; var h4_s = adjustments.global.hsl[4].saturation; var h4_l = adjustments.global.hsl[4].luminance;
    var h5_h = adjustments.global.hsl[5].hue; var h5_s = adjustments.global.hsl[5].saturation; var h5_l = adjustments.global.hsl[5].luminance;
    var h6_h = adjustments.global.hsl[6].hue; var h6_s = adjustments.global.hsl[6].saturation; var h6_l = adjustments.global.hsl[6].luminance;
    var h7_h = adjustments.global.hsl[7].hue; var h7_s = adjustments.global.hsl[7].saturation; var h7_l = adjustments.global.hsl[7].luminance;

    var t_moire = adjustments.global.moire.x;
    var t_moire_step = adjustments.global.moire.y;
    for (var i = 0u; ENABLE_MASKS && i < adjustments.mask_count; i = i + 1u) {
        if (PREPARED_LAYERS && (prepared_layers[i + 1u].flags.x & 1u) == 0u) { continue; }
        let influence = get_mask_influence(i, absolute_coord);
        if (influence > 0.001) {
            if (PREPARED_LAYERS) {

            t_exposure += adjustments.mask_adjustments[i].exposure * influence;
            t_brightness += adjustments.mask_adjustments[i].brightness * influence;
            t_contrast += adjustments.mask_adjustments[i].contrast * influence;
            t_highlights += adjustments.mask_adjustments[i].highlights * influence;
            t_shadows += adjustments.mask_adjustments[i].shadows * influence;
            t_whites += adjustments.mask_adjustments[i].whites * influence;
            t_blacks += adjustments.mask_adjustments[i].blacks * influence;

            t_saturation += adjustments.mask_adjustments[i].saturation * influence;
            t_temperature += adjustments.mask_adjustments[i].temperature * influence;
            t_tint += adjustments.mask_adjustments[i].tint * influence;
            t_vibrance += adjustments.mask_adjustments[i].vibrance * influence;

            t_luma_nr += adjustments.mask_adjustments[i].luma_noise_reduction * influence;
            t_color_nr += adjustments.mask_adjustments[i].color_noise_reduction * influence;
            t_clarity += adjustments.mask_adjustments[i].clarity * influence;
            t_dehaze += adjustments.mask_adjustments[i].dehaze * influence;
            t_structure += adjustments.mask_adjustments[i].structure * influence;

            t_glow += adjustments.mask_adjustments[i].glow_amount * influence;
            t_halation += adjustments.mask_adjustments[i].halation_amount * influence;
            t_flare += adjustments.mask_adjustments[i].flare_amount * influence;
            t_hue += adjustments.mask_adjustments[i].hue * influence;
            t_defringe += adjustments.mask_adjustments[i].defringe * influence;
            t_sharpness += adjustments.mask_adjustments[i].sharpness * influence;
            t_moire += adjustments.mask_adjustments[i].moire.x * influence;
            if (adjustments.mask_adjustments[i].moire.x > 0.0) { t_moire_step = adjustments.mask_adjustments[i].moire.y; }

            h0_h += adjustments.mask_adjustments[i].hsl[0].hue * influence; h0_s += adjustments.mask_adjustments[i].hsl[0].saturation * influence; h0_l += adjustments.mask_adjustments[i].hsl[0].luminance * influence;
            h1_h += adjustments.mask_adjustments[i].hsl[1].hue * influence; h1_s += adjustments.mask_adjustments[i].hsl[1].saturation * influence; h1_l += adjustments.mask_adjustments[i].hsl[1].luminance * influence;
            h2_h += adjustments.mask_adjustments[i].hsl[2].hue * influence; h2_s += adjustments.mask_adjustments[i].hsl[2].saturation * influence; h2_l += adjustments.mask_adjustments[i].hsl[2].luminance * influence;
            h3_h += adjustments.mask_adjustments[i].hsl[3].hue * influence; h3_s += adjustments.mask_adjustments[i].hsl[3].saturation * influence; h3_l += adjustments.mask_adjustments[i].hsl[3].luminance * influence;
            h4_h += adjustments.mask_adjustments[i].hsl[4].hue * influence; h4_s += adjustments.mask_adjustments[i].hsl[4].saturation * influence; h4_l += adjustments.mask_adjustments[i].hsl[4].luminance * influence;
            h5_h += adjustments.mask_adjustments[i].hsl[5].hue * influence; h5_s += adjustments.mask_adjustments[i].hsl[5].saturation * influence; h5_l += adjustments.mask_adjustments[i].hsl[5].luminance * influence;
            h6_h += adjustments.mask_adjustments[i].hsl[6].hue * influence; h6_s += adjustments.mask_adjustments[i].hsl[6].saturation * influence; h6_l += adjustments.mask_adjustments[i].hsl[6].luminance * influence;
            h7_h += adjustments.mask_adjustments[i].hsl[7].hue * influence; h7_s += adjustments.mask_adjustments[i].hsl[7].saturation * influence; h7_l += adjustments.mask_adjustments[i].hsl[7].luminance * influence;
            } else {
            let m = adjustments.mask_adjustments[i];

            t_exposure += m.exposure * influence;
            t_brightness += m.brightness * influence;
            t_contrast += m.contrast * influence;
            t_highlights += m.highlights * influence;
            t_shadows += m.shadows * influence;
            t_whites += m.whites * influence;
            t_blacks += m.blacks * influence;

            t_saturation += m.saturation * influence;
            t_temperature += m.temperature * influence;
            t_tint += m.tint * influence;
            t_vibrance += m.vibrance * influence;

            t_luma_nr += m.luma_noise_reduction * influence;
            t_color_nr += m.color_noise_reduction * influence;
            t_clarity += m.clarity * influence;
            t_dehaze += m.dehaze * influence;
            t_structure += m.structure * influence;

            t_glow += m.glow_amount * influence;
            t_halation += m.halation_amount * influence;
            t_flare += m.flare_amount * influence;
            t_hue += m.hue * influence;
            t_defringe += m.defringe * influence;
            t_sharpness += m.sharpness * influence;
            t_moire += m.moire.x * influence;
            if (m.moire.x > 0.0) { t_moire_step = m.moire.y; }

            h0_h += m.hsl[0].hue * influence; h0_s += m.hsl[0].saturation * influence; h0_l += m.hsl[0].luminance * influence;
            h1_h += m.hsl[1].hue * influence; h1_s += m.hsl[1].saturation * influence; h1_l += m.hsl[1].luminance * influence;
            h2_h += m.hsl[2].hue * influence; h2_s += m.hsl[2].saturation * influence; h2_l += m.hsl[2].luminance * influence;
            h3_h += m.hsl[3].hue * influence; h3_s += m.hsl[3].saturation * influence; h3_l += m.hsl[3].luminance * influence;
            h4_h += m.hsl[4].hue * influence; h4_s += m.hsl[4].saturation * influence; h4_l += m.hsl[4].luminance * influence;
            h5_h += m.hsl[5].hue * influence; h5_s += m.hsl[5].saturation * influence; h5_l += m.hsl[5].luminance * influence;
            h6_h += m.hsl[6].hue * influence; h6_s += m.hsl[6].saturation * influence; h6_l += m.hsl[6].luminance * influence;
            h7_h += m.hsl[7].hue * influence; h7_s += m.hsl[7].saturation * influence; h7_l += m.hsl[7].luminance * influence;
            }
        }
    }

    let final_hsl = array<HslColor, 8>(
        HslColor(h0_h, h0_s, h0_l, 0.0), HslColor(h1_h, h1_s, h1_l, 0.0),
        HslColor(h2_h, h2_s, h2_l, 0.0), HslColor(h3_h, h3_s, h3_l, 0.0),
        HslColor(h4_h, h4_s, h4_l, 0.0), HslColor(h5_h, h5_s, h5_l, 0.0),
        HslColor(h6_h, h6_s, h6_l, 0.0), HslColor(h7_h, h7_s, h7_l, 0.0)
    );

    let tonal_blurred = textureLoad(tonal_blur_texture, id.xy, 0).rgb;
    let clarity_blurred = textureLoad(clarity_blur_texture, id.xy, 0).rgb;
    let structure_blurred = textureLoad(structure_blur_texture, id.xy, 0).rgb;

    // Layers that change noise reduction or sharpening need their own amounts.
    let cached_detail = adjustments.detail_cache.x != 0u
        && t_luma_nr == adjustments.global.luma_noise_reduction
        && t_color_nr == adjustments.global.color_noise_reduction
        && t_sharpness == adjustments.global.sharpness
        && t_moire == adjustments.global.moire.x;
    var locally_contrasted_rgb: vec3<f32>;
    if (cached_detail) {
        locally_contrasted_rgb = textureLoad(detail_texture, id.xy, 0).rgb;
    } else {
        locally_contrasted_rgb = detail_stage(
            initial_linear_rgb, id.xy, absolute_coord_i,
            t_luma_nr, t_color_nr, t_sharpness, t_sharp_thresh, scale, is_raw,
            vec2<f32>(min(t_moire, 1.0), t_moire_step)
        );
    }

    if (adjustments.global.defringe[0].x > 0.0 || adjustments.global.defringe[1].x > 0.0 || t_defringe > 0.0) {
        locally_contrasted_rgb = apply_defringe(locally_contrasted_rgb, absolute_coord_i, t_defringe);
    }

    // Structure works on fine detail (the small blur), Clarity on the medium
    // scale (the large blur), as in Capture One and Lightroom.
    let exposure_gain = exp2(t_exposure);
    locally_contrasted_rgb = apply_band_contrast(locally_contrasted_rgb, clarity_blurred,
        band_amount(t_structure, STRUCTURE_GAIN_RAISE, STRUCTURE_GAIN_LOWER), is_raw, exposure_gain);
    locally_contrasted_rgb = apply_band_contrast(locally_contrasted_rgb, structure_blurred,
        band_amount(t_clarity, CLARITY_GAIN_RAISE, CLARITY_GAIN_LOWER), is_raw, exposure_gain);
    locally_contrasted_rgb = apply_centre_local_contrast(locally_contrasted_rgb, adjustments.global.centre, absolute_coord_i, clarity_blurred, is_raw);

    // Camera profile and/or Kelvin white balance (camera_profile.rs).
    if (adjustments.global.camera_profile.w != 0u && is_raw != 0u) {
        locally_contrasted_rgb = apply_camera_profile(locally_contrasted_rgb);
    }
    var processed_rgb = apply_linear_exposure(locally_contrasted_rgb, t_exposure);
    processed_rgb = apply_relight(processed_rgb, vec2<f32>(absolute_coord) / full_dims, full_dims, exposure_gain);

    if (t_glow > 0.0) {
        processed_rgb = apply_glow_bloom(
            processed_rgb, structure_blurred, t_glow, is_raw,
            t_exposure, t_brightness, t_contrast, t_whites
        );
    }
    if (t_halation > 0.0) {
        processed_rgb = apply_halation(
            processed_rgb, clarity_blurred, t_halation, is_raw,
            t_exposure, t_brightness, t_contrast, t_whites
        );
    }
    if (t_flare > 0.0) {
        let uv = vec2<f32>(absolute_coord) / full_dims;
        var flare_color = textureSampleLevel(flare_texture, flare_sampler, uv, 0.0).rgb;
        flare_color *= 1.4;
        flare_color = flare_color * flare_color;
        let linear_luma = get_luma(max(processed_rgb, vec3<f32>(0.0)));
        var perceptual_luma: f32;
        if (linear_luma <= 1.0) {
            perceptual_luma = pow(max(linear_luma, 0.0), 1.0 / 2.2);
        } else {
            perceptual_luma = 1.0 + pow(linear_luma - 1.0, 1.0 / 2.2);
        }
        let protection = 1.0 - smoothstep(0.7, 1.8, perceptual_luma);
        processed_rgb += flare_color * t_flare * protection;
    }

    var composite_rgb_linear = apply_dehaze(processed_rgb, structure_blurred, is_raw, t_dehaze);
    composite_rgb_linear = apply_white_balance(composite_rgb_linear, t_temperature, t_tint);
    composite_rgb_linear = apply_centre_tonal_and_color(composite_rgb_linear, adjustments.global.centre, absolute_coord_i);
    composite_rgb_linear = apply_tonal_adjustments(composite_rgb_linear, tonal_blurred, is_raw, t_contrast, t_shadows, t_whites, t_blacks, exposure_gain);
    if (ENABLE_HIGHLIGHTS) {
        composite_rgb_linear = apply_highlights_adjustment(composite_rgb_linear, absolute_coord_i, scale, is_raw, t_highlights);
    }
    composite_rgb_linear = apply_color_calibration(composite_rgb_linear, adjustments.global.color_calibration);
    if (adjustments.global.calibration_active.x != 0u) {
        composite_rgb_linear = apply_camera_raw_calibration(composite_rgb_linear);
    }
    composite_rgb_linear = apply_hsl_panel(composite_rgb_linear, final_hsl, absolute_coord_i);
    composite_rgb_linear = apply_hue_shift(composite_rgb_linear, t_hue);
    composite_rgb_linear = apply_creative_color(composite_rgb_linear, t_saturation, t_vibrance);

    composite_rgb_linear = apply_color_grading(
        composite_rgb_linear,
        adjustments.global.color_grading_shadows,
        adjustments.global.color_grading_midtones,
        adjustments.global.color_grading_highlights,
        adjustments.global.color_grading_global,
        adjustments.global.color_grading_blending,
        adjustments.global.color_grading_balance
    );

    for (var i = 0u; ENABLE_MASKS && i < adjustments.mask_count; i = i + 1u) {
        if (PREPARED_LAYERS && (prepared_layers[i + 1u].flags.x & 2u) == 0u) { continue; }
        let influence = get_mask_influence(i, absolute_coord);
        if (influence > 0.001) {
            if (PREPARED_LAYERS) {
            let mask_graded = apply_color_grading(
                composite_rgb_linear,
                adjustments.mask_adjustments[i].color_grading_shadows, adjustments.mask_adjustments[i].color_grading_midtones, adjustments.mask_adjustments[i].color_grading_highlights, adjustments.mask_adjustments[i].color_grading_global, adjustments.mask_adjustments[i].color_grading_blending, adjustments.mask_adjustments[i].color_grading_balance
            );
            composite_rgb_linear = mix(composite_rgb_linear, mask_graded, influence);
            } else {
            let m = adjustments.mask_adjustments[i];
            let mask_graded = apply_color_grading(
                composite_rgb_linear,
                m.color_grading_shadows, m.color_grading_midtones, m.color_grading_highlights, m.color_grading_global, m.color_grading_blending, m.color_grading_balance
            );
            composite_rgb_linear = mix(composite_rgb_linear, mask_graded, influence);
            }
        }
    }

    if (adjustments.global.vignette_amount != 0.0 && adjustments.global.tonemapper_mode < 2u) {
        let full_dims_f = vec2<f32>(textureDimensions(input_texture));
        let coord_f = vec2<f32>(absolute_coord);
        let v_amount = adjustments.global.vignette_amount;
        let v_mid = adjustments.global.vignette_midpoint;
        let v_round = 1.0 - adjustments.global.vignette_roundness;
        let v_feather = adjustments.global.vignette_feather * 0.5;
        let aspect = full_dims_f.y / full_dims_f.x;
        let uv_centered = (coord_f / full_dims_f - 0.5) * 2.0;
        let uv_round = sign(uv_centered) * pow(abs(uv_centered), vec2<f32>(v_round, v_round));
        let d = length(uv_round * vec2<f32>(1.0, aspect)) * 0.5;
        let vignette_mask = smoothstep(v_mid - v_feather, v_mid + v_feather, d);
        if (v_amount < 0.0) {
            composite_rgb_linear *= (1.0 + v_amount * vignette_mask);
        } else {
            composite_rgb_linear = mix(composite_rgb_linear, vec3<f32>(1.0), v_amount * vignette_mask);
        }
    }

    var default_tonemapped: vec3<f32>;
    if (adjustments.global.tonemapper_mode == 1u) {
        default_tonemapped = agx_full_transform(composite_rgb_linear);
    } else if (adjustments.global.tonemapper_mode >= 2u && is_raw == 1u) {
        default_tonemapped = camera_raw_tonemap(composite_rgb_linear * exp2(adjustments.global.baseline_exposure));
    } else if (is_raw == 1u) {
        var srgb_emulated = linear_to_srgb(composite_rgb_linear);
        const BRIGHTNESS_GAMMA: f32 = 1.1;
        srgb_emulated = pow(srgb_emulated, vec3<f32>(1.0 / BRIGHTNESS_GAMMA));
        const CONTRAST_MIX: f32 = 0.75;
        let contrast_curve = srgb_emulated * srgb_emulated * (3.0 - 2.0 * srgb_emulated);
        default_tonemapped = mix(srgb_emulated, contrast_curve, CONTRAST_MIX);
        // The per-channel curve acted on the colour clipped to sRGB;
        // negative channels (colour beyond sRGB) follow its tone change.
        let encoded = encode_signed(gamut_floor(composite_rgb_linear));
        let clipped = clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0));
        let beyond = select(clipped, encoded, encoded < vec3<f32>(0.0));
        default_tonemapped = carry_beyond_srgb(beyond, clipped, default_tonemapped);
    } else {
        default_tonemapped = linear_to_display(composite_rgb_linear);
    }
    // Curve choice on top of the base look (RAW only).
    if (is_raw == 1u && adjustments.global.tonemapper_mode >= 2u) {
        default_tonemapped = apply_base_curve(default_tonemapped, u32(adjustments.global.base_curve + 0.5));
    }
    var range_view = -1.0;
    if (adjustments.global.show_clipping >= 2u
        && adjustments.global.show_clipping - 2u < adjustments.global.color_range_count.x) {
        range_view = color_range_view_weight(default_tonemapped, adjustments.global.show_clipping - 2u);
    }
    if (adjustments.global.color_range_count.x > 0u) {
        default_tonemapped = apply_color_ranges(default_tonemapped, 0u, adjustments.global.color_range_count.x);
    }
    for (var i = 0u; ENABLE_MASKS && i < adjustments.mask_count; i = i + 1u) {
        if (PREPARED_LAYERS && (prepared_layers[i + 1u].flags.x & 32u) == 0u) { continue; }
        let count = adjustments.mask_adjustments[i].color_range_count;
        if (count == 0u) { continue; }
        let influence = get_mask_influence(i, absolute_coord);
        if (influence > 0.001) {
            default_tonemapped = mix(default_tonemapped, apply_color_ranges(default_tonemapped, i + 1u, count), influence);
        }
    }
    if (adjustments.global.bw_enabled != 0u) {
        default_tonemapped = apply_black_and_white(default_tonemapped);
    }
    if (adjustments.global.grading_active != 0u) {
        default_tonemapped = apply_grading(default_tonemapped, 0u);
    }
    if (adjustments.global.vignette_amount != 0.0 && adjustments.global.tonemapper_mode >= 2u) {
        default_tonemapped = apply_vignette(default_tonemapped, vec2<f32>(absolute_coord), vec2<f32>(textureDimensions(input_texture)));
    }
    for (var i = 0u; ENABLE_MASKS && i < adjustments.mask_count; i = i + 1u) {
        if (PREPARED_LAYERS && (prepared_layers[i + 1u].flags.x & 16u) == 0u) { continue; }
        if (!PREPARED_LAYERS && adjustments.mask_adjustments[i].grading_active == 0u) { continue; }
        let influence = get_mask_influence(i, absolute_coord);
        if (influence > 0.001) {
            default_tonemapped = mix(default_tonemapped, apply_grading(default_tonemapped, i + 1u), influence);
        }
    }
    var base_srgb: vec3<f32>;
    let is_scene_lut = (adjustments.global.has_lut == 1u && adjustments.global.lut_is_scene_referred == 1u);

    if (is_scene_lut) {
        let vlog_encoded = linear_to_vlog(composite_rgb_linear);
        let lut_color = sample_lut_tetrahedral(vlog_encoded);
        base_srgb = mix(default_tonemapped, lut_color, adjustments.global.lut_intensity);
    } else {
        base_srgb = default_tonemapped;
    }

    base_srgb = apply_filmic_exposure(base_srgb, t_brightness);
    // The per-channel tools below act on the colour clipped to the sRGB
    // cube and carry the rest (carry_beyond_srgb).
    if (adjustments.global.levels_active.x != 0u) {
        let clipped = clamp(base_srgb, vec3<f32>(0.0), vec3<f32>(1.0));
        base_srgb = carry_beyond_srgb(base_srgb, clipped, apply_levels(clipped));
    }
    if (adjustments.global.region_curve_active.x != 0u) {
        let clipped = clamp(base_srgb, vec3<f32>(0.0), vec3<f32>(1.0));
        base_srgb = carry_beyond_srgb(base_srgb, clipped, apply_region_curve(clipped));
    }

    let display = display_unit(base_srgb);
    let curve_input = clamp(display, vec3<f32>(0.0), vec3<f32>(1.0));
    var final_rgb = curve_input;
    if (ENABLE_CURVES) {
        if (PREPARED_LAYERS) {
            final_rgb = apply_curves_prepared(curve_input, 0u);
        } else if (DIRECT_CURVES) {
            final_rgb = apply_all_curves_direct(curve_input, 0u);
        } else {
            final_rgb = apply_all_curves(curve_input,
                adjustments.global.luma_curve, adjustments.global.luma_curve_count,
                adjustments.global.red_curve, adjustments.global.red_curve_count,
                adjustments.global.green_curve, adjustments.global.green_curve_count,
                adjustments.global.blue_curve, adjustments.global.blue_curve_count
            );
        }
    }
    final_rgb = carry_beyond_srgb(display, curve_input, final_rgb);

    for (var i = 0u; ENABLE_MASKS && ENABLE_CURVES && i < adjustments.mask_count; i = i + 1u) {
        if (PREPARED_LAYERS && (prepared_layers[i + 1u].flags.x & 4u) == 0u) { continue; }
        let influence = get_mask_influence(i, absolute_coord);
        if (influence > 0.001) {
            var mask_curved_srgb: vec3<f32>;
            let mask_input = clamp(final_rgb, vec3<f32>(0.0), vec3<f32>(1.0));
            if (PREPARED_LAYERS && (prepared_layers[i + 1u].flags.x & 8u) != 0u) {
                mask_curved_srgb = apply_curve_lut(mask_input, i + 1u);
            } else if (PREPARED_LAYERS) {
                mask_curved_srgb = apply_curves_prepared(mask_input, i + 1u);
            } else if (DIRECT_CURVES) {
                mask_curved_srgb = apply_all_curves_direct(mask_input, i + 1u);
            } else {
                let m = adjustments.mask_adjustments[i];
                mask_curved_srgb = apply_all_curves(mask_input,
                    m.luma_curve, m.luma_curve_count,
                    m.red_curve, m.red_curve_count,
                    m.green_curve, m.green_curve_count,
                    m.blue_curve, m.blue_curve_count
                );
            }
            mask_curved_srgb = carry_beyond_srgb(final_rgb, mask_input, mask_curved_srgb);
            final_rgb = mix(final_rgb, mask_curved_srgb, influence);
        }
    }

    if (adjustments.global.has_lut == 1u && adjustments.global.lut_is_scene_referred == 0u) {
        let lut_input = clamp(final_rgb, vec3<f32>(0.0), vec3<f32>(1.0));
        let lut_color = carry_beyond_srgb(final_rgb, lut_input, sample_lut_tetrahedral(lut_input));
        final_rgb = mix(final_rgb, lut_color, adjustments.global.lut_intensity);
    }

    if (adjustments.global.grain_amount > 0.0 && adjustments.global.grain_structure == GRAIN_EVEN) {
        let coord = vec2<f32>(absolute_coord_i);
        var amount = adjustments.global.grain_amount * 0.5;
        let grain_frequency = (1.0 / max(adjustments.global.grain_size, 0.1)) / scale;
        let roughness = adjustments.global.grain_roughness;
        let luma = max(0.0, get_luma(final_rgb));
        var luma_mask = smoothstep(0.0, 0.15, luma) * (1.0 - smoothstep(0.6, 1.0, luma));
        if (adjustments.global.tonemapper_mode >= 2u) {
            // Lightroom's grain (measured in Camera Raw, notes: reference/grain.md):
            // about twice as strong, even across the tonal range up to near
            // black and white, and stronger as it gets finer below size 25.
            amount *= GRAIN_GAIN * (1.0 + GRAIN_FINE_BOOST * max(0.0, 1.0 - adjustments.global.grain_size * 2.0));
            luma_mask = smoothstep(0.0, 0.03, luma) * (1.0 - smoothstep(0.97, 1.0, luma));
        }
        let base_coord = coord * grain_frequency;
        let rough_coord = coord * grain_frequency * 0.6;
        let noise_base = gradient_noise(base_coord);
        let noise_rough = gradient_noise(rough_coord + vec2<f32>(5.2, 1.3));
        let noise_val = mix(noise_base, noise_rough, roughness);
        final_rgb += vec3<f32>(noise_val) * amount * luma_mask;
    }
    if (adjustments.global.grain_amount > 0.0 && adjustments.global.grain_structure != GRAIN_EVEN) {
        final_rgb = apply_grain_structure(final_rgb, vec2<f32>(absolute_coord_i), scale);
    }

    if (adjustments.global.show_clipping == 1u) {
        let HIGHLIGHT_WARNING_COLOR = vec3<f32>(1.0, 0.0, 0.0);
        let SHADOW_WARNING_COLOR = vec3<f32>(0.0, 0.0, 1.0);
        let HIGHLIGHT_CLIP_THRESHOLD = 0.998;
        let SHADOW_CLIP_THRESHOLD = 0.002;
        if (any(final_rgb > vec3<f32>(HIGHLIGHT_CLIP_THRESHOLD))) {
            final_rgb = HIGHLIGHT_WARNING_COLOR;
        } else if (any(final_rgb < vec3<f32>(SHADOW_CLIP_THRESHOLD))) {
            final_rgb = SHADOW_WARNING_COLOR;
        }
    }

    // Colour editor "Show range": the selected range in colour, the rest grey.
    if (range_view >= 0.0) {
        let grey = vec3<f32>(get_luma(clamp(final_rgb, vec3<f32>(0.0), vec3<f32>(1.0))));
        final_rgb = mix(grey * 0.6 + 0.2, final_rgb, range_view);
    }

    final_rgb = encode_output(final_rgb);

    if (HIGH_PRECISION_OUTPUT == 0u) {
        let dither_amount = 1.0 / 255.0;
        final_rgb += dither(id.xy) * dither_amount;
    }

    textureStore(output_texture, id.xy, vec4<f32>(clamp(final_rgb, vec3<f32>(0.0), vec3<f32>(1.0)), original_alpha));
}
