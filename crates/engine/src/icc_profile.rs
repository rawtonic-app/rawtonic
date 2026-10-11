//! The ICC profiles embedded in exports.
//!
//! Every export carries the profile of its output colour space (sRGB by
//! default, or Display P3, Adobe RGB (1998), ProPhoto RGB). Each is a version 2
//! matrix/TRC display profile, the form Lightroom and Capture One embed, which
//! older readers handle more reliably than version 4. The bytes are built here
//! from the published primaries, white points and curves rather than copied
//! from a vendor profile, so no third-party licence applies.

use std::sync::LazyLock;

use crate::output_color_space::{OutputColorSpace, PCS_D50, TransferCurve};

/// sRGB primaries adapted to the D50 profile connection space (Bradford).
const SRGB_COLORANTS: [[f64; 3]; 3] = [
    [0.436_074_7, 0.222_504_5, 0.013_932_2],
    [0.385_064_9, 0.716_878_6, 0.097_104_5],
    [0.143_080_4, 0.060_616_9, 0.714_173_3],
];
/// Media white point: D65, as in the IEC 61966-2.1 reference profile.
const D65: [f64; 3] = [0.950_455, 1.0, 1.089_058];
const CURVE_POINTS: usize = 1024;

static SRGB: LazyLock<Vec<u8>> = LazyLock::new(build_srgb);
static DISPLAY_P3: LazyLock<Vec<u8>> = LazyLock::new(|| build_wide(OutputColorSpace::DisplayP3));
static ADOBE_RGB: LazyLock<Vec<u8>> = LazyLock::new(|| build_wide(OutputColorSpace::AdobeRgb));
static PROPHOTO: LazyLock<Vec<u8>> = LazyLock::new(|| build_wide(OutputColorSpace::ProPhoto));

/// Version 2 sRGB IEC61966-2.1 profile.
pub fn srgb() -> &'static [u8] {
    &SRGB
}

/// The version 2 profile of an output colour space.
pub fn for_space(space: OutputColorSpace) -> &'static [u8] {
    match space {
        OutputColorSpace::Srgb => &SRGB,
        OutputColorSpace::DisplayP3 => &DISPLAY_P3,
        OutputColorSpace::AdobeRgb => &ADOBE_RGB,
        OutputColorSpace::ProPhoto => &PROPHOTO,
    }
}

fn s15_fixed16(value: f64) -> [u8; 4] {
    ((value * 65536.0).round() as i32).to_be_bytes()
}

fn xyz_tag(xyz: [f64; 3]) -> Vec<u8> {
    let mut tag = b"XYZ \0\0\0\0".to_vec();
    for v in xyz {
        tag.extend_from_slice(&s15_fixed16(v));
    }
    tag
}

fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The IEC 61966-2-1 curve, sampled.
fn srgb_curve() -> Vec<u8> {
    let mut curve = b"curv\0\0\0\0".to_vec();
    curve.extend_from_slice(&(CURVE_POINTS as u32).to_be_bytes());
    for i in 0..CURVE_POINTS {
        let linear = srgb_to_linear(i as f64 / (CURVE_POINTS - 1) as f64);
        curve.extend_from_slice(&((linear * 65535.0).round() as u16).to_be_bytes());
    }
    curve
}

fn build_srgb() -> Vec<u8> {
    build("sRGB IEC61966-2.1", D65, SRGB_COLORANTS, srgb_curve())
}

fn build_wide(space: OutputColorSpace) -> Vec<u8> {
    let curve = match space.transfer() {
        TransferCurve::Srgb => srgb_curve(),
        // One entry: the exponent as u8Fixed8Number (563/256, 461/256).
        TransferCurve::Gamma(gamma) => {
            let mut curve = b"curv\0\0\0\0".to_vec();
            curve.extend_from_slice(&1u32.to_be_bytes());
            curve.extend_from_slice(&((gamma * 256.0).round() as u16).to_be_bytes());
            curve
        }
    };
    let white = match space {
        OutputColorSpace::ProPhoto => PCS_D50,
        _ => D65,
    };
    build(space.profile_name(), white, space.pcs_colorants(), curve)
}

