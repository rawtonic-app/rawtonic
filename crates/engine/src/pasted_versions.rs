//! Version keys carried by pasted settings (tool copy, presets, Edit all
//! selected): a version key goes with the values it renders only when
//! nothing on the destination that the paste leaves alone renders with it
//! (notes: reference/vkl-version-keys.md).
//!
//! A version key (`structureVersion`, `toneVersion`, …) is photo-wide: the
//! photo's slider and its layers' render with it, added together and run
//! once per pixel. Carrying the source's key with a pasted value would also
//! change the destination's layers (and other settings) that use the tool.
//! So the destination keeps its key whenever something it keeps renders
//! with it, and the pasted values are re-expressed in its response: exact
//! where the versions differ only in the slider's travel (Whites 16 / 17,
//! Blacks 16 / 18 as the RAW look renders them, Structure 1 / 2), as the
//! nearest value where the algorithm differs (the
//! measured error per key: docs/RECIPE_SCHEMA.md). With nothing that uses
//! it, the key goes along and the paste renders as on the source.
//!
//! The editor applies the same rules to the open photograph
//! (`expressPastedSettings`, src/utils/pastedVersions.ts); both are checked
//! against `tests/data/pasted_versions.json`.

use serde_json::Value;

use crate::image_processing as ip;
use crate::recipe_versions::{self as rv, ToneVersion};

/// The photograph pasted onto.
#[derive(Clone, Copy, Debug)]
pub struct PasteTarget<'a> {
    /// Its recipe before the paste (after a style it replaces is taken back).
    pub recipe: &'a Value,
    /// It has no edit yet: it takes whatever the paste brings.
    pub is_new: bool,
    /// A RAW file: Sharpening and Luminance noise reduction without a value
    /// are its defaults (`RAW_DEFAULT_*`), which render with the key.
    pub is_raw: bool,
    /// Pasted layers replace all of its own (else they join them, a pasted
    /// layer replacing its own of the same id: `masks::paste_layers`).
    pub replace_layers: bool,
    /// A style: its nested settings join the photograph's own setting by
    /// setting (`applied_style::merged`, a colour mixer's other colours stay)
    /// instead of replacing them.
    pub merge_groups: bool,
}

/// What on a photograph renders with a version key besides pasted values.
#[derive(Clone, Copy)]
enum Users {
    /// Everything: the key also changes how the photograph renders without
    /// any of its sliders (tone versions 6 and 11 change Clarity's base and
    /// the white roll-off; colour noise versions the decode).
    Always,
    /// These sliders of the photograph (`global`) and of its layers (`layer`).
    Sliders {
        global: &'static [&'static str],
        layer: &'static [&'static str],
    },
}

/// A slider's value at one version re-expressed at another.
type Translate = fn(slider: &str, value: f64, from: u64, to: u64) -> f64;

/// The version a recipe renders with (`recipe_versions`: its default when
/// missing, its clamp); `is_raw`: the photograph is a RAW file.
type Effective = fn(recipe: &Value, is_raw: bool) -> u64;

struct VersionKey {
    key: &'static str,
    effective: Effective,
    users: Users,
    /// The sliders (photograph and layers) whose values the key's versions
    /// render differently, with the translation between versions (none:
    /// the value stays, the nearest the destination's response offers).
    translated: &'static [&'static str],
    translate: Option<Translate>,
}

