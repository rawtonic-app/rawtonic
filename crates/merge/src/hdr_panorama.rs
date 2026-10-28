//! HDR panorama: bracketed exposures at each camera position are merged to
//! HDR first, then the HDR frames are stitched.
//!
//! The selection is split into brackets by exposure: a bracket holds one
//! frame of every exposure, in shooting order (the order the photos are
//! given in). Each merged position is lens-corrected and parked on disk as
//! 16-bit floats until the stitch reads it (twice: small, then full).
use crate::frame::{Capture, Frame, read_capture, rebalance};
use crate::hdr::{HdrConfig, merge_hdr};
use crate::panorama::{PanoramaConfig, PanoramaResult, stitch_frames};
use anyhow::{Context, Result, anyhow, ensure};
use half::f16;
use image::Rgb32FImage;
use rawler::decoders::RawMetadata;
use rawtonic_engine::image_loader::BaseImageOptions;
use rawtonic_rawdecode::raw_processing::ImageColor;
use rayon::prelude::*;
use std::io::{Read, Write};
use std::path::PathBuf;

/// Splits `captures` (in shooting order) into brackets: consecutive groups
/// of the same size whose exposures are distinct and form the same pattern
/// relative to the group's darkest frame (within 1/6 EV). The base exposure
/// may shift from position to position (auto exposure, e.g. 1/167 1/118
/// 1/236 then 1/236 1/167 1/334 s); the smallest such size is taken.
pub fn group_brackets(captures: &[Capture]) -> Result<Vec<Vec<usize>>> {
    const TOLERANCE_EV: f64 = 1.0 / 6.0;
    let evs: Vec<Option<f64>> = captures.iter().map(Capture::exposure_ev).collect();
    ensure!(
        evs.iter().all(Option::is_some),
        "An HDR panorama needs the exposure time of every photo."
    );
    let evs: Vec<f64> = evs.into_iter().flatten().collect();
    let n = evs.len();
    let pattern = |group: &[f64]| -> Option<Vec<f64>> {
        let mut sorted = group.to_vec();
        sorted.sort_by(f64::total_cmp);
        let offsets: Vec<f64> = sorted.iter().map(|e| e - sorted[0]).collect();
        offsets
            .windows(2)
            .all(|w| w[1] - w[0] > TOLERANCE_EV)
            .then_some(offsets)
    };
    let same = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(x, y)| (x - y).abs() <= TOLERANCE_EV);
    for k in 2..=n / 2 {
        if !n.is_multiple_of(k) {
            continue;
        }
        let Some(first) = pattern(&evs[..k]) else {
            continue;
        };
        if evs
            .chunks(k)
            .all(|g| pattern(g).is_some_and(|p| same(&p, &first)))
        {
            return Ok((0..n / k).map(|g| (g * k..(g + 1) * k).collect()).collect());
        }
    }
    let (lo, hi) = evs
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &e| {
            (lo.min(e), hi.max(e))
        });
    ensure!(
        hi - lo > TOLERANCE_EV,
        "The photos have one exposure only; merge them as a panorama."
    );
    Err(anyhow!(
        "{n} photos do not split into brackets with the same exposure steps at two or more positions (each position needs one photo of every step, in shooting order)."
    ))
}

struct Parked {
    file: tempfile::NamedTempFile,
    width: u32,
    height: u32,
    color: Option<ImageColor>,
    metadata: Option<RawMetadata>,
    path: PathBuf,
}

fn park(
    image: &Rgb32FImage,
    color: Option<ImageColor>,
    metadata: Option<RawMetadata>,
    path: PathBuf,
) -> Result<Parked> {
    let mut file = tempfile::NamedTempFile::new().context("temporary file for an HDR position")?;
    let raw: &[f32] = image;
    let bytes: Vec<u8> = raw
        .par_iter()
        .flat_map_iter(|v| f16::from_f32(*v).to_le_bytes())
        .collect();
    file.write_all(&bytes)?;
    file.flush()?;
    Ok(Parked {
        file,
        width: image.width(),
        height: image.height(),
        color,
        metadata,
        path,
    })
}

fn unpark(p: &Parked) -> Result<Rgb32FImage> {
    let mut bytes = Vec::new();
    std::fs::File::open(p.file.path())?.read_to_end(&mut bytes)?;
    let data: Vec<f32> = bytes
        .par_chunks_exact(2)
        .map(|b| f16::from_le_bytes([b[0], b[1]]).to_f32())
        .collect();
    Rgb32FImage::from_raw(p.width, p.height, data).ok_or_else(|| anyhow!("HDR position size"))
}