fn build(name: &str, white: [f64; 3], colorants: [[f64; 3]; 3], curve: Vec<u8>) -> Vec<u8> {
    // textDescriptionType: ASCII, then empty Unicode and ScriptCode parts.
    let mut desc = b"desc\0\0\0\0".to_vec();
    desc.extend_from_slice(&(name.len() as u32 + 1).to_be_bytes());
    desc.extend_from_slice(name.as_bytes());
    desc.push(0);
    desc.extend_from_slice(&[0; 4 + 4 + 2 + 1 + 67]);

    let mut cprt = b"text\0\0\0\0".to_vec();
    cprt.extend_from_slice(b"No copyright, use freely\0");

    // The three TRC tags share one curve, as the reference profile does.
    let data: [(&[u8; 4], Vec<u8>); 7] = [
        (b"desc", desc),
        (b"cprt", cprt),
        (b"wtpt", xyz_tag(white)),
        (b"rXYZ", xyz_tag(colorants[0])),
        (b"gXYZ", xyz_tag(colorants[1])),
        (b"bXYZ", xyz_tag(colorants[2])),
        (b"rTRC", curve),
    ];
    let tag_count = data.len() + 2;
    let mut offset = 128 + 4 + tag_count * 12;
    let mut table = Vec::new();
    let mut body = Vec::new();
    let mut curve_entry = (0u32, 0u32);
    for (signature, bytes) in &data {
        let entry = (offset as u32, bytes.len() as u32);
        table.extend_from_slice(*signature);
        table.extend_from_slice(&entry.0.to_be_bytes());
        table.extend_from_slice(&entry.1.to_be_bytes());
        if *signature == b"rTRC" {
            curve_entry = entry;
        }
        body.extend_from_slice(bytes);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        offset = 128 + 4 + tag_count * 12 + body.len();
    }
    for signature in [b"gTRC", b"bTRC"] {
        table.extend_from_slice(signature);
        table.extend_from_slice(&curve_entry.0.to_be_bytes());
        table.extend_from_slice(&curve_entry.1.to_be_bytes());
    }

    let size = 128 + 4 + table.len() + body.len();
    let mut profile = Vec::with_capacity(size);
    profile.extend_from_slice(&(size as u32).to_be_bytes());
    profile.extend_from_slice(&[0; 4]); // preferred CMM
    profile.extend_from_slice(&[0x02, 0x10, 0, 0]); // version 2.1
    profile.extend_from_slice(b"mntrRGB XYZ ");
    // 2026-01-01 00:00:00, fixed so the bytes never change.
    for v in [2026u16, 1, 1, 0, 0, 0] {
        profile.extend_from_slice(&v.to_be_bytes());
    }
    profile.extend_from_slice(b"acsp");
    profile.extend_from_slice(&[0; 4 + 4 + 4 + 4 + 8]); // platform, flags, device, model, attributes
    profile.extend_from_slice(&0u32.to_be_bytes()); // perceptual intent
    for v in PCS_D50 {
        profile.extend_from_slice(&s15_fixed16(v));
    }
    profile.extend_from_slice(&[0; 4]); // creator
    profile.extend_from_slice(&[0; 16 + 28]); // profile ID (v4 only), reserved
    debug_assert_eq!(profile.len(), 128);
    profile.extend_from_slice(&(tag_count as u32).to_be_bytes());
    profile.extend_from_slice(&table);
    profile.extend_from_slice(&body);
    profile
}

#[cfg(test)]
mod tests {
    use super::*;
    use moxcms::{ColorProfile, Layout, TransformOptions};

    #[test]
    fn header_is_a_version_2_rgb_display_profile() {
        let p = srgb();
        assert_eq!(
            u32::from_be_bytes(p[0..4].try_into().unwrap()) as usize,
            p.len()
        );
        assert_eq!(&p[8..12], &[0x02, 0x10, 0, 0]);
        assert_eq!(&p[12..24], b"mntrRGB XYZ ");
        assert_eq!(&p[36..40], b"acsp");
        assert!(p.len() < 4096, "{} bytes", p.len());
    }

