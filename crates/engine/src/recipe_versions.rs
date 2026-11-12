//! The develop recipe's version keys, read once (plan 2.1, notes:
//! plans/architecture-cleanup-2026-11.md).
//!
//! A recipe records which response of a tool it was made with
//! (`renderVersion`, `toneVersion`, …) so saved edits keep rendering as they
//! were made. Each key has one type here that reads it exactly as the
//! renderer always has (its default when missing, its clamp to the versions
//! this build knows) and names every gate a version opens, so no caller
//! compares a version with a bare number. The key names and the newest
//! versions stay with the tools they describe (`image_processing`,
//! `color_noise`, `edge_fringe`, …); their documentation is the history of
//! each response.
//!
//! [`RecipeVersions`] holds the photo-wide keys of one recipe; keys that
//! belong to a part of the recipe (a mask, a brush line, a light, a
//! selection) have their own types, read from that part.

use serde_json::Value;

use crate::image_processing as ip;

/// Reads `key` of `recipe` as a version: `missing` when absent (or not an
/// unsigned integer).
fn read(recipe: &Value, key: &str, missing: u64) -> u64 {
    recipe.get(key).and_then(Value::as_u64).unwrap_or(missing)
}

/// One step of a [`delta_chain`]: the version it starts at and how it
/// changes the tuning of the version before.
pub type DeltaStep<T> = (u64, fn(T) -> T);

/// Tuning values that a version selects, each version a change of the one
/// before: `first` for versions up to the first step, then every step
/// `(since, change)` with `since <= version` applied in order. The pattern of
/// `color_noise::Tuning::for_version`.
pub fn delta_chain<T>(version: u64, first: T, steps: &[DeltaStep<T>]) -> T {
    steps
        .iter()
        .filter(|(since, _)| *since <= version)
        .fold(first, |tuning, (_, change)| change(tuning))
}

/// The render version from which a geometry warp or straightening keeps
/// the negative (wide gamut) channels the source has
/// (`GeometryParams::keep_negative`; `renderVersion` 2).
pub const KEEP_NEGATIVE_RENDER_VERSION: u64 = 2;
/// The render version from which the large blur of Clarity, Dehaze and
/// Glow gets its whole radius at tile edges (`PreparedAdjustments::whole_blur_radius`;
/// `renderVersion` 2).
pub const WHOLE_BLUR_RADIUS_RENDER_VERSION: u64 = 2;
/// The render version from which the Classic and AgX looks' Color Grading
/// and Calibration follow their tool switches (`renderVersion` 2).
pub const LOOK_TOOL_SWITCHES_RENDER_VERSION: u64 = 2;

/// `renderVersion`: the render pipeline's fixes a recipe was made with
/// (`image_processing::RENDER_VERSION`). 1 when missing, never newer than
/// this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RenderVersion(u64);

