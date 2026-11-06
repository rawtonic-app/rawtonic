//! Detail values as stored, defaulted and switched, without clamping.
//!
//! Ranges follow StudioDevelop's sliders. Sharpening amount reaches 150
//! from response 2 (100 for response 1); noise refinements are 0–100.
//! The renderer keeps its existing clamps and f32 conversion order.

use crate::tone::{Amount, slider_value};
use crate::ts::{TsField, TsSection};
use crate::{Key, Section, StoredRef};

/// Import defaults for RAW files (frontend `RAW_DETAIL_DEFAULTS`):
/// Lightroom's sharpening and colour noise reduction, and a light luminance
/// noise reduction: Camera Raw and Capture One render visibly cleaner at
/// their defaults (Camera Raw's Luminance 0 has 0.5–0.8 of Rawtonic's
/// unfiltered noise at ISO 6400–12800); 15 brings Rawtonic's absolute noise
/// near Camera Raw's and below Capture One's default on an ISO 200 GFX file
/// (notes: reference/detail.md).
pub const RAW_DEFAULT_SHARPNESS: f64 = 40.0;
pub const RAW_DEFAULT_COLOR_NOISE_REDUCTION: f64 = 25.0;
pub const RAW_DEFAULT_LUMA_NOISE_REDUCTION: f64 = 15.0;

pub const SHARPNESS: Key<f64> = Key::new("sharpness", 0.0, 1);
pub const SHARPNESS_THRESHOLD: Key<f64> = Key::new("sharpnessThreshold", 15.0, 1);
pub const SHARPEN_RADIUS: Key<f64> = Key::new("sharpenRadius", 1.0, 1);
pub const SHARPEN_DETAIL: Key<f64> = Key::new("sharpenDetail", 25.0, 1);
pub const SHARPEN_MASKING: Key<f64> = Key::new("sharpenMasking", 0.0, 1);
pub const SHARPNESS_FALLOFF: Key<f64> = Key::new("sharpnessFalloff", 0.0, 1);
pub const LUMA_NOISE_REDUCTION: Key<f64> = Key::new("lumaNoiseReduction", 0.0, 1);
pub const COLOR_NOISE_REDUCTION: Key<f64> = Key::new("colorNoiseReduction", 0.0, 1);
pub const LUMA_NOISE_DETAIL: Key<f64> = Key::new("lumaNoiseDetail", 50.0, 1);
pub const LUMA_NOISE_CONTRAST: Key<f64> = Key::new("lumaNoiseContrast", 0.0, 1);
pub const COLOR_NOISE_DETAIL: Key<f64> = Key::new("colorNoiseDetail", 50.0, 1);
pub const COLOR_NOISE_SMOOTHNESS: Key<f64> = Key::new("colorNoiseSmoothness", 50.0, 1);
pub const CLARITY: Key<f64> = Key::new("clarity", 0.0, 1);
pub const DEHAZE: Key<f64> = Key::new("dehaze", 0.0, 1);
pub const STRUCTURE: Key<f64> = Key::new("structure", 0.0, 1);
pub const CHROMATIC_ABERRATION_RED_CYAN: Key<f64> = Key::new("chromaticAberrationRedCyan", 0.0, 1);
pub const CHROMATIC_ABERRATION_BLUE_YELLOW: Key<f64> =
    Key::new("chromaticAberrationBlueYellow", 0.0, 1);
pub const DEFRINGE_PURPLE_AMOUNT: Key<f64> = Key::new("defringePurpleAmount", 0.0, 1);
pub const DEFRINGE_PURPLE_HUE_LO: Key<f64> = Key::new("defringePurpleHueLo", 30.0, 1);
pub const DEFRINGE_PURPLE_HUE_HI: Key<f64> = Key::new("defringePurpleHueHi", 70.0, 1);
pub const DEFRINGE_GREEN_AMOUNT: Key<f64> = Key::new("defringeGreenAmount", 0.0, 1);
pub const DEFRINGE_GREEN_HUE_LO: Key<f64> = Key::new("defringeGreenHueLo", 40.0, 1);
pub const DEFRINGE_GREEN_HUE_HI: Key<f64> = Key::new("defringeGreenHueHi", 60.0, 1);
pub const MOIRE_AMOUNT: Key<f64> = Key::new("moireAmount", 0.0, 1);
pub const MOIRE_PATTERN: Key<f64> = Key::new("moirePattern", 30.0, 1);
pub const DIFFRACTION_CORRECTION: Key<bool> = Key::new("diffractionCorrection", false, 1);