/// Every photo-wide key whose meaning applies to values a paste carries.
const KEYS: &[VersionKey] = &[
    VersionKey {
        key: ip::TONE_VERSION_KEY,
        effective: |r, _| rv::ToneVersion::of(r).get(),
        users: Users::Always,
        translated: &["whites", "blacks"],
        translate: Some(translate_tone),
    },
    VersionKey {
        key: crate::color_noise::VERSION_KEY,
        effective: |r, raw| rv::ColorNoiseVersion::of(r, raw).get(),
        users: Users::Always,
        translated: &[],
        translate: None,
    },
    VersionKey {
        key: ip::CLARITY_VERSION_KEY,
        effective: |r, _| rv::ClarityVersion::of(r).get(),
        users: Users::Sliders {
            global: &["clarity"],
            layer: &["clarity"],
        },
        translated: &[],
        translate: None,
    },
    VersionKey {
        key: ip::STRUCTURE_VERSION_KEY,
        effective: |r, _| rv::StructureVersion::of(r).get(),
        users: Users::Sliders {
            global: &["structure"],
            layer: &["structure"],
        },
        translated: &["structure"],
        translate: Some(translate_structure),
    },
    VersionKey {
        key: ip::DEHAZE_VERSION_KEY,
        effective: |r, _| rv::DehazeVersion::of(r).get(),
        users: Users::Sliders {
            global: &["dehaze"],
            layer: &["dehaze"],
        },
        translated: &[],
        translate: None,
    },
    VersionKey {
        key: ip::VIBRANCE_VERSION_KEY,
        effective: |r, _| rv::VibranceVersion::of(r).get(),
        users: Users::Sliders {
            global: &["vibrance"],
            layer: &["vibrance"],
        },
        translated: &[],
        translate: None,
    },
    VersionKey {
        key: ip::TEMPERATURE_VERSION_KEY,
        effective: |r, _| rv::TemperatureVersion::of(r).get(),
        users: Users::Sliders {
            global: &["temperature"],
            layer: &["temperature"],
        },
        translated: &["temperature"],
        translate: Some(translate_temperature),
    },
    VersionKey {
        key: ip::TINT_VERSION_KEY,
        effective: |r, _| rv::TintVersion::of(r).get(),
        users: Users::Sliders {
            global: &["tint"],
            layer: &["tint"],
        },
        translated: &["tint"],
        translate: Some(translate_tint),
    },
    VersionKey {
        key: ip::HSL_VERSION_KEY,
        effective: |r, _| rv::HslVersion::of(r).get(),
        users: Users::Sliders {
            global: &["hsl"],
            layer: &["hsl"],
        },
        translated: &[],
        translate: None,
    },
    VersionKey {
        key: ip::SHARPEN_VERSION_KEY,
        effective: |r, _| rv::SharpenVersion::of(r).get(),
        users: Users::Sliders {
            global: &["sharpness"],
            layer: &["sharpness"],
        },
        translated: &[],
        translate: None,
    },
    VersionKey {
        key: ip::LUMA_NOISE_VERSION_KEY,
        effective: |r, _| rv::LumaNoiseVersion::of(r).get(),
        users: Users::Sliders {
            global: &["lumaNoiseReduction"],
            layer: &["lumaNoiseReduction"],
        },
        translated: &[],
        translate: None,
    },
    VersionKey {
        key: ip::DEFRINGE_VERSION_KEY,
        effective: |r, _| rv::DefringeVersion::of(r).get(),
        users: Users::Sliders {
            global: &["defringePurpleAmount", "defringeGreenAmount"],
            layer: &["defringe"],
        },
        translated: &[],
        translate: None,
    },
];

/// Sliders a RAW file renders with a default when its recipe has none.
const RAW_DEFAULTED: &[&str] = &["sharpness", "lumaNoiseReduction"];

/// Whether a slider value renders anything: a number off 0, a colour mixer
/// with a slider off 0.
fn moved(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.as_f64().is_some_and(|v| v != 0.0),
        Value::Object(channels) => channels.values().any(|channel| {
            channel
                .as_object()
                .is_some_and(|c| c.values().any(|v| v.as_f64().is_some_and(|v| v != 0.0)))
        }),
        _ => false,
    }
}

/// Whether a setting `own` keeps, merged by setting with `pasted`, a
/// moved value the paste does not set (a colour mixer's other colours).
fn keeps_moved_leaves(own: &Value, pasted: &Value) -> bool {
    match (own, pasted) {
        (Value::Object(own), Value::Object(pasted)) => {
            own.iter().any(|(key, value)| match pasted.get(key) {
                None => moved_anywhere(value),
                Some(pasted) => keeps_moved_leaves(value, pasted),
            })
        }
        _ => false,
    }
}

/// A number off 0 anywhere in `value`.
fn moved_anywhere(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.as_f64().is_some_and(|v| v != 0.0),
        Value::Object(map) => map.values().any(moved_anywhere),
        _ => false,
    }
}