impl RenderVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::RENDER_VERSION_KEY, 1).clamp(1, ip::RENDER_VERSION))
    }

    /// The version a recipe develops with: a new recipe's
    /// (`is_new_recipe`) is the newest, a saved one keeps its own.
    pub fn effective(recipe: &Value) -> Self {
        if ip::is_new_recipe(recipe) {
            Self(ip::RENDER_VERSION)
        } else {
            Self::of(recipe)
        }
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// Warps and straightening keep negative (wide gamut) channels.
    pub fn keeps_negative(self) -> bool {
        self.0 >= KEEP_NEGATIVE_RENDER_VERSION
    }

    /// The large blur gets its whole radius at tile edges.
    pub fn whole_blur_radius(self) -> bool {
        self.0 >= WHOLE_BLUR_RADIUS_RENDER_VERSION
    }

    /// The Classic and AgX looks' grading and calibration follow their tool
    /// switches.
    pub fn look_tool_switches(self) -> bool {
        self.0 >= LOOK_TOOL_SWITCHES_RENDER_VERSION
    }

    /// A dead photosite must also be darker than its adjacent samples of
    /// the other colours (`image_loader::DeadTest::Adjacent`).
    pub fn adjacent_dead_test(self) -> bool {
        self.0 >= crate::image_loader::DEAD_TEST_RENDER_VERSION
    }

    /// Colours outside the output space keep their hue.
    pub fn gamut_mapping(self) -> bool {
        self.0 >= ip::GAMUT_MAPPING_RENDER_VERSION
    }

    /// The shader's `gamut_mapping`: 0 clipped per channel, 1 clipped at the
    /// colour's own hue, 2 eased towards the edge first.
    pub fn gamut_mapping_uniform(self) -> u32 {
        if self.0 >= ip::GAMUT_KNEE_RENDER_VERSION {
            2
        } else {
            u32::from(self.gamut_mapping())
        }
    }

    /// Exports render with the shader variant specialized for the recipe.
    pub fn specialized_export(self) -> bool {
        self.0 >= ip::SPECIALIZED_EXPORT_RENDER_VERSION
    }

    /// The geometry warp computes in f32.
    pub fn f32_warp(self) -> bool {
        self.0 >= ip::F32_WARP_RENDER_VERSION
    }

    /// The geometry warp runs on the GPU.
    pub fn gpu_warp(self) -> bool {
        self.0 >= ip::GPU_WARP_RENDER_VERSION
    }

    /// RAW shadows keep their means through the black clip
    /// (`image_loader::BlackLevels::Unbiased`).
    pub fn unbiased_black(self) -> bool {
        self.0 >= crate::image_loader::BLACK_RENDER_VERSION
    }

    /// The masked border's black levels replace a decoder's that spread too
    /// far (`image_loader::BlackLevels::UnbiasedBorder`).
    pub fn border_black(self) -> bool {
        self.0 >= crate::image_loader::BORDER_BLACK_RENDER_VERSION
    }

    /// Clipped colours in larger partly clipped areas are rebuilt
    /// (`image_loader::HighlightRebuild::Ratios`).
    pub fn highlight_rebuild(self) -> bool {
        self.0 >= crate::image_loader::HIGHLIGHT_RENDER_VERSION
    }

    /// RAWs at extended low ISOs get the default exposure of their capture
    /// gain.
    pub fn capture_gain(self) -> bool {
        self.0 >= crate::baseline_exposure::CAPTURE_GAIN_RENDER_VERSION
    }

    /// The neutral look keeps its black point at a scene level.
    pub fn scene_black(self) -> bool {
        self.0 >= crate::baseline_exposure::SCENE_BLACK_RENDER_VERSION
    }
}

/// The tone-slider response from which raised Shadows keep their local
/// contrast through a wider base (`toneVersion` 6).
pub const SHADOW_DETAIL_TONE_VERSION: u64 = 6;
/// The tone-slider response from which lowered Whites act at an
/// edge-aware local base (`toneVersion` 10).
pub const WHITES_LOCAL_BASE_TONE_VERSION: u64 = 10;
/// The tone-slider response from which lowered Whites reach only the bright
/// tones (`toneVersion` 12).
pub const WHITES_BRIGHT_REACH_TONE_VERSION: u64 = 12;
/// The tone-slider response from which raised Shadows lift the midtones less
/// and no longer boost the shadows' texture (`toneVersion` 13).
pub const SHADOWS_MIDTONE_CUT_TONE_VERSION: u64 = 13;
/// The tone-slider response from which lowered Highlights take a local base
/// that is edge-aware in log luminance (`toneVersion` 14).
pub const HIGHLIGHTS_EDGE_BASE_TONE_VERSION: u64 = 14;
/// The tone-slider response from which lowered Blacks' black point reaches
/// into the midtones (`toneVersion` 16).
pub const BLACKS_REACH_TONE_VERSION: u64 = 16;
/// The tone-slider response from which raised Whites move at the reference
/// editors' pace (`toneVersion` 17, `image_processing::whites_amount_v17`).
pub const WHITES_TRAVEL_TONE_VERSION: u64 = 17;
/// The tone-slider response from which lowered Blacks move at Capture One's
/// pace and reach its −100 (`toneVersion` 18,
/// `image_processing::blacks_amount_v18`).
pub const BLACKS_TRAVEL_TONE_VERSION: u64 = 18;

/// `toneVersion` (`image_processing::TONE_VERSION`): 1 when missing, never
/// newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ToneVersion(u64);

