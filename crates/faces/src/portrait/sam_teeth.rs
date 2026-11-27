// SPDX-License-Identifier: AGPL-3.0-or-later
//! Teeth the parser saw only in part, outlined by SAM 2.1 Hiera-T (the
//! object-mask models, docs/MODELS.md) prompted by the parser itself.
//!
//! The parser's teeth probability falls off on teeth in shadow (a dark
//! beard, a shaded side of the mouth): its outline then holds the lit
//! teeth only and Teeth whitens them in patches. Growing the outline only
//! trades coverage for lips and gums (notes: benchmark-results/eys1-2026-11-23).
//! SAM follows the tooth edge instead. It sees a square around the mouth
//! (`CROP_WIDTHS` mouth widths) and is asked with the parser's most certain
//! teeth (`POSITIVES` points above `POSITIVE`) as positives and the middle
//! of the lips (Face Mesh's outer and inner lip rings) as negatives; its
//! mask is kept within the inner lip outline grown by `GUARD` mouth widths.
//! It replaces the parser's outline only when it is clearly larger
//! (`MIN_GROWTH`): where the parser saw the teeth, its outline stays.
//! The prompt rule and the growth threshold were chosen on EYE2's tuning
//! portraits (notes: benchmark-results/eys2-2026-11-24). The positives are
//! taken on the parser's grid; which cells are taken moves SAM's area by
//! up to 0.24 × the parser's on the same photo (notes:
//! benchmark-results/eys3-2026-11-27), so the growth threshold alone is not
//! a robust switch (wip, not shipped).

use anyhow::{Result, ensure};
use ndarray::{Array2, Array3, Array4};
use ort::value::Tensor;
use rawtonic_onnx::Model;

use super::{SIZE, TEETH, Window};
use crate::geometry::{Affine, sample_crop};
use crate::landmarks::{LIPS_INNER, LIPS_OUTER};

/// SAM 2.1's encoder and decoder, as the app loads them for object masks.
#[derive(Clone, Copy)]
pub struct Sam<'a> {
    pub encoder: &'a Model,
    pub decoder: &'a Model,
}

/// SAM 2.1 Hiera-T's files in the model pack (the app's object-mask models).
pub const ENCODER_FILE: &str = "sam2.1_tiny_encoder.onnx";
pub const DECODER_FILE: &str = "sam2.1_tiny_decoder.onnx";

/// SAM 2.1's encoder and decoder from `dir`, `None` when they are not
/// installed there.
pub fn load(dir: &std::path::Path) -> Result<Option<(Model, Model)>> {
    let (encoder, decoder) = (dir.join(ENCODER_FILE), dir.join(DECODER_FILE));
    if !encoder.is_file() || !decoder.is_file() {
        return Ok(None);
    }
    Ok(Some((
        Model::load(&encoder, rawtonic_onnx::ModelKind::SamEncoder)?,
        Model::load(&decoder, rawtonic_onnx::ModelKind::SamDecoder)?,
    )))
}

/// SAM 2.1's input side.
const INPUT: u32 = 1024;
/// Side of the square SAM sees, in mouth widths (Face Mesh's outer lip).
const CROP_WIDTHS: f32 = 2.2;
/// Positive points: this many, spread over the parser's teeth above
/// `POSITIVE` (or above its outline's threshold when none is).
const POSITIVES: usize = 6;
const POSITIVE: f32 = 0.6;
/// Lip landmarks (indices into the paired outer and inner rings) whose
/// midpoints are the negative points: both corners, upper and lower lip.
const NEGATIVES: [usize; 6] = [0, 4, 8, 11, 15, 19];
/// How far SAM's teeth may reach beyond the inner lip outline, mouth widths.
const GUARD: f32 = 0.03;
/// SAM's teeth replace the parser's when they cover at least this many
/// times its area.
const MIN_GROWTH: f32 = 1.25;

