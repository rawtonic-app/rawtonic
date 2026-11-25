//! Capture One's Exposure on a non-RAW photo as what it renders (G50;
//! notes: benchmark-results/g50x-2026-11-25, c1r16-2026-11-25,
//! g50y-2026-11-25).
//!
//! On a JPEG, TIFF or PNG Capture One multiplies each channel's value, as
//! the file encodes it, by k = 2^(E/1.8) and clips at white:
//! `out = min(1, k · in)`, before the conversion to the output space —
//! measured exact for E = −2 … +2 on filled layers of sRGB, Adobe RGB and
//! ProPhoto files, and on the photo's own slider at −1 (C1R16). The 1.8 is
//! the tool's constant, not the file's gamma. Rawtonic's Exposure multiplies
//! linear light by 2^E and clips at white, so:
//!
//! - a file whose profile is one pure power curve, linear = value^γ (Adobe
//!   RGB, ProPhoto), gets Rawtonic's Exposure E·γ/1.8: k on the value is
//!   k^γ on linear light, the same law exactly, greys and colours alike
//!   (a ProPhoto file's amount stays E; Adobe RGB's +1 becomes +1.22);
//! - any other file (sRGB or untagged, whose sRGB curve bends into a
//!   straight toe; Display P3; other matrix curves) gets the law as a point
//!   curve on the RGB curve and its Exposure stays 0: for the values the
//!   renderer reads as they are (sRGB, untagged) a straight line to the clip
//!   point, exact (curves version 2, display values); for other matrix
//!   profiles `L' = T(min(1, k · T⁻¹(L)))` sampled on the sRGB encoding at
//!   16 points (a layer renders its first 16) in the wide working space
//!   (curves version 3), within 0.25 of an 8-bit level of the law on greys.
//!
//! Where a channel clips, Capture One clips in the file's primaries,
//! Rawtonic in its output's (or for a curve in ProPhoto's): the same for
//! greys. Above 0 Capture One's own (global) Exposure rolls the highlights
//! off from about half the range instead of clipping (C1R16 point 6): the
//! import uses the law with its clip there too (no shoulder fitted), and the
//! import report says so.
use crate::input_profile::{SOURCE_TONE_SAMPLES, SourceTone};
use rawtonic_recipe::masks;
use rawtonic_recipe::tone;
use serde_json::{Map, Value, json};
use std::path::Path;

/// Capture One's Exposure gain per EV on encoded values: 2^(E/1.8).
pub const ENCODED_EV_PER_STOP: f64 = 1.0 / 1.8;
/// Points of a curve for a file in another profile than sRGB.
const PROFILE_CURVE_POINTS: usize = 16;

/// How the renderer reads the non-RAW file at `path` (`SourceTone`); `None`
/// where the import cannot tell (unreadable, AVIF/HEIF, linear-light
/// sources), and the Exposure then stays Rawtonic's.
pub fn source_tone_of(path: &Path) -> Option<SourceTone> {
    use std::io::Read;
    let mut head = Vec::with_capacity(4096);
    std::fs::File::open(path)
        .ok()?
        .take(4096)
        .read_to_end(&mut head)
        .ok()?;
    if crate::avif::is_avif(&head) || crate::heif::is_heif(&head) {
        return None;
    }
    let reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    if matches!(
        reader.format(),
        None | Some(image::ImageFormat::OpenExr | image::ImageFormat::Hdr)
    ) {
        return None;
    }
    let mut decoder = reader.into_decoder().ok()?;
    match image::ImageDecoder::icc_profile(&mut decoder)
        .ok()
        .flatten()
    {
        Some(icc) => Some(crate::input_profile::source_tone(&icc)),
        None => Some(SourceTone::AsRead),
    }
}

