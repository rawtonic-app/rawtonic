//! Per-camera (unit) white-balance calibration, as Adobe applies it.
//!
//! Sensors of one model differ slightly in their red and blue response.
//! Canon, Sony, Fujifilm and Olympus bodies store white-balance references
//! measured on the individual camera, and Adobe DNG Converter turns them
//! into a CameraCalibration (DNG tag 50723/50724): `diag(nominal /
//! reference)` for red and blue, with the model's nominal reference from
//! an internal table. Camera Raw's Kelvin numbers (the As Shot readout,
//! typed temperatures, presets) go through that calibration, so without it
//! the same number means a different balance on each body.
//!
//! Which reference Adobe reads was found by patching single maker-note
//! values of copies and converting them (notes: reference/white-balance.md):
//!
//! - Sony: the Daylight preset of the SR2 directory (0x7820, R G B).
//! - Canon: one preset of the ColorData WB table, at a position per
//!   ColorData version (mostly Daylight).
//! - Fujifilm: the D65 entry of WB_GRBLevelsStandard (0xf00c).
//! - Olympus / OM System: one or two Kelvin presets of the ImageProcessing
//!   directory, chosen per model (the TG-4 uses 3000 K for standard light A
//!   and 6600 K for D65, the E-M1 II 5300 K for both).
//!
//! The per-model nominal values in `unit_calibration_table.rs` are the
//! converter's CameraCalibration times the sample file's reference, one
//! sample per model. Nikon, Panasonic, Pentax, Leica and Hasselblad files get
//! no per-unit calibration from Adobe.

use crate::color_calibration::CameraCalibration;
use crate::unit_calibration_table::{NOMINAL, Source};

/// Red/green and blue/green ratios of a white-balance reference.
type Ratios = [f64; 2];

/// The per-unit calibration of a raw file, when its model is in the table
/// and the file carries the reference Adobe reads.
pub fn camera_calibration(path: &str, make: &str, model: &str) -> Option<CameraCalibration> {
    let entry = NOMINAL
        .iter()
        .find(|e| e.make.eq_ignore_ascii_case(make) && e.model.eq_ignore_ascii_case(model))?;
    let low = reference(path, entry.source, entry.low_key)?;
    let high = if entry.high_key == entry.low_key {
        low
    } else {
        reference(path, entry.source, entry.high_key)?
    };
    let gains = |nominal: [f64; 2], unit: Ratios| -> Option<[f64; 3]> {
        let g = [nominal[0] / unit[0], 1.0, nominal[1] / unit[1]];
        // Adobe drops the calibration when a gain falls below 0.8 (patched
        // references: 0.801 kept, 0.795 dropped); 1.25 assumed above.
        g.iter()
            .all(|v| v.is_finite() && (0.8..=1.25).contains(v))
            .then_some(g)
    };
    Some([gains(entry.low, low)?, gains(entry.high, high)?])
}

/// The reference `key` of the file for `source` (see the module docs).
pub fn reference(path: &str, source: Source, key: u16) -> Option<Ratios> {
    match source {
        Source::SonyDaylight => sony_daylight(path),
        Source::CanonDaylight => canon_reference(path),
        Source::FujifilmStandard => {
            let bytes = std::fs::read(path).ok()?;
            crate::capture_info::fujifilm_standard_white_balance(&bytes)
                .into_iter()
                .find(|(light, _)| *light == key as u32)
                .and_then(|(_, [g, r, b])| ratios(r as f64, g as f64, b as f64))
        }
        Source::OlympusPreset => crate::capture_info::olympus_white_balance_presets(path)
            .into_iter()
            .find(|(tag, _)| *tag == key)
            .and_then(|(_, [r, b, g1, g2])| {
                ratios(r as f64, (g1 as f64 + g2 as f64) / 2.0, b as f64)
            }),
    }
}

fn ratios(r: f64, g: f64, b: f64) -> Option<Ratios> {
    (r > 0.0 && g > 0.0 && b > 0.0).then(|| [r / g, b / g])
}

fn sony_daylight(path: &str) -> Option<Ratios> {
    let bytes = std::fs::read(path).ok()?;
    let source = rawler::rawsource::RawSource::new_from_slice(&bytes);
    let sr2 = rawler::decoders::arw::ArwDecoder::sr2_subifd(&source).ok()?;
    let value = &sr2.get_entry(0x7820u16)?.value;
    if value.count() != 3 {
        return None;
    }
    ratios(
        value.force_f32(0) as f64,
        value.force_f32(1) as f64,
        value.force_f32(2) as f64,
    )
}