impl ToneVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::TONE_VERSION_KEY, 1).clamp(1, ip::TONE_VERSION))
    }

    /// The version the shader uniform (`GlobalAdjustments::tone_version`)
    /// carries.
    pub fn from_uniform(value: f32) -> Self {
        Self(value.round() as u64)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn uniform(self) -> f32 {
        self.0 as f32
    }

    /// Raised Shadows keep their local contrast (wider base).
    pub fn shadow_detail(self) -> bool {
        self.0 >= SHADOW_DETAIL_TONE_VERSION
    }

    /// Lowered Whites act at an edge-aware local base.
    pub fn whites_local_base(self) -> bool {
        self.0 >= WHITES_LOCAL_BASE_TONE_VERSION
    }

    /// Lowered Whites reach only the bright tones.
    pub fn whites_bright_reach(self) -> bool {
        self.0 >= WHITES_BRIGHT_REACH_TONE_VERSION
    }

    /// Raised Shadows lift the midtones less, without a texture boost.
    pub fn shadows_midtone_cut(self) -> bool {
        self.0 >= SHADOWS_MIDTONE_CUT_TONE_VERSION
    }

    /// Lowered Highlights take a local base that a neighbour across a strong
    /// edge hardly pulls.
    pub fn highlights_edge_base(self) -> bool {
        self.0 >= HIGHLIGHTS_EDGE_BASE_TONE_VERSION
    }

    /// Lowered Blacks' black point reaches into the midtones (up to L* 80).
    pub fn blacks_reach(self) -> bool {
        self.0 >= BLACKS_REACH_TONE_VERSION
    }

    /// The Whites amount (recipe units) the renderer applies for the slider
    /// value `value`: from version 17 raised Whites follow the reference
    /// editors' travel.
    pub fn whites_amount(self, value: f64) -> f64 {
        if self.0 >= WHITES_TRAVEL_TONE_VERSION {
            ip::whites_amount_v17(value)
        } else {
            ip::whites_amount_v1(value)
        }
    }

    /// The slider value that renders the Whites amount `amount` (the inverse
    /// of [`Self::whites_amount`]).
    pub fn whites_value(self, amount: f64) -> f64 {
        if self.0 >= WHITES_TRAVEL_TONE_VERSION {
            ip::whites_value_v17(amount)
        } else {
            amount
        }
    }

    /// The Blacks amount (recipe units, version 16's slider value; −200 is
    /// the end of version 18's reach) that the RAW look's black point
    /// applies for the slider value `value`: from version 18 lowered Blacks
    /// follow Capture One's travel.
    pub fn blacks_amount(self, value: f64) -> f64 {
        if self.0 >= BLACKS_TRAVEL_TONE_VERSION {
            ip::blacks_amount_v18(value)
        } else {
            value
        }
    }

    /// The slider value that renders the Blacks amount `amount` (the inverse
    /// of [`Self::blacks_amount`]; amounts beyond the version's reach end at
    /// −100).
    pub fn blacks_value(self, amount: f64) -> f64 {
        if self.0 >= BLACKS_TRAVEL_TONE_VERSION {
            ip::blacks_value_v18(amount)
        } else {
            amount.max(-100.0)
        }
    }
}

/// `sharpenVersion` (`image_processing::SHARPEN_VERSION`): 1 when missing,
/// never newer than this renderer knows. Its responses are told apart in
/// the shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SharpenVersion(u64);

impl SharpenVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::SHARPEN_VERSION_KEY, 1).clamp(1, ip::SHARPEN_VERSION))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// The luminance noise reduction that also takes out isolated specks
/// (`lumaNoiseVersion` 2).
pub const SPECK_LUMA_NOISE_VERSION: u64 = 2;

/// `lumaNoiseVersion` (`image_processing::LUMA_NOISE_VERSION`): 1 when
/// missing, never newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LumaNoiseVersion(u64);

impl LumaNoiseVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::LUMA_NOISE_VERSION_KEY, 1).clamp(1, ip::LUMA_NOISE_VERSION))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// Isolated luminance specks are taken out.
    pub fn removes_specks(self) -> bool {
        self.0 >= SPECK_LUMA_NOISE_VERSION
    }
}

/// `colorNoiseVersion` (`color_noise::VERSION`): never newer than this
/// renderer knows; without the key 1 (the cross-bilateral filter) for
/// recipes that set `colorNoiseReduction` and non-RAW photos, the newest
/// for RAW recipes that leave it to the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ColorNoiseVersion(u64);

impl ColorNoiseVersion {
    pub fn of(recipe: &Value, is_raw: bool) -> Self {
        Self(
            match recipe
                .get(crate::color_noise::VERSION_KEY)
                .and_then(Value::as_u64)
            {
                Some(version) => version.clamp(1, crate::color_noise::VERSION),
                None if is_raw
                    && recipe
                        .get(rawtonic_recipe::detail::COLOR_NOISE_REDUCTION.name())
                        .is_none() =>
                {
                    crate::color_noise::VERSION
                }
                None => 1,
            },
        )
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// The multi-scale filter (`color_noise::MULTISCALE_VERSION`), not the
    /// cross-bilateral one.
    pub fn multiscale(self) -> bool {
        self.0 >= crate::color_noise::MULTISCALE_VERSION
    }

    /// The shader's filter selector: the multi-scale filter's version − 1,
    /// 0 for the cross-bilateral filter.
    pub fn uniform(self) -> f32 {
        if self.multiscale() {
            (self.0 - 1) as f32
        } else {
            0.0
        }
    }
}

/// `defringeVersion` (`image_processing::DEFRINGE_VERSION`): 1 when missing,
/// never newer than this renderer knows. Told apart in the shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefringeVersion(u64);

impl DefringeVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::DEFRINGE_VERSION_KEY, 1).clamp(1, ip::DEFRINGE_VERSION))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// The B&W mix that lowers more strongly the further a slider is pulled
/// (`bwVersion` 2).
pub const STRONG_PULL_BW_VERSION: u64 = 2;

