//! Embedded ICC profiles of non-RAW sources (JPEG, PNG, TIFF, WebP, AVIF;
//! AVIF's `nclx` colour code points through `profile_from_cicp`).
//!
//! The renderer reads non-RAW pixels as sRGB-encoded values. A source in
//! another space (Display P3, Adobe RGB, ProPhoto, a camera's own profile)
//! is converted to that encoding, as Camera Raw honours embedded profiles:
//! matrix/TRC profiles exactly and without clipping (colours outside sRGB
//! become values below 0 or above 1, encoded so that the shader's
//! `srgb_to_linear` returns the exact linear value), other profiles through
//! moxcms into sRGB. Sources without a profile, or with an sRGB one, are
//! left untouched.

use image::{DynamicImage, ImageBuffer, Rgb, Rgba};
use moxcms::{
    CicpColorPrimaries, CicpProfile, ColorProfile, DataColorSpace, Layout, MatrixCoefficients,
    TransferCharacteristics, TransformOptions,
};

/// Linear → sRGB encoding whose inverse is the shader's `srgb_to_linear`
/// also beyond 0…1 (negative values take the linear segment).
pub(crate) fn encode(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Whether `profile` describes sRGB (within a quarter of an 8-bit level).
fn is_srgb(profile: &ColorProfile, matrix: &[[f64; 3]; 3]) -> bool {
    let identity =
        (0..3).all(|r| (0..3).all(|c| (matrix[r][c] - f64::from(u8::from(r == c))).abs() < 2e-3));
    if !identity {
        return false;
    }
    [&profile.red_trc, &profile.green_trc, &profile.blue_trc]
        .iter()
        .all(|trc| {
            let Some(evaluator) = trc.as_ref().and_then(|t| t.make_linear_evaluator().ok()) else {
                return false;
            };
            (0..=32).all(|i| {
                let x = i as f32 / 32.0;
                (evaluator.evaluate_value(x) - srgb_to_linear(x)).abs() < 1e-3
            })
        })
}

/// The source pixels as sRGB-encoded values for an embedded `icc` profile;
/// unchanged when the profile is sRGB, unreadable or not RGB.
pub fn to_working_space(image: DynamicImage, icc: &[u8]) -> DynamicImage {
    let Ok(profile) = ColorProfile::new_from_slice(icc) else {
        log::warn!("Embedded ICC profile unreadable; the source is read as sRGB");
        return image;
    };
    profile_to_working_space(image, &profile)
}

/// The RGB profile of H.273 colour primaries and transfer characteristics
/// (an AVIF `nclx` property); `None` for code points moxcms has no
/// chromaticities or curve for.
pub fn profile_from_cicp(primaries: u16, transfer: u16) -> Option<ColorProfile> {
    let cicp = CicpProfile {
        color_primaries: CicpColorPrimaries::try_from(u8::try_from(primaries).ok()?).ok()?,
        transfer_characteristics: TransferCharacteristics::try_from(u8::try_from(transfer).ok()?)
            .ok()?,
        matrix_coefficients: MatrixCoefficients::Bt709,
        full_range: true,
    };
    let profile = ColorProfile::new_from_cicp(cicp);
    let complete = profile.cicp.is_some()
        && [&profile.red_trc, &profile.green_trc, &profile.blue_trc]
            .iter()
            .all(|trc| trc.is_some());
    complete.then_some(profile)
}

/// The linear-light matrix from H.273 colour primaries to the working
/// space's (sRGB, BT.709) primaries, as `profile_to_working_space` converts
/// matrix profiles; `None` for primaries moxcms has no chromaticities for.
pub fn primaries_to_working_space(primaries: u16) -> Option<[[f32; 3]; 3]> {
    // Transfer 8: linear.
    let profile = profile_from_cicp(primaries, 8)?;
    let matrix = profile.transform_matrix(&ColorProfile::new_srgb()).v;
    Some(matrix.map(|row| row.map(|v| v as f32)))
}

/// The source pixels as sRGB-encoded values for a source `profile`.
pub fn profile_to_working_space(image: DynamicImage, profile: &ColorProfile) -> DynamicImage {
    if profile.color_space != DataColorSpace::Rgb {
        return image;
    }
    let srgb = ColorProfile::new_srgb();
    if !profile.is_matrix_shaper() {
        return through_moxcms(image, profile, &srgb);
    }
    let matrix = profile.transform_matrix(&srgb).v;
    if is_srgb(profile, &matrix) {
        return image;
    }
    let evaluators: Vec<_> = [&profile.red_trc, &profile.green_trc, &profile.blue_trc]
        .iter()
        .filter_map(|trc| trc.as_ref().and_then(|t| t.make_linear_evaluator().ok()))
        .collect();
    if evaluators.len() != 3 {
        return through_moxcms(image, profile, &srgb);
    }
    // Per-channel linearisation tables for 8- and 16-bit sources.
    let steps = if matches!(
        image,
        DynamicImage::ImageRgb8(_) | DynamicImage::ImageRgba8(_)
    ) {
        255
    } else {
        65535
    };
    // A display curve maps 0…1 onto 0…1. moxcms evaluates a pure gamma
    // curve at exactly 0 to about -1.7e8 (ProPhoto's 1.8: black became
    // -2e9 after encoding), so the values are held to that range.
    let tables: Vec<Vec<f32>> = evaluators
        .iter()
        .map(|e| {
            (0..=steps)
                .map(|i| match e.evaluate_value(i as f32 / steps as f32) {
                    v if v.is_finite() => v.clamp(0.0, 1.0),
                    _ => 0.0,
                })
                .collect()
        })
        .collect();
    let m = matrix.map(|row| row.map(|v| v as f32));
    let convert = |rgb: [f32; 3]| -> [f32; 3] {
        let lin: [f32; 3] = std::array::from_fn(|c| {
            let x = rgb[c].clamp(0.0, 1.0) * steps as f32;
            let i = (x as usize).min(steps - 1);
            let t = x - i as f32;
            tables[c][i] + (tables[c][i + 1] - tables[c][i]) * t
        });
        std::array::from_fn(|r| encode(m[r][0] * lin[0] + m[r][1] * lin[1] + m[r][2] * lin[2]))
    };
    let (width, height) = (image.width(), image.height());
    if image.color().has_alpha() {
        let source = image.into_rgba32f();
        let mut out = ImageBuffer::<Rgba<f32>, Vec<f32>>::new(width, height);
        for (dst, src) in out.pixels_mut().zip(source.pixels()) {
            let [r, g, b] = convert([src[0], src[1], src[2]]);
            *dst = Rgba([r, g, b, src[3]]);
        }
        DynamicImage::ImageRgba32F(out)
    } else {
        let source = image.into_rgb32f();
        let mut out = ImageBuffer::<Rgb<f32>, Vec<f32>>::new(width, height);
        for (dst, src) in out.pixels_mut().zip(source.pixels()) {
            *dst = Rgb(convert([src[0], src[1], src[2]]));
        }
        DynamicImage::ImageRgb32F(out)
    }
}

/// LUT profiles: moxcms converts into sRGB (relative colorimetric, clipped).
fn through_moxcms(
    image: DynamicImage,
    profile: &ColorProfile,
    srgb: &ColorProfile,
) -> DynamicImage {
    let Ok(transform) =
        profile.create_transform_16bit(Layout::Rgb, srgb, Layout::Rgb, TransformOptions::default())
    else {
        log::warn!("Embedded ICC profile not convertible; the source is read as sRGB");
        return image;
    };
    let alpha = image.color().has_alpha().then(|| image.to_rgba16());
    let source = image.into_rgb16();
    let mut out = source.clone();
    if transform.transform(source.as_raw(), &mut out).is_err() {
        return DynamicImage::ImageRgb16(source);
    }
    match alpha {
        Some(alpha) => {
            DynamicImage::ImageRgba16(ImageBuffer::from_fn(out.width(), out.height(), |x, y| {
                let p = out.get_pixel(x, y);
                Rgba([p[0], p[1], p[2], alpha.get_pixel(x, y)[3]])
            }))
        }
        None => DynamicImage::ImageRgb16(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_color_space::OutputColorSpace;

    fn pixel_of(space: OutputColorSpace, rgb8: [u8; 3]) -> [f32; 3] {
        let image = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(2, 2, Rgb(rgb8)));
        let out = to_working_space(image, crate::icc_profile::for_space(space)).into_rgb32f();
        let p = out.get_pixel(0, 0);
        [p[0], p[1], p[2]]
    }

    #[test]
    fn srgb_sources_stay_untouched() {
        let image = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(2, 2, Rgb([200, 100, 50])));
        let out = to_working_space(image.clone(), crate::icc_profile::srgb());
        assert_eq!(out, image);
    }

    #[test]
    fn black_stays_black_in_every_space() {
        for space in [
            OutputColorSpace::DisplayP3,
            OutputColorSpace::AdobeRgb,
            OutputColorSpace::ProPhoto,
        ] {
            assert_eq!(pixel_of(space, [0, 0, 0]), [0.0; 3], "{space:?}");
            let image = DynamicImage::ImageRgb32F(ImageBuffer::from_pixel(1, 1, Rgb([0.0; 3])));
            let out = to_working_space(image, crate::icc_profile::for_space(space)).into_rgb32f();
            assert_eq!(out.get_pixel(0, 0).0, [0.0; 3], "{space:?} float");
        }
    }

    #[test]
    fn wide_sources_keep_colours_beyond_srgb() {
        // Display P3 green is outside sRGB: red and blue go negative.
        let green = pixel_of(OutputColorSpace::DisplayP3, [0, 255, 0]);
        let linear = green.map(|v| {
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        });
        assert!(
            linear[0] < -0.2 && linear[2] < -0.05 && (linear[1] - 1.042).abs() < 0.01,
            "{linear:?}"
        );
        // White stays white, grey stays grey, in every space.
        for space in [
            OutputColorSpace::DisplayP3,
            OutputColorSpace::AdobeRgb,
            OutputColorSpace::ProPhoto,
        ] {
            let white = pixel_of(space, [255, 255, 255]);
            assert!(
                white.iter().all(|v| (v - 1.0).abs() < 2e-3),
                "{space:?} {white:?}"
            );
            let grey = pixel_of(space, [128, 128, 128]);
            assert!(
                (grey[0] - grey[1]).abs() < 2e-3 && (grey[1] - grey[2]).abs() < 2e-3,
                "{space:?} {grey:?}"
            );
        }
        // An sRGB colour encoded in Adobe RGB comes back as itself.
        let back = pixel_of(OutputColorSpace::AdobeRgb, [0x9e, 0x6d, 0x40]);
        // The same colour in sRGB (primaries and 563/256 gamma, computed independently).
        let expected = [0.6847, 0.4292, 0.2301];
        for (b, e) in back.iter().zip(expected) {
            assert!((b - e).abs() < 0.01, "{back:?}");
        }
    }
}
