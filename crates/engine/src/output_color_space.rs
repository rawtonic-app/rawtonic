//! Output colour spaces of exports: sRGB, Display P3, Adobe RGB (1998) and
//! ProPhoto RGB (ROMM RGB). The renderer works in linear sRGB primaries;
//! an export in another space ends with this space's matrix and transfer
//! curve, and carries the matching ICC profile (`icc_profile`).

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "camelCase")]
pub enum OutputColorSpace {
    #[default]
    Srgb,
    DisplayP3,
    AdobeRgb,
    ProPhoto,
}

/// How encoded values relate to linear ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransferCurve {
    /// IEC 61966-2-1 (sRGB and Display P3).
    Srgb,
    /// A pure power law: Adobe RGB 563/256, ProPhoto 461/256 (1.8 in the
    /// profile's u8Fixed8 encoding, as the common ProPhoto profiles store it).
    Gamma(f64),
}

const D65: [f64; 2] = [0.3127, 0.3290];
const D50: [f64; 2] = [0.3457, 0.3585];
const SRGB_PRIMARIES: [[f64; 2]; 3] = [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]];
/// SMPTE EG 432-1 primaries with a D65 white.
const DISPLAY_P3_PRIMARIES: [[f64; 2]; 3] = [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]];
const ADOBE_RGB_PRIMARIES: [[f64; 2]; 3] = [[0.64, 0.33], [0.21, 0.71], [0.15, 0.06]];
/// ISO 22028-2 (ROMM RGB), D50 white.
const PROPHOTO_PRIMARIES: [[f64; 2]; 3] = [[0.7347, 0.2653], [0.1596, 0.8404], [0.0366, 0.0001]];
/// ICC profile connection space white.
pub const PCS_D50: [f64; 3] = [0.9642, 1.0, 0.8249];

type Mat = [[f64; 3]; 3];

impl OutputColorSpace {
    pub const ALL: [Self; 4] = [Self::Srgb, Self::DisplayP3, Self::AdobeRgb, Self::ProPhoto];

    /// Code in the shader's `output_space` uniform (0 = sRGB).
    pub fn shader_code(self) -> u32 {
        match self {
            Self::Srgb => 0,
            Self::DisplayP3 => 1,
            Self::AdobeRgb => 2,
            Self::ProPhoto => 3,
        }
    }

