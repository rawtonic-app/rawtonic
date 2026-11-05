//! Colour slider values, kept as stored without clamping.

use crate::tone::Amount;
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
pub const COLOR_RANGES: Key<Value> = Key::new("colorRanges", Value::Null, 1);
pub const COLOR_RANGE_SHOW: Key<String> = Key::new("colorRangeShow", String::new(), 1);
pub const RANGE_ID: Key<String> = Key::new("id", String::new(), 1);
pub const RANGE_KEYS: [Key<f64>; 12] = [
    Key::new("hue", 0.0, 1),
    Key::new("chroma", 0.0, 1),
    Key::new("lightness", 0.5, 1),
    Key::new("lightnessRange", 100.0, 1),
    Key::new("hueRange", 25.0, 1),
    Key::new("chromaRange", 70.0, 1),
    Key::new("smoothness", 50.0, 1),
    Key::new("hueShift", 0.0, 1),
    Key::new("saturation", 0.0, 1),
    Key::new("lightnessShift", 0.0, 1),
    Key::new("hueUniformity", 0.0, 1),
    Key::new("saturationUniformity", 0.0, 1),
];
pub const LIGHTNESS_UNIFORMITY: Key<f64> = Key::new("lightnessUniformity", 0.0, 1);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColorRange {
    pub values: [f64; 12],
    pub lightness_uniformity: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColorRanges {
    pub ranges: [ColorRange; 8],
    pub count: usize,
    pub shown: Option<usize>,
}
pub const BW_VERSION: Key<u64> = Key::new("bwVersion", 1, 1);
pub const GRAY_MIXER: Key<GrayMixer> = Key::new("grayMixer", GrayMixer { channels: [0.0; 8] }, 1);
pub const GRAY_CHANNELS: [&str; 8] = [
    "red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta",
];

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GrayMixer {
    pub channels: [f64; 8],
}
impl KeyValue for GrayMixer {
    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            channels: GRAY_CHANNELS
                .map(|name| value.get(name).and_then(Value::as_f64).unwrap_or(0.0)),
        })
    }
    fn to_json(&self) -> Value {
        Value::Object(
            GRAY_CHANNELS
                .iter()
                .zip(self.channels)
                .map(|(name, v)| ((*name).into(), v.into()))
                .collect(),
        )
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
pub const COLOR_CALIBRATION: Key<Calibration> =
    Key::new("colorCalibration", Calibration { values: [0.0; 7] }, 1);
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
pub const GRADING_WHEELS: [&str; 4] = ["shadows", "midtones", "highlights", "global"];
pub const HSL_CHANNELS: [&str; 8] = [
    "reds", "oranges", "yellows", "greens", "aquas", "blues", "purples", "magentas",
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
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hsl {
    pub channels: [HslChannel; 8],
}
impl KeyValue for Hsl {
    fn from_json(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let channels = std::array::from_fn(|i| {
            let channel = object.get(HSL_CHANNELS[i]).unwrap_or(&Value::Null);
            let read = |key: &Key<f64>| {
                channel
                    .get(key.name())
                    .and_then(Value::as_f64)
                    .unwrap_or(key.default())
            };
            HslChannel {
                hue: read(&HUE),
                saturation: read(&SATURATION),
                luminance: read(&LUMINANCE),
            }
        });
        Some(Self { channels })
    }
    fn to_json(&self) -> Value {
        let channels = self.channels.iter().enumerate().map(|(i, channel)| {
            (HSL_CHANNELS[i].into(), serde_json::json!({HUE.name(): channel.hue, SATURATION.name(): channel.saturation, LUMINANCE.name(): channel.luminance}))
        }).collect();
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
            wheels: GRADING_WHEELS.map(|name| {
                let wheel = value.get(name).unwrap_or(&Value::Null);
                HslChannel {
                    hue: read(wheel, &HUE),
                    saturation: read(wheel, &SATURATION),
                    luminance: read(wheel, &LUMINANCE),
                }
            }),
            blending: read(value, &BLENDING),
            balance: read(value, &BALANCE),
        })
    }
    fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        for (name, wheel) in GRADING_WHEELS.iter().zip(self.wheels) {
            map.insert((*name).into(), serde_json::json!({HUE.name(): wheel.hue, SATURATION.name(): wheel.saturation, LUMINANCE.name(): wheel.luminance}));
        }
        map.insert(BLENDING.name().into(), self.blending.into());
        map.insert(BALANCE.name().into(), self.balance.into());
        Value::Object(map)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Calibration {
    pub values: [f64; 7],
}
impl KeyValue for Calibration {
    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            values: CALIBRATION_KEYS.map(|key| {
                value
                    .get(key.name())
                    .and_then(Value::as_f64)
                    .unwrap_or(key.default())
            }),
        })
    }
    fn to_json(&self) -> Value {
        Value::Object(
            CALIBRATION_KEYS
                .iter()
                .zip(self.values)
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

impl KeyValue for WhiteBalance {
    fn from_json(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "asShot" => Some(Self::AsShot),
            "daylight" => Some(Self::Daylight),
            "cloudy" => Some(Self::Cloudy),
            "shade" => Some(Self::Shade),
            "tungsten" => Some(Self::Tungsten),
            "fluorescent" => Some(Self::Fluorescent),
            "flash" => Some(Self::Flash),
            "custom" => Some(Self::Custom),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
    fn to_json(&self) -> Value {
        Value::String(
            match self {
                Self::AsShot => "asShot",
                Self::Daylight => "daylight",
                Self::Cloudy => "cloudy",
                Self::Shade => "shade",
                Self::Tungsten => "tungsten",
                Self::Fluorescent => "fluorescent",
                Self::Flash => "flash",
                Self::Custom => "custom",
                Self::Auto => "auto",
            }
            .into(),
        )
    }
}

pub const KEYS: [&Key<f64>; 5] = [&TEMPERATURE, &TINT, &VIBRANCE, &SATURATION, &HUE];

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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Color {
    pub temperature: Amount,
    pub tint: Amount,
    pub vibrance: Amount,
    pub saturation: Amount,
    pub hue: Hue,
    /// Preset or custom white balance, independent of section visibility.
    pub white_balance: Option<(f64, f64)>,
    pub white_balance_visible: bool,
    pub hsl: Hsl,
    pub grading: Grading,
    pub grading_visible: bool,
    pub calibration: Calibration,
    pub calibration_visible: bool,
    pub black_and_white: bool,
    pub gray_mixer: GrayMixer,
    pub color_ranges: ColorRanges,
}

impl Color {
    pub(crate) fn resolve(stored: StoredRef<'_>) -> Self {
        let mut color_ranges = ColorRanges::default();
        if stored.section_visible(Section::COLOR) && stored.section_visible(Section::COLOR_EDITOR) {
            let root = stored.as_value();
            let shown = root.get(COLOR_RANGE_SHOW.name()).and_then(Value::as_str);
            if let Some(ranges) = root.get(COLOR_RANGES.name()).and_then(Value::as_array) {
                for (i, range) in ranges.iter().take(8).enumerate() {
                    let read = |key: &Key<f64>| {
                        range
                            .get(key.name())
                            .and_then(Value::as_f64)
                            .unwrap_or(key.default())
                    };
                    color_ranges.ranges[i] = ColorRange {
                        values: RANGE_KEYS.each_ref().map(read),
                        lightness_uniformity: read(&LIGHTNESS_UNIFORMITY),
                    };
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
        };
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
            color_ranges,
        }
    }
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
        assert_eq!(color.white_balance, Some((50_000.0, -150.0)));
        assert_eq!(color.hsl, Hsl::default());
        assert_eq!(color.grading.wheels[0].hue, -45.0);
        assert_eq!(color.grading.wheels[0].saturation, 130.0);
        assert_eq!(color.grading.blending, 125.0);
        assert!(!color.grading_visible && !color.calibration_visible);
        assert_eq!(color.calibration.values[1], 160.0);
    }
}
