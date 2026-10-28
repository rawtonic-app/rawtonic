//! What a recipe says about AI and provenance: areas a generative model
//! made (exports carry the IPTC Digital Source Type for them), AI
//! selections and AI Denoise, and AI results a photograph still waits for.
//! The render and export read these predicates (`rawtonic_engine` re-exports
//! them), and the library indexes them (`ai_facts`, catalog schema 17), so
//! what the Library finds is what an export marks.
use serde_json::Value;

/// Mask component types the AI detects on each photograph: Subject, Sky,
/// depth range and People (detected again when the photograph is opened) and
/// an Object selection (pointed at again by the user). Pasted or synced onto
/// another photograph they never keep the source's bitmap.
pub const DETECTED_TYPES: [&str; 5] = [
    "ai-subject",
    "ai-foreground",
    "ai-sky",
    "ai-depth",
    "ai-people",
];

/// The parameter that marks a component pasted or synced from another
/// photograph whose detection has not run on this one yet.
pub const DETECT_PENDING: &str = "detectPending";

/// Whether a component's parameters carry a bitmap.
pub fn has_bitmap(parameters: &Value) -> bool {
    ["maskDataBase64", "mask_data_base64"]
        .iter()
        .any(|key| parameters.get(key).is_some_and(|v| !v.is_null()))
}

/// Whether a component's parameters leave nothing to detect: a People
/// selection without parts (renders nothing on any photograph).
pub fn nothing_to_detect(parameters: &Value) -> bool {
    parameters
        .get("parts")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
}

fn shown(value: &Value) -> bool {
    value.get("visible").and_then(Value::as_bool) != Some(false)
}

/// The shown components of shown mask layers.
fn shown_components(adjustments: &Value) -> impl Iterator<Item = (&Value, &Value)> {
    adjustments
        .get("masks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|layer| shown(layer))
        .flat_map(|layer| {
            layer
                .get("subMasks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|sub| shown(sub))
                .map(move |sub| (layer, sub))
        })
}

/// A component of a shown layer waiting for its AI detection on this
/// photograph (`DETECT_PENDING` without a bitmap, something to detect): its
/// layer's name and its type. Rendered without it, the layer would apply its adjustments to the
/// wrong part of the photograph, or nowhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDetection {
    pub layer: String,
    pub kind: String,
}

/// The components of shown layers waiting for their AI detection.
pub fn pending_detections(adjustments: &Value) -> Vec<PendingDetection> {
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    shown_components(adjustments)
        .filter(|(_, sub)| {
            sub.get("parameters").is_some_and(|p| {
                p.get(DETECT_PENDING).and_then(Value::as_bool) == Some(true)
                    && !has_bitmap(p)
                    && !nothing_to_detect(p)
            })
        })
        .map(|(layer, sub)| PendingDetection {
            layer: text(layer, "name"),
            kind: text(sub, "type"),
        })
        .collect()
}

/// The recipe's AI Denoise amount (`aiDenoise`, 0 … 100) as 0 … 1; zero
/// when the Detail section or its Noise reduction tool is switched off.
pub fn ai_denoise_amount(adjustments: &Value) -> f32 {
    let visibility = &adjustments["sectionVisibility"];
    let off = |key: &str| visibility[key].as_bool() == Some(false);
    if off("details") || off("noiseReduction") {
        return 0.0;
    }
    (adjustments["aiDenoise"].as_f64().unwrap_or(0.0) as f32 / 100.0).clamp(0.0, 1.0)
}

/// The newest AI Denoise version this build renders.
pub const AI_DENOISE_LATEST: u8 = 3;

/// Sub-mask types of the inherited patches that no model makes: clone and
/// heal copy pixels, liquify warps them, retouch smooths them.
const HAND_MADE_PATCH_TYPES: [&str; 4] = ["clone", "heal", "liquify", "retouch"];

/// Whether a model makes `patch`'s pixels: not a hand-made type only
/// (clone, heal, liquify, retouch) and not marked `"generated": false`
/// (Fill edges' patch-based completion).
fn model_patch(patch: &Value) -> bool {
    let hand_made = patch
        .get("subMasks")
        .and_then(Value::as_array)
        .is_some_and(|subs| {
            !subs.is_empty()
                && subs.iter().all(|sub| {
                    sub.get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| HAND_MADE_PATCH_TYPES.contains(&t))
                })
        });
    !hand_made && patch.get("generated").and_then(Value::as_bool) != Some(false)
}

