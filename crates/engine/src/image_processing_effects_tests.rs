//! Actual uniform conversions pinned to the effects reads before extraction.
use super::{GlobalAdjustments, get_global_adjustments_from_json};
use serde_json::{Value, json};

type Field = fn(&GlobalAdjustments) -> f32;
const OLD_FIELDS: &[(&str, f64, f32, Field)] = &[
    ("vignetteAmount", 0.0, 100.0, |g| g.vignette_amount),
    ("vignetteMidpoint", 50.0, 100.0, |g| g.vignette_midpoint),
    ("vignetteRoundness", 0.0, 100.0, |g| g.vignette_roundness),
    ("vignetteFeather", 50.0, 100.0, |g| g.vignette_feather),
    ("grainAmount", 0.0, 200.0, |g| g.grain_amount),
    ("grainSize", 25.0, 50.0, |g| g.grain_size),
    ("grainRoughness", 50.0, 100.0, |g| g.grain_roughness),
    ("glowAmount", 0.0, 100.0, |g| g.glow_amount),
    ("halationAmount", 0.0, 100.0, |g| g.halation_amount),
    ("flareAmount", 0.0, 100.0, |g| g.flare_amount),
];

#[test]
fn effects_uniforms_keep_old_cast_and_scale_order() {
    for (name, default, scale, get) in OLD_FIELDS {
        for value in [
            Value::Null,
            json!(false),
            json!("37"),
            json!([]),
            json!({}),
            json!(-1e300),
            json!(-137.25),
            json!(0),
            json!(0.123456789123),
            json!(67.5),
            json!(999),
            json!(1e300),
        ] {
            for visible in [true, false] {
                let recipe = json!({(*name): value, "sectionVisibility": {"effects": visible}});
                let old = if visible {
                    recipe[*name].as_f64().unwrap_or(*default)
                } else {
                    *default
                } as f32
                    / scale;
                for is_raw in [false, true] {
                    let actual = get(&get_global_adjustments_from_json(&recipe, is_raw, None));
                    assert_eq!(actual.to_bits(), old.to_bits(), "{name}: {recipe}");
                }
            }
        }
    }
}

#[test]
fn clamped_and_discrete_effect_uniforms_keep_old_reads() {
    for value in [
        Value::Null,
        json!(false),
        json!(true),
        json!(""),
        json!("colorPriority"),
        json!("paintOverlay"),
        json!("soft"),
        json!("clumped"),
        json!("speckled"),
        json!("gritty"),
        json!("future"),
        json!(-1),
        json!(-1e300),
        json!(0),
        json!(0.123456789123),
        json!(73.25),
        json!(999),
        json!(1e300),
        json!(u64::MAX),
    ] {
        for visible in [true, false] {
            let recipe = json!({"vignetteStyle":value,"vignetteHighlights":value,"grainColor":value,"grainStructure":value,"grainVersion":value,"lutPath":value,"lutIntensity":value,"lutIsSceneReferred":value,"sectionVisibility":{"effects":visible}});
            let actual = get_global_adjustments_from_json(&recipe, false, None);
            let style = if visible {
                match recipe["vignetteStyle"].as_str() {
                    Some("colorPriority") => 1,
                    Some("paintOverlay") => 2,
                    _ => 0,
                }
            } else {
                0
            };
            let highlights = if visible {
                (recipe["vignetteHighlights"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .clamp(0.0, 100.0)
                    * 10.0) as u32
            } else {
                0
            };
            let grain = match recipe["grainStructure"].as_str() {
                Some("soft") => 1,
                Some("clumped") => 2,
                Some("speckled") => 3,
                Some("gritty") => 4,
                _ => 0,
            };
            let color =
                (recipe["grainColor"].as_f64().unwrap_or(0.0) / 100.0).clamp(0.0, 1.0) as f32;
            let lut = if visible {
                (recipe["lutIntensity"].as_f64().unwrap_or(100.0) as f32 / 100.0).min(2.0)
            } else {
                1.0
            };
            assert_eq!(actual.vignette_style, [style, highlights, 0, 0]);
            assert_eq!(actual.grain_structure, grain);
            assert_eq!(actual.grain_color.to_bits(), color.to_bits());
            assert_eq!(
                actual.grain_version,
                recipe["grainVersion"].as_u64().unwrap_or(1).clamp(1, 3) as u32
            );
            assert_eq!(
                actual.has_lut,
                u32::from(visible && recipe["lutPath"].is_string())
            );
            assert_eq!(actual.lut_intensity.to_bits(), lut.to_bits());
            assert_eq!(
                actual.lut_is_scene_referred,
                u32::from(visible && recipe["lutIsSceneReferred"].as_bool().unwrap_or(false))
            );
        }
    }
}
