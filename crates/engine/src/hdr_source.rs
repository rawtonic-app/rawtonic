//! PQ and HLG sources (HDR AVIF and HEIF: Canon/Sony/Fujifilm HDR HIF,
//! HDR AVIF exports) in the SDR edit.
//!
//! Their decode keeps the HDR range (avif.rs `hdr_to_working_space`):
//! linear light with diffuse white (ITU-R BT.2408 HDR Reference White,
//! 203 cd/m²) at 1.0 and brighter highlights above it, up to the source's
//! peak. Everything below the roll-off's knee renders as the file grades
//! it; from renderVersion 20 (`ROLLOFF_RENDER_VERSION`) the output rolls the
//! highlights between the knee and the peak off smoothly into SDR white
//! instead of clipping everything above diffuse white (shader
//! `hdr_source_rolloff`). The headroom stays in the working data, so
//! Exposure, Highlights or Whites bring the highlights back down with their
//! detail, and a later HDR output (notes: parity-gaps/G78) can show them.
//!
//! The peak is the BT.2100 reference display's 1000 cd/m² for both
//! transfers: HLG is defined against it (the decode's OOTF), and it is what
//! cameras grade PQ stills to (ImageIO reports the same headroom, 4.93, for
//! a Canon HDR PQ HIF). Content light level metadata (`clli`, `mdcv`) is not
//! read (none of the measured files carries it); a PQ file brighter than
//! 1000 cd/m² clips above it.

use rawtonic_fs::FileStamp;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// The render version from which PQ/HLG highlights roll off into SDR white
/// (`renderVersion` 20, 2026-11-25).
pub const ROLLOFF_RENDER_VERSION: u64 = 20;

/// The peak of PQ and HLG sources relative to diffuse white: the BT.2100
/// reference display's 1000 cd/m² over BT.2408's 203 cd/m².
pub const HEADROOM: f32 = 1000.0 / crate::avif::HDR_REFERENCE_WHITE;

/// Where the roll-off starts, relative to diffuse white (the largest of
/// the linear ProPhoto channels): below it the output is the file's own
/// grade. 0.75 puts diffuse white at 0.88 of SDR white (0.945 encoded, 241
/// of 255) for any peak from 1.5 to 50 times diffuse white, leaving the top
/// codes for the highlights above it.
pub const KNEE: f32 = 0.75;

/// The roll-off of a channel maximum `m` (linear, 1.0 = diffuse white) for
/// a source peaking at `headroom`: `m` up to `KNEE`; above it a rational
/// shoulder of slope 1 at the knee that reaches 1.0 exactly at the peak.
/// The CPU twin of the shader's `hdr_source_rolloff` (tests).
pub fn rolloff(m: f32, headroom: f32) -> f32 {
    if m <= KNEE || headroom <= 1.0 {
        return m;
    }
    let a = (headroom - KNEE) / (1.0 - KNEE);
    let t = ((m - KNEE) / (headroom - KNEE)).min(1.0);
    KNEE + (1.0 - KNEE) * (a * t / (1.0 + (a - 1.0) * t))
}

/// Sets `global.hdr_headroom` for the photo at `path`: its headroom when
/// the recipe's render version rolls PQ/HLG highlights off
/// (`hdr_rolloff`, never for RAWs), else 0. Every render of a recipe calls
/// it (`baseline_exposure::apply_base_look`, the mask images of an export).
pub fn apply(global: &mut crate::image_processing::GlobalAdjustments, path: &str) {
    global.hdr_headroom = if global.hdr_rolloff != 0 {
        headroom(path).unwrap_or(0.0)
    } else {
        0.0
    };
}