pub const KEYS: [&Key<f64>; 25] = [
    &SHARPNESS,
    &SHARPNESS_THRESHOLD,
    &SHARPEN_RADIUS,
    &SHARPEN_DETAIL,
    &SHARPEN_MASKING,
    &SHARPNESS_FALLOFF,
    &LUMA_NOISE_REDUCTION,
    &COLOR_NOISE_REDUCTION,
    &LUMA_NOISE_DETAIL,
    &LUMA_NOISE_CONTRAST,
    &COLOR_NOISE_DETAIL,
    &COLOR_NOISE_SMOOTHNESS,
    &CLARITY,
    &DEHAZE,
    &STRUCTURE,
    &CHROMATIC_ABERRATION_RED_CYAN,
    &CHROMATIC_ABERRATION_BLUE_YELLOW,
    &DEFRINGE_PURPLE_AMOUNT,
    &DEFRINGE_PURPLE_HUE_LO,
    &DEFRINGE_PURPLE_HUE_HI,
    &DEFRINGE_GREEN_AMOUNT,
    &DEFRINGE_GREEN_HUE_LO,
    &DEFRINGE_GREEN_HUE_HI,
    &MOIRE_AMOUNT,
    &MOIRE_PATTERN,
];

/// The visibility switch for a detail key (in addition to the details master switch).
pub fn tool_switch(key: &str) -> Option<Section> {
    if [CLARITY.name(), DEHAZE.name(), STRUCTURE.name()].contains(&key) {
        return Some(Section::PRESENCE);
    }
    if [
        SHARPNESS.name(),
        SHARPNESS_THRESHOLD.name(),
        SHARPEN_RADIUS.name(),
        SHARPEN_DETAIL.name(),
        SHARPEN_MASKING.name(),
        SHARPNESS_FALLOFF.name(),
    ]
    .contains(&key)
    {
        return Some(Section::SHARPENING);
    }
    if [LUMA_NOISE_REDUCTION.name(), COLOR_NOISE_REDUCTION.name()].contains(&key) {
        return Some(Section::NOISE_REDUCTION);
    }
    None
}

slider_value!(
    /// Sharpening amount: 0–150, step 1 (response 1 uses 0–100).
    Sharpness,
    0.0,
    150.0,
    1.0
);
slider_value!(
    /// Sharpening threshold: 0–80, step 1.
    Threshold,
    0.0,
    80.0,
    1.0
);
slider_value!(
    /// Sharpening radius in pixels: 0.5–3, step 0.1.
    Radius,
    0.5,
    3.0,
    0.1
);
slider_value!(
    /// A detail percentage: 0–100, step 1.
    Percent,
    0.0,
    100.0,
    1.0
);
slider_value!(
    /// Defringe amount: 0–20, step 1.
    DefringeAmount,
    0.0,
    20.0,
    1.0
);