fn canon_reference(path: &str) -> Option<Ratios> {
    let data = crate::capture_info::canon_color_data(path)?;
    let at = canon_reference_offset(&data)?;
    let (r, g1, g2, b) = (
        *data.get(at)?,
        *data.get(at + 1)?,
        *data.get(at + 2)?,
        *data.get(at + 3)?,
    );
    ratios(r as f64, (g1 as f64 + g2 as f64) / 2.0, b as f64)
}

/// Offset in a Canon ColorData array of the WB entry (R, G, G, B) Adobe
/// calibrates with, by ColorData version: a Daylight preset (5200 K, 5829 K
/// on the PowerShots of version −4), on versions 12 and 15 the entry after
/// it. Found by patching each entry of one file per version (and length,
/// for the bodies without a version word); others have none.
pub fn canon_reference_offset(data: &[u16]) -> Option<usize> {
    Some(match (*data.first()? as i16, data.len()) {
        // 20D/350D and 1D Mark II/1Ds Mark II: no version word.
        (_, 582) => 0x23,
        (_, 653) => 0x27,
        (_, 796) => 0x4e,
        (-4, _) => 0x67,
        (-3, _) => 0x56,
        (1, _) => 0x4e,
        (2..=7 | 9, _) => 0x53,
        (10, 1273 | 1275) => 0x67,
        (10 | 11, _) => 0x80,
        (12 | 15, _) => 0x8a,
        (13 | 14, _) => 0x85,
        (16..=19, _) => 0x88,
        (32 | 33, _) => 0x96,
        (34 | 48, _) => 0xfa,
        (64..=66, _) => 0x6e,
        _ => return None,
    })
    .filter(|at| at + 4 <= data.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon_reference_follows_the_colordata_version() {
        let mut data = vec![0u16; 0x200];
        data[0] = 15;
        assert_eq!(canon_reference_offset(&data), Some(0x8a));
        data[0] = 33;
        assert_eq!(canon_reference_offset(&data), Some(0x96));
        data[0] = (-4i16) as u16;
        assert_eq!(canon_reference_offset(&data), Some(0x67));
        data[0] = 5;
        assert_eq!(canon_reference_offset(&data), Some(0x53));
        data[0] = 8;
        assert_eq!(canon_reference_offset(&data), None);
        assert_eq!(canon_reference_offset(&data[..0x60]), None);
    }

    /// Tool: the references of files (`$UNIT_FILES`, `;`-separated) for
    /// the table generator: Sony daylight, Canon shade, Fujifilm standard
    /// entries, Olympus presets.
    #[test]
    #[ignore = "tool: needs UNIT_FILES"]
    fn print_unit_references() {
        for path in std::env::var("UNIT_FILES").unwrap().split(';') {
            let Some((make, model, _, _)) = crate::capture_info::read_camera(path) else {
                println!("{path}\tunreadable");
                continue;
            };
            let mut refs = Vec::new();
            if let Some(r) = sony_daylight(path) {
                refs.push(format!("sony:0x7820={},{}", r[0], r[1]));
            }
            if let Some(r) = canon_reference(path) {
                refs.push(format!("canon:daylight={},{}", r[0], r[1]));
            }
            if let Some(data) = crate::capture_info::canon_color_data(path) {
                refs.push(format!("canon:version={},{}", data[0] as i16, data.len()));
            }
            if let Ok(bytes) = std::fs::read(path) {
                for (light, [g, r, b]) in
                    crate::capture_info::fujifilm_standard_white_balance(&bytes)
                {
                    refs.push(format!(
                        "fuji:{light}={},{}",
                        r as f64 / g as f64,
                        b as f64 / g as f64
                    ));
                }
            }
            for (tag, [r, b, g1, g2]) in crate::capture_info::olympus_white_balance_presets(path) {
                let g = (g1 as f64 + g2 as f64) / 2.0;
                refs.push(format!("oly:{tag:#06x}={},{}", r as f64 / g, b as f64 / g));
            }
            println!("{path}\t{make}\t{model}\t{}", refs.join(" "));
        }
    }

    /// Tool: Canon ColorData arrays of `$UNIT_FILES` as JSON lines.
    #[test]
    #[ignore = "tool: needs UNIT_FILES"]
    fn print_canon_color_data() {
        for path in std::env::var("UNIT_FILES").unwrap().split(';') {
            if let Some(data) = crate::capture_info::canon_color_data(path) {
                println!("{{\"path\": {path:?}, \"words\": {data:?}}}");
            }
        }
    }

    /// Tool: the calibration predicted for `$UNIT_FILES`.
    #[test]
    #[ignore = "tool: needs UNIT_FILES"]
    fn print_unit_calibration() {
        for path in std::env::var("UNIT_FILES").unwrap().split(';') {
            let found = crate::capture_info::read_camera(path)
                .and_then(|(make, model, _, _)| camera_calibration(path, &make, &model));
            println!("{path}\t{found:?}");
        }
    }
}
