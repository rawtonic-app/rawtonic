//! Colour slider values, kept as stored without clamping.

use crate::tone::Amount;
use crate::ts::{TsField, TsSection, TsType, TsUnion};
use crate::{Key, KeyValue, Section, StoredRef};
use serde_json::Value;

pub const TEMPERATURE: Key<f64> = Key::new("temperature", 0.0, 1);
pub const TINT: Key<f64> = Key::new("tint", 0.0, 1);
pub const VIBRANCE: Key<f64> = Key::new("vibrance", 0.0, 1);
pub const SATURATION: Key<f64> = Key::new("saturation", 0.0, 1);
pub const HUE: Key<f64> = Key::new("hue", 0.0, 1);
pub const WHITE_BALANCE: Key<WhiteBalance> = Key::new("whiteBalance", WhiteBalance::AsShot, 1);
pub const WB_TEMPERATURE: Key<f64> = Key::new("wbTemperature", 0.0, 1);
pub const WB_TINT: Key<f64> = Key::new("wbTint", 0.0, 1);
pub const BLACK_AND_WHITE: Key<bool> = Key::new("blackAndWhite", false, 1);
pub const COLOR_RANGES: Key<StoredColorRanges> =
    Key::new("colorRanges", StoredColorRanges(Vec::new()), 1);
pub const COLOR_RANGE_SHOW: Key<String> = Key::new("colorRangeShow", String::new(), 1);
pub const RANGE_ID: Key<String> = Key::new("id", String::new(), 1);
pub const RANGE_HUE: Key<f64> = Key::new("hue", 0.0, 1);
pub const RANGE_CHROMA: Key<f64> = Key::new("chroma", 0.0, 1);
pub const RANGE_LIGHTNESS: Key<f64> = Key::new("lightness", 0.5, 1);
pub const RANGE_LIGHTNESS_RANGE: Key<f64> = Key::new("lightnessRange", 100.0, 1);
pub const RANGE_HUE_RANGE: Key<f64> = Key::new("hueRange", 25.0, 1);
pub const RANGE_CHROMA_RANGE: Key<f64> = Key::new("chromaRange", 70.0, 1);
pub const RANGE_SMOOTHNESS: Key<f64> = Key::new("smoothness", 50.0, 1);
pub const RANGE_HUE_SHIFT: Key<f64> = Key::new("hueShift", 0.0, 1);
pub const RANGE_SATURATION: Key<f64> = Key::new("saturation", 0.0, 1);
pub const RANGE_LIGHTNESS_SHIFT: Key<f64> = Key::new("lightnessShift", 0.0, 1);
pub const RANGE_HUE_UNIFORMITY: Key<f64> = Key::new("hueUniformity", 0.0, 1);
pub const RANGE_SATURATION_UNIFORMITY: Key<f64> = Key::new("saturationUniformity", 0.0, 1);
pub const RANGE_LIGHTNESS_UNIFORMITY: Key<f64> = Key::new("lightnessUniformity", 0.0, 1);

