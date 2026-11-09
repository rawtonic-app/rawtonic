//! Literal old mask reader, retained only as a per-field compatibility oracle.
use super::*;

fn old_mask_adjustments(
    adj: &serde_json::Value,
    tonemapper_mode: u32,
    render: RenderVersion,
) -> MaskAdjustments {
    if adj.is_null() {
        return MaskAdjustments::default();
    }

    let visibility = adj.get("sectionVisibility");
    let is_visible = |section: &str| -> bool {
        visibility
            .and_then(|v| v.get(section))
            .and_then(|s| s.as_bool())
            .unwrap_or(true)
    };

    let tool_on = |key: &str| old_tool_switch(key).is_none_or(&is_visible);
    // As for the photo (`get_global_adjustments_from_json`).
    let classic_grading =
        is_visible("color") && (!render.look_tool_switches() || tool_on("colorGrading"));
    let get_val = |section: &str, key: &str, scale: f32| -> f32 {
        if is_visible(section) && tool_on(key) {
            adj[key].as_f64().unwrap_or(0.0) as f32 / scale
        } else {
            0.0
        }
    };

    let curves_obj = adj.get("curves").cloned().unwrap_or_default();
    let luminance_points = if is_visible("curves") {
        crate::point_curve::luminance_points(&curves_obj)
    } else {
        Vec::new()
    };
    let luma_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj["luma"].as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let red_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj["red"].as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let green_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj["green"].as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let blue_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj["blue"].as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let cg_obj = adj.get("colorGrading").cloned().unwrap_or_default();

    let mut mask = MaskAdjustments {
        exposure: get_val("basic", "exposure", SCALES.exposure),
        brightness: get_val("basic", "brightness", SCALES.brightness),
        contrast: get_val("basic", "contrast", SCALES.contrast),
        highlights: get_val("basic", "highlights", SCALES.highlights),
        shadows: get_val("basic", "shadows", SCALES.shadows),
        whites: get_val("basic", "whites", SCALES.whites),
        blacks: get_val("basic", "blacks", SCALES.blacks),

        saturation: get_val("color", "saturation", SCALES.saturation),
        temperature: get_val("color", "temperature", SCALES.temperature),
        tint: get_val("color", "tint", SCALES.tint),
        vibrance: get_val("color", "vibrance", SCALES.vibrance),

        sharpness: get_val("details", "sharpness", SCALES.sharpness),
        luma_noise_reduction: get_val("details", "lumaNoiseReduction", SCALES.luma_noise_reduction),
        color_noise_reduction: get_val(
            "details",
            "colorNoiseReduction",
            SCALES.color_noise_reduction,
        ),

        clarity: get_val("details", "clarity", SCALES.clarity),
        dehaze: get_val("details", "dehaze", SCALES.dehaze),
        structure: get_val("details", "structure", SCALES.structure),

        glow_amount: get_val("effects", effects::GLOW_AMOUNT.name(), SCALES.glow),
        halation_amount: get_val("effects", effects::HALATION_AMOUNT.name(), SCALES.halation),
        flare_amount: get_val("effects", effects::FLARE_AMOUNT.name(), SCALES.flares),
        sharpness_threshold: get_val("details", "sharpnessThreshold", SCALES.sharpness_threshold),

        hue: get_val("color", "hue", 1.0),
        defringe: (get_val("details", "defringe", 100.0)).clamp(-1.0, 1.0),
        _pad_cg2: 0.0,
        color_grading_shadows: if classic_grading {
            parse_color_grade_settings(&cg_obj["shadows"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_midtones: if classic_grading {
            parse_color_grade_settings(&cg_obj["midtones"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_highlights: if classic_grading {
            parse_color_grade_settings(&cg_obj["highlights"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_global: if classic_grading {
            parse_color_grade_settings(&cg_obj["global"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_blending: if classic_grading {
            cg_obj["blending"].as_f64().unwrap_or(50.0) as f32 / SCALES.color_grading_blending
        } else {
            0.5
        },
        color_grading_balance: if classic_grading {
            cg_obj["balance"].as_f64().unwrap_or(0.0) as f32 / SCALES.color_grading_balance
        } else {
            0.0
        },
        grading_active: 0,
        color_range_count: 0,

        hsl: if is_visible("color") && tool_on("hsl") {
            parse_hsl_adjustments(&adj.get("hsl").cloned().unwrap_or_default())
        } else {
            [HslColor::default(); 8]
        },
        luma_curve: convert_points_to_aligned(luma_points.clone()),
        red_curve: convert_points_to_aligned(red_points.clone()),
        green_curve: convert_points_to_aligned(green_points.clone()),
        blue_curve: convert_points_to_aligned(blue_points.clone()),
        luma_curve_count: luma_points.len().min(16) as u32,
        red_curve_count: red_points.len().min(16) as u32,
        green_curve_count: green_points.len().min(16) as u32,
        blue_curve_count: blue_points.len().min(16) as u32,
        curve_space: if is_visible("curves") {
            crate::point_curve::curve_space(&curves_obj)
        } else {
            0
        },
        _pad_end5: 0.0,
        _pad_end6: 0.0,
        _pad_end7: 0.0,
        grading_lut: [[0.0; 4]; GRADING_LUT_SIZE],
        color_ranges: [[0.0; 4]; 16],
        moire: if is_visible("details") {
            moire_settings(adj)
        } else {
            [0.0; 4]
        },
        luminance_curve: convert_points_to_aligned(luminance_points.clone()),
        luminance_curve_count: luminance_points.len().min(16) as u32,
        _pad_luminance: [0; 3],
    };
    if is_visible("color") && tool_on("colorRanges") {
        let ranges = pack_color_ranges(&adj["colorRanges"], 4);
        mask.color_range_count = ranges.len() as u32;
        for (i, range) in ranges.iter().enumerate() {
            mask.color_ranges[i * 4..i * 4 + 4].copy_from_slice(range);
        }
    }
    if modern_grading(tonemapper_mode) && is_visible("color") {
        let off = serde_json::Value::Null;
        mask.apply_grading(crate::color_grading::Grading::from_json(
            if tool_on("colorGrading") {
                &cg_obj
            } else {
                &off
            },
        ));
    }
    mask
}

pub(super) fn pack_color_ranges(ranges: &serde_json::Value, limit: usize) -> Vec<[[f32; 4]; 4]> {
    let Some(ranges) = ranges.as_array() else {
        return Vec::new();
    };
    ranges
        .iter()
        .take(limit)
        .map(|range| {
            let get = |key: &str, default: f64| range[key].as_f64().unwrap_or(default);
            let chroma = get("chroma", 0.0).max(0.0);
            let chroma_range = get("chromaRange", 70.0).clamp(0.0, 100.0) / 100.0;
            let lightness_range = get("lightnessRange", 100.0).clamp(0.0, 100.0) / 100.0;
            [
                [
                    get("hue", 0.0).rem_euclid(360.0) as f32,
                    chroma as f32,
                    get("lightness", 0.5).clamp(0.0, 1.0) as f32,
                    (0.02 + lightness_range) as f32,
                ],
                [
                    get("hueRange", 25.0).clamp(5.0, 90.0) as f32,
                    (chroma * (1.0 - chroma_range)) as f32,
                    (chroma * (1.0 + 2.0 * chroma_range) + 0.01) as f32,
                    (0.1 + 0.9 * get("smoothness", 50.0).clamp(0.0, 100.0) / 100.0) as f32,
                ],
                [
                    (get("hueShift", 0.0).clamp(-100.0, 100.0) * 0.5) as f32,
                    (get("saturation", 0.0).clamp(-100.0, 100.0) / 100.0) as f32,
                    (get("lightnessShift", 0.0).clamp(-100.0, 100.0) / 100.0 * 0.2) as f32,
                    0.0,
                ],
                [
                    (get("hueUniformity", 0.0).clamp(0.0, 100.0) / 100.0) as f32,
                    (get("saturationUniformity", 0.0).clamp(0.0, 100.0) / 100.0) as f32,
                    (get("lightnessUniformity", 0.0).clamp(0.0, 100.0) / 100.0) as f32,
                    0.0,
                ],
            ]
        })
        .collect()
}

pub(super) fn parse_hsl_adjustments(js_hsl: &serde_json::Value) -> [HslColor; 8] {
    let mut hsl_array = [HslColor::default(); 8];
    if let Some(hsl_map) = js_hsl.as_object() {
        let color_map = [
            ("reds", 0),
            ("oranges", 1),
            ("yellows", 2),
            ("greens", 3),
            ("aquas", 4),
            ("blues", 5),
            ("purples", 6),
            ("magentas", 7),
        ];
        for (name, index) in color_map.iter() {
            if let Some(color_data) = hsl_map.get(*name) {
                hsl_array[*index] = HslColor {
                    hue: color_data["hue"].as_f64().unwrap_or(0.0) as f32
                        * SCALES.hsl_hue_multiplier,
                    saturation: color_data["saturation"].as_f64().unwrap_or(0.0) as f32
                        / SCALES.hsl_saturation,
                    luminance: color_data["luminance"].as_f64().unwrap_or(0.0) as f32
                        / SCALES.hsl_luminance,
                    response: 0.0,
                };
            }
        }
    }
    hsl_array
}

fn parse_color_grade_settings(js_cg: &serde_json::Value) -> ColorGradeSettings {
    if js_cg.is_null() {
        return ColorGradeSettings::default();
    }
    ColorGradeSettings {
        hue: js_cg["hue"].as_f64().unwrap_or(0.0) as f32,
        saturation: js_cg["saturation"].as_f64().unwrap_or(0.0) as f32
            / SCALES.color_grading_saturation,
        luminance: js_cg["luminance"].as_f64().unwrap_or(0.0) as f32
            / SCALES.color_grading_luminance,
        _pad: 0.0,
    }
}

fn moire_settings(adjustments: &serde_json::Value) -> [f32; 4] {
    let get = |key: &str, default: f64| adjustments[key].as_f64().unwrap_or(default);
    resolved_moire_settings(get("moireAmount", 0.0), get("moirePattern", 30.0))
}

fn old_tool_switch(key: &str) -> Option<&'static str> {
    if let Some(section) = rawtonic_recipe::detail::tool_switch(key) {
        return Some(section.name());
    }
    match key {
        "temperature" | "tint" => Some("whiteBalance"),
        "vibrance" | "saturation" => Some("presence"),
        "hsl" => Some("colorMixer"),
        "colorGrading" => Some("colorGrading"),
        "levels" => Some("levels"),
        "colorCalibration" => Some("calibration"),
        "colorRanges" => Some("colorEditor"),
        _ => None,
    }
}

fn compare(recipe: &Value) {
    for version in [1, 7, 11] {
        let render = RenderVersion::of(&json!({"renderVersion":version}));
        for mode in [
            tonemapper_mode_of("standard"),
            tonemapper_mode_of("classic"),
        ] {
            let old = old_mask_adjustments(recipe, mode, render);
            let new = get_mask_adjustments_from_json(recipe, mode, render);
            assert_eq!(
                bytemuck::bytes_of(&new),
                bytemuck::bytes_of(&old),
                "r{version}, mode {mode}: {recipe}"
            );
        }
    }
}

#[test]
fn local_fields_match_the_old_reader_at_edges() {
    let keys = [
        "exposure",
        "brightness",
        "contrast",
        "highlights",
        "shadows",
        "whites",
        "blacks",
        "temperature",
        "tint",
        "vibrance",
        "saturation",
        "hue",
        "sharpness",
        "sharpnessThreshold",
        "lumaNoiseReduction",
        "colorNoiseReduction",
        "clarity",
        "dehaze",
        "structure",
        "moireAmount",
        "moirePattern",
        "defringe",
        "glowAmount",
        "halationAmount",
        "flareAmount",
    ];
    let sections = [
        "basic",
        "color",
        "details",
        "effects",
        "curves",
        "whiteBalance",
        "presence",
        "sharpening",
        "noiseReduction",
        "colorMixer",
        "colorEditor",
        "colorGrading",
    ];
    for value in [
        Value::Null,
        json!([]),
        json!(false),
        json!(12),
        json!("wrong"),
        json!({}),
    ] {
        compare(&value);
    }
    for key in keys {
        for value in [
            Value::Null,
            json!(false),
            json!([]),
            json!({}),
            json!("5"),
            json!(-10001.25),
            json!(-1.0),
            json!(0),
            json!(37.125),
            json!(10002.5),
            json!(1e300),
        ] {
            let mut recipe = json!({key:value});
            compare(&recipe);
            for section in sections {
                recipe["sectionVisibility"] = json!({section:false});
                compare(&recipe);
            }
        }
    }
}

#[test]
fn nested_local_fields_match_the_old_reader_at_edges() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/mask_adjustments.json")).unwrap();
    // The full local block every recorded all-types layer uses.
    let full = fixture["localAdjustments"]["shared"].clone();
    for section in [
        "basic",
        "color",
        "details",
        "effects",
        "curves",
        "whiteBalance",
        "presence",
        "sharpening",
        "noiseReduction",
        "colorMixer",
        "colorEditor",
        "colorGrading",
    ] {
        let mut recipe = full.clone();
        recipe["sectionVisibility"] = json!({section:false});
        compare(&recipe);
    }
    let edges = [
        Value::Null,
        json!(false),
        json!("7"),
        json!([]),
        json!({}),
        json!(-10001.25),
        json!(73.125),
        json!(10002.5),
    ];
    for key in ["hsl", "colorGrading", "colorRanges", "curves"] {
        for value in &edges {
            let mut recipe = full.clone();
            recipe[key] = value.clone();
            compare(&recipe);
        }
    }
    for (group, channels) in [
        (
            "hsl",
            &[
                "reds", "oranges", "yellows", "greens", "aquas", "blues", "purples", "magentas",
            ][..],
        ),
        (
            "colorGrading",
            &["shadows", "midtones", "highlights", "global"][..],
        ),
    ] {
        for channel in channels {
            for value in &edges {
                let mut recipe = full.clone();
                recipe[group][channel] = value.clone();
                compare(&recipe);
            }
            for key in ["hue", "saturation", "luminance"] {
                for value in &edges {
                    let mut recipe = full.clone();
                    recipe[group][channel][key] = value.clone();
                    compare(&recipe);
                }
            }
        }
    }
    for key in ["blending", "balance"] {
        for value in &edges {
            let mut r = full.clone();
            r["colorGrading"][key] = value.clone();
            compare(&r);
        }
    }
    for key in [
        "hue",
        "chroma",
        "lightness",
        "lightnessRange",
        "hueRange",
        "chromaRange",
        "smoothness",
        "hueShift",
        "saturation",
        "lightnessShift",
        "hueUniformity",
        "saturationUniformity",
        "lightnessUniformity",
    ] {
        for value in &edges {
            let mut r = full.clone();
            r["colorRanges"][0][key] = value.clone();
            compare(&r);
        }
    }
    for key in ["luma", "red", "green", "blue", "luminance", "version"] {
        for value in edges.iter().cloned().chain([
            json!([null,{"x":"1","y":5},{"x":30,"y":50}]),
            json!(
                (0..25)
                    .map(|x| json!({"x":x*10,"y":x*9+3}))
                    .collect::<Vec<_>>()
            ),
        ]) {
            let mut r = full.clone();
            r["curves"][key] = value;
            compare(&r);
        }
    }
}

fn recipe() -> Value {
    json!({"masks":[{"id":"layer","name":"A","visible":true,"invert":false,"adjustments":{},"subMasks":[{"id":"component","type":"linear","visible":true,"mode":"additive","parameters":{"startX":10,"future":{"x":1}}}]}]})
}
fn compare_container(recipe: &Value) {
    let old = recipe
        .get("masks")
        .and_then(|v| Vec::<MaskDefinition>::deserialize(v).ok())
        .unwrap_or_default();
    let actual = mask_definitions_from_json(recipe);
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(old).unwrap(),
        "{recipe}"
    );
}

#[test]
fn containers_match_the_old_serde_reader_per_field() {
    let base = recipe();
    let edges = [
        Value::Null,
        json!(false),
        json!(true),
        json!("future"),
        json!(0),
        json!(1.0),
        json!(1.25),
        json!(-1),
        json!(4294967296u64),
        json!(1e300),
        json!([]),
        json!({}),
    ];
    for key in [
        "id",
        "name",
        "visible",
        "invert",
        "opacity",
        "adjustments",
        "subMasks",
        "maskVersion",
        "refineEdge",
    ] {
        let mut missing = base.clone();
        missing["masks"][0].as_object_mut().unwrap().remove(key);
        compare_container(&missing);
        for value in &edges {
            let mut r = base.clone();
            r["masks"][0][key] = value.clone();
            compare_container(&r);
        }
    }
    for key in [
        "id",
        "type",
        "visible",
        "invert",
        "opacity",
        "mode",
        "parameters",
    ] {
        let mut missing = base.clone();
        missing["masks"][0]["subMasks"][0]
            .as_object_mut()
            .unwrap()
            .remove(key);
        compare_container(&missing);
        for value in edges.iter().cloned().chain([
            json!("additive"),
            json!("subtractive"),
            json!("intersect"),
        ]) {
            let mut r = base.clone();
            r["masks"][0]["subMasks"][0][key] = value;
            compare_container(&r);
        }
    }
    for value in edges {
        let mut r = base.clone();
        r["masks"] = value;
        compare_container(&r);
    }
    for r in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"masks":[base["masks"][0].clone(),{}]}),
    ] {
        compare_container(&r);
    }
}
