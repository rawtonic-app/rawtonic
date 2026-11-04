//! The tone section: exposure, contrast, the tone sliders, brightness and
//! centré.
//!
//! Ranges are the Develop panel's sliders (`StudioDevelop.tsx`,
//! `BRIGHTNESS_CONTROL` in `src/utils/adjustments.ts`). A stored value
//! outside them (a preset, an import, a hand edit) renders as stored, as it
//! always has: the value objects carry their range, they do not clamp.

use crate::key::Key;
use crate::stored::{Section, StoredRef};

/// Exposure in stops (EV).
pub const EXPOSURE: Key<f64> = Key::new("exposure", 0.0, 1);
pub const CONTRAST: Key<f64> = Key::new("contrast", 0.0, 1);
pub const HIGHLIGHTS: Key<f64> = Key::new("highlights", 0.0, 1);
pub const SHADOWS: Key<f64> = Key::new("shadows", 0.0, 1);
pub const WHITES: Key<f64> = Key::new("whites", 0.0, 1);
pub const BLACKS: Key<f64> = Key::new("blacks", 0.0, 1);
pub const BRIGHTNESS: Key<f64> = Key::new("brightness", 0.0, 1);
/// Local contrast around the photograph's centre of interest.
pub const CENTRE: Key<f64> = Key::new("centré", 0.0, 1);

/// Every key of the section.
pub const KEYS: [&Key<f64>; 8] = [
    &EXPOSURE,
    &CONTRAST,
    &HIGHLIGHTS,
    &SHADOWS,
    &WHITES,
    &BLACKS,
    &BRIGHTNESS,
    &CENTRE,
];

macro_rules! slider_value {
    ($(#[$doc:meta])* $name:ident, $min:expr, $max:expr, $step:expr) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
        pub struct $name(f64);

        impl $name {
            /// The slider's range and step.
            pub const MIN: f64 = $min;
            pub const MAX: f64 = $max;
            pub const STEP: f64 = $step;

            /// `value` as stored (not clamped, see the module).
            pub const fn new(value: f64) -> Self {
                Self(value)
            }

            pub const fn get(self) -> f64 {
                self.0
            }

            /// Whether the value lies on the slider.
            pub fn in_range(self) -> bool {
                (Self::MIN..=Self::MAX).contains(&self.0)
            }
        }
    };
}

slider_value!(
    /// Exposure in stops: −5 … +5 EV in steps of 0.01.
    Ev,
    -5.0,
    5.0,
    0.01
);
slider_value!(
    /// Brightness (a midtone gain): −2 … +2 in steps of 0.01.
    Brightness,
    -2.0,
    2.0,
    0.01
);
slider_value!(
    /// A tone slider: −100 … +100 in steps of 1. Centré has no slider in
    /// the studio; its values (auto adjust, the inherited panel) use this
    /// range too.
    Amount,
    -100.0,
    100.0,
    1.0
);

/// The tone section as it renders: each value as stored, its default when
/// absent, and 0 for the sliders of a section switched off (`basic` for
/// everything but centré, which sits in `details`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Tone {
    pub exposure: Ev,
    pub contrast: Amount,
    pub highlights: Amount,
    pub shadows: Amount,
    pub whites: Amount,
    pub blacks: Amount,
    pub brightness: Brightness,
    pub centre: Amount,
}

impl Tone {
    pub(crate) fn resolve(stored: StoredRef<'_>) -> Self {
        let read = |section: Section, key: &Key<f64>| {
            if stored.section_visible(section) {
                stored.get_or_default(key)
            } else {
                0.0
            }
        };
        let basic = |key: &Key<f64>| read(Section::BASIC, key);
        Self {
            exposure: Ev::new(basic(&EXPOSURE)),
            contrast: Amount::new(basic(&CONTRAST)),
            highlights: Amount::new(basic(&HIGHLIGHTS)),
            shadows: Amount::new(basic(&SHADOWS)),
            whites: Amount::new(basic(&WHITES)),
            blacks: Amount::new(basic(&BLACKS)),
            brightness: Brightness::new(basic(&BRIGHTNESS)),
            centre: Amount::new(read(Section::DETAILS, &CENTRE)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// How the renderer read each value before the tone section moved here
    /// (`get_global_adjustments_from_json`'s `get_val`).
    fn by_string_keys(recipe: &Value, section: &str, key: &str) -> f64 {
        let visible = recipe
            .get("sectionVisibility")
            .and_then(|v| v.get(section))
            .and_then(|s| s.as_bool())
            .unwrap_or(true);
        if visible {
            recipe[key].as_f64().unwrap_or(0.0)
        } else {
            0.0
        }
    }

    #[test]
    fn tone_resolves_as_the_string_keys_did() {
        let recipes = [
            Value::Null,
            json!({}),
            json!([1, 2]),
            json!({"exposure": 1.5, "contrast": 20, "highlights": -100, "shadows": 37.5,
                   "whites": "12", "blacks": null, "brightness": 0.4, "centré": 30}),
            json!({"exposure": 9.0, "brightness": -4.0, "centré": 250}),
            json!({"exposure": 1.0, "centré": 10, "sectionVisibility": {"basic": false}}),
            json!({"exposure": 1.0, "centré": 10, "sectionVisibility": {"details": false}}),
            json!({"exposure": 1.0, "sectionVisibility": {"basic": "off"}}),
            json!({"exposure": 1.0, "sectionVisibility": null}),
        ];
        for recipe in &recipes {
            let tone = Tone::resolve(StoredRef::new(recipe));
            let basic = |key| by_string_keys(recipe, "basic", key);
            assert_eq!(tone.exposure.get(), basic("exposure"), "{recipe}");
            assert_eq!(tone.contrast.get(), basic("contrast"), "{recipe}");
            assert_eq!(tone.highlights.get(), basic("highlights"), "{recipe}");
            assert_eq!(tone.shadows.get(), basic("shadows"), "{recipe}");
            assert_eq!(tone.whites.get(), basic("whites"), "{recipe}");
            assert_eq!(tone.blacks.get(), basic("blacks"), "{recipe}");
            assert_eq!(tone.brightness.get(), basic("brightness"), "{recipe}");
            assert_eq!(
                tone.centre.get(),
                by_string_keys(recipe, "details", "centré"),
                "{recipe}"
            );
        }
    }

    #[test]
    fn values_outside_the_slider_are_kept() {
        let recipe = json!({"exposure": 9.0, "contrast": -250});
        let tone = Tone::resolve(StoredRef::new(&recipe));
        assert_eq!(tone.exposure.get(), 9.0);
        assert!(!tone.exposure.in_range());
        assert_eq!(tone.contrast.get(), -250.0);
        assert!(Ev::new(Ev::MAX).in_range() && Brightness::new(-2.0).in_range());
    }
}