/// Amounts and presence values are neutral when hidden. Sharpening parameters
/// keep their defaults when hidden. Noise refinements and defringe retain the
/// stored values; the latter's consumer also reads `visible`. Diffraction is
/// independent of section visibility, as it was before resolution moved here.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct Detail {
    pub visible: bool,
    pub diffraction_correction: bool,
    pub sharpness: Sharpness,
    pub sharpness_threshold: Threshold,
    pub sharpen_radius: Radius,
    pub sharpen_detail: Percent,
    pub sharpen_masking: Percent,
    pub sharpness_falloff: Percent,
    pub luma_noise_reduction: Percent,
    pub color_noise_reduction: Percent,
    pub luma_noise_detail: Percent,
    pub luma_noise_contrast: Percent,
    pub color_noise_detail: Percent,
    pub color_noise_smoothness: Percent,
    pub clarity: Amount,
    pub dehaze: Amount,
    pub structure: Amount,
    pub chromatic_aberration_red_cyan: Amount,
    pub chromatic_aberration_blue_yellow: Amount,
    pub defringe_purple_amount: DefringeAmount,
    pub defringe_purple_hue_lo: Percent,
    pub defringe_purple_hue_hi: Percent,
    pub defringe_green_amount: DefringeAmount,
    pub defringe_green_hue_lo: Percent,
    pub defringe_green_hue_hi: Percent,
    pub moire_amount: Percent,
    pub moire_pattern: Percent,
}
impl Detail {
    pub(crate) fn resolve(stored: StoredRef<'_>, is_raw: bool) -> Self {
        let visible = stored.section_visible(Section::DETAILS);
        let read = |key: &Key<f64>, raw_default: Option<f64>| {
            if tool_switch(key.name())
                .is_some_and(|section| !visible || !stored.section_visible(section))
            {
                return key.default();
            }
            stored.get(key).unwrap_or_else(|| {
                if is_raw {
                    raw_default.unwrap_or(key.default())
                } else {
                    key.default()
                }
            })
        };
        let visible_read = |key: &Key<f64>| {
            if visible {
                stored.get_or_default(key)
            } else {
                key.default()
            }
        };
        Self {
            visible,
            diffraction_correction: stored.get_or_default(&DIFFRACTION_CORRECTION),
            sharpness: Sharpness::new(read(&SHARPNESS, Some(RAW_DEFAULT_SHARPNESS))),
            sharpness_threshold: Threshold::new(read(&SHARPNESS_THRESHOLD, None)),
            sharpen_radius: Radius::new(read(&SHARPEN_RADIUS, None)),
            sharpen_detail: Percent::new(read(&SHARPEN_DETAIL, None)),
            sharpen_masking: Percent::new(read(&SHARPEN_MASKING, None)),
            sharpness_falloff: Percent::new(read(&SHARPNESS_FALLOFF, None)),
            luma_noise_reduction: Percent::new(read(
                &LUMA_NOISE_REDUCTION,
                Some(RAW_DEFAULT_LUMA_NOISE_REDUCTION),
            )),
            color_noise_reduction: Percent::new(read(
                &COLOR_NOISE_REDUCTION,
                Some(RAW_DEFAULT_COLOR_NOISE_REDUCTION),
            )),
            luma_noise_detail: Percent::new(read(&LUMA_NOISE_DETAIL, None)),
            luma_noise_contrast: Percent::new(read(&LUMA_NOISE_CONTRAST, None)),
            color_noise_detail: Percent::new(read(&COLOR_NOISE_DETAIL, None)),
            color_noise_smoothness: Percent::new(read(&COLOR_NOISE_SMOOTHNESS, None)),
            clarity: Amount::new(read(&CLARITY, None)),
            dehaze: Amount::new(read(&DEHAZE, None)),
            structure: Amount::new(read(&STRUCTURE, None)),
            chromatic_aberration_red_cyan: Amount::new(visible_read(
                &CHROMATIC_ABERRATION_RED_CYAN,
            )),
            chromatic_aberration_blue_yellow: Amount::new(visible_read(
                &CHROMATIC_ABERRATION_BLUE_YELLOW,
            )),
            defringe_purple_amount: DefringeAmount::new(read(&DEFRINGE_PURPLE_AMOUNT, None)),
            defringe_purple_hue_lo: Percent::new(read(&DEFRINGE_PURPLE_HUE_LO, None)),
            defringe_purple_hue_hi: Percent::new(read(&DEFRINGE_PURPLE_HUE_HI, None)),
            defringe_green_amount: DefringeAmount::new(read(&DEFRINGE_GREEN_AMOUNT, None)),
            defringe_green_hue_lo: Percent::new(read(&DEFRINGE_GREEN_HUE_LO, None)),
            defringe_green_hue_hi: Percent::new(read(&DEFRINGE_GREEN_HUE_HI, None)),
            moire_amount: Percent::new(visible_read(&MOIRE_AMOUNT)),
            moire_pattern: Percent::new(visible_read(&MOIRE_PATTERN)),
        }
    }
}