/// `bwVersion` (`image_processing::BW_VERSION`): 1 when missing, never newer
/// than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BwVersion(u64);

impl BwVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::BW_VERSION_KEY, 1).clamp(1, ip::BW_VERSION))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// The mix lowers more strongly the further a slider is pulled.
    pub fn strong_pull(self) -> bool {
        self.0 >= STRONG_PULL_BW_VERSION
    }
}

/// `hslVersion` (`image_processing::HSL_VERSION`): the colour mixer's
/// response, 1 when missing, never newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HslVersion(u64);

impl HslVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::HSL_VERSION_KEY, 1).clamp(1, ip::HSL_VERSION))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// The value the shader reads (`hsl[0]` of the photo's adjustments,
    /// `HslColor::response`): the version − 1, so recipes of the first
    /// response keep their uniform bytes.
    pub fn uniform(self) -> f32 {
        (self.0 - 1) as f32
    }
}

/// The film grain (`grainVersion` 3).
pub const FILM_GRAIN_VERSION: u64 = 3;

/// `grainVersion` (`image_processing::GRAIN_VERSION`): 1 when missing, never
/// newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GrainVersion(u64);

impl GrainVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::GRAIN_VERSION_KEY, 1).clamp(1, ip::GRAIN_VERSION))
    }

    /// The version the shader uniform (`GlobalAdjustments::grain_version`)
    /// carries.
    pub fn from_uniform(value: u32) -> Self {
        Self(u64::from(value))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn uniform(self) -> u32 {
        self.0 as u32
    }

    /// Film grain (Boolean-model grains per dye layer).
    pub fn film(self) -> bool {
        self.0 >= FILM_GRAIN_VERSION
    }
}

/// Clarity measured against an edge-aware base (`clarityVersion` 2).
pub const EDGE_AWARE_CLARITY_VERSION: u64 = 2;

/// `clarityVersion` (`image_processing::CLARITY_VERSION`): 1 when missing,
/// never newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClarityVersion(u64);

impl ClarityVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::CLARITY_VERSION_KEY, 1).clamp(1, ip::CLARITY_VERSION))
    }

    /// The version the shader uniform (`GlobalAdjustments::clarity_version`)
    /// carries.
    pub fn from_uniform(value: u32) -> Self {
        Self(u64::from(value))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn uniform(self) -> u32 {
        self.0 as u32
    }

    /// Clarity's band is measured against an edge-aware base.
    pub fn edge_aware(self) -> bool {
        self.0 >= EDGE_AWARE_CLARITY_VERSION
    }
}

/// Temperature with the references' brightness and magenta coupling and
/// warm range (`temperatureVersion` 2).
pub const COUPLED_TEMPERATURE_VERSION: u64 = 2;

/// `temperatureVersion` (`image_processing::TEMPERATURE_VERSION`): 1 when
/// missing, never newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TemperatureVersion(u64);

impl TemperatureVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::TEMPERATURE_VERSION_KEY, 1).clamp(1, ip::TEMPERATURE_VERSION))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// The version the shader uniform carries (bits 0–7 of
    /// `GlobalAdjustments::temperature_version`).
    pub fn uniform(self) -> u32 {
        self.0 as u32
    }

    /// The Temperature slider brightens when warming, darkens when cooling
    /// and adds magenta towards the warm end (shader `apply_white_balance_v2`).
    pub fn coupled(self) -> bool {
        self.0 >= COUPLED_TEMPERATURE_VERSION
    }
}

/// `tintVersion` (`image_processing::TINT_VERSION`): 1 when missing, never
/// newer than this renderer knows. 2 = Camera Raw's travel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TintVersion(u64);

impl TintVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::TINT_VERSION_KEY, 1).clamp(1, ip::TINT_VERSION))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// The version the shader uniform carries (bits 8–15 of
    /// `GlobalAdjustments::temperature_version`).
    pub fn uniform(self) -> u32 {
        self.0 as u32
    }
}

