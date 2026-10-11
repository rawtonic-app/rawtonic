//! Lightroom's Calibration (red, green and blue primary hue and saturation;
//! shadows tint) as Camera Raw renders it (notes: reference/calibration.md):
//! each primary of the linear ProPhoto working space turns about the grey
//! axis (hue) and moves away from or towards it (saturation); the matrix
//! keeps white. Shadows tint takes green out of the shadows (positive,
//! magenta) or red and blue (negative, green).
use glam::{Mat3, Vec3};
use serde_json::Value;

/// Degrees of rotation per slider unit for lowering / raising, per primary.
const HUE_GAIN: [[f32; 2]; 3] = [[0.245, 0.28], [0.287, 0.257], [0.335, 0.30]];
/// Saturation: the primary's distance from grey scales by
/// exp(gain · slider / 100), gains for lowering / raising.
const SATURATION_GAIN: [[f32; 2]; 3] = [[0.88, 1.1], [1.06, 1.3], [0.99, 1.2]];
/// Share of the luminance change the saturation sliders take back (red,
/// green, blue; negative: Camera Raw changes luminance more than the
/// matrix does).
const SATURATION_LUMINANCE_KEEP: [f32; 3] = [0.5, -0.45, 0.2];
/// Shadows tint: strength, and the shadow weight exp(−(Y / sigma)^power) on
/// ProPhoto luminance.
pub const SHADOW_TINT: [f32; 3] = [0.12, 0.1995, 0.9006];

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Calibration {
    /// Hue and saturation of red, green and blue (−100…100).
    pub hue: [f32; 3],
    pub saturation: [f32; 3],
    /// Shadows tint −100…100 (positive towards magenta).
    pub shadows_tint: f32,
}

impl Calibration {
    pub fn from_json(value: &Value) -> Self {
        let get = |key: &str| (value[key].as_f64().unwrap_or(0.0) as f32).clamp(-100.0, 100.0);
        Self {
            hue: [get("redHue"), get("greenHue"), get("blueHue")],
            saturation: [
                get("redSaturation"),
                get("greenSaturation"),
                get("blueSaturation"),
            ],
            shadows_tint: get("shadowsTint"),
        }
    }

    pub fn is_neutral(&self) -> bool {
        self.hue.iter().chain(&self.saturation).all(|v| *v == 0.0) && self.shadows_tint == 0.0
    }

    /// How much of the matrix's luminance change to undo: the saturation
    /// sliders' shares weighted by their size.
    pub fn luminance_keep(&self) -> f32 {
        let total: f32 = self.saturation.iter().map(|s| s.abs()).sum();
        if total == 0.0 {
            return 0.0;
        }
        self.saturation
            .iter()
            .zip(SATURATION_LUMINANCE_KEEP)
            .map(|(s, k)| s.abs() * k)
            .sum::<f32>()
            / total
    }

    /// Linear ProPhoto → linear ProPhoto matrix (white stays white).
    pub fn matrix(&self) -> Mat3 {
        let grey_axis = Vec3::ONE.normalize();
        let columns: [Vec3; 3] = std::array::from_fn(|k| {
            let primary = Vec3::from(std::array::from_fn::<f32, 3, _>(|i| f32::from(i == k)));
            let grey = Vec3::splat(primary.element_sum() / 3.0);
            let s = self.saturation[k];
            let scale = (SATURATION_GAIN[k][usize::from(s > 0.0)] * s / 100.0).exp();
            let moved = grey + (primary - grey) * scale;
            let h = self.hue[k];
            let angle = (HUE_GAIN[k][usize::from(h > 0.0)] * h).to_radians();
            // Rodrigues rotation about the grey axis.
            moved * angle.cos()
                + grey_axis.cross(moved) * angle.sin()
                + grey_axis * grey_axis.dot(moved) * (1.0 - angle.cos())
        });
        let m = Mat3::from_cols(columns[0], columns[1], columns[2]);
        let keep_white = m.inverse() * Vec3::ONE;
        m * Mat3::from_diagonal(keep_white)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn neutral_calibration_is_identity_and_white_stays() {
        let neutral = Calibration::from_json(&json!({}));
        assert!(neutral.is_neutral());
        assert!(neutral.matrix().abs_diff_eq(Mat3::IDENTITY, 1.0e-6));
        let c = Calibration::from_json(
            &json!({"redHue": 60, "greenSaturation": -40, "blueSaturation": 80, "blueHue": -30}),
        );
        assert!((c.matrix() * Vec3::ONE).abs_diff_eq(Vec3::ONE, 1.0e-5));
        assert!((c.matrix() * Vec3::splat(0.2)).abs_diff_eq(Vec3::splat(0.2), 1.0e-5));
    }

    #[test]
    fn blue_saturation_saturates_other_colours_too() {
        // Lightroom's classic: raising blue saturation makes reds and greens
        // more saturated as well.
        let m = Calibration::from_json(&json!({"blueSaturation": 100})).matrix();
        let orange = Vec3::new(0.6, 0.35, 0.1);
        let out = m * orange;
        let spread = |v: Vec3| v.max_element() - v.min_element();
        assert!(spread(out) > spread(orange) * 1.1);
    }
}