/// Whether `patch` has pixels (its fill finished).
fn has_pixels(patch: &Value) -> bool {
    patch
        .get("patchData")
        .and_then(|data| data.get("color"))
        .and_then(Value::as_str)
        .is_some_and(|color| !color.is_empty())
}

/// The shown patches a model makes.
fn shown_model_patches(adjustments: &Value) -> impl Iterator<Item = &Value> {
    adjustments
        .get("aiPatches")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|patch| model_patch(patch) && shown(patch))
}

/// Whether the recipe shows areas made by a generative model (visible AI
/// patches with pixels: object removal, generative fill). Exports of such
/// recipes are marked (`DIGITAL_SOURCE_COMPOSITE_AI`). Patches whose every
/// sub-mask is a hand-made type (clone, heal, liquify, retouch) do not
/// count: no model made their pixels. Neither do patches marked
/// `"generated": false` (Fill edges' patch-based completion).
pub fn has_generated_areas(adjustments: &Value) -> bool {
    shown_model_patches(adjustments).any(has_pixels)
}

/// The model that filled a patch, as Remove records it in the patch's
/// `patchData.model` (since 2026-10-27; earlier fills do not say).
pub mod fill_model {
    /// Moebius guided the completion (large holes).
    pub const MOEBIUS: &str = "moebius";
    /// MI-GAN guided the completion.
    pub const MI_GAN: &str = "mi-gan";
    /// An external generative service the user connected filled it (no
    /// longer written since that path was removed on 2026-10-28; read so
    /// recipes filled before keep their fact).
    pub const EXTERNAL: &str = "external";
}

/// One fact the library indexes per recipe (`ai_facts`): its bit and the
/// name a search rule uses for it.
pub struct AiFact {
    pub bit: u32,
    pub name: &'static str,
}

/// Shown areas a generative model made: exports are marked
/// (`has_generated_areas`).
pub const GENERATED: u32 = 1;
/// A shown AI selection (`DETECTED_TYPES`) in a shown mask layer.
pub const AI_MASK: u32 = 1 << 1;
/// AI Denoise is on (`ai_denoise_amount`; it develops RAW mosaics only).
pub const AI_DENOISE: u32 = 1 << 2;
/// A generated area Moebius guided (`fill_model`).
pub const FILL_MOEBIUS: u32 = 1 << 3;
/// A generated area MI-GAN guided.
pub const FILL_MI_GAN: u32 = 1 << 4;
/// A generated area an external service filled.
pub const FILL_EXTERNAL: u32 = 1 << 5;
/// A generated area whose model is not recorded (filled before Remove
/// recorded it).
pub const FILL_UNRECORDED: u32 = 1 << 6;
/// An AI selection waiting for its detection on this photograph
/// (`pending_detections`: pasted, synced or from a preset); exports refuse
/// it until it is detected.
pub const PENDING_DETECTION: u32 = 1 << 7;
/// A shown model patch without its pixels (a fill that never finished):
/// it changes nothing until it is filled again.
pub const MISSING_FILL: u32 = 1 << 8;
/// AI Denoise at an older version than this build's newest
/// (`AI_DENOISE_LATEST`; kept so saved recipes render as before).
pub const OLDER_DENOISE: u32 = 1 << 9;