    #[test]
    fn decodes_to_the_standard_srgb_colorimetry() {
        let ours = ColorProfile::new_from_slice(srgb()).unwrap();
        let decoded = [ours.red_colorant, ours.green_colorant, ours.blue_colorant];
        for (got, want) in decoded.iter().zip(SRGB_COLORANTS) {
            for (g, w) in [got.x, got.y, got.z].into_iter().zip(want) {
                // s15Fixed16 resolution is 1.5e-5.
                assert!((g - w).abs() < 2e-5, "{g} vs {w}");
            }
        }
    }

    #[test]
    fn converts_like_the_moxcms_srgb_profile() {
        // moxcms derives its sRGB colorants with slightly different white
        // points (differences in the fourth decimal), so allow one 8-bit code.
        let ours = ColorProfile::new_from_slice(srgb()).unwrap();
        let transform = ours
            .create_transform_8bit(
                Layout::Rgb,
                &ColorProfile::new_srgb(),
                Layout::Rgb,
                TransformOptions::default(),
            )
            .unwrap();
        let mut source = Vec::new();
        for r in (0..=255u16).step_by(15) {
            for g in (0..=255u16).step_by(15) {
                for b in (0..=255u16).step_by(15) {
                    source.extend([r as u8, g as u8, b as u8]);
                }
            }
        }
        let mut converted = vec![0u8; source.len()];
        transform.transform(&source, &mut converted).unwrap();
        for (pixel, out) in source.chunks(3).zip(converted.chunks(3)) {
            let worst = pixel
                .iter()
                .zip(out)
                .map(|(a, b)| (*a as i16 - *b as i16).abs())
                .max()
                .unwrap();
            if pixel[0] == pixel[1] && pixel[1] == pixel[2] {
                assert_eq!(pixel, out, "grey");
            } else {
                assert!(worst <= 1, "{pixel:?} -> {out:?}");
            }
        }
    }

    fn worst_8bit_difference(ours: &[u8], reference: &ColorProfile, target: &ColorProfile) -> i16 {
        let ours = ColorProfile::new_from_slice(ours).unwrap();
        let options = TransformOptions::default();
        let a = ours
            .create_transform_8bit(Layout::Rgb, target, Layout::Rgb, options)
            .unwrap();
        let b = reference
            .create_transform_8bit(Layout::Rgb, target, Layout::Rgb, options)
            .unwrap();
        let mut source = Vec::new();
        for r in (0..=255u16).step_by(15) {
            for g in (0..=255u16).step_by(15) {
                for b in (0..=255u16).step_by(15) {
                    source.extend([r as u8, g as u8, b as u8]);
                }
            }
        }
        let mut via_ours = vec![0u8; source.len()];
        let mut via_reference = vec![0u8; source.len()];
        a.transform(&source, &mut via_ours).unwrap();
        b.transform(&source, &mut via_reference).unwrap();
        via_ours
            .iter()
            .zip(&via_reference)
            .map(|(x, y)| (*x as i16 - *y as i16).abs())
            .max()
            .unwrap()
    }

    #[test]
    fn wide_profiles_convert_like_the_moxcms_profiles() {
        // Into a profile as wide as the source, so nothing clips.
        let target = ColorProfile::new_pro_photo_rgb();
        for (space, reference) in [
            (OutputColorSpace::DisplayP3, ColorProfile::new_display_p3()),
            (OutputColorSpace::AdobeRgb, ColorProfile::new_adobe_rgb()),
            (
                OutputColorSpace::ProPhoto,
                ColorProfile::new_pro_photo_rgb(),
            ),
        ] {
            let worst = worst_8bit_difference(for_space(space), &reference, &target);
            assert!(worst <= 1, "{space:?}: {worst}");
        }
    }

    #[test]
    fn wide_profiles_are_version_2_with_their_names() {
        for space in OutputColorSpace::ALL {
            let p = for_space(space);
            assert_eq!(
                u32::from_be_bytes(p[0..4].try_into().unwrap()) as usize,
                p.len()
            );
            assert_eq!(&p[8..12], &[0x02, 0x10, 0, 0]);
            let name = space.profile_name().as_bytes();
            assert!(p.windows(name.len()).any(|w| w == name), "{space:?}");
        }
        assert_eq!(for_space(OutputColorSpace::Srgb), srgb());
    }
}
