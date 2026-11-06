//! Detail values as stored, defaulted and switched, without clamping.
//!
//! Ranges follow StudioDevelop's sliders. Sharpening amount reaches 150
//! from response 2 (100 for response 1); noise refinements are 0–100.
//! The renderer keeps its existing clamps and f32 conversion order.

use crate::tone::Amount;
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
    if [
        CHROMATIC_ABERRATION_RED_CYAN.name(),
        CHROMATIC_ABERRATION_BLUE_YELLOW.name(),
        MOIRE_AMOUNT.name(),
        MOIRE_PATTERN.name(),
    ]
    .contains(&key)
    {
        return Some(Section::DETAILS);
    }
    None
}

macro_rules! slider_value {
    ($name:ident, $min:expr, $max:expr, $step:expr) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
        pub struct $name(f64);
        impl $name {
            pub const MIN: f64 = $min;
            pub const MAX: f64 = $max;
            pub const STEP: f64 = $step;
            pub const fn new(value: f64) -> Self {
                Self(value)
            }
            pub const fn get(self) -> f64 {
                self.0
            }
            pub fn in_range(self) -> bool {
                (Self::MIN..=Self::MAX).contains(&self.0)
            }
        }
    };
}
slider_value!(Sharpness, 0.0, 150.0, 1.0);
slider_value!(Threshold, 0.0, 80.0, 1.0);
slider_value!(Radius, 0.5, 3.0, 0.1);
slider_value!(Percent, 0.0, 100.0, 1.0);
slider_value!(DefringeAmount, 0.0, 20.0, 1.0);

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
            chromatic_aberration_red_cyan: Amount::new(read(&CHROMATIC_ABERRATION_RED_CYAN, None)),
            chromatic_aberration_blue_yellow: Amount::new(read(
                &CHROMATIC_ABERRATION_BLUE_YELLOW,
                None,
            )),
            defringe_purple_amount: DefringeAmount::new(read(&DEFRINGE_PURPLE_AMOUNT, None)),
            defringe_purple_hue_lo: Percent::new(read(&DEFRINGE_PURPLE_HUE_LO, None)),
            defringe_purple_hue_hi: Percent::new(read(&DEFRINGE_PURPLE_HUE_HI, None)),
            defringe_green_amount: DefringeAmount::new(read(&DEFRINGE_GREEN_AMOUNT, None)),
            defringe_green_hue_lo: Percent::new(read(&DEFRINGE_GREEN_HUE_LO, None)),
            defringe_green_hue_hi: Percent::new(read(&DEFRINGE_GREEN_HUE_HI, None)),
            moire_amount: Percent::new(read(&MOIRE_AMOUNT, None)),
            moire_pattern: Percent::new(read(&MOIRE_PATTERN, None)),
        }
    }
}

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
    fn sharpness_matches_old_read() {
        for recipe in edge_recipes("sharpness") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.sharpness.get(),
                    old_read(
                        &recipe,
                        "sharpness",
                        0.0,
                        Some("sharpening"),
                        Some(40.0),
                        is_raw
                    ),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn sharpness_threshold_matches_old_read() {
        for recipe in edge_recipes("sharpnessThreshold") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.sharpness_threshold.get(),
                    old_read(
                        &recipe,
                        "sharpnessThreshold",
                        15.0,
                        Some("sharpening"),
                        None,
                        is_raw
                    ),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn sharpen_radius_matches_old_read() {
        for recipe in edge_recipes("sharpenRadius") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.sharpen_radius.get(),
                    old_read(
                        &recipe,
                        "sharpenRadius",
                        1.0,
                        Some("sharpening"),
                        None,
                        is_raw
                    ),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn sharpen_detail_matches_old_read() {
        for recipe in edge_recipes("sharpenDetail") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.sharpen_detail.get(),
                    old_read(
                        &recipe,
                        "sharpenDetail",
                        25.0,
                        Some("sharpening"),
                        None,
                        is_raw
                    ),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn sharpen_masking_matches_old_read() {
        for recipe in edge_recipes("sharpenMasking") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.sharpen_masking.get(),
                    old_read(
                        &recipe,
                        "sharpenMasking",
                        0.0,
                        Some("sharpening"),
                        None,
                        is_raw
                    ),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn sharpness_falloff_matches_old_read() {
        for recipe in edge_recipes("sharpnessFalloff") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.sharpness_falloff.get(),
                    old_read(
                        &recipe,
                        "sharpnessFalloff",
                        0.0,
                        Some("sharpening"),
                        None,
                        is_raw
                    ),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn luma_noise_reduction_matches_old_read() {
        for recipe in edge_recipes("lumaNoiseReduction") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.luma_noise_reduction.get(),
                    old_read(
                        &recipe,
                        "lumaNoiseReduction",
                        0.0,
                        Some("noiseReduction"),
                        Some(15.0),
                        is_raw
                    ),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn color_noise_reduction_matches_old_read() {
        for recipe in edge_recipes("colorNoiseReduction") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.color_noise_reduction.get(),
                    old_read(
                        &recipe,
                        "colorNoiseReduction",
                        0.0,
                        Some("noiseReduction"),
                        Some(25.0),
                        is_raw
                    ),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn luma_noise_detail_matches_old_read() {
        for recipe in edge_recipes("lumaNoiseDetail") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.luma_noise_detail.get(),
                    old_read(&recipe, "lumaNoiseDetail", 50.0, None, None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn luma_noise_contrast_matches_old_read() {
        for recipe in edge_recipes("lumaNoiseContrast") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.luma_noise_contrast.get(),
                    old_read(&recipe, "lumaNoiseContrast", 0.0, None, None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn color_noise_detail_matches_old_read() {
        for recipe in edge_recipes("colorNoiseDetail") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.color_noise_detail.get(),
                    old_read(&recipe, "colorNoiseDetail", 50.0, None, None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn color_noise_smoothness_matches_old_read() {
        for recipe in edge_recipes("colorNoiseSmoothness") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.color_noise_smoothness.get(),
                    old_read(&recipe, "colorNoiseSmoothness", 50.0, None, None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn clarity_matches_old_read() {
        for recipe in edge_recipes("clarity") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.clarity.get(),
                    old_read(&recipe, "clarity", 0.0, Some("presence"), None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn dehaze_matches_old_read() {
        for recipe in edge_recipes("dehaze") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.dehaze.get(),
                    old_read(&recipe, "dehaze", 0.0, Some("presence"), None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn structure_matches_old_read() {
        for recipe in edge_recipes("structure") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.structure.get(),
                    old_read(&recipe, "structure", 0.0, Some("presence"), None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn chromatic_aberration_red_cyan_matches_old_read() {
        for recipe in edge_recipes("chromaticAberrationRedCyan") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.chromatic_aberration_red_cyan.get(),
                    old_read(
                        &recipe,
                        "chromaticAberrationRedCyan",
                        0.0,
                        Some("details"),
                        None,
                        is_raw
                    ),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn chromatic_aberration_blue_yellow_matches_old_read() {
        for recipe in edge_recipes("chromaticAberrationBlueYellow") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.chromatic_aberration_blue_yellow.get(),
                    old_read(
                        &recipe,
                        "chromaticAberrationBlueYellow",
                        0.0,
                        Some("details"),
                        None,
                        is_raw
                    ),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn defringe_purple_amount_matches_old_read() {
        for recipe in edge_recipes("defringePurpleAmount") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.defringe_purple_amount.get(),
                    old_read(&recipe, "defringePurpleAmount", 0.0, None, None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn defringe_purple_hue_lo_matches_old_read() {
        for recipe in edge_recipes("defringePurpleHueLo") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.defringe_purple_hue_lo.get(),
                    old_read(&recipe, "defringePurpleHueLo", 30.0, None, None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn defringe_purple_hue_hi_matches_old_read() {
        for recipe in edge_recipes("defringePurpleHueHi") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.defringe_purple_hue_hi.get(),
                    old_read(&recipe, "defringePurpleHueHi", 70.0, None, None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn defringe_green_amount_matches_old_read() {
        for recipe in edge_recipes("defringeGreenAmount") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.defringe_green_amount.get(),
                    old_read(&recipe, "defringeGreenAmount", 0.0, None, None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn defringe_green_hue_lo_matches_old_read() {
        for recipe in edge_recipes("defringeGreenHueLo") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.defringe_green_hue_lo.get(),
                    old_read(&recipe, "defringeGreenHueLo", 40.0, None, None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn defringe_green_hue_hi_matches_old_read() {
        for recipe in edge_recipes("defringeGreenHueHi") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.defringe_green_hue_hi.get(),
                    old_read(&recipe, "defringeGreenHueHi", 60.0, None, None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn moire_amount_matches_old_read() {
        for recipe in edge_recipes("moireAmount") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.moire_amount.get(),
                    old_read(&recipe, "moireAmount", 0.0, Some("details"), None, is_raw),
                    "{recipe}"
                );
            }
        }
    }
    #[test]
    fn moire_pattern_matches_old_read() {
        for recipe in edge_recipes("moirePattern") {
            for is_raw in [false, true] {
                let resolved = crate::normalize(StoredRef::new(&recipe), is_raw);
                assert_eq!(
                    resolved.detail.moire_pattern.get(),
                    old_read(&recipe, "moirePattern", 30.0, Some("details"), None, is_raw),
                    "{recipe}"
                );
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