/// The shader's `hdr_source_rolloff` on an sRGB-encoded decode (32-bit
/// float, as the PQ/HLG decode hands it over), for renders without the GPU:
/// a never-edited photo's plain Library tile. Other pixel types (never an
/// HDR decode) are left as they are.
pub fn rolloff_image(image: &mut image::DynamicImage, headroom: f32) {
    use rayon::prelude::*;
    const SRGB_TO_PROPHOTO: [[f32; 3]; 3] = [
        [0.529_345_8, 0.330_072_8, 0.140_581_2],
        [0.098_374_4, 0.873_461_1, 0.028_164_7],
        [0.016_883_2, 0.117_672_5, 0.865_444_3],
    ];
    // The decode's encoding (input_profile::encode) and the shader's
    // `srgb_to_linear`: negative values (colour beyond sRGB) take the
    // linear segment.
    let decode = |v: f32| {
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let encode = |v: f32| {
        if v <= 0.003_130_8 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    let pixel = |p: &mut [f32]| {
        let c = [decode(p[0]), decode(p[1]), decode(p[2])];
        let m = SRGB_TO_PROPHOTO
            .iter()
            .map(|r| r[0] * c[0] + r[1] * c[1] + r[2] * c[2])
            .fold(f32::MIN, f32::max);
        if m > KNEE {
            let scale = rolloff(m, headroom) / m;
            for (out, v) in p.iter_mut().zip(c) {
                *out = encode(v * scale);
            }
        }
    };
    match image {
        image::DynamicImage::ImageRgb32F(img) => img.par_chunks_mut(3).for_each(pixel),
        image::DynamicImage::ImageRgba32F(img) => img.par_chunks_mut(4).for_each(pixel),
        _ => {}
    }
}

/// Whether `transfer` (ITU-T H.273) is PQ (16) or HLG (18).
fn is_hdr_transfer(transfer: u16) -> bool {
    matches!(transfer, 16 | 18)
}

/// The headroom of a HEIF/AVIF file's primary image when its container
/// colour (`colr` `nclx`) names a PQ or HLG transfer; `None` otherwise.
pub fn headroom_of_bytes(bytes: &[u8]) -> Option<f32> {
    let nclx = crate::avif::primary_nclx(bytes)?;
    is_hdr_transfer(nclx.transfer).then_some(HEADROOM)
}

/// Whether `path` names a HEIF or AVIF file (by its extension only; never
/// reads it): the files that can be PQ/HLG sources.
pub fn may_be_hdr_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".avif", ".heic", ".heif", ".hif"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

type Cache = Mutex<HashMap<String, (FileStamp, Option<f32>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The container prefix read first: HEIF writers put the `meta` box (with
/// the colour properties) before the coded data.
const PREFIX: u64 = 1 << 20;

/// The headroom of the photo at `path` (`headroom_of_bytes`), read once per
/// file version; `None` for other files and files that cannot be read.
pub fn headroom(path: &str) -> Option<f32> {
    let path = rawtonic_metadata::photo_ref::source_of(path);
    if path.is_empty() || !may_be_hdr_file(path) {
        return None;
    }
    let stamp = FileStamp::read(Path::new(path)).ok()?;
    if let Some((cached, value)) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(path)
        && *cached == stamp
    {
        return *value;
    }
    let value = read_headroom(Path::new(path));
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_owned(), (stamp, value));
    value
}

fn read_headroom(path: &Path) -> Option<f32> {
    use std::io::Read;
    let mut prefix = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(PREFIX)
        .read_to_end(&mut prefix)
        .ok()?;
    if crate::avif::primary_nclx(&prefix).is_some() || (prefix.len() as u64) < PREFIX {
        return headroom_of_bytes(&prefix);
    }
    // The colour lies beyond the prefix (or the file has none): the whole file.
    headroom_of_bytes(&std::fs::read(path).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolloff_keeps_the_grade_below_the_knee_and_reaches_white_at_the_peak() {
        for m in [0.0, 0.128, 0.5, KNEE] {
            assert_eq!(rolloff(m, HEADROOM), m);
        }
        assert!((rolloff(HEADROOM, HEADROOM) - 1.0).abs() < 1e-6);
        assert_eq!(rolloff(2.0 * HEADROOM, HEADROOM), 1.0);
        // Diffuse white lands near SDR white for any realistic peak.
        for peak in [1.5, HEADROOM, 10_000.0 / 203.0] {
            let white = rolloff(1.0, peak);
            assert!((0.86..=0.91).contains(&white), "{peak}: {white}");
        }
        // Monotone, slope 1 at the knee, every highlight below white.
        let mut last = rolloff(KNEE, HEADROOM);
        for i in 1..=400 {
            let m = KNEE + (HEADROOM - KNEE) * i as f32 / 400.0;
            let v = rolloff(m, HEADROOM);
            assert!(v > last, "{m}");
            last = v;
        }
        let slope = (rolloff(KNEE + 1e-3, HEADROOM) - KNEE) / 1e-3;
        assert!((slope - 1.0).abs() < 0.01, "{slope}");
        assert!(rolloff(0.99 * HEADROOM, HEADROOM) < 1.0);
    }

    #[test]
    fn sdr_sources_are_not_hdr() {
        assert_eq!(headroom(""), None);
        assert_eq!(headroom("/nonexistent/photo.jpg"), None);
        assert_eq!(headroom_of_bytes(b"\x89PNG\r\n\x1a\n"), None);
    }

    /// A HEIC ramp in an ImageIO colour space (`encode_heic`), saved as
    /// `name` in `dir`.
    #[cfg(target_os = "macos")]
    fn heic_file(dir: &Path, name: &str, space: &str) -> String {
        let pixels: Vec<[u16; 3]> = (0..64 * 16).map(|i| [(i % 64 * 1000) as u16; 3]).collect();
        let bytes = crate::heif::imageio::encode_heic(64, 16, &pixels, space, 1, &[]);
        let path = dir.join(name);
        rawtonic_fs::atomic_write(&path, &bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pq_and_hlg_heif_files_have_the_reference_headroom() {
        let dir = tempfile::tempdir().unwrap();
        let pq = heic_file(dir.path(), "PQ.HIF", "kCGColorSpaceITUR_2100_PQ");
        let hlg = heic_file(dir.path(), "HLG.HEIC", "kCGColorSpaceITUR_2100_HLG");
        let sdr = heic_file(dir.path(), "SDR.HEIC", "kCGColorSpaceDisplayP3");
        assert_eq!(headroom(&pq), Some(HEADROOM));
        assert_eq!(headroom(&hlg), Some(HEADROOM));
        assert_eq!(headroom(&sdr), None);
        // A virtual copy's path is its source file's.
        assert_eq!(headroom(&format!("{pq}?vc=2")), Some(HEADROOM));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_recipes_of_render_version_20_roll_hdr_sources_off() {
        let dir = tempfile::tempdir().unwrap();
        let pq = heic_file(dir.path(), "PQ.HIF", "kCGColorSpaceITUR_2100_PQ");
        let sdr = heic_file(dir.path(), "SDR.HEIC", "kCGColorSpaceDisplayP3");
        let global = |path: &str, version: u64, is_raw: bool| {
            let recipe = serde_json::json!({"renderVersion": version, "exposure": 0.0});
            let mut global =
                crate::image_processing::get_all_adjustments_from_json(&recipe, is_raw, None)
                    .uniforms
                    .global;
            crate::baseline_exposure::apply_base_look(&mut global, path, None, None);
            (global.hdr_rolloff, global.hdr_headroom)
        };
        assert_eq!(global(&pq, 20, false), (1, HEADROOM));
        // Saved edits keep rendering as they were made.
        assert_eq!(global(&pq, 19, false), (0, 0.0));
        assert_eq!(global(&sdr, 20, false), (1, 0.0));
        assert_eq!(global(&pq, 20, true).1, 0.0);
    }

    #[test]
    fn rolloff_image_matches_the_curve_and_leaves_lower_tones() {
        let encode = |v: f32| 1.055 * v.powf(1.0 / 2.4) - 0.055;
        let values = [0.128f32, 0.5, 1.0, 2.0, HEADROOM];
        let mut image =
            image::DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(5, 1, |x, _| {
                image::Rgb([encode(values[x as usize]); 3])
            }));
        rolloff_image(&mut image, HEADROOM);
        let out = image.as_rgb32f().unwrap();
        for (x, &v) in values.iter().enumerate() {
            // A grey's largest ProPhoto channel is its value.
            let expected = encode(rolloff(v, HEADROOM));
            let got = out.get_pixel(x as u32, 0)[1];
            assert!(
                (got - expected).abs() < 2e-4,
                "{v}: {got} against {expected}"
            );
        }
    }

    #[test]
    fn rolloff_image_reads_colour_beyond_srgb_as_the_decode_wrote_it() {
        // BT.2020 cyan at diffuse white in linear sRGB: red below zero,
        // encoded as the decode does (negative values on the linear segment).
        let linear = [-0.29f32, 1.06, 1.02];
        let encode = |v: f32| {
            if v <= 0.003_130_8 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            }
        };
        let mut image = image::DynamicImage::ImageRgb32F(image::ImageBuffer::from_pixel(
            1,
            1,
            image::Rgb(linear.map(encode)),
        ));
        rolloff_image(&mut image, HEADROOM);
        let m = [
            [0.529_345_8f32, 0.330_072_8, 0.140_581_2],
            [0.098_374_4, 0.873_461_1, 0.028_164_7],
            [0.016_883_2, 0.117_672_5, 0.865_444_3],
        ]
        .map(|r| r[0] * linear[0] + r[1] * linear[1] + r[2] * linear[2])
        .into_iter()
        .fold(f32::MIN, f32::max);
        assert!(m > KNEE, "{m}");
        let scale = rolloff(m, HEADROOM) / m;
        let out = image.as_rgb32f().unwrap().get_pixel(0, 0).0;
        for (got, v) in out.iter().zip(linear) {
            let expected = encode(v * scale);
            assert!((got - expected).abs() < 2e-4, "{got} against {expected}");
        }
        // Rolled off below white, not clipped.
        assert!(out[1] < 1.0 && out[2] < 1.0, "{out:?}");
    }
}