/// Vibrance weighted by hue and chroma in Oklab (`vibranceVersion` 2).
pub const OKLAB_VIBRANCE_VERSION: u64 = 2;

/// `vibranceVersion` (`image_processing::VIBRANCE_VERSION`): 1 when missing,
/// never newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VibranceVersion(u64);

impl VibranceVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::VIBRANCE_VERSION_KEY, 1).clamp(1, ip::VIBRANCE_VERSION))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// The version the shader uniform (`GlobalAdjustments::vibrance_version`)
    /// carries.
    pub fn uniform(self) -> u32 {
        self.0 as u32
    }

    pub fn from_uniform(value: u32) -> Self {
        Self(u64::from(value))
    }

    /// Vibrance is weighted by Oklab hue and chroma.
    pub fn oklab(self) -> bool {
        self.0 >= OKLAB_VIBRANCE_VERSION
    }
}

/// Lowered Dehaze as a veil fitted to Camera Raw's (`dehazeVersion` 2).
pub const VEIL_DEHAZE_VERSION: u64 = 2;
/// Dehaze adapted to the photo's airlight (`dehazeVersion` 3).
pub const AIRLIGHT_DEHAZE_VERSION: u64 = 3;

/// `dehazeVersion` (`image_processing::DEHAZE_VERSION`): 1 when missing,
/// never newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DehazeVersion(u64);

impl DehazeVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::DEHAZE_VERSION_KEY, 1).clamp(1, ip::DEHAZE_VERSION))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// The version the shader uniform (`GlobalAdjustments::dehaze_version`)
    /// carries.
    pub fn uniform(self) -> u32 {
        self.0 as u32
    }

    pub fn from_uniform(value: u32) -> Self {
        Self(u64::from(value))
    }

    /// Lowering Dehaze lays a veil (shader `apply_dehaze_lower_v2`).
    pub fn veil(self) -> bool {
        self.0 >= VEIL_DEHAZE_VERSION
    }

    /// Dehaze adapts to the photo's airlight (shader
    /// `apply_dehaze_lower_v3`, `apply_dehaze_raise_v3`).
    pub fn airlight(self) -> bool {
        self.0 >= AIRLIGHT_DEHAZE_VERSION
    }
}

/// `structureVersion` (`image_processing::STRUCTURE_VERSION`): 1 when
/// missing, never newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StructureVersion(u64);

impl StructureVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, ip::STRUCTURE_VERSION_KEY, 1).clamp(1, ip::STRUCTURE_VERSION))
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// The Structure amount (recipe units) the renderer applies for the
    /// slider value `value` (`image_processing::structure_amount`).
    pub fn amount(self, value: f64) -> f64 {
        if self.0 >= ip::STRUCTURE_EDGE_AWARE_VERSION {
            ip::structure_amount_v3(value)
        } else if self.0 >= ip::STRUCTURE_TRAVEL_VERSION {
            ip::structure_amount_v2(value)
        } else {
            ip::structure_amount_v1(value)
        }
    }

    /// The version the shader uniform (`GlobalAdjustments::structure_version`)
    /// carries.
    pub fn from_uniform(value: u32) -> Self {
        Self(u64::from(value))
    }

    pub fn uniform(self) -> u32 {
        self.0 as u32
    }

    /// Structure's fine band is measured against an edge-aware base.
    pub fn edge_aware(self) -> bool {
        self.0 >= ip::STRUCTURE_EDGE_AWARE_VERSION
    }
}

/// Edge-fringe treatments (`fringeVersion`, `edge_fringe::VERSION`).
pub mod fringe {
    /// 7 × 7 windows (and, from 3, colour lines) on low-noise photos.
    pub const WIDE_WINDOWS: u64 = 2;
    /// The colour-line stage.
    pub const COLOUR_LINES: u64 = 3;
    /// Chromatic aberration across strong edges corrected first.
    pub const AXIAL: u64 = 4;
    /// Computed on the GPU.
    pub const GPU: u64 = 5;
    /// 7 × 7 windows also on photos of moderate noise.
    pub const MODERATE_NOISE_WINDOWS: u64 = 6;
}

/// `fringeVersion` (`edge_fringe::VERSION`): 0 (no pass) when missing, never
/// newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FringeVersion(u64);

impl FringeVersion {
    pub fn of(recipe: &Value) -> Self {
        Self::clamped(read(recipe, crate::edge_fringe::VERSION_KEY, 0))
    }