/// Every fact with its rule name, in the order the Library lists them.
pub const AI_FACTS: [AiFact; 10] = [
    AiFact {
        bit: GENERATED,
        name: "generated",
    },
    AiFact {
        bit: FILL_MOEBIUS,
        name: "fillMoebius",
    },
    AiFact {
        bit: FILL_MI_GAN,
        name: "fillMiGan",
    },
    AiFact {
        bit: FILL_EXTERNAL,
        name: "fillExternal",
    },
    AiFact {
        bit: FILL_UNRECORDED,
        name: "fillUnrecorded",
    },
    AiFact {
        bit: AI_MASK,
        name: "aiMask",
    },
    AiFact {
        bit: AI_DENOISE,
        name: "aiDenoise",
    },
    AiFact {
        bit: PENDING_DETECTION,
        name: "pendingDetection",
    },
    AiFact {
        bit: MISSING_FILL,
        name: "missingFill",
    },
    AiFact {
        bit: OLDER_DENOISE,
        name: "olderDenoise",
    },
];

/// Facts that only RAW files have (AI Denoise develops the mosaic; on any
/// other file the recipe's AI Denoise does nothing).
pub const RAW_ONLY: u32 = AI_DENOISE | OLDER_DENOISE;

/// The AI facts of a recipe's adjustments (bits above).
pub fn ai_facts(adjustments: &Value) -> u32 {
    let mut facts = 0;
    for patch in shown_model_patches(adjustments) {
        if !has_pixels(patch) {
            facts |= MISSING_FILL;
            continue;
        }
        facts |= GENERATED;
        facts |= match patch
            .get("patchData")
            .and_then(|data| data.get("model"))
            .and_then(Value::as_str)
        {
            Some(fill_model::MOEBIUS) => FILL_MOEBIUS,
            Some(fill_model::MI_GAN) => FILL_MI_GAN,
            Some(fill_model::EXTERNAL) => FILL_EXTERNAL,
            _ => FILL_UNRECORDED,
        };
    }
    if shown_components(adjustments).any(|(_, sub)| {
        sub.get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| DETECTED_TYPES.contains(&t))
    }) {
        facts |= AI_MASK;
    }
    if !pending_detections(adjustments).is_empty() {
        facts |= PENDING_DETECTION;
    }
    if ai_denoise_amount(adjustments) > 0.0 {
        facts |= AI_DENOISE;
        if adjustments["aiDenoiseVersion"].as_u64().unwrap_or(1) < u64::from(AI_DENOISE_LATEST) {
            facts |= OLDER_DENOISE;
        }
    }
    facts
}

/// `ai_facts` of a stored recipe (`{"adjustments": …}`, as the library
/// keeps them) or an editor state (`{"adjustments": …}` too, or the
/// recipe's `editor_state`), as JSON text. Text that names none of the
/// keys the facts read has none, without being parsed.
pub fn ai_facts_of_text(text: &str) -> u32 {
    if !["aiPatches", "aiDenoise", "\"ai-"]
        .iter()
        .any(|key| text.contains(key))
    {
        return 0;
    }
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return 0;
    };
    let adjustments = match value.get("adjustments") {
        Some(adjustments) if adjustments.is_object() => adjustments,
        _ => &value["editor_state"]["adjustments"],
    };
    ai_facts(adjustments)
}

/// The IPTC NewsCodes vocabulary of Digital Source Types.
const SOURCE_TYPE_VOCABULARY: [&str; 2] = [
    "http://cv.iptc.org/newscodes/digitalsourcetype/",
    "https://cv.iptc.org/newscodes/digitalsourcetype/",
];