/// An array as saved, retaining unknown fields and ranges beyond the renderer's limit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoredColorRanges(Vec<Value>);
impl KeyValue for StoredColorRanges {
    fn from_json(value: &Value) -> Option<Self> {
        Some(Self(value.as_array()?.clone()))
    }
    fn to_json(&self) -> Value {
        Value::Array(self.0.clone())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColorRange {
    pub hue: f64,
    pub chroma: f64,
    pub lightness: f64,
    pub lightness_range: f64,
    pub hue_range: f64,
    pub chroma_range: f64,
    pub smoothness: f64,
    pub hue_shift: f64,
    pub saturation: f64,
    pub lightness_shift: f64,
    pub hue_uniformity: f64,
    pub saturation_uniformity: f64,
    pub lightness_uniformity: f64,
}

impl ColorRange {
    /// Shared photo/layer read; clipping and f32 packing stay in the engine.
    pub(crate) fn resolve(stored: StoredRef<'_>) -> Self {
        let read = |key: &Key<f64>| stored.get_or_default(key);
        Self {
            hue: read(&RANGE_HUE),
            chroma: read(&RANGE_CHROMA),
            lightness: read(&RANGE_LIGHTNESS),
            lightness_range: read(&RANGE_LIGHTNESS_RANGE),
            hue_range: read(&RANGE_HUE_RANGE),
            chroma_range: read(&RANGE_CHROMA_RANGE),
            smoothness: read(&RANGE_SMOOTHNESS),
            hue_shift: read(&RANGE_HUE_SHIFT),
            saturation: read(&RANGE_SATURATION),
            lightness_shift: read(&RANGE_LIGHTNESS_SHIFT),
            hue_uniformity: read(&RANGE_HUE_UNIFORMITY),
            saturation_uniformity: read(&RANGE_SATURATION_UNIFORMITY),
            lightness_uniformity: read(&RANGE_LIGHTNESS_UNIFORMITY),
        }
    }
}

/// The most colour editor ranges the photo and each mask layer hold
/// (Lightroom's Point Color takes up to eight swatches per photo and per mask).
pub const MAX_COLOR_RANGES: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColorRanges {
    pub ranges: [ColorRange; MAX_COLOR_RANGES],
    pub count: usize,
    pub shown: Option<usize>,
}
pub const GRAY_MIXER: Key<GrayMixer> = Key::new("grayMixer", GrayMixer { channels: [0.0; 8] }, 1);
pub const GRAY_CHANNELS: [Key<f64>; 8] = [
    Key::new("red", 0.0, 1),
    Key::new("orange", 0.0, 1),
    Key::new("yellow", 0.0, 1),
    Key::new("green", 0.0, 1),
    Key::new("aqua", 0.0, 1),
    Key::new("blue", 0.0, 1),
    Key::new("purple", 0.0, 1),
    Key::new("magenta", 0.0, 1),
];

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GrayMixer {
    pub channels: [f64; 8],
}
impl KeyValue for GrayMixer {
    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            channels: GRAY_CHANNELS.map(|key| {
                value
                    .get(key.name())
                    .and_then(Value::as_f64)
                    .unwrap_or(key.default())
            }),
        })
    }
    fn to_json(&self) -> Value {
        Value::Object(
            GRAY_CHANNELS
                .iter()
                .zip(self.channels)
                .map(|(key, v)| (key.name().into(), v.into()))
                .collect(),
        )
    }
}
/// B&W Toning (`blackAndWhiteToning`): the tint the black-and-white
/// conversion's grey takes by its lightness (the engine's `bw_toning`).
pub const BW_TONING: Key<BwToning> = Key::new("blackAndWhiteToning", BwToning::DEFAULT, 1);
pub const BW_TONING_BANDS: [Key<ToneBand>; 2] = [
    Key::new("shadows", ToneBand::DEFAULT, 1),
    Key::new("highlights", ToneBand::DEFAULT, 1),
];
pub const TONE_DENSITY: Key<f64> = Key::new("density", 0.0, 1);
pub const TONE_PAPER: Key<f64> = Key::new("paper", 0.0, 1);

/// One tone of B&W Toning: hue 0–359 (the hue of a fully saturated sRGB
/// colour; 40, a warm tone, when missing), saturation 0–100, density 0–100
/// (how much the tint darkens).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneBand {
    pub hue: f64,
    pub saturation: f64,
    pub density: f64,
}
impl ToneBand {
    pub const DEFAULT_HUE: f64 = 40.0;
    const DEFAULT: Self = Self {
        hue: Self::DEFAULT_HUE,
        saturation: 0.0,
        density: 0.0,
    };
}
impl Default for ToneBand {
    fn default() -> Self {
        Self::DEFAULT
    }
}
impl KeyValue for ToneBand {
    fn from_json(value: &Value) -> Option<Self> {
        let read = |key: &Key<f64>, default: f64| {
            value
                .get(key.name())
                .and_then(Value::as_f64)
                .unwrap_or(default)
        };
        Some(Self {
            hue: read(&HUE, Self::DEFAULT_HUE),
            saturation: read(&SATURATION, 0.0),
            density: read(&TONE_DENSITY, 0.0),
        })
    }
    fn to_json(&self) -> Value {
        serde_json::json!({HUE.name(): self.hue, SATURATION.name(): self.saturation, TONE_DENSITY.name(): self.density})
    }
}

