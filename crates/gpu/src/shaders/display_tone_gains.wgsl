// Display variants only (`display_variant_ir::DisplayShader`): the tone
// gains of shader.wgsl with one call site of their unordered gain, a loop
// over the pixel's level and the ordering anchors, replacing the functions
// of the same name there. AMD's Windows Vulkan compiler inlines every call:
// with these the Contrast variant 4145 compiled in 27.6 instead of 54.0 s and
// raised Shadows (53299) in 58 instead of 174 s on a Radeon 680M
// (compile_probe round 12, G76/WCC3). Exports keep shader.wgsl's functions:
// on Metal these changed 49 of 280 preservation exports.

fn camera_raw_shadows_blacks_gain(level_luma: f32, sh: f32, bl: f32) -> f32 {
    // One call site of the unordered gain: the pixel's level, then the
    // anchors that keep the tones in order (AMD's compiler inlines each).
    var levels = array<f32, 3>(level_luma, 0.0, 0.0);
    var rises = array<f32, 3>(0.0, 0.0, 0.0);
    var n = 1u;
    let bottom = cr_toe_bottom_level();
    if (bottom > 0.0 && level_luma > bottom) {
        if (sh < 0.0 || bl < 0.0) {
            levels[1] = bottom;
            rises[1] = 0.0;
            levels[2] = cr_toe_top_level();
            rises[2] = CR_ORDER_RISE_TOE;
            n = 3u;
        }
        if (sh > 0.0 && bl > 0.0) {
            levels[1] = camera_raw_black() * CR_TOE_LAST_SEGMENT / exp2(adjustments.global.baseline_exposure);
            rises[1] = CR_ORDER_RISE_TOE;
            levels[2] = cr_curve_level(CR_RAISE_PEAK_DISPLAY);
            rises[2] = CR_ORDER_RISE_TOE;
            n = 3u;
        }
    }
    var gain = 0.0;
    for (var k = 0u; k < n; k = k + 1u) {
        let g = camera_raw_shadows_blacks_gain_unordered(levels[k], sh, bl);
        if (k == 0u) {
            gain = g;
        } else {
            gain = max(gain, cr_order_gain(level_luma, levels[k], g, rises[k]));
        }
    }
    return gain;
}

fn cr_whites_gain(x: f32, amount: f32) -> f32 {
    var level = array<f32, 2>(x, 0.0);
    var rise = 0.0;
    var n = 1u;
    if (amount > 0.0) {
        let peak = cr_curve_level(CR_WHITES_RAISE_PEAK_DISPLAY);
        if (x > peak) {
            level[1] = peak;
            rise = CR_ORDER_RISE_WHITES_RAISE;
            n = 2u;
        }
    } else {
        let knee = cr_knee_level();
        if (amount < 0.0 && knee > 0.0 && x > knee) {
            level[1] = knee;
            rise = CR_ORDER_RISE_SHOULDER;
            n = 2u;
        }
    }
    var gain = 0.0;
    for (var k = 0u; k < n; k = k + 1u) {
        let g = cr_whites_gain_unordered(level[k], amount);
        if (k == 0u) {
            gain = g;
        } else {
            gain = max(gain, cr_order_gain(x, level[1], g, rise));
        }
    }
    return gain;
}