fn srgb_decode(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_encode(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// The file's value → linear light, interpolated in the sampled curve.
fn to_linear(table: &[f32], x: f64) -> f64 {
    let last = (table.len() - 1) as f64;
    let at = (x.clamp(0.0, 1.0) * last).min(last);
    let i = (at as usize).min(table.len() - 2);
    let t = at - i as f64;
    f64::from(table[i]) + (f64::from(table[i + 1]) - f64::from(table[i])) * t
}

/// Linear light → the file's value (the sampled curve is monotone).
fn to_file(table: &[f32], linear: f64) -> f64 {
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..48 {
        let mid = 0.5 * (lo + hi);
        if to_linear(table, mid) < linear {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

fn point(x: f64, y: f64) -> Value {
    let unit = |v: f64| (v.clamp(0.0, 1.0) * 255.0 * 10_000.0).round() / 10_000.0;
    json!({"x": unit(x), "y": unit(y)})
}

/// The RGB curve (0…1 in, 0…1 out) that renders Capture One's Exposure
/// `exposure` on a file read as `tone`, with the curves version it needs;
/// `None` for `SourceTone::Other` and `SourceTone::Power` (that one is
/// Rawtonic's Exposure, `rawtonic_exposure`).
pub fn exposure_curve(exposure: f64, tone: &SourceTone) -> Option<(Vec<Value>, u64)> {
    let gain = (exposure * ENCODED_EV_PER_STOP).exp2();
    match tone {
        SourceTone::AsRead => {
            let end = if gain > 1.0 {
                point(1.0 / gain, 1.0)
            } else {
                point(1.0, gain)
            };
            Some((
                vec![point(0.0, 0.0), end],
                crate::point_curve::LONG_CURVE_VERSION,
            ))
        }
        SourceTone::Matrix(table) if table.len() == SOURCE_TONE_SAMPLES => {
            let law = |s: f64| {
                let file = to_file(table, srgb_decode(s));
                srgb_encode(to_linear(table, (gain * file).min(1.0)))
            };
            // Up to the clip point, denser in the shadows (where the sRGB
            // encoding and the file's curve bend most), flat beyond it.
            let end = if gain > 1.0 {
                srgb_encode(to_linear(table, 1.0 / gain))
            } else {
                1.0
            };
            let last = (PROFILE_CURVE_POINTS - 1) as f64;
            let points = (0..PROFILE_CURVE_POINTS)
                .map(|i| {
                    let s = end * (i as f64 / last).powi(2);
                    let y = if i == PROFILE_CURVE_POINTS - 1 && gain > 1.0 {
                        1.0
                    } else {
                        law(s)
                    };
                    point(s, y)
                })
                .collect();
            Some((points, crate::point_curve::WIDE_CURVE_VERSION))
        }
        _ => None,
    }
}

/// Rawtonic's Exposure that renders Capture One's `exposure` on a file whose
/// values are linear light to the power `gamma`: E·γ/1.8.
pub fn rawtonic_exposure(exposure: f64, gamma: f64) -> f64 {
    exposure * gamma * ENCODED_EV_PER_STOP
}

/// Whether `adjustments` hold a point curve on the RGB or a colour channel
/// that changes something: the Exposure curve would share the RGB curve and
/// set the version those render with.
fn has_point_curves(adjustments: &Map<String, Value>) -> bool {
    let Some(curves) = adjustments.get(masks::CURVES.name()) else {
        return false;
    };
    masks::CURVE_CHANNELS[..4].iter().any(|channel| {
        let points = crate::point_curve::curve_points(
            curves[channel.name()].as_array().map_or(&[], Vec::as_slice),
        );
        points.len() >= 2 && !crate::point_curve::is_identity(&points)
    })
}

/// Why Capture One's Exposure on a non-RAW photo that needs the curve stays
/// Rawtonic's Exposure: the file's tone is unknown or not one curve, or the
/// layer (or photo) has Capture One's own RGB or colour curves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    UnknownTone,
    Curves,
}

/// Replaces Capture One's Exposure in `adjustments` (the photo's or a
/// layer's, as imported) by what renders it on a file read as `tone`: a
/// scaled Exposure or the RGB curve. `Err` says why it stays the same amount
/// of Rawtonic's Exposure; `Ok(false)`: no Exposure to replace.
pub fn express(
    adjustments: &mut Map<String, Value>,
    tone: Option<&SourceTone>,
) -> Result<bool, Kept> {
    let exposure = adjustments
        .get(tone::EXPOSURE.name())
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if exposure == 0.0 || !exposure.is_finite() {
        return Ok(false);
    }
    if let Some(SourceTone::Power(gamma)) = tone {
        adjustments.insert(
            tone::EXPOSURE.name().into(),
            json!(rawtonic_exposure(exposure, *gamma)),
        );
        return Ok(true);
    }
    if has_point_curves(adjustments) {
        return Err(Kept::Curves);
    }
    let Some((points, version)) = tone.and_then(|tone| exposure_curve(exposure, tone)) else {
        return Err(Kept::UnknownTone);
    };
    let identity = json!([{"x": 0, "y": 0}, {"x": 255, "y": 255}]);
    let curves = adjustments
        .entry(masks::CURVES.name())
        .or_insert_with(|| json!({}));
    if !curves.is_object() {
        *curves = json!({});
    }
    for channel in &masks::CURVE_CHANNELS[1..4] {
        if curves.get(channel.name()).is_none_or(Value::is_null) {
            curves[channel.name()] = identity.clone();
        }
    }
    curves[masks::CURVE_CHANNELS[0].name()] = Value::Array(points);
    curves[masks::CURVE_VERSION.name()] = json!(version);
    adjustments.insert(tone::EXPOSURE.name().into(), json!(0.0));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_color_space::OutputColorSpace;

    fn curve_of(points: &[Value]) -> Vec<(f32, f32)> {
        crate::point_curve::curve_points(points)
    }

    fn tone_of(space: OutputColorSpace) -> SourceTone {
        crate::input_profile::source_tone(crate::icc_profile::for_space(space))
    }

    /// Capture One's law on a neutral ramp of the file's values (C1R16).
    fn law(file: f64, exposure: f64) -> f64 {
        ((exposure / 1.8).exp2() * file).min(1.0)
    }

    const AMOUNTS: [f64; 6] = [-2.0, -1.0, -0.5, 0.5, 1.0, 2.0];

    #[test]
    fn srgb_files_get_the_exact_line_to_the_clip_point() {
        for exposure in AMOUNTS {
            let (points, version) = exposure_curve(exposure, &SourceTone::AsRead).unwrap();
            assert_eq!(version, 2);
            let curve = curve_of(&points);
            for step in 0..=255 {
                let x = step as f64 / 255.0;
                let got = f64::from(crate::point_curve::curve_value(&curve, x as f32));
                let want = law(x, exposure);
                assert!(
                    (got - want).abs() * 255.0 < 0.002,
                    "E {exposure} step {step}: {got} {want}"
                );
            }
        }
        // C1R16's sRGB ramp: the clip steps are 255/k (211, 174, 119).
        let (points, _) = exposure_curve(1.0, &SourceTone::AsRead).unwrap();
        assert!((points[1]["x"].as_f64().unwrap() - 173.5).abs() < 0.01);
    }

    /// Adobe RGB and ProPhoto files: their profiles are pure power curves,
    /// and Capture One's gain on their values is Rawtonic's Exposure E·γ/1.8.
    #[test]
    fn power_profiles_get_rawtonics_exposure_scaled() {
        for (space, gamma) in [
            (OutputColorSpace::AdobeRgb, 563.0 / 256.0),
            (OutputColorSpace::ProPhoto, 461.0 / 256.0),
        ] {
            let SourceTone::Power(found) = tone_of(space) else {
                panic!("{space:?}");
            };
            assert!((found - gamma).abs() < 1e-3, "{space:?}: {found}");
            for exposure in AMOUNTS {
                let mut layer = json!({"exposure": exposure});
                let tone = SourceTone::Power(gamma);
                assert_eq!(
                    express(layer.as_object_mut().unwrap(), Some(&tone)),
                    Ok(true)
                );
                let ours = layer["exposure"].as_f64().unwrap();
                assert!(layer.get("curves").is_none());
                for step in 1..=255 {
                    let file = step as f64 / 255.0;
                    let linear = (file.powf(gamma) * ours.exp2()).min(1.0);
                    let want = law(file, exposure).powf(gamma);
                    assert!(
                        (linear - want).abs() < 1e-9,
                        "{space:?} E {exposure} step {step}"
                    );
                }
            }
        }
    }

    /// Other matrix curves (Display P3 has sRGB's): the law on the file's
    /// own values, through the file's tone curve, on the sRGB-encoded grey
    /// the renderer reads.
    #[test]
    fn other_profiles_follow_the_law_on_their_own_values() {
        let tone = tone_of(OutputColorSpace::DisplayP3);
        let SourceTone::Matrix(table) = &tone else {
            panic!("{tone:?}");
        };
        for exposure in AMOUNTS {
            let (points, version) = exposure_curve(exposure, &tone).unwrap();
            assert_eq!(version, 3);
            assert!(points.len() <= 16);
            let curve = curve_of(&points);
            let mut worst = 0.0_f64;
            for step in 0..=255 {
                let file = step as f64 / 255.0;
                let read = srgb_encode(to_linear(table, file));
                let got = f64::from(crate::point_curve::curve_value(&curve, read as f32));
                let want = srgb_encode(to_linear(table, law(file, exposure)));
                worst = worst.max((got - want).abs() * 255.0);
            }
            assert!(worst < 0.25, "E {exposure}: {worst}");
        }
        // A curve made from a power profile's table follows it as closely.
        let power: Vec<f32> = (0..SOURCE_TONE_SAMPLES)
            .map(|i| (i as f64 / (SOURCE_TONE_SAMPLES - 1) as f64).powf(2.2) as f32)
            .collect();
        for exposure in AMOUNTS {
            let (points, _) = exposure_curve(exposure, &SourceTone::Matrix(power.clone())).unwrap();
            let curve = curve_of(&points);
            for step in 0..=255 {
                let file = step as f64 / 255.0;
                let got =
                    crate::point_curve::curve_value(&curve, srgb_encode(file.powf(2.2)) as f32);
                let want = srgb_encode(law(file, exposure).powf(2.2));
                assert!(
                    (f64::from(got) - want).abs() * 255.0 < 0.25,
                    "E {exposure} step {step}"
                );
            }
        }
    }

    #[test]
    fn the_exposure_becomes_the_rgb_curve() {
        let mut layer = json!({"exposure": 1.0, "contrast": 20.0});
        let map = layer.as_object_mut().unwrap();
        assert_eq!(express(map, Some(&SourceTone::AsRead)), Ok(true));
        assert_eq!(layer["exposure"], 0.0);
        assert_eq!(layer["contrast"], 20.0);
        assert_eq!(layer["curves"]["version"], 2);
        assert_eq!(layer["curves"]["luma"].as_array().unwrap().len(), 2);
        assert_eq!(
            layer["curves"]["red"],
            json!([{"x": 0, "y": 0}, {"x": 255, "y": 255}])
        );
    }

    #[test]
    fn exposure_stays_where_the_curve_cannot_say_it() {
        let mut none = json!({"exposure": 0.0});
        assert_eq!(express(none.as_object_mut().unwrap(), None), Ok(false));
        let mut unknown = json!({"exposure": 1.0});
        assert_eq!(
            express(unknown.as_object_mut().unwrap(), None),
            Err(Kept::UnknownTone)
        );
        assert_eq!(unknown["exposure"], 1.0);
        let mut lut = json!({"exposure": 1.0});
        assert_eq!(
            express(lut.as_object_mut().unwrap(), Some(&SourceTone::Other)),
            Err(Kept::UnknownTone)
        );
        let mut curved = json!({"exposure": -1.0, "curves": {"luma": [{"x": 0, "y": 0}, {"x": 128, "y": 150}, {"x": 255, "y": 255}]}});
        assert_eq!(
            express(curved.as_object_mut().unwrap(), Some(&SourceTone::AsRead)),
            Err(Kept::Curves)
        );
        assert_eq!(curved["exposure"], -1.0);
        let mut red =
            json!({"exposure": 1.0, "curves": {"red": [{"x": 0, "y": 10}, {"x": 255, "y": 255}]}});
        assert_eq!(
            express(red.as_object_mut().unwrap(), Some(&SourceTone::AsRead)),
            Err(Kept::Curves)
        );
        // A power profile needs no curve: Capture One's curves stay beside
        // the scaled Exposure.
        let mut beside = json!({"exposure": 1.0, "curves": {"luma": [{"x": 0, "y": 0}, {"x": 128, "y": 150}, {"x": 255, "y": 255}]}});
        assert_eq!(
            express(
                beside.as_object_mut().unwrap(),
                Some(&SourceTone::Power(1.8))
            ),
            Ok(true)
        );
        assert!((beside["exposure"].as_f64().unwrap() - 1.0).abs() < 1e-12);
        assert_eq!(beside["curves"]["luma"][1]["y"], 150);
        // Capture One's Luma curve (the Luminance curve) is independent of
        // the version and stays beside it.
        let mut luminance = json!({"exposure": 1.0, "curves": {"luminance": [{"x": 0, "y": 10}, {"x": 255, "y": 255}]}});
        assert_eq!(
            express(
                luminance.as_object_mut().unwrap(),
                Some(&SourceTone::AsRead)
            ),
            Ok(true)
        );
        assert_eq!(luminance["curves"]["luminance"][0]["y"], 10);
    }
}