fn layer_id(layer: &Value) -> Option<&str> {
    layer.get("id").and_then(Value::as_str)
}

/// The destination's layers a paste leaves in place.
fn kept_layers<'a>(pasted: &Value, target: &PasteTarget<'a>) -> Vec<&'a Value> {
    let own = target
        .recipe
        .get("masks")
        .and_then(Value::as_array)
        .map(|layers| layers.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    let Some(pasted_layers) = pasted.get("masks") else {
        return own;
    };
    if target.replace_layers {
        return Vec::new();
    }
    let ids: Vec<&str> = pasted_layers
        .as_array()
        .map(|layers| layers.iter().filter_map(layer_id).collect())
        .unwrap_or_default();
    own.into_iter()
        .filter(|layer| layer_id(layer).is_none_or(|id| !ids.contains(&id)))
        .collect()
}

/// Whether something the paste leaves on the destination renders with `key`.
fn used_by_kept(key: &VersionKey, pasted: &Value, target: &PasteTarget) -> bool {
    let (global, layer) = match key.users {
        Users::Always => return true,
        Users::Sliders { global, layer } => (global, layer),
    };
    let global_used =
        global.iter().any(
            |slider| match (target.recipe.get(*slider), pasted.get(*slider)) {
                (Some(own), Some(pasted)) => target.merge_groups && keeps_moved_leaves(own, pasted),
                (_, Some(_)) => false,
                (Some(own), None) => moved(own),
                (None, None) => target.is_raw && RAW_DEFAULTED.contains(slider),
            },
        );
    global_used
        || kept_layers(pasted, target).iter().any(|own| {
            layer.iter().any(|slider| {
                own.pointer(&format!("/adjustments/{slider}"))
                    .is_some_and(moved)
            })
        })
}

/// `pasted` (settings carried from another photograph, a preset, Edit all
/// selected) as `target` takes them: Whites carried as their amount in its
/// tone-slider response ([`ip::express_pasted_whites`]), and every version
/// key that something `target` keeps renders with stated as `target`
/// renders it, the pasted values re-expressed in that response.
pub fn express_pasted(pasted: &mut Value, target: &PasteTarget) {
    ip::express_pasted_whites(pasted, target.recipe);
    if target.is_new {
        return;
    }
    for key in KEYS {
        let Some(from) = pasted.get(key.key).and_then(Value::as_u64) else {
            continue;
        };
        let to = (key.effective)(target.recipe, target.is_raw);
        if from == to || !used_by_kept(key, pasted, target) {
            continue;
        }
        let Some(map) = pasted.as_object_mut() else {
            return;
        };
        // Stated, as the photograph renders it: a missing key may read
        // otherwise once pasted values join (colour noise without an amount).
        map.insert(key.key.to_string(), serde_json::json!(to));
        let Some(translate) = key.translate else {
            continue;
        };
        let express = |value: &mut Value, slider: &str| {
            if let Some(v) = value.as_f64() {
                let expressed = translate(slider, v, from, to);
                if expressed != v {
                    *value = serde_json::json!(expressed);
                }
            }
        };
        for slider in key.translated {
            if let Some(value) = map.get_mut(*slider) {
                express(value, slider);
            }
            if let Some(layers) = map.get_mut("masks").and_then(Value::as_array_mut) {
                for layer in layers {
                    if let Some(value) = layer.pointer_mut(&format!("/adjustments/{slider}")) {
                        express(value, slider);
                    }
                }
            }
        }
    }
}

/// `value` of `slider` at version `from` of `key` re-expressed at version
/// `to`, as a paste that keeps the destination's key does (the value itself
/// where the key's versions take it alike, or where no translation
/// exists). For measurements and tests.
pub fn translate(key: &str, slider: &str, value: f64, from: u64, to: u64) -> f64 {
    KEYS.iter()
        .find(|k| k.key == key && k.translated.contains(&slider))
        .and_then(|k| k.translate)
        .map_or(value, |translate| translate(slider, value, from, to))
}