/// Merges each bracket to HDR, then stitches the positions.
pub fn hdr_panorama(
    paths: &[PathBuf],
    options: &BaseImageOptions,
    hdr: &HdrConfig,
    panorama: &PanoramaConfig,
    progress: &(dyn Fn(&str) + Sync),
) -> Result<PanoramaResult> {
    let captures: Vec<Capture> = paths.iter().map(|p| read_capture(p)).collect();
    let groups = group_brackets(&captures)?;
    let mut parked: Vec<Parked> = Vec::with_capacity(groups.len());
    for (g, group) in groups.iter().enumerate() {
        progress(&format!("HDR position {} of {}", g + 1, groups.len()));
        let group_paths: Vec<PathBuf> = group.iter().map(|&i| paths[i].clone()).collect();
        let merged = merge_hdr(&group_paths, options, hdr, progress)?;
        // The merge keeps the reference frame's geometry: correct its lens.
        let adjustments = crate::initial_adjustments(&merged.reference_path, false);
        let corrected = rawtonic_engine::image_processing::apply_geometry_warp(
            image::DynamicImage::ImageRgb32F(merged.image),
            &adjustments,
            // A merge is linear light.
            true,
        )
        .map_err(|e| anyhow!(e))?
        .into_owned()
        .into_rgb32f();
        parked.push(park(
            &corrected,
            merged.color,
            merged.metadata,
            merged.reference_path,
        )?);
    }
    let position_paths: Vec<PathBuf> = parked.iter().map(|p| p.path.clone()).collect();
    let load = |i: usize, reference: Option<&ImageColor>| -> Result<Frame> {
        let p = &parked[i];
        let mut image = unpark(p)?;
        if let Some(m) = rebalance(p.color.as_ref(), reference) {
            let raw: &mut [f32] = &mut image;
            raw.par_chunks_mut(3).for_each(|px| {
                let v = m * nalgebra::Vector3::new(px[0], px[1], px[2]);
                px.copy_from_slice(v.as_slice());
            });
        }
        Ok(Frame {
            path: p.path.clone(),
            image,
            color: p.color.clone(),
            metadata: p.metadata.clone(),
            capture: Capture::default(),
        })
    };
    stitch_frames(&position_paths, &load, panorama, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(t: f64) -> Capture {
        Capture {
            exposure_time: Some(t),
            f_number: Some(8.0),
            iso: Some(100.0),
        }
    }

    #[test]
    fn brackets_split_by_exposure_count() {
        let c: Vec<Capture> = [1.0 / 250.0, 1.0 / 60.0, 1.0 / 1000.0]
            .iter()
            .cycle()
            .take(9)
            .map(|&t| capture(t))
            .collect();
        assert_eq!(
            group_brackets(&c).unwrap(),
            vec![vec![0, 1, 2], vec![3, 4, 5], vec![6, 7, 8]]
        );
    }

    #[test]
    fn incomplete_or_single_exposure_sets_are_refused() {
        let same: Vec<Capture> = (0..6).map(|_| capture(1.0 / 125.0)).collect();
        assert!(group_brackets(&same).is_err());
        let mut odd: Vec<Capture> = [1.0 / 250.0, 1.0 / 60.0]
            .iter()
            .cycle()
            .take(6)
            .map(|&t| capture(t))
            .collect();
        odd[3] = capture(1.0 / 250.0);
        assert!(group_brackets(&odd).is_err());
        let short: Vec<Capture> = [1.0 / 250.0, 1.0 / 60.0, 1.0 / 1000.0]
            .iter()
            .map(|&t| capture(t))
            .collect();
        assert!(group_brackets(&short).is_err());
    }

    #[test]
    fn brackets_may_shift_their_base_exposure() {
        // Auto exposure per position, ±1/2 EV brackets (a real PowerShot set).
        let times = [
            167.0, 118.0, 236.0, 236.0, 167.0, 334.0, 334.0, 236.0, 473.0,
        ];
        let c: Vec<Capture> = times.iter().map(|&t| capture(1.0 / t)).collect();
        assert_eq!(
            group_brackets(&c).unwrap(),
            vec![vec![0, 1, 2], vec![3, 4, 5], vec![6, 7, 8]]
        );
    }
}