    /// `version`, never newer than this renderer knows.
    pub fn clamped(version: u64) -> Self {
        Self(version.min(crate::edge_fringe::VERSION))
    }

    /// A version carried along as a number.
    pub fn from_raw(version: u64) -> Self {
        Self(version)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// The recipe has the pass.
    pub fn enabled(self) -> bool {
        self.0 > 0
    }

    pub fn wide_windows(self) -> bool {
        self.0 >= fringe::WIDE_WINDOWS
    }

    pub fn colour_lines(self) -> bool {
        self.0 >= fringe::COLOUR_LINES
    }

    pub fn axial(self) -> bool {
        self.0 >= fringe::AXIAL
    }

    pub fn gpu(self) -> bool {
        self.0 >= fringe::GPU
    }

    pub fn moderate_noise_windows(self) -> bool {
        self.0 >= fringe::MODERATE_NOISE_WINDOWS
    }
}

/// The automatic lateral CA estimate refined at full resolution and applied
/// only when trustworthy (`autoCaVersion` 2).
pub const REFINED_AUTO_CA_VERSION: u64 = 2;
/// The estimate that competes with the lens data's own lateral CA
/// correction (`autoCaVersion` 3).
pub const COMPETING_AUTO_CA_VERSION: u64 = 3;

/// `autoCaVersion` (`lateral_ca::VERSION`): 1 when missing, never newer than
/// this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AutoCaVersion(u64);

impl AutoCaVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, crate::lateral_ca::VERSION_KEY, 1).clamp(1, crate::lateral_ca::VERSION))
    }

    /// A version carried along as a number (`GeometryParams::auto_ca_version`).
    pub fn from_raw(version: u64) -> Self {
        Self(version)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// Refined at full resolution, shared from the photo with its reduced
    /// previews, applied by default where the lens has no CA data.
    pub fn refined(self) -> bool {
        self.0 >= REFINED_AUTO_CA_VERSION
    }

    /// Competes with the lens data's own lateral CA correction.
    pub fn competes(self) -> bool {
        self.0 >= COMPETING_AUTO_CA_VERSION
    }
}

/// The camera CA data that include Panasonic's (`cameraCaVersion` 1).
pub const PANASONIC_CAMERA_CA_VERSION: u64 = 1;

/// `cameraCaVersion` (`embedded_lens::CA_VERSION`): 0 when missing, never
/// newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CameraCaVersion(u64);

impl CameraCaVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(
            read(recipe, crate::embedded_lens::CA_VERSION_KEY, 0)
                .min(crate::embedded_lens::CA_VERSION),
        )
    }

    /// A version carried along as a number.
    pub fn from_raw(version: u64) -> Self {
        Self(version)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// Panasonic's lateral CA data are read.
    pub fn panasonic(self) -> bool {
        self.0 >= PANASONIC_CAMERA_CA_VERSION
    }
}

/// `cameraGeometryVersion` (`embedded_lens::GEOMETRY_VERSION`; the gates are
/// `embedded_lens`'s own constants): 0 when missing, never newer than this
/// renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CameraGeometryVersion(u64);

impl CameraGeometryVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(
            read(recipe, crate::embedded_lens::GEOMETRY_VERSION_KEY, 0)
                .min(crate::embedded_lens::GEOMETRY_VERSION),
        )
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// `diffractionVersion` (`diffraction::VERSION`): 1 when missing (read as
/// stored, not clamped).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiffractionVersion(u64);

impl DiffractionVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(read(recipe, crate::diffraction::VERSION_KEY, 1))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// The recipe key of AI Denoise's version.
pub const AI_DENOISE_VERSION_KEY: &str = "aiDenoiseVersion";
/// The AI Denoise that only blends in the network's result (version 1);
/// later versions also set its noise scale.
pub const BLEND_AI_DENOISE_VERSION: u8 = 1;
/// The AI Denoise version that runs the mixed-precision model package.
pub const MIXED_PRECISION_AI_DENOISE_VERSION: u8 = 3;
/// The AI Denoise version whose result keeps the sensor's local means
/// (`MosaicDenoise::sensor_means`).
pub const SENSOR_MEANS_AI_DENOISE_VERSION: u8 = 4;

/// `aiDenoiseVersion` (`image_loader::AI_DENOISE_VERSION`): 1 when missing,
/// never newer than this renderer knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AiDenoiseVersion(u8);

impl AiDenoiseVersion {
    pub fn of(recipe: &Value) -> Self {
        Self(recipe[AI_DENOISE_VERSION_KEY].as_u64().map_or(1, |v| {
            v.clamp(1, crate::image_loader::AI_DENOISE_LATEST as u64) as u8
        }))
    }