/// Detail keys in the generated frontend recipe type.
pub const TS: TsSection = TsSection {
    name: "Detail",
    fields: &[
        TsField::new(SHARPNESS, ""),
        TsField::new(SHARPNESS_THRESHOLD, ""),
        TsField::new(
            SHARPEN_RADIUS,
            "Lightroom's sharpening Radius / Detail / Masking (photo only).",
        ),
        TsField::new(SHARPEN_DETAIL, ""),
        TsField::new(SHARPEN_MASKING, ""),
        TsField::new(
            SHARPNESS_FALLOFF,
            "Capture One's Sharpness Falloff (0–100): more sharpening towards the corners.",
        ),
        TsField::new(LUMA_NOISE_REDUCTION, ""),
        TsField::new(COLOR_NOISE_REDUCTION, ""),
        TsField::new(
            LUMA_NOISE_DETAIL,
            "Lightroom's noise reduction refinements (0–100): luminance Detail (default 50) and Contrast (0), colour Detail (50) and Smoothness (50).",
        ),
        TsField::new(LUMA_NOISE_CONTRAST, ""),
        TsField::new(COLOR_NOISE_DETAIL, ""),
        TsField::new(COLOR_NOISE_SMOOTHNESS, ""),
        TsField::new(CLARITY, ""),
        TsField::new(DEHAZE, ""),
        TsField::new(STRUCTURE, ""),
        TsField::new(CHROMATIC_ABERRATION_RED_CYAN, ""),
        TsField::new(CHROMATIC_ABERRATION_BLUE_YELLOW, ""),
        TsField::new(
            DEFRINGE_PURPLE_AMOUNT,
            "Lightroom's Defringe: amounts 0–20, hue ranges 0–100.",
        ),
        TsField::new(DEFRINGE_PURPLE_HUE_LO, ""),
        TsField::new(DEFRINGE_PURPLE_HUE_HI, ""),
        TsField::new(DEFRINGE_GREEN_AMOUNT, ""),
        TsField::new(DEFRINGE_GREEN_HUE_LO, ""),
        TsField::new(DEFRINGE_GREEN_HUE_HI, ""),
        TsField::new(
            MOIRE_AMOUNT,
            "Capture One's Moiré: amount and pattern size, 0–100.",
        ),
        TsField::new(MOIRE_PATTERN, ""),
        TsField::new(
            DIFFRACTION_CORRECTION,
            "Capture One's diffraction correction: sharpening by the f-number and pixel pitch (diffraction.rs), RAW only.",
        ),
    ],
    unions: &[],
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn edge_recipes(key: &str) -> Vec<Value> {
        let mut out = vec![Value::Null, json!([]), json!("bad"), json!({})];
        for value in [json!(null), json!("9"), json!(-300.5), json!(999.25)] {
            for visibility in [
                json!(null),
                json!([]),
                json!({"details": false}),
                json!({"sharpening": false}),
                json!({"noiseReduction": false}),
                json!({"presence": false}),
                json!({"details": "off"}),
            ] {
                out.push(json!({key: value, "sectionVisibility": visibility}));
            }
        }
        out
    }
    fn old_read(
        recipe: &Value,
        key: &str,
        default: f64,
        tool: Option<&str>,
        raw: Option<f64>,
        is_raw: bool,
    ) -> f64 {
        let visible = |name: &str| {
            recipe
                .get("sectionVisibility")
                .and_then(|v| v.get(name))
                .and_then(Value::as_bool)
                .unwrap_or(true)
        };
        if tool.is_some_and(|name| !visible("details") || !visible(name)) {
            return default;
        }
        recipe[key].as_f64().unwrap_or(if is_raw {
            raw.unwrap_or(default)
        } else {
            default
        })
    }
    #[test]
    fn detail_fields_match_old_reads() {
        for (key, default, tool, raw, value) in [
            (
                "sharpness",
                0.0,
                Some("sharpening"),
                Some(40.0),
                (|d: &Detail| d.sharpness.get()) as fn(&Detail) -> f64,
            ),
            (
                "sharpnessThreshold",
                15.0,
                Some("sharpening"),
                None,
                (|d: &Detail| d.sharpness_threshold.get()) as fn(&Detail) -> f64,
            ),
            (
                "sharpenRadius",
                1.0,
                Some("sharpening"),
                None,
                (|d: &Detail| d.sharpen_radius.get()) as fn(&Detail) -> f64,
            ),
            (
                "sharpenDetail",
                25.0,
                Some("sharpening"),
                None,
                (|d: &Detail| d.sharpen_detail.get()) as fn(&Detail) -> f64,
            ),
            (
                "sharpenMasking",
                0.0,
                Some("sharpening"),
                None,
                (|d: &Detail| d.sharpen_masking.get()) as fn(&Detail) -> f64,
            ),
            (
                "sharpnessFalloff",
                0.0,
                Some("sharpening"),
                None,
                (|d: &Detail| d.sharpness_falloff.get()) as fn(&Detail) -> f64,
            ),
            (
                "lumaNoiseReduction",
                0.0,
                Some("noiseReduction"),
                Some(15.0),
                (|d: &Detail| d.luma_noise_reduction.get()) as fn(&Detail) -> f64,
            ),
            (
                "colorNoiseReduction",
                0.0,
                Some("noiseReduction"),
                Some(25.0),
                (|d: &Detail| d.color_noise_reduction.get()) as fn(&Detail) -> f64,
            ),
            (
                "lumaNoiseDetail",
                50.0,
                None,
                None,
                (|d: &Detail| d.luma_noise_detail.get()) as fn(&Detail) -> f64,
            ),
            (
                "lumaNoiseContrast",
                0.0,
                None,
                None,
                (|d: &Detail| d.luma_noise_contrast.get()) as fn(&Detail) -> f64,
            ),
            (
                "colorNoiseDetail",
                50.0,
                None,
                None,
                (|d: &Detail| d.color_noise_detail.get()) as fn(&Detail) -> f64,
            ),
            (
                "colorNoiseSmoothness",
                50.0,
                None,
                None,
                (|d: &Detail| d.color_noise_smoothness.get()) as fn(&Detail) -> f64,
            ),
            (
                "clarity",
                0.0,
                Some("presence"),
                None,
                (|d: &Detail| d.clarity.get()) as fn(&Detail) -> f64,
            ),
            (
                "dehaze",
                0.0,
                Some("presence"),
                None,
                (|d: &Detail| d.dehaze.get()) as fn(&Detail) -> f64,
            ),
            (
                "structure",
                0.0,
                Some("presence"),
                None,
                (|d: &Detail| d.structure.get()) as fn(&Detail) -> f64,
            ),
            (
                "chromaticAberrationRedCyan",
                0.0,
                Some("details"),
                None,
                (|d: &Detail| d.chromatic_aberration_red_cyan.get()) as fn(&Detail) -> f64,
            ),
            (
                "chromaticAberrationBlueYellow",
                0.0,
                Some("details"),
                None,
                (|d: &Detail| d.chromatic_aberration_blue_yellow.get()) as fn(&Detail) -> f64,
            ),
            (
                "defringePurpleAmount",
                0.0,
                None,
                None,
                (|d: &Detail| d.defringe_purple_amount.get()) as fn(&Detail) -> f64,
            ),
            (
                "defringePurpleHueLo",
                30.0,
                None,
                None,
                (|d: &Detail| d.defringe_purple_hue_lo.get()) as fn(&Detail) -> f64,
            ),
            (
                "defringePurpleHueHi",
                70.0,
                None,
                None,
                (|d: &Detail| d.defringe_purple_hue_hi.get()) as fn(&Detail) -> f64,
            ),
            (
                "defringeGreenAmount",
                0.0,
                None,
                None,
                (|d: &Detail| d.defringe_green_amount.get()) as fn(&Detail) -> f64,
            ),
            (
                "defringeGreenHueLo",
                40.0,
                None,
                None,
                (|d: &Detail| d.defringe_green_hue_lo.get()) as fn(&Detail) -> f64,
            ),
            (
                "defringeGreenHueHi",
                60.0,
                None,
                None,
                (|d: &Detail| d.defringe_green_hue_hi.get()) as fn(&Detail) -> f64,
            ),
            (
                "moireAmount",
                0.0,
                Some("details"),
                None,
                (|d: &Detail| d.moire_amount.get()) as fn(&Detail) -> f64,
            ),
            (
                "moirePattern",
                30.0,
                Some("details"),
                None,
                (|d: &Detail| d.moire_pattern.get()) as fn(&Detail) -> f64,
            ),
        ] {
            for recipe in edge_recipes(key) {
                for is_raw in [false, true] {
                    let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                    assert_eq!(
                        value(&resolved.detail),
                        old_read(&recipe, key, default, tool, raw, is_raw),
                        "{key}: {recipe}"
                    );
                }
            }
        }
    }
    #[test]
    fn visibility_matches_old_switch() {
        for recipe in edge_recipes("sharpness") {
            assert_eq!(
                crate::normalize(StoredRef::new(&recipe), true)
                    .detail
                    .visible,
                recipe
                    .get("sectionVisibility")
                    .and_then(|v| v.get("details"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            );
        }
    }
    #[test]
    fn diffraction_matches_old_read() {
        let mut recipes = edge_recipes("diffractionCorrection");
        for enabled in [true, false] {
            for visible in [true, false] {
                recipes.push(json!({"diffractionCorrection": enabled, "sectionVisibility": {"details": visible}}));
            }
        }
        for recipe in recipes {
            assert_eq!(
                crate::normalize(StoredRef::new(&recipe), true)
                    .detail
                    .diffraction_correction,
                recipe["diffractionCorrection"].as_bool().unwrap_or(false)
            );
        }
    }
}