    /// The space of a shader `output_space` code (`shader_code`).
    pub fn from_shader_code(code: u32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|space| space.shader_code() == code)
    }

    pub fn is_wide(self) -> bool {
        self != Self::Srgb
    }

    /// From a command-line name: `srgb`, `display-p3`, `adobe-rgb`,
    /// `prophoto` (case and separators ignored; the serde names work too).
    pub fn parse(name: &str) -> Option<Self> {
        let key: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        match key.as_str() {
            "srgb" => Some(Self::Srgb),
            "displayp3" | "p3" => Some(Self::DisplayP3),
            "adobergb" | "adobergb1998" => Some(Self::AdobeRgb),
            "prophoto" | "prophotorgb" | "rommrgb" => Some(Self::ProPhoto),
            _ => None,
        }
    }

    /// The name shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            Self::Srgb => "sRGB",
            Self::DisplayP3 => "Display P3",
            Self::AdobeRgb => "Adobe RGB (1998)",
            Self::ProPhoto => "ProPhoto RGB",
        }
    }

    /// The ICC profile description.
    pub fn profile_name(self) -> &'static str {
        match self {
            Self::Srgb => "sRGB IEC61966-2.1",
            Self::DisplayP3 => "Display P3",
            Self::AdobeRgb => "Adobe RGB (1998) compatible",
            Self::ProPhoto => "ROMM RGB (ProPhoto RGB compatible)",
        }
    }

    pub fn primaries(self) -> [[f64; 2]; 3] {
        match self {
            Self::Srgb => SRGB_PRIMARIES,
            Self::DisplayP3 => DISPLAY_P3_PRIMARIES,
            Self::AdobeRgb => ADOBE_RGB_PRIMARIES,
            Self::ProPhoto => PROPHOTO_PRIMARIES,
        }
    }

    pub fn white(self) -> [f64; 2] {
        match self {
            Self::ProPhoto => D50,
            _ => D65,
        }
    }

    pub fn transfer(self) -> TransferCurve {
        match self {
            Self::Srgb | Self::DisplayP3 => TransferCurve::Srgb,
            Self::AdobeRgb => TransferCurve::Gamma(563.0 / 256.0),
            Self::ProPhoto => TransferCurve::Gamma(461.0 / 256.0),
        }
    }

    /// Linear RGB → XYZ under the space's own white.
    pub fn to_xyz(self) -> Mat {
        rgb_to_xyz(self.primaries(), self.white())
    }

    /// The primaries' XYZ adapted to the D50 connection space (Bradford):
    /// the profile's rXYZ, gXYZ and bXYZ, one row per colorant.
    pub fn pcs_colorants(self) -> [[f64; 3]; 3] {
        let adapted = multiply(&bradford(xy_to_xyz(self.white()), PCS_D50), &self.to_xyz());
        std::array::from_fn(|c| std::array::from_fn(|r| adapted[r][c]))
    }

    /// Linear sRGB (D65) → this space's linear RGB, white adapted with
    /// Bradford where the whites differ.
    pub fn from_linear_srgb(self) -> Mat {
        let srgb = Self::Srgb.to_xyz();
        let adapt = bradford(xy_to_xyz(D65), xy_to_xyz(self.white()));
        multiply(&invert(&self.to_xyz()), &multiply(&adapt, &srgb))
    }

    /// This space's linear RGB → linear sRGB (D65).
    pub fn to_linear_srgb(self) -> Mat {
        invert(&self.from_linear_srgb())
    }

    /// Linear RGB in these primaries → linear sRGB as if the space's white
    /// were D65 (no adaptation), the conversion encoders such as JPEG XL's
    /// apply to custom primaries.
    pub fn to_linear_srgb_with_d65_white(self) -> Mat {
        let srgb = Self::Srgb.to_xyz();
        multiply(&invert(&srgb), &rgb_to_xyz(self.primaries(), D65))
    }

    /// The same matrix in f32 for the shader.
    pub fn from_linear_srgb_f32(self) -> [[f32; 3]; 3] {
        let m = self.from_linear_srgb();
        std::array::from_fn(|r| std::array::from_fn(|c| m[r][c] as f32))
    }
}

impl TransferCurve {
    pub fn to_linear(self, v: f64) -> f64 {
        match self {
            Self::Srgb if v <= 0.040_45 => v / 12.92,
            Self::Srgb => ((v + 0.055) / 1.055).powf(2.4),
            Self::Gamma(g) => v.max(0.0).powf(g),
        }
    }

    pub fn from_linear(self, v: f64) -> f64 {
        match self {
            Self::Srgb if v <= 0.003_130_8 => v * 12.92,
            Self::Srgb => 1.055 * v.powf(1.0 / 2.4) - 0.055,
            Self::Gamma(g) => v.max(0.0).powf(1.0 / g),
        }
    }
}

fn xy_to_xyz([x, y]: [f64; 2]) -> [f64; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}

fn rgb_to_xyz(primaries: [[f64; 2]; 3], white: [f64; 2]) -> Mat {
    let columns = primaries.map(xy_to_xyz);
    let p: Mat = std::array::from_fn(|r| std::array::from_fn(|c| columns[c][r]));
    let s = apply(&invert(&p), xy_to_xyz(white));
    std::array::from_fn(|r| std::array::from_fn(|c| p[r][c] * s[c]))
}

fn bradford(source: [f64; 3], destination: [f64; 3]) -> Mat {
    const B: Mat = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let s = apply(&B, source);
    let d = apply(&B, destination);
    let scale: Mat =
        std::array::from_fn(|r| std::array::from_fn(|c| if r == c { d[r] / s[r] } else { 0.0 }));
    multiply(&invert(&B), &multiply(&scale, &B))
}

fn apply(m: &Mat, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| (0..3).map(|c| m[r][c] * v[c]).sum())
}

/// 3 × 3 matrix product.
pub fn multiply(a: &Mat, b: &Mat) -> Mat {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum()))
}