    /// A version carried along as a number (`AiDenoiseSettings::version`).
    pub fn from_raw(version: u8) -> Self {
        Self(version)
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// Only blends in the network's result.
    pub fn blend_only(self) -> bool {
        self.0 == BLEND_AI_DENOISE_VERSION
    }

    /// Runs the mixed-precision model package.
    pub fn mixed_precision(self) -> bool {
        self.0 >= MIXED_PRECISION_AI_DENOISE_VERSION
    }

    /// Keeps the sensor's local means in the network's result.
    pub fn sensor_means(self) -> bool {
        self.0 >= SENSOR_MEANS_AI_DENOISE_VERSION
    }
}

/// The photo-wide versions of one recipe, each read as its key's type
/// reads it. `is_raw` decides the colour noise default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecipeVersions {
    pub render: RenderVersion,
    pub tone: ToneVersion,
    pub sharpen: SharpenVersion,
    pub luma_noise: LumaNoiseVersion,
    pub color_noise: ColorNoiseVersion,
    pub defringe: DefringeVersion,
    pub bw: BwVersion,
    pub hsl: HslVersion,
    pub grain: GrainVersion,
    pub clarity: ClarityVersion,
    pub temperature: TemperatureVersion,
    pub tint: TintVersion,
    pub vibrance: VibranceVersion,
    pub structure: StructureVersion,
    pub dehaze: DehazeVersion,
    pub fringe: FringeVersion,
    pub auto_ca: AutoCaVersion,
    pub camera_ca: CameraCaVersion,
    pub camera_geometry: CameraGeometryVersion,
    pub diffraction: DiffractionVersion,
    pub ai_denoise: AiDenoiseVersion,
    /// The frame it develops (`source_frame::requested`).
    pub source_frame: u32,
}

impl RecipeVersions {
    pub fn of(recipe: &Value, is_raw: bool) -> Self {
        Self {
            render: RenderVersion::of(recipe),
            tone: ToneVersion::of(recipe),
            sharpen: SharpenVersion::of(recipe),
            luma_noise: LumaNoiseVersion::of(recipe),
            color_noise: ColorNoiseVersion::of(recipe, is_raw),
            defringe: DefringeVersion::of(recipe),
            bw: BwVersion::of(recipe),
            hsl: HslVersion::of(recipe),
            grain: GrainVersion::of(recipe),
            clarity: ClarityVersion::of(recipe),
            temperature: TemperatureVersion::of(recipe),
            tint: TintVersion::of(recipe),
            vibrance: VibranceVersion::of(recipe),
            structure: StructureVersion::of(recipe),
            dehaze: DehazeVersion::of(recipe),
            fringe: FringeVersion::of(recipe),
            auto_ca: AutoCaVersion::of(recipe),
            camera_ca: CameraCaVersion::of(recipe),
            camera_geometry: CameraGeometryVersion::of(recipe),
            diffraction: DiffractionVersion::of(recipe),
            ai_denoise: AiDenoiseVersion::of(recipe),
            source_frame: crate::source_frame::requested(recipe),
        }
    }
}

/// The frame (`sourceFrameVersion`, `source_frame`) of edits from before the
/// key: the frame every file always developed.
pub const ORIGINAL_SOURCE_FRAME: u32 = 0;

/// Versions of a mask layer (`maskVersion`, missing = none).
pub mod mask {
    /// Computed at 16 bits.
    pub const PRECISE: u32 = 2;
    /// Refine edge decides a thin stroke's subject by the side its colour
    /// is mostly made of.
    pub const REFINE_BOTH_SIDES: u32 = 3;
    /// Selections grow and feather from beyond the crop.
    pub const BEYOND_CROP: u32 = 4;
}

/// `maskVersion` of a mask layer: none for masks from before the key, read
/// as stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaskVersion(Option<u32>);

impl MaskVersion {
    pub fn new(version: Option<u32>) -> Self {
        Self(version)
    }

    fn at_least(self, version: u32) -> bool {
        self.0.is_some_and(|v| v >= version)
    }

    pub fn precise(self) -> bool {
        self.at_least(mask::PRECISE)
    }

    pub fn refine_both_sides(self) -> bool {
        self.at_least(mask::REFINE_BOTH_SIDES)
    }

    pub fn beyond_crop(self) -> bool {
        self.at_least(mask::BEYOND_CROP)
    }
}