/// Whites and Blacks: the amount version `from` renders, as `to`'s slider
/// value (exact: every response is a monotone bijection of the amount;
/// Blacks deeper than version 16 / 17 reach end at −100 there).
fn translate_tone(slider: &str, value: f64, from: u64, to: u64) -> f64 {
    let from = ToneVersion::of(&serde_json::json!({ ip::TONE_VERSION_KEY: from }));
    let to = ToneVersion::of(&serde_json::json!({ ip::TONE_VERSION_KEY: to }));
    if slider == "blacks" {
        to.blacks_value(from.blacks_amount(value))
    } else {
        to.whites_value(from.whites_amount(value))
    }
}

/// Structure as the strength of version 2's travel: version 1 renders its
/// value as the amount; version 3 renders a value with version 2's strength
/// on the references' scale (`STRUCTURE3_RAISE`), its own amount scaled for
/// its edge-aware base. Exact between 1 and 2 (the amount, to the f32 the
/// renderer takes); v1 amounts beyond version 2's reach (+29.5 / −41) end at
/// ±100.
fn translate_structure(_: &str, value: f64, from: u64, to: u64) -> f64 {
    if (from <= 1) == (to <= 1) {
        return value;
    }
    let strength = if from <= 1 {
        value
    } else {
        ip::structure_amount_v2(value)
    };
    if to <= 1 {
        strength
    } else {
        structure_value_v2(strength)
    }
}

/// The version-2 Structure slider value whose amount is `amount` (clamped
/// to the slider's reach), nudged so the renderer's f32 amount is the one
/// asked for.
pub fn structure_value_v2(amount: f64) -> f64 {
    let (a, b, reach) = if amount >= 0.0 {
        (0.45, -0.00155, ip::structure_amount_v2(100.0))
    } else {
        (0.6, -0.0019, -ip::structure_amount_v2(-100.0))
    };
    let target = amount.abs().min(reach);
    if target == 0.0 {
        return 0.0;
    }
    // a·x + b·x² = target, the root in 0..=100 (b < 0).
    let x = (-a + (a * a + 4.0 * b * target).sqrt()) / (2.0 * b);
    let mut value = amount.signum() * x.min(100.0);
    let wanted = (amount.signum() * target) as f32;
    for _ in 0..8 {
        let got = ip::structure_amount_v2(value) as f32;
        if got == wanted {
            break;
        }
        let step = f64::EPSILON * value.abs().max(1.0);
        value += if got < wanted { step } else { -step };
    }
    value
}

/// Temperature: version 2 compresses warming (t − t²/16 of version 1's
/// travel at t = slider / 25, so its +100 is version 1's +75) and couples
/// brightness and magenta, which version 1 cannot render; cooling travels
/// alike. The value with the same warm–cool shift.
fn translate_temperature(_: &str, value: f64, from: u64, to: u64) -> f64 {
    if value <= 0.0 || from.min(2) == to.min(2) {
        return value;
    }
    if to >= 2 {
        // Version 1's warm value as version 2's: s − s²/400 = value.
        if value >= 75.0 {
            100.0
        } else {
            200.0 - (40000.0 - 400.0 * value).sqrt()
        }
    } else {
        let s = value.min(100.0);
        s - s * s / 400.0
    }
}