/// An IPTC Digital Source Type as the library keeps it: the code of the
/// IPTC vocabulary (`trainedAlgorithmicMedia`), any other value as given
/// (trimmed); empty for none.
pub fn source_type_code(value: &str) -> String {
    let value = value.trim();
    SOURCE_TYPE_VOCABULARY
        .iter()
        .find_map(|prefix| value.strip_prefix(prefix))
        .unwrap_or(value)
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn patch(model: Option<&str>) -> Value {
        let mut data = json!({"color": "AAAA", "mask": "BBBB"});
        if let Some(model) = model {
            data["model"] = json!(model);
        }
        json!({"id": "p", "visible": true, "subMasks": [{"type": "brush"}], "patchData": data})
    }

    #[test]
    fn names_each_fill_model_and_old_fills() {
        for (model, bit) in [
            (Some("moebius"), FILL_MOEBIUS),
            (Some("mi-gan"), FILL_MI_GAN),
            (Some("external"), FILL_EXTERNAL),
            (None, FILL_UNRECORDED),
        ] {
            let facts = ai_facts(&json!({"aiPatches": [patch(model)]}));
            assert_eq!(facts, GENERATED | bit, "{model:?}");
        }
    }

    #[test]
    fn generated_is_what_exports_mark() {
        let hidden = json!({"aiPatches": [{"visible": false, "patchData": {"color": "A"}}]});
        let clone =
            json!({"aiPatches": [{"subMasks": [{"type": "clone"}], "patchData": {"color": "A"}}]});
        let fill_edges = json!({"aiPatches": [{"generated": false, "patchData": {"color": "A"}}]});
        for recipe in [&hidden, &clone, &fill_edges] {
            assert!(!has_generated_areas(recipe));
            assert_eq!(ai_facts(recipe), 0, "{recipe}");
        }
        let unfinished = json!({"aiPatches": [{"patchData": null}]});
        assert!(!has_generated_areas(&unfinished));
        assert_eq!(ai_facts(&unfinished), MISSING_FILL);
    }

    #[test]
    fn masks_waiting_and_denoise() {
        let masks = json!({"masks": [
            {"visible": true, "subMasks": [{"type": "ai-sky", "parameters": {"maskDataBase64": "x"}}]},
        ]});
        assert_eq!(ai_facts(&masks), AI_MASK);
        let pasted = json!({"masks": [
            {"subMasks": [{"type": "ai-subject", "parameters": {"detectPending": true}}]},
        ]});
        assert_eq!(ai_facts(&pasted), AI_MASK | PENDING_DETECTION);
        let hidden = json!({"masks": [
            {"visible": false, "subMasks": [{"type": "ai-subject", "parameters": {"detectPending": true}}]},
        ]});
        assert_eq!(ai_facts(&hidden), 0);
        let brush = json!({"masks": [{"subMasks": [{"type": "brush"}]}]});
        assert_eq!(ai_facts(&brush), 0);
        assert_eq!(
            ai_facts(&json!({"aiDenoise": 40, "aiDenoiseVersion": 3})),
            AI_DENOISE
        );
        assert_eq!(
            ai_facts(&json!({"aiDenoise": 40})),
            AI_DENOISE | OLDER_DENOISE
        );
        let off = json!({"aiDenoise": 40, "sectionVisibility": {"details": false}});
        assert_eq!(ai_facts(&off), 0);
    }

    #[test]
    fn reads_stored_recipes_and_editor_states() {
        let adjustments = json!({"aiDenoise": 30, "aiDenoiseVersion": 3});
        let recipe = json!({"schema_version": 1, "adjustments": adjustments, "editor_state": {}});
        let state = json!({"version": 1, "adjustments": adjustments});
        let wrapped = json!({"adjustments": null, "editor_state": {"adjustments": adjustments}});
        for value in [recipe, state, wrapped] {
            assert_eq!(ai_facts_of_text(&value.to_string()), AI_DENOISE, "{value}");
        }
        assert_eq!(ai_facts_of_text("{\"adjustments\":{\"exposure\":1}}"), 0);
        assert_eq!(ai_facts_of_text("aiPatches not json"), 0);
    }

    #[test]
    fn source_types_are_kept_as_their_code() {
        assert_eq!(
            source_type_code(
                " http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia "
            ),
            "trainedAlgorithmicMedia"
        );
        assert_eq!(
            source_type_code(crate::DIGITAL_SOURCE_COMPOSITE_AI),
            "compositeWithTrainedAlgorithmicMedia"
        );
        assert_eq!(
            source_type_code("https://example.com/other"),
            "https://example.com/other"
        );
        assert_eq!(source_type_code("  "), "");
    }
}