/// B&W Toning as stored: shadows and highlights tones, Balance −100–100
/// (where the highlights tone starts) and Paper 0–100 (how far the
/// highlights tone reaches into white).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BwToning {
    pub shadows: ToneBand,
    pub highlights: ToneBand,
    pub balance: f64,
    pub paper: f64,
}
impl BwToning {
    pub const HUE_MAX: f64 = 359.0;
    pub const AMOUNT_MAX: f64 = 100.0;
    pub const BALANCE_MIN: f64 = -100.0;
    pub const BALANCE_MAX: f64 = 100.0;
    const DEFAULT: Self = Self {
        shadows: ToneBand::DEFAULT,
        highlights: ToneBand::DEFAULT,
        balance: 0.0,
        paper: 0.0,
    };
}
impl Default for BwToning {
    fn default() -> Self {
        Self::DEFAULT
    }
}
impl KeyValue for BwToning {
    fn from_json(value: &Value) -> Option<Self> {
        if value.is_null() {
            return Some(Self::DEFAULT);
        }
        let band = |key: &Key<ToneBand>| {
            ToneBand::from_json(value.get(key.name()).unwrap_or(&Value::Null)).unwrap_or_default()
        };
        let read = |key: &Key<f64>| {
            value
                .get(key.name())
                .and_then(Value::as_f64)
                .unwrap_or(key.default())
        };
        Some(Self {
            shadows: band(&BW_TONING_BANDS[0]),
            highlights: band(&BW_TONING_BANDS[1]),
            balance: read(&BALANCE),
            paper: read(&TONE_PAPER),
        })
    }
    fn to_json(&self) -> Value {
        serde_json::json!({
            BW_TONING_BANDS[0].name(): self.shadows.to_json(),
            BW_TONING_BANDS[1].name(): self.highlights.to_json(),
            BALANCE.name(): self.balance,
            TONE_PAPER.name(): self.paper,
        })
    }
}
pub const HSL: Key<Hsl> = Key::new(
    "hsl",
    Hsl {
        channels: [HslChannel::ZERO; 8],
    },
    1,
);
pub const LUMINANCE: Key<f64> = Key::new("luminance", 0.0, 1);
pub const COLOR_GRADING: Key<Grading> = Key::new("colorGrading", Grading::DEFAULT, 1);
pub const COLOR_CALIBRATION: Key<Calibration> = Key::new(
    "colorCalibration",
    Calibration {
        shadows_tint: 0.0,
        hue: [0.0; 3],
        saturation: [0.0; 3],
    },
    1,
);
pub const CALIBRATION_KEYS: [Key<f64>; 7] = [
    Key::new("shadowsTint", 0.0, 1),
    Key::new("redHue", 0.0, 1),
    Key::new("redSaturation", 0.0, 1),
    Key::new("greenHue", 0.0, 1),
    Key::new("greenSaturation", 0.0, 1),
    Key::new("blueHue", 0.0, 1),
    Key::new("blueSaturation", 0.0, 1),
];
pub const BLENDING: Key<f64> = Key::new("blending", 50.0, 1);
pub const BALANCE: Key<f64> = Key::new("balance", 0.0, 1);
pub const GRADING_WHEELS: [Key<HslChannel>; 4] = [
    Key::new("shadows", HslChannel::ZERO, 1),
    Key::new("midtones", HslChannel::ZERO, 1),
    Key::new("highlights", HslChannel::ZERO, 1),
    Key::new("global", HslChannel::ZERO, 1),
];
pub const HSL_CHANNELS: [Key<HslChannel>; 8] = [
    Key::new("reds", HslChannel::ZERO, 1),
    Key::new("oranges", HslChannel::ZERO, 1),
    Key::new("yellows", HslChannel::ZERO, 1),
    Key::new("greens", HslChannel::ZERO, 1),
    Key::new("aquas", HslChannel::ZERO, 1),
    Key::new("blues", HslChannel::ZERO, 1),
    Key::new("purples", HslChannel::ZERO, 1),
    Key::new("magentas", HslChannel::ZERO, 1),
];

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HslChannel {
    pub hue: f64,
    pub saturation: f64,
    pub luminance: f64,
}
impl HslChannel {
    const ZERO: Self = Self {
        hue: 0.0,
        saturation: 0.0,
        luminance: 0.0,
    };
}
impl KeyValue for HslChannel {
    fn from_json(value: &Value) -> Option<Self> {
        let read = |key: &Key<f64>| {
            value
                .get(key.name())
                .and_then(Value::as_f64)
                .unwrap_or(key.default())
        };
        Some(Self {
            hue: read(&HUE),
            saturation: read(&SATURATION),
            luminance: read(&LUMINANCE),
        })
    }
    fn to_json(&self) -> Value {
        serde_json::json!({HUE.name(): self.hue, SATURATION.name(): self.saturation, LUMINANCE.name(): self.luminance})
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hsl {
    pub channels: [HslChannel; 8],
}
impl Hsl {
    pub const MIN: f64 = -100.0;
    pub const MAX: f64 = 100.0;
    pub const STEP: f64 = 1.0;
}
impl KeyValue for Hsl {
    fn from_json(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let channels = std::array::from_fn(|i| {
            let channel = object.get(HSL_CHANNELS[i].name()).unwrap_or(&Value::Null);
            HslChannel::from_json(channel).unwrap_or_default()
        });
        Some(Self { channels })
    }
    fn to_json(&self) -> Value {
        let channels = self
            .channels
            .iter()
            .enumerate()
            .map(|(i, channel)| (HSL_CHANNELS[i].name().into(), channel.to_json()))
            .collect();
        Value::Object(channels)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grading {
    pub wheels: [HslChannel; 4],
    pub blending: f64,
    pub balance: f64,
}
impl Grading {
    pub const HUE_MIN: f64 = 0.0;
    pub const HUE_MAX: f64 = 359.0;
    pub const SATURATION_MIN: f64 = 0.0;
    pub const SATURATION_MAX: f64 = 100.0;
    pub const LUMINANCE_MIN: f64 = -100.0;
    pub const LUMINANCE_MAX: f64 = 100.0;
    pub const STEP: f64 = 1.0;
    const DEFAULT: Self = Self {
        wheels: [HslChannel::ZERO; 4],
        blending: 50.0,
        balance: 0.0,
    };
}
impl Default for Grading {
    fn default() -> Self {
        Self::DEFAULT
    }
}
impl KeyValue for Grading {
    fn from_json(value: &Value) -> Option<Self> {
        if value.is_null() {
            return Some(Self::DEFAULT);
        }
        let read = |v: &Value, key: &Key<f64>| {
            v.get(key.name())
                .and_then(Value::as_f64)
                .unwrap_or(key.default())
        };
        Some(Self {
            wheels: GRADING_WHEELS.map(|key| {
                let wheel = value.get(key.name()).unwrap_or(&Value::Null);
                HslChannel::from_json(wheel).unwrap_or_default()
            }),
            blending: read(value, &BLENDING),
            balance: read(value, &BALANCE),
        })
    }
    fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        for (name, wheel) in GRADING_WHEELS.iter().zip(self.wheels) {
            map.insert(name.name().into(), wheel.to_json());
        }
        map.insert(BLENDING.name().into(), self.blending.into());
        map.insert(BALANCE.name().into(), self.balance.into());
        Value::Object(map)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Calibration {
    pub shadows_tint: f64,
    pub hue: [f64; 3],
    pub saturation: [f64; 3],
}
impl Calibration {
    pub const MIN: f64 = -100.0;
    pub const MAX: f64 = 100.0;
    pub const STEP: f64 = 1.0;
}
impl KeyValue for Calibration {
    fn from_json(value: &Value) -> Option<Self> {
        let v = CALIBRATION_KEYS.map(|key| {
            value
                .get(key.name())
                .and_then(Value::as_f64)
                .unwrap_or(key.default())
        });
        Some(Self {
            shadows_tint: v[0],
            hue: [v[1], v[3], v[5]],
            saturation: [v[2], v[4], v[6]],
        })
    }
    fn to_json(&self) -> Value {
        let values = [
            self.shadows_tint,
            self.hue[0],
            self.saturation[0],
            self.hue[1],
            self.saturation[1],
            self.hue[2],
            self.saturation[2],
        ];
        Value::Object(
            CALIBRATION_KEYS
                .iter()
                .zip(values)
                .map(|(key, value)| (key.name().into(), value.into()))
                .collect(),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WhiteBalance {
    #[default]
    AsShot,
    Daylight,
    Cloudy,
    Shade,
    Tungsten,
    Fluorescent,
    Flash,
    Custom,
    Auto,
}

impl WhiteBalance {
    /// Every value, in the order of [`WhiteBalance::NAMES`].
    pub const ALL: [Self; 9] = [
        Self::AsShot,
        Self::Daylight,
        Self::Cloudy,
        Self::Shade,
        Self::Tungsten,
        Self::Fluorescent,
        Self::Flash,
        Self::Custom,
        Self::Auto,
    ];
    /// Each value's name in the saved JSON.
    pub const NAMES: [&'static str; 9] = [
        "asShot",
        "daylight",
        "cloudy",
        "shade",
        "tungsten",
        "fluorescent",
        "flash",
        "custom",
        "auto",
    ];
}

impl KeyValue for WhiteBalance {
    fn from_json(value: &Value) -> Option<Self> {
        let name = value.as_str()?;
        let i = Self::NAMES.iter().position(|n| *n == name)?;
        Some(Self::ALL[i])
    }
    fn to_json(&self) -> Value {
        Value::String(Self::NAMES[*self as usize].into())
    }
}

pub const KEYS: [&Key<f64>; 5] = [&TEMPERATURE, &TINT, &VIBRANCE, &SATURATION, &HUE];

/// The section's keys in the frontend's recipe type (`crate::ts`).
/// `colorRangeShow` is left out: the view state the Develop view adds to
/// each render, never part of the edited recipe.
pub const TS: TsSection = TsSection {
    name: "colour",
    fields: &[
        TsField::new(TEMPERATURE, ""),
        TsField::new(TINT, ""),
        TsField::new(VIBRANCE, ""),
        TsField::new(SATURATION, ""),
        TsField::new(HUE, ""),
        TsField::new(
            WHITE_BALANCE,
            "Absolute white balance of RAWs (Lightroom's Temp/Tint, camera space): unset or `asShot` keeps \
             the camera's; a preset or `custom` with `wbTemperature` (K) and `wbTint`. The relative \
             `temperature`/`tint` apply on top (JPEGs, layers, older edits).",
        ),
        TsField::new(WB_TEMPERATURE, ""),
        TsField::new(WB_TINT, ""),
        TsField::new(
            BLACK_AND_WHITE,
            "Black & white conversion (Lightroom's Treatment, Capture One's B&W).",
        ),
        TsField::new(GRAY_MIXER, ""),
        TsField::new(
            BW_TONING,
            "B&W Toning: the tint the black-and-white grey takes by its lightness (shadows and highlights \
             tones, Balance, Paper); only while `blackAndWhite` is on.",
        ),
        TsField::new(COLOR_RANGES, "Colour editor ranges, applied in order."),
        TsField::new(HSL, ""),
        TsField::new(COLOR_GRADING, ""),
        TsField::new(COLOR_CALIBRATION, ""),
    ],
    unions: &[TsUnion {
        name: WhiteBalance::TS,
        doc: "",
        values: &WhiteBalance::NAMES,
    }],
};

impl TsType for WhiteBalance {
    const TS: &'static str = "WhiteBalanceMode";
}
impl TsType for StoredColorRanges {
    const TS: &'static str = "ColorRange[]";
    const IMPORT: Option<&'static str> = Some("ColorRange");
}
impl TsType for GrayMixer {
    const TS: &'static str = "GrayMixer";
    const IMPORT: Option<&'static str> = Some("GrayMixer");
}
impl TsType for BwToning {
    const TS: &'static str = "BwToning";
    const IMPORT: Option<&'static str> = Some("BwToning");
}
impl TsType for Hsl {
    const TS: &'static str = "Hsl";
    const IMPORT: Option<&'static str> = Some("Hsl");
}
impl TsType for Grading {
    const TS: &'static str = "ColorGradingProps";
    const IMPORT: Option<&'static str> = Some("ColorGradingProps");
}
impl TsType for Calibration {
    const TS: &'static str = "ColorCalibration";
    const IMPORT: Option<&'static str> = Some("ColorCalibration");
}

/// Hue angle in degrees, as stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct Hue(f64);

impl Hue {
    pub const MIN: f64 = -180.0;
    pub const MAX: f64 = 180.0;
    pub const STEP: f64 = 1.0;
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

/// Absolute white-balance temperature (Develop slider: 2,000–50,000 K, step 50).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Kelvin(f64);
impl Kelvin {
    pub const MIN: f64 = 2000.0;
    pub const MAX: f64 = 50_000.0;
    pub const STEP: f64 = 50.0;
    pub const fn get(self) -> f64 {
        self.0
    }
    pub fn in_range(self) -> bool {
        (Self::MIN..=Self::MAX).contains(&self.0)
    }
}

/// Absolute white-balance tint (Develop slider: −150–150, step 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhiteBalanceTint(f64);
impl WhiteBalanceTint {
    pub const MIN: f64 = -150.0;
    pub const MAX: f64 = 150.0;
    pub const STEP: f64 = 1.0;
    pub const fn get(self) -> f64 {
        self.0
    }
    pub fn in_range(self) -> bool {
        (Self::MIN..=Self::MAX).contains(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhiteBalanceSetting {
    pub temperature: Kelvin,
    pub tint: WhiteBalanceTint,
}

/// Resolved sliders, HSL and colour ranges are already neutral when hidden.
/// Grading, calibration, gray mixer and white balance retain their stored values;
/// consumers must apply their `*_visible` flags (or `black_and_white` for the mixer).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Color {
    pub temperature: Amount,
    pub tint: Amount,
    pub vibrance: Amount,
    pub saturation: Amount,
    pub hue: Hue,
    /// Preset or custom white balance, independent of section visibility.
    pub white_balance: Option<WhiteBalanceSetting>,
    pub white_balance_visible: bool,
    pub hsl: Hsl,
    pub grading: Grading,
    pub grading_visible: bool,
    pub calibration: Calibration,
    pub calibration_visible: bool,
    pub black_and_white: bool,
    pub gray_mixer: GrayMixer,
    /// Stored values; consumers apply it only with `black_and_white`.
    pub bw_toning: BwToning,
    pub color_ranges: ColorRanges,
}

impl Color {
    pub(crate) fn resolve(stored: StoredRef<'_>) -> Self {
        let mut color_ranges = ColorRanges::default();
        if stored.section_visible(Section::COLOR) && stored.section_visible(Section::COLOR_EDITOR) {
            let root = stored.as_value();
            let shown = root.get(COLOR_RANGE_SHOW.name()).and_then(Value::as_str);
            if let Some(ranges) = root.get(COLOR_RANGES.name()).and_then(Value::as_array) {
                for (i, range) in ranges.iter().take(MAX_COLOR_RANGES).enumerate() {
                    color_ranges.ranges[i] = ColorRange::resolve(StoredRef::new(range));
                    if color_ranges.shown.is_none()
                        && shown.is_some()
                        && range.get(RANGE_ID.name()).and_then(Value::as_str) == shown
                    {
                        color_ranges.shown = Some(i);
                    }
                    color_ranges.count += 1;
                }
            }
        }
        let white_balance = match stored.get_or_default(&WHITE_BALANCE) {
            WhiteBalance::Daylight => Some((5500.0, 10.0)),
            WhiteBalance::Cloudy => Some((6500.0, 10.0)),
            WhiteBalance::Shade => Some((7500.0, 10.0)),
            WhiteBalance::Tungsten => Some((2850.0, 0.0)),
            WhiteBalance::Fluorescent => Some((3800.0, 21.0)),
            WhiteBalance::Flash => Some((5500.0, 0.0)),
            WhiteBalance::Custom | WhiteBalance::Auto => stored.get(&WB_TEMPERATURE).map(|t| {
                (
                    t.clamp(2000.0, 50_000.0),
                    stored.get_or_default(&WB_TINT).clamp(-150.0, 150.0),
                )
            }),
            WhiteBalance::AsShot => None,
        }
        .map(|(temperature, tint)| WhiteBalanceSetting {
            temperature: Kelvin(temperature),
            tint: WhiteBalanceTint(tint),
        });
        let read = |key, section| {
            if stored.section_visible(Section::COLOR) && stored.section_visible(section) {
                stored.get_or_default(key)
            } else {
                0.0
            }
        };
        Self {
            temperature: Amount::new(read(&TEMPERATURE, Section::WHITE_BALANCE)),
            tint: Amount::new(read(&TINT, Section::WHITE_BALANCE)),
            vibrance: Amount::new(read(&VIBRANCE, Section::PRESENCE)),
            saturation: Amount::new(read(&SATURATION, Section::PRESENCE)),
            hue: Hue::new(if stored.section_visible(Section::COLOR) {
                stored.get_or_default(&HUE)
            } else {
                0.0
            }),
            white_balance,
            white_balance_visible: stored.section_visible(Section::COLOR)
                && stored.section_visible(Section::WHITE_BALANCE),
            hsl: if stored.section_visible(Section::COLOR)
                && stored.section_visible(Section::COLOR_MIXER)
            {
                stored.get_or_default(&HSL)
            } else {
                HSL.default()
            },
            grading: stored.get_or_default(&COLOR_GRADING),
            grading_visible: stored.section_visible(Section::COLOR)
                && stored.section_visible(Section::COLOR_GRADING),
            calibration: stored.get_or_default(&COLOR_CALIBRATION),
            calibration_visible: stored.section_visible(Section::COLOR)
                && stored.section_visible(Section::CALIBRATION),
            black_and_white: stored.section_visible(Section::COLOR)
                && stored.get_or_default(&BLACK_AND_WHITE),
            gray_mixer: stored.get_or_default(&GRAY_MIXER),
            bw_toning: stored.get_or_default(&BW_TONING),
            color_ranges,
        }
    }
}

/// Resolve only this section for a stage that does not consume other sections.
pub fn resolve(stored: StoredRef<'_>) -> Color {
    Color::resolve(stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn slider_values_match_the_previous_reads() {
        for recipe in [
            Value::Null,
            json!([]),
            json!({}),
            json!({"temperature": 150, "tint": "2", "vibrance": -20, "saturation": null, "hue": 300}),
            json!({"temperature": 1, "vibrance": 2, "hue": 3, "sectionVisibility": {"whiteBalance": false}}),
            json!({"temperature": 1, "vibrance": 2, "hue": 3, "sectionVisibility": {"presence": false}}),
            json!({"temperature": 1, "vibrance": 2, "hue": 3, "sectionVisibility": {"color": false}}),
        ] {
            let value = Color::resolve(StoredRef::new(&recipe));
            let read = |key: &str, tool: Option<&str>| {
                let visible = |name: &str| {
                    recipe
                        .get("sectionVisibility")
                        .and_then(|v| v.get(name))
                        .and_then(Value::as_bool)
                        .unwrap_or(true)
                };
                if visible("color") && tool.is_none_or(visible) {
                    recipe[key].as_f64().unwrap_or(0.0)
                } else {
                    0.0
                }
            };
            assert_eq!(
                value.temperature.get(),
                read("temperature", Some("whiteBalance"))
            );
            assert_eq!(value.tint.get(), read("tint", Some("whiteBalance")));
            assert_eq!(value.vibrance.get(), read("vibrance", Some("presence")));
            assert_eq!(value.saturation.get(), read("saturation", Some("presence")));
            assert_eq!(value.hue.get(), read("hue", None));
        }
    }

    #[test]
    fn nested_color_inputs_keep_raw_values_and_visibility() {
        let recipe = json!({"whiteBalance": "custom", "wbTemperature": 90000, "wbTint": -200,
            "hsl": {"reds": {"hue": 250, "saturation": -130}, "blues": null},
            "colorGrading": {"shadows": {"hue": -45, "saturation": 130}, "blending": 125},
            "colorCalibration": {"redHue": 160},
            "sectionVisibility": {"colorMixer": false, "colorGrading": false, "calibration": false}});
        let color = Color::resolve(StoredRef::new(&recipe));
        let wb = color.white_balance.unwrap();
        assert_eq!((wb.temperature.get(), wb.tint.get()), (50_000.0, -150.0));
        assert_eq!(color.hsl, Hsl::default());
        assert_eq!(color.grading.wheels[0].hue, -45.0);
        assert_eq!(color.grading.wheels[0].saturation, 130.0);
        assert_eq!(color.grading.blending, 125.0);
        assert!(!color.grading_visible && !color.calibration_visible);
        assert_eq!(color.calibration.hue[0], 160.0);
    }

    #[test]
    fn stored_ranges_keep_unknown_fields_and_extra_entries() {
        let ranges = json!([{"id": "a", "unknown": {"v": null}}, 3, null, {}, {}, {}, {}, {}, {"id": "ninth"}]);
        let recipe = json!({"colorRanges": ranges});
        let stored = StoredRef::new(&recipe).get(&COLOR_RANGES).unwrap();
        assert_eq!(stored.to_json(), ranges);
        assert_eq!(
            Color::resolve(StoredRef::new(&recipe)).color_ranges.count,
            8
        );
    }
}