/// Tint: version 2 travels ×1.9 towards magenta and ×1.55 towards green of
/// version 1 (and brightens towards magenta, which version 1 cannot
/// render). The value with the same tint, within ±100.
fn translate_tint(_: &str, value: f64, from: u64, to: u64) -> f64 {
    if from.min(2) == to.min(2) {
        return value;
    }
    let factor = if value > 0.0 { 1.9 } else { 1.55 };
    if to >= 2 {
        value / factor
    } else {
        (value * factor).clamp(-100.0, 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The cases the editor's `expressPastedSettings` is checked against too.
    #[test]
    fn pasted_versions_follow_the_shared_cases() {
        let cases: Value =
            serde_json::from_str(include_str!("../tests/data/pasted_versions.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let mut pasted = case["pasted"].clone();
            express_pasted(
                &mut pasted,
                &PasteTarget {
                    recipe: &case["destination"],
                    is_new: case["isNew"].as_bool().unwrap_or(false),
                    is_raw: case["isRaw"].as_bool().unwrap_or(false),
                    replace_layers: case["replaceLayers"].as_bool().unwrap_or(false),
                    merge_groups: case["mergeGroups"].as_bool().unwrap_or(false),
                },
            );
            assert_close(&pasted, &case["expected"], name);
        }
    }

    fn assert_close(got: &Value, want: &Value, name: &str) {
        match (got, want) {
            (Value::Number(a), Value::Number(b)) => {
                let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
                assert!((a - b).abs() <= 1e-6, "{name}: {a} != {b}");
            }
            (Value::Object(a), Value::Object(b)) => {
                let keys = |m: &serde_json::Map<String, Value>| {
                    let mut k: Vec<_> = m.keys().cloned().collect();
                    k.sort();
                    k
                };
                assert_eq!(keys(a), keys(b), "{name}: keys");
                for (k, v) in a {
                    assert_close(v, &b[k], &format!("{name}.{k}"));
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.len(), b.len(), "{name}: length");
                for (i, (x, y)) in a.iter().zip(b).enumerate() {
                    assert_close(x, y, &format!("{name}[{i}]"));
                }
            }
            _ => assert_eq!(got, want, "{name}"),
        }
    }

    /// Structure between versions 1 and 2 keeps the amount the renderer
    /// takes, to the bit of its f32, over the whole slider.
    #[test]
    fn structure_between_versions_1_and_2_keeps_the_amount_exactly() {
        for i in -1000..=1000 {
            let value = f64::from(i) / 10.0;
            let amount = ip::structure_amount_v2(value);
            let as_v1 = translate_structure("", value, 2, 1);
            assert_eq!(as_v1.to_bits(), amount.to_bits(), "{value}");
            let back = translate_structure("", as_v1, 1, 2);
            assert_eq!(
                (ip::structure_amount_v2(back) as f32).to_bits(),
                (amount as f32).to_bits(),
                "{value}"
            );
        }
        // Version 1's amounts beyond version 2's reach end at its ends.
        assert!((translate_structure("", 60.0, 1, 2) - 100.0).abs() < 1e-9);
        assert!((translate_structure("", -80.0, 1, 2) + 100.0).abs() < 1e-9);
        // Versions 2 and 3 share the slider's strength.
        assert_eq!(translate_structure("", 37.0, 2, 3), 37.0);
        assert_eq!(translate_structure("", 37.0, 3, 2), 37.0);
    }

    #[test]
    fn temperature_and_tint_translations_invert_each_other() {
        for i in -100..=100 {
            let v = f64::from(i);
            let t = translate_temperature("", translate_temperature("", v, 2, 1), 1, 2);
            assert!((t - v).abs() < 1e-9, "temperature {v}: {t}");
            let back = translate_tint("", translate_tint("", v, 1, 2), 2, 1);
            assert!((back - v).abs() < 1e-9, "tint {v}: {back}");
        }
        assert_eq!(translate_temperature("", 100.0, 2, 1), 75.0);
        assert_eq!(translate_temperature("", 90.0, 1, 2), 100.0);
        assert_eq!(translate_tint("", 100.0, 2, 1), 100.0);
    }

    /// A destination whose layer uses Structure keeps its travel and its
    /// layer's value; one without any keeps nothing of its own.
    #[test]
    fn a_layer_using_the_tool_keeps_the_destination_version() {
        let destination = json!({
            "structureVersion": 1,
            "masks": [{"id": "dodge", "adjustments": {"structure": 40}}]
        });
        let mut pasted = json!({"structure": 50, "structureVersion": 3});
        express_pasted(
            &mut pasted,
            &PasteTarget {
                recipe: &destination,
                is_new: false,
                is_raw: true,
                replace_layers: false,
                merge_groups: false,
            },
        );
        assert_eq!(
            pasted,
            json!({"structure": ip::structure_amount_v2(50.0), "structureVersion": 1})
        );
        let mut pasted = json!({"structure": 50, "structureVersion": 3});
        express_pasted(
            &mut pasted,
            &PasteTarget {
                recipe: &json!({"structureVersion": 1, "exposure": 1}),
                is_new: false,
                is_raw: true,
                replace_layers: false,
                merge_groups: false,
            },
        );
        assert_eq!(pasted, json!({"structure": 50, "structureVersion": 3}));
    }
}
