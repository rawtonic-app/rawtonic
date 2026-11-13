// Display variants only (`display_variant_ir::DisplayShader`): shader.wgsl's
// tone stage as one function. shader.wgsl keeps tone versions 12 and 13 in
// functions of their own (`apply_tonal_adjustments_v12`, `_v13`) so older
// recipes' exports compile exactly as before; their callers choose one by
// the recipe's version, so a display variant with lowered Whites or raised
// Shadows carried two or three copies of the tone stage, which AMD's Windows
// Vulkan compiler inlines each (G76/WCC4). This replaces
// `apply_tonal_adjustments`, and the display source drops the callers'
// version-12 and -13 branches, so every caller reaches this one function,
// which takes the versions' differences inside: lowered Whites of version
// 12 (`whites_v12_lower`) and raised Shadows of version 13
// (`shadows_v13_gain`). Exports keep shader.wgsl's functions.

fn apply_tonal_adjustments(
    color: vec3<f32>,
    blurred_color_input_space: vec3<f32>,
    wide_luma_input_space: f32,
    whites_offset_log: f32,
    is_raw: u32,
    con: f32,
    sh: f32,
    wh: f32,
    bl_in: f32,
    exposure_gain: f32
) -> vec3<f32> {
    var rgb = color;
    // The tone versions shader.wgsl gives functions of their own
    // (`apply_tonal_adjustments_v12`, `_v13`), chosen as its callers did.
    let shadows_v13 = SHADOWS_V13 && adjustments.global.tone_version > 12.5;
    var whites_v12 = shadows_v13;
    if (WHITES_V12 && adjustments.global.tone_version > 11.5) {
        whites_v12 = true;
    }

    var blurred_linear: vec3<f32>;
    if (is_raw == 1u) {
        blurred_linear = blurred_color_input_space;
    } else {
        blurred_linear = srgb_to_linear(blurred_color_input_space);
    }

    // Whites' luminance ratio at the pixel, which the blurred levels follow.
    var whites_ratio = 1.0;
    if (wh != 0.0 && base_look_is_camera_raw()) {
        // Renderer value is slider / 30.
        let amount = clamp(wh * 0.3, -1.0, 1.0);
        let negative = any(rgb < vec3<f32>(0.0));
        let floored = max(rgb, vec3<f32>(0.0));
        // One `cr_whites` for every response (shader.wgsl calls it from
        // `whites_v10_lower`, `whites_v12_lower` and here).
        let input = select(rgb, floored, negative);
        let lower = amount < 0.0 && adjustments.global.tone_version > 9.5;
        var base = input;
        if (lower) {
            let r = exp2(whites_offset_log);
            base = max(input / r, vec3<f32>(1e-6));
        }
        let whited = cr_whites(base, amount);
        var whitened = whited;
        if (lower) {
            let gain = max(whited, vec3<f32>(1e-9)) / base;
            if (whites_v12) {
                // `whites_v12_lower`
                let level = lstar_of_luma(exp2(camera_raw_level_slope(get_luma(base)).x));
                let u = clamp((level - WHITES_V12_FROM) / (100.0 - WHITES_V12_FROM), 0.0, 1.0);
                let over = max(u - WHITES_V12_KNEE, 0.0);
                let h = u - over * over / (2.0 * (1.0 - WHITES_V12_KNEE));
                let weight = WHITES_V12_STRENGTH * h * h;
                whitened = input * exp2(weight * WHITES_V10_POWER * log2(gain));
            } else {
                // `whites_v10_lower`
                whitened = input * exp2(WHITES_V10_POWER * log2(gain));
            }
        }
        if (negative) {
            whitened = carry_scene(rgb, floored, whitened);
        }
        let luma_before = max(get_luma(max(rgb, vec3<f32>(0.0))), 1e-6);
        let ratio = max(get_luma(max(whitened, vec3<f32>(0.0))), 0.0) / luma_before;
        rgb = whitened;
        blurred_linear *= ratio;
        whites_ratio = ratio;
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
        // The result is (t · (1 + k · t^reach))^2.2, which turns down where
        // 1 + k · (1 + reach) · t^reach < 0: Whites below about −75 under
        // Standard darkened the tones from −0.37 EV up to white (−100: 0.09
        // scene EV, the brightest grey lower than one 0.37 EV darker). From
        // that level on the gain stays the level's, so brighter stays
        // brighter; tones below it are unchanged.
        let k = gain * w;
        var t_gain = t;
        if (k * (1.0 + reach) < -1.0) {
            t_gain = min(t, pow(-1.0 / (k * (1.0 + reach)), 1.0 / reach));
        }
        let ratio = pow(max(1.0 + k * pow(t_gain, reach), 0.05), 2.2);
        rgb *= ratio;
        blurred_linear *= ratio;
        whites_ratio = ratio;
    }

    // Version 9 lowers Blacks after the tone map (blacks_v9_black_point).
    let bl = select(bl_in, 0.0, blacks_v9_active(bl_in, is_raw));
    let pixel_luma = get_luma(max(rgb, vec3<f32>(0.0)));
    let blurred_luma = get_luma(max(blurred_linear, vec3<f32>(0.0)));

    let safe_pixel_luma = max(pixel_luma, 0.0001);
    let safe_blurred_luma = max(blurred_luma, 0.0001);

    if ((sh != 0.0 || bl != 0.0) && base_look_is_camera_raw()) {
        // Lightroom's measured response by display level, applied as a
        // gain from the blurred (exposed) luminance so detail is kept.
        // Where the blur differs from the pixel by no more than its 16-bit
        // float storage rounds (≤ 0.05 %), the pixel's own luminance is
        // used: near the black point the black subtraction magnified that
        // rounding into ±3-code wobble on smooth gradients (Blacks −40 fell
        // 0.013 EV at display −6.6 EV). Detail keeps the blurred level.
        let level_ratio = safe_blurred_luma / safe_pixel_luma;
        let level_snap = smoothstep(0.001, 0.004, abs(log(level_ratio)));
        var level_luma = safe_pixel_luma * pow(level_ratio, level_snap);
        let v6 = sh > 0.0 && adjustments.global.tone_version > 5.5;
        let tonal_level = level_luma;
        // Faded in (quadratically) over Shadows 0 … +SH_V6_FADE, so raising
        // Shadows from 0 does not switch the level of Blacks' (shared) gain
        // at once.
        let ramp = clamp(sh / SH_V6_FADE, 0.0, 1.0);
        let fade = ramp * ramp;
        if (v6) {
            let wide = max(wide_luma_input_space * whites_ratio, 0.0001);
            level_luma = exp2(mix(log2(max(tonal_level, 0.0001)), log2(wide), fade));
        }
        let gain = camera_raw_shadows_blacks_gain(level_luma * exposure_gain, sh, bl);
        if (shadows_v13) {
            // Version 13: no texture boost, less lift in the midtones
            // (`shadows_v13_gain`, with this gain).
            rgb *= display_shadows_v13_gain(gain, level_luma * exposure_gain, sh, bl, fade);
        } else {
            rgb *= gain;
            if (v6) {
                rgb *= pow(shadows_v6_texture(tonal_level, level_luma, gain, exposure_gain), fade);
            }
        }
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

// `shadows_v13_gain` with its gain at the level already taken.
fn display_shadows_v13_gain(gain: f32, level_luma: f32, sh: f32, bl: f32, fade: f32) -> f32 {
    if (sh <= 0.0) {
        return gain;
    }
    // Only Shadows' share is cut: with Blacks raised too, cutting Blacks'
    // lift as well turned tones over (+100 / +100 fell 2 × the limit).
    var blacks_log = 0.0;
    if (bl != 0.0) {
        blacks_log = log2(max(camera_raw_shadows_blacks_gain(level_luma, 0.0, bl), 1e-9));
    }
    let shadows_log = log2(max(gain, 1e-9)) - blacks_log;
    if (shadows_log <= 0.0) {
        return gain;
    }
    let display = camera_raw_level_slope(level_luma).x;
    let cut = SH_V13_MID_CUT * smoothstep(SH_V13_MID_FROM, SH_V13_MID_TO, display) * fade;
    return exp2(blacks_log + shadows_log * (1.0 - cut));
}