/// 3 × 3 matrix inverse.
pub fn invert(m: &Mat) -> Mat {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let cofactor = |r: usize, c: usize| {
        let rows: Vec<usize> = (0..3).filter(|&i| i != r).collect();
        let cols: Vec<usize> = (0..3).filter(|&i| i != c).collect();
        let minor =
            m[rows[0]][cols[0]] * m[rows[1]][cols[1]] - m[rows[0]][cols[1]] * m[rows[1]][cols[0]];
        if (r + c).is_multiple_of(2) {
            minor
        } else {
            -minor
        }
    };
    std::array::from_fn(|r| std::array::from_fn(|c| cofactor(c, r) / det))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_maps_to_white() {
        for space in OutputColorSpace::ALL {
            let white = apply(&space.from_linear_srgb(), [1.0; 3]);
            for v in white {
                assert!((v - 1.0).abs() < 1e-9, "{space:?} {white:?}");
            }
        }
    }

    #[test]
    fn srgb_is_identity() {
        let m = OutputColorSpace::Srgb.from_linear_srgb();
        for (r, row) in m.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                assert!((v - if r == c { 1.0 } else { 0.0 }).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn wide_spaces_hold_the_srgb_cube() {
        // Every sRGB colour lies inside the wider spaces: all coefficients
        // of the conversion are non-negative (sRGB and P3 share blue).
        for space in [
            OutputColorSpace::DisplayP3,
            OutputColorSpace::AdobeRgb,
            OutputColorSpace::ProPhoto,
        ] {
            for row in space.from_linear_srgb() {
                assert!(row.iter().all(|v| *v >= -1e-12), "{space:?} {row:?}");
            }
        }
    }

    #[test]
    fn known_conversions() {
        // sRGB red in Display P3 (CSS Color 4: color(display-p3 0.9175 0.2003 0.1386)).
        let p3 = apply(
            &OutputColorSpace::DisplayP3.from_linear_srgb(),
            [1.0, 0.0, 0.0],
        )
        .map(|v| TransferCurve::Srgb.from_linear(v));
        for (got, want) in p3.iter().zip([0.9175, 0.2003, 0.1386]) {
            assert!((got - want).abs() < 5e-4, "{p3:?}");
        }
        // Linear sRGB → ProPhoto, as Lindbloom's Bradford-adapted matrices.
        let m = OutputColorSpace::ProPhoto.from_linear_srgb();
        assert!(
            (m[0][0] - 0.5293).abs() < 2e-4 && (m[1][1] - 0.8735).abs() < 2e-4,
            "{m:?}"
        );
    }

    /// The clipping warnings (shader.wgsl `clipping_sides`) test a wide
    /// space only where sRGB clips: every space holds sRGB, so a colour inside
    /// sRGB's thresholds lies inside its own.
    #[test]
    fn colours_srgb_does_not_clip_clip_in_no_space() {
        let srgb = TransferCurve::Srgb;
        for space in OutputColorSpace::ALL {
            for row in space.from_linear_srgb() {
                assert!(row.iter().all(|&c| c > -1e-9), "{space:?} {row:?}");
                assert!(
                    (row.iter().sum::<f64>() - 1.0).abs() < 1e-6,
                    "{space:?} {row:?}"
                );
            }
            let t = space.transfer();
            assert!(t.to_linear(0.998) >= srgb.to_linear(0.998), "{space:?}");
            assert!(t.to_linear(0.002) <= srgb.to_linear(0.002), "{space:?}");
        }
    }

    #[test]
    fn transfer_curves_invert() {
        for space in OutputColorSpace::ALL {
            let t = space.transfer();
            for i in 0..=100 {
                let v = i as f64 / 100.0;
                assert!((t.to_linear(t.from_linear(v)) - v).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn parses_command_line_names() {
        for (name, space) in [
            ("srgb", OutputColorSpace::Srgb),
            ("display-p3", OutputColorSpace::DisplayP3),
            ("displayP3", OutputColorSpace::DisplayP3),
            ("adobe-rgb", OutputColorSpace::AdobeRgb),
            ("ProPhoto", OutputColorSpace::ProPhoto),
        ] {
            assert_eq!(OutputColorSpace::parse(name), Some(space), "{name}");
        }
        assert_eq!(OutputColorSpace::parse("rec2020"), None);
    }

    #[test]
    fn serde_names() {
        let names: Vec<String> = OutputColorSpace::ALL
            .iter()
            .map(|s| serde_json::to_string(s).unwrap())
            .collect();
        assert_eq!(
            names,
            ["\"srgb\"", "\"displayP3\"", "\"adobeRgb\"", "\"proPhoto\""]
        );
    }
}
