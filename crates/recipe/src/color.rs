//! Colour slider values, kept as stored without clamping.

use crate::tone::Amount;
use crate::{Key, Section, StoredRef};

pub const TEMPERATURE: Key<f64> = Key::new("temperature", 0.0, 1);
pub const TINT: Key<f64> = Key::new("tint", 0.0, 1);
pub const VIBRANCE: Key<f64> = Key::new("vibrance", 0.0, 1);
pub const SATURATION: Key<f64> = Key::new("saturation", 0.0, 1);
pub const HUE: Key<f64> = Key::new("hue", 0.0, 1);

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
}

impl Color {
    pub(crate) fn resolve(stored: StoredRef<'_>) -> Self {
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
}