/// Versions of a brush line's automatic mask (`autoMaskVersion`, missing =
/// 1).
pub mod auto_mask {
    /// The edge-aware automatic mask.
    pub const EDGE_AWARE: u32 = 2;
    /// Colour models per dab.
    pub const COLOUR_MODELS: u32 = 3;
}

/// Versions of a brush line's dabs (`dabVersion`, missing = 1).
pub mod dab {
    /// The dab tip that ends at the last point without scallops.
    pub const SMOOTH_END: u32 = 2;
}

/// Versions of a relight light's ranges (`rangeVersion`, missing = 1).
pub mod light_range {
    /// The relight light key of its range version.
    pub const KEY: &str = "rangeVersion";
    pub const SECOND: u64 = 2;
    pub const THIRD: u64 = 3;
    pub const FOURTH: u64 = 4;
}

/// The key of a Select Subject component's version (`subjectVersion`).
pub const SUBJECT_VERSION_KEY: &str = "subjectVersion";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn each_key_reads_as_its_accessor_did() {
        let future = json!({
            "renderVersion": 999, "toneVersion": 999, "sharpenVersion": 999,
            "lumaNoiseVersion": 999, "colorNoiseVersion": 999, "defringeVersion": 999,
            "bwVersion": 999, "hslVersion": 999, "grainVersion": 999, "clarityVersion": 999, "vibranceVersion": 999, "structureVersion": 999, "dehazeVersion": 999,
            "fringeVersion": 999, "autoCaVersion": 999, "cameraCaVersion": 999,
            "cameraGeometryVersion": 999, "diffractionVersion": 999, "aiDenoiseVersion": 999,
        });
        let v = RecipeVersions::of(&future, true);
        assert_eq!(v.render.get(), ip::RENDER_VERSION);
        assert_eq!(v.tone.get(), ip::TONE_VERSION);
        assert_eq!(v.vibrance.get(), ip::VIBRANCE_VERSION);
        assert_eq!(v.structure.get(), ip::STRUCTURE_VERSION);
        assert_eq!(v.dehaze.get(), ip::DEHAZE_VERSION);
        assert_eq!(v.hsl.get(), ip::HSL_VERSION);
        assert_eq!(v.color_noise.get(), crate::color_noise::VERSION);
        assert_eq!(v.fringe.get(), crate::edge_fringe::VERSION);
        assert_eq!(
            v.camera_geometry.get(),
            crate::embedded_lens::GEOMETRY_VERSION
        );
        // Diffraction was never clamped.
        assert_eq!(v.diffraction.get(), 999);
        let missing = RecipeVersions::of(&json!({"exposure": 1}), true);
        assert_eq!(missing.render.get(), 1);
        assert_eq!(missing.structure.get(), 1);
        assert_eq!(missing.hsl.get(), 1);
        assert_eq!(missing.fringe.get(), 0);
        assert_eq!(missing.camera_ca.get(), 0);
        assert_eq!(missing.ai_denoise.get(), 1);
        assert_eq!(missing.vibrance.get(), 1);
        assert_eq!(missing.color_noise.get(), crate::color_noise::VERSION);
        assert_eq!(
            RecipeVersions::of(&json!({"exposure": 1}), false)
                .color_noise
                .get(),
            1
        );
        assert_eq!(
            RecipeVersions::of(&json!({"colorNoiseReduction": 25}), true)
                .color_noise
                .get(),
            1
        );
        let zero = json!({"renderVersion": 0, "toneVersion": 0, "aiDenoiseVersion": 0});
        assert_eq!(RenderVersion::of(&zero).get(), 1);
        assert_eq!(ToneVersion::of(&zero).get(), 1);
        assert_eq!(AiDenoiseVersion::of(&zero).get(), 1);
    }

    #[test]
    fn a_delta_chain_applies_the_steps_up_to_the_version() {
        let steps: [DeltaStep<Vec<u64>>; 2] = [
            (3, |mut t| {
                t.push(3);
                t
            }),
            (5, |mut t| {
                t.push(5);
                t
            }),
        ];
        assert_eq!(delta_chain(1, vec![], &steps), Vec::<u64>::new());
        assert_eq!(delta_chain(4, vec![], &steps), vec![3]);
        assert_eq!(delta_chain(9, vec![], &steps), vec![3, 5]);
    }

    #[test]
    fn uniforms_round_trip() {
        for v in 1..=ip::TONE_VERSION {
            let tone = ToneVersion::of(&json!({"toneVersion": v}));
            assert_eq!(ToneVersion::from_uniform(tone.uniform()), tone);
        }
    }
}
