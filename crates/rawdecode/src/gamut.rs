//! The working gamut of wide-gamut developments: linear sRGB values may go
//! negative for colours outside sRGB, down to the ProPhoto gamut boundary.
//! The matrices are the shader's `srgb_to_prophoto` / `prophoto_to_srgb`
//! (Bradford-adapted, D65 → D50).

const SRGB_TO_PROPHOTO: [[f32; 3]; 3] = [
    [0.529_345_8, 0.330_072_8, 0.140_581_2],
    [0.098_374_4, 0.873_461_1, 0.028_164_7],
    [0.016_883_2, 0.117_672_5, 0.865_444_3],
];
const PROPHOTO_TO_SRGB: [[f32; 3]; 3] = [
    [2.034_076_3, -0.727_334_3, -0.306_741_7],
    [-0.228_813_5, 1.231_730_2, -0.002_916_9],
    [-0.008_569_8, -0.153_286_6, 1.161_856_5],
];

fn apply(m: &[[f32; 3]; 3], [r, g, b]: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| m[i][0] * r + m[i][1] * g + m[i][2] * b)
}

/// Linear sRGB clipped at the ProPhoto gamut from below (colours with no
/// negative channel are returned unchanged).
#[inline]
pub fn floor_to_prophoto(rgb: [f32; 3]) -> [f32; 3] {
    if rgb.iter().all(|v| *v >= 0.0) {
        return rgb;
    }
    let pro = apply(&SRGB_TO_PROPHOTO, rgb);
    if pro.iter().all(|v| *v >= 0.0) {
        return rgb;
    }
    apply(&PROPHOTO_TO_SRGB, pro.map(|v| v.max(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrices_invert_each_other() {
        for rgb in [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.3, 0.5, 0.7],
        ] {
            let back = apply(&PROPHOTO_TO_SRGB, apply(&SRGB_TO_PROPHOTO, rgb));
            for (a, b) in back.iter().zip(rgb) {
                assert!((a - b).abs() < 1e-4, "{back:?} {rgb:?}");
            }
        }
    }

    #[test]
    fn keeps_colours_inside_prophoto_and_clips_beyond() {
        // A saturated green outside sRGB but inside ProPhoto stays.
        let green = apply(&PROPHOTO_TO_SRGB, [0.2, 0.8, 0.1]);
        assert!(green.iter().any(|v| *v < 0.0));
        assert_eq!(floor_to_prophoto(green), green);
        // Beyond ProPhoto: its negative ProPhoto channel goes to zero.
        let beyond = apply(&PROPHOTO_TO_SRGB, [0.5, -0.1, 0.2]);
        let clipped = apply(&SRGB_TO_PROPHOTO, floor_to_prophoto(beyond));
        assert!(
            clipped[1].abs() < 1e-5 && (clipped[0] - 0.5).abs() < 1e-4,
            "{clipped:?}"
        );
    }
}