/// SAM's teeth for the parser's window `window` of `teeth_p` (crop `to_photo`)
/// on `image`, the face's Face Mesh `points`; `None` where the parser's
/// outline stays.
pub(super) fn refine(
    sam: Sam,
    image: &image::RgbImage,
    to_photo: &Affine,
    teeth_p: &[f32],
    window: &Window,
    points: &[[f32; 2]],
) -> Result<Option<Vec<f32>>> {
    let outer: Vec<[f32; 2]> = LIPS_OUTER.iter().map(|&i| points[i]).collect();
    let inner: Vec<[f32; 2]> = LIPS_INNER.iter().map(|&i| points[i]).collect();
    let width = outer
        .iter()
        .flat_map(|a| outer.iter().map(move |b| (a[0] - b[0]).hypot(a[1] - b[1])))
        .fold(0.0f32, f32::max);
    if width < 1.0 {
        return Ok(None);
    }
    let n = outer.len() as f32;
    let centre = outer
        .iter()
        .fold([0.0f32; 2], |s, p| [s[0] + p[0] / n, s[1] + p[1] / n]);
    let side = CROP_WIDTHS * width;
    let k = side / INPUT as f32;
    let to_sam_photo = Affine {
        a: k,
        b: 0.0,
        c: centre[0] - side / 2.0,
        d: 0.0,
        e: k,
        f: centre[1] - side / 2.0,
    };
    let to_input = |p: [f32; 2]| [(p[0] - to_sam_photo.c) / k, (p[1] - to_sam_photo.f) / k];

    let positives = positives(teeth_p, window);
    if positives.is_empty() {
        return Ok(None);
    }
    let mut prompt: Vec<([f32; 2], f32)> = positives
        .iter()
        .map(|&(u, v)| (to_input(to_photo.apply(u, v)), 1.0))
        .collect();
    for &i in &NEGATIVES {
        let mid = [
            (outer[i][0] + inner[i][0]) / 2.0,
            (outer[i][1] + inner[i][1]) / 2.0,
        ];
        prompt.push((to_input(mid), 0.0));
    }
    let logits = run(sam, &sample_crop(image, &to_sam_photo, INPUT), &prompt)?;

    // SAM's mask on the parser's grid, within the guarded inner lip.
    let reach = GUARD * width;
    let side_n = INPUT as usize;
    let logit_at = |x: f32, y: f32| -> f32 {
        let (x, y) = (x - 0.5, y - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let get = |i: f32, j: f32| -> f32 {
            if i < 0.0 || j < 0.0 || i >= INPUT as f32 || j >= INPUT as f32 {
                return -20.0;
            }
            logits[j as usize * side_n + i as usize]
        };
        let top = get(x0, y0) + (get(x0 + 1.0, y0) - get(x0, y0)) * fx;
        let bottom = get(x0, y0 + 1.0) + (get(x0 + 1.0, y0 + 1.0) - get(x0, y0 + 1.0)) * fx;
        top + (bottom - top) * fy
    };
    let net = SIZE as usize;
    let mut values = vec![0.0f32; net * net];
    let (mut sam_cells, mut parser_cells) = (0usize, 0usize);
    for v in window.y0..window.y1 {
        for u in window.x0..window.x1 {
            let i = v * net + u;
            if teeth_p[i] > TEETH {
                parser_cells += 1;
            }
            let p = to_photo.apply(u as f32 + 0.5, v as f32 + 0.5);
            if signed_distance(&inner, p) > reach {
                continue;
            }
            let [x, y] = to_input(p);
            let value = 1.0 / (1.0 + (-logit_at(x, y)).exp());
            if value > 0.5 {
                sam_cells += 1;
            }
            values[i] = value;
        }
    }
    if parser_cells == 0 || (sam_cells as f32) < MIN_GROWTH * parser_cells as f32 {
        return Ok(None);
    }
    Ok(Some(values))
}

/// Up to `POSITIVES` cells of `window` (centres, crop pixels), the most
/// certain first, each next one the farthest from those taken.
fn positives(teeth_p: &[f32], window: &Window) -> Vec<(f32, f32)> {
    let net = SIZE as usize;
    let above = |t: f32| -> Vec<(f32, f32, f32)> {
        (window.y0..window.y1)
            .flat_map(|v| (window.x0..window.x1).map(move |u| (u, v)))
            .filter_map(|(u, v)| {
                let p = teeth_p[v * net + u];
                (p > t).then_some((p, u as f32 + 0.5, v as f32 + 0.5))
            })
            .collect()
    };
    let mut cells = above(POSITIVE);
    if cells.is_empty() {
        cells = above(TEETH);
    }
    let Some(first) = cells
        .iter()
        .copied()
        .reduce(|a, b| if b.0 > a.0 { b } else { a })
    else {
        return Vec::new();
    };
    let mut taken = vec![(first.1, first.2)];
    let mut nearest: Vec<f32> = cells
        .iter()
        .map(|c| (c.1 - first.1).hypot(c.2 - first.2))
        .collect();
    while taken.len() < POSITIVES.min(cells.len()) {
        let (at, _) = nearest
            .iter()
            .enumerate()
            .fold((0, -1.0f32), |b, (i, &d)| if d > b.1 { (i, d) } else { b });
        let (u, v) = (cells[at].1, cells[at].2);
        taken.push((u, v));
        for (d, c) in nearest.iter_mut().zip(&cells) {
            *d = d.min((c.1 - u).hypot(c.2 - v));
        }
    }
    taken
}

/// Signed distance of `p` to the closed polygon `ring` (negative inside).
fn signed_distance(ring: &[[f32; 2]], p: [f32; 2]) -> f32 {
    let mut inside = false;
    let mut best = f32::INFINITY;
    for (i, a) in ring.iter().enumerate() {
        let b = ring[(i + 1) % ring.len()];
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < a[0] + (p[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1])
        {
            inside = !inside;
        }
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        best = best.min((p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy));
    }
    if inside { -best } else { best }
}

/// SAM's mask logits (`INPUT`² values) of the crop `pixels` (0..255) for
/// `prompt` (input pixels, label 1 positive, 0 negative).
fn run(sam: Sam, pixels: &[[f32; 3]], prompt: &[([f32; 2], f32)]) -> Result<Vec<f32>> {
    let n = INPUT as usize;
    let mut planar = vec![0u8; 3 * n * n];
    for (i, p) in pixels.iter().enumerate() {
        for c in 0..3 {
            planar[c * n * n + i] = p[c].round().clamp(0.0, 255.0) as u8;
        }
    }
    let (embed, high_0, high_1) = sam.encoder.run(|session| {
        let input = Array4::from_shape_vec((1, 3, n, n), planar.clone())?;
        let outputs = session.run(ort::inputs!["image" => Tensor::from_array(input)?])?;
        let take = |name: &str| -> Result<ndarray::ArrayD<f32>> {
            let array = outputs[name].try_extract_array::<f32>()?.to_owned();
            rawtonic_onnx::ensure_finite(array.iter())?;
            Ok(array)
        };
        Ok((
            take("image_embed")?,
            take("high_res_feats_0")?,
            take("high_res_feats_1")?,
        ))
    })?;
    let coords: Vec<f32> = prompt.iter().flat_map(|(p, _)| *p).collect();
    let labels: Vec<f32> = prompt.iter().map(|&(_, l)| l).collect();
    let logits = sam.decoder.run(|session| {
        let outputs = session.run(ort::inputs![
            "image_embed" => Tensor::from_array(embed.clone())?,
            "high_res_feats_0" => Tensor::from_array(high_0.clone())?,
            "high_res_feats_1" => Tensor::from_array(high_1.clone())?,
            "point_coords" => Tensor::from_array(Array3::from_shape_vec((1, prompt.len(), 2), coords.clone())?)?,
            "point_labels" => Tensor::from_array(Array2::from_shape_vec((1, prompt.len()), labels.clone())?)?,
            "mask_input" => Tensor::from_array(Array4::<f32>::zeros((1, 1, 256, 256)))?,
            "has_mask_input" => Tensor::from_array(ndarray::Array1::from_vec(vec![0.0f32]))?,
            "orig_im_size" => Tensor::from_array(ndarray::Array1::from_vec(vec![INPUT as f32, INPUT as f32]))?,
        ])?;
        let masks = outputs["masks"].try_extract_array::<f32>()?;
        Ok(masks.iter().take(n * n).copied().collect::<Vec<f32>>())
    })?;
    ensure!(
        logits.len() == n * n,
        "SAM teeth mask: {} values",
        logits.len()
    );
    rawtonic_onnx::ensure_finite(&logits)?;
    Ok(logits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positives_spread_from_the_most_certain_cell() {
        let net = SIZE as usize;
        let mut p = vec![0.0f32; net * net];
        // A row of teeth from u = 100 to 199 at v = 50, most certain at 150.
        for u in 100..200 {
            p[50 * net + u] = 0.7 + 0.2 * (1.0 - (u as f32 - 150.0).abs() / 50.0);
        }
        let window = Window {
            values: &p,
            x0: 90,
            y0: 40,
            x1: 210,
            y1: 60,
        };
        let taken = positives(&p, &window);
        assert_eq!(taken.len(), POSITIVES);
        assert_eq!(taken[0], (150.5, 50.5));
        // The next ones go to the ends first.
        let ends: Vec<f32> = taken[1..3].iter().map(|t| t.0).collect();
        assert!(ends.contains(&100.5) && ends.contains(&199.5), "{taken:?}");
    }

    #[test]
    fn lower_teeth_probability_still_gives_prompts() {
        let net = SIZE as usize;
        let mut p = vec![0.0f32; net * net];
        p[20 * net + 30] = 0.45;
        let window = Window {
            values: &p,
            x0: 0,
            y0: 0,
            x1: 64,
            y1: 64,
        };
        assert_eq!(positives(&p, &window), vec![(30.5, 20.5)]);
    }

    #[test]
    fn signed_distance_is_negative_inside() {
        let square = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        assert!((signed_distance(&square, [5.0, 5.0]) + 5.0).abs() < 1e-5);
        assert!((signed_distance(&square, [13.0, 5.0]) - 3.0).abs() < 1e-5);
    }
}
