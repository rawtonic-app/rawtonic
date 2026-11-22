// SPDX-License-Identifier: AGPL-3.0-or-later
//! The parts of a landscape (Lightroom's Landscape mask: sky, mountains,
//! water, vegetation, natural and artificial ground, architecture) from the
//! sky model's network with another head (scripts/models/export_landscape.py):
//! one run gives every part's probability. A mask of some parts is the sum
//! of their probabilities (the network's classes are exclusive, so the sum
//! is the probability of any of them), cut off the sky along the sky's
//! fitted edge (`refine_sky_mask`), and brought to the photo's size.

use std::path::Path;

use anyhow::Result;
use image::{DynamicImage, GrayImage};

use crate::{
    FittedInput, SKYSEG_INPUT_SIZE, fitted_input, refine_sky_mask, resize_gray_triangle, run_single,
};

// UperNet + ConvNeXt-T on ADE20K (the sky model's checkpoint, MIT), exported
// with a head of seven part probabilities, each the softmax summed over the
// ADE20K classes of the part. On 12 public scenes against human labels its
// parts scored above Lightroom Classic 15's Landscape masks (notes:
// benchmark-results/lsm1-2026-11-21/REPORT.md; docs/MODELS.md).
pub const LANDSCAPE_FILENAME: &str = "landscape_upernet_convnext_t.onnx";
pub const LANDSCAPE_SHA256: &str =
    "36faaefebdaacc743fa89a592b2a0ebcc35c561661a56ecf8566ab0d90f21451";

/// A part of a landscape; the order is the model's output channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LandscapePart {
    Mountains,
    Water,
    Vegetation,
    NaturalGround,
    ArtificialGround,
    Architecture,
    Sky,
}

impl LandscapePart {
    pub const ALL: [LandscapePart; 7] = [
        Self::Mountains,
        Self::Water,
        Self::Vegetation,
        Self::NaturalGround,
        Self::ArtificialGround,
        Self::Architecture,
        Self::Sky,
    ];

    /// The part's output channel.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&p| p == self).unwrap_or(0)
    }

    /// The part's name in a recipe (an `ai-landscape` component's `parts`).
    pub fn key(self) -> &'static str {
        match self {
            Self::Mountains => "mountains",
            Self::Water => "water",
            Self::Vegetation => "vegetation",
            Self::NaturalGround => "naturalGround",
            Self::ArtificialGround => "artificialGround",
            Self::Architecture => "architecture",
            Self::Sky => "sky",
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == key)
    }
}

/// The landscape model on ONNX Runtime (a GPU provider when one works,
/// else the CPU).
pub struct LandscapeModel {
    model: rawtonic_onnx::Model,
}

impl LandscapeModel {
    pub fn onnx(path: &Path) -> Result<Self> {
        Ok(Self {
            model: rawtonic_onnx::Model::load(path, rawtonic_onnx::ModelKind::Landscape)?,
        })
    }

    pub fn backend_name(&self) -> String {
        format!("ONNX Runtime ({})", self.model.provider())
    }
}

/// What one run of the network gives for a photo, from which each mask is
/// made (`landscape_mask`): every part's probability (0 … 255) at the size
/// the photo was fitted to in the network's input (at most
/// `SKYSEG_INPUT_SIZE` long), in the order of `LandscapePart::ALL`, and the
/// sky with its edge fitted to the photo (`refine_sky_mask`, at its grid:
/// at most `SKY_GUIDE_SIZE` / 2 long).
#[derive(Clone, Debug)]
pub struct LandscapeProbabilities {
    pub parts: Vec<GrayImage>,
    pub sky: GrayImage,
}

impl LandscapeProbabilities {
    pub fn part(&self, part: LandscapePart) -> &GrayImage {
        &self.parts[part.index()]
    }
}

pub fn landscape_probabilities(
    image: &DynamicImage,
    model: &LandscapeModel,
    linear: bool,
    run_options: Option<&ort::session::RunOptions>,
) -> Result<LandscapeProbabilities> {
    let start = std::time::Instant::now();
    let FittedInput {
        tensor,
        size: (w, h),
        paste: (px, py),
    } = fitted_input(image, linear);
    let input_ms = start.elapsed().as_millis();
    let out = run_single(&model.model, &tensor.into_dyn(), run_options)?;
    let network_ms = start.elapsed().as_millis() - input_ms;
    let side = SKYSEG_INPUT_SIZE as usize;
    let n = LandscapePart::ALL.len();
    anyhow::ensure!(
        out.len() == n * side * side,
        "the landscape model returned {} values",
        out.len()
    );
    let values = out.as_standard_layout();
    let values = values.as_slice().expect("a standard-layout array");
    let (w_, h_) = (w as usize, h as usize);
    let parts: Vec<GrayImage> = (0..n)
        .map(|c| {
            let plane = &values[c * side * side..(c + 1) * side * side];
            let mut data = Vec::with_capacity(w_ * h_);
            for y in 0..h_ {
                let row = &plane[(y + py) * side + px..(y + py) * side + px + w_];
                data.extend(
                    row.iter()
                        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8),
                );
            }
            GrayImage::from_raw(w, h, data).expect("sized")
        })
        .collect();
    let edges = std::time::Instant::now();
    let sky = refine_sky_mask(image, linear, &parts[LandscapePart::Sky.index()]);
    log::info!(
        "AI landscape: input {input_ms} ms, network {network_ms} ms on {}, sky edge {} ms",
        model.backend_name(),
        edges.elapsed().as_millis()
    );
    Ok(LandscapeProbabilities { parts, sky })
}

/// Parts of the landscape as a mask of `size` (the photo's). The sky is the
/// sky with its fitted edge. The other parts are their share of what the
/// network does not call sky, times what the fitted edge does not call sky:
/// where the fitted edge finds the sky, theirs follows it (the network's own
/// edge, from a quarter of its 512² input, spread 30-60 pixels into the sky
/// above a ridge on a 36 MP photo; checked by eye on two full-size RAWs);
/// where it is unsure, they keep part of the network's softer edge. Elsewhere they keep the network's edge, which the export
/// upsampled in logit space: the sky's colour-based edge fitting, applied
/// to each part, lowered every part's boundary F on the 12 measured scenes
/// (vegetation 0.69 → 0.49) and spread halos into flat areas (notes:
/// benchmark-results/lsm1-2026-11-21/REPORT.md). A photo without those
/// parts (or no parts) stays black.
pub fn landscape_mask(
    size: (u32, u32),
    probabilities: &LandscapeProbabilities,
    parts: &[LandscapePart],
    version: u32,
) -> GrayImage {
    let grid = landscape_mask_grid(probabilities, parts, version);
    resize_gray_triangle(&grid, size.0, size.1)
}

/// The Landscape mask's version (`landscapeVersion` of an `ai-landscape`
/// component; absent = 1): new components get this one.
///
/// 1: the fitted sky as `refine_sky_mask` gives it.
/// 2: the fitted sky only where the network sees some sky
/// (`FITTED_SKY_REACH`): the fitted edge's colour step had taken snowfields,
/// a glacier and a pale roof beside the sky for sky where the network was
/// sure of ground. On 12 COCO-Stuff scenes against human labels the sky
/// part went from IoU 0.926 / boundary F 0.734 to 0.937 / 0.811 (Lightroom
/// Classic 15: 0.931 / 0.798; 0.884 / 0.622 → 0.904 / 0.718 on the half not
/// used to choose the rule, Lightroom 0.888 / 0.700), the ground parts cut
/// off it along with it (notes: benchmark-results/lsm4-2026-11-22/REPORT.md).
pub const LANDSCAPE_VERSION: u32 = 2;

/// Version 2's limit on the fitted sky: at most this many times the
/// network's sky probability. A tenth, not more: within the network's soft
/// edge, where the true horizon lies on a full-size photo, the fitted edge
/// keeps deciding (larger values scored higher on the 640-pixel scenes,
/// whose coarse labels cannot tell a few pixels of edge apart).
const FITTED_SKY_REACH: u16 = 10;

/// `landscape_mask` at the grid it is made on (the fitted sky's, at most
/// `SKY_GUIDE_SIZE` / 2 long): all the mask holds, which the photo's size
/// only interpolates. The editor stores this (`bitmapGridOf`): a 36 MP
/// photo's mask is then decoded and placed in a few milliseconds instead of
/// ~80 (notes: benchmark-results/lsm2-2026-11-21/REPORT.md). `version`:
/// `LANDSCAPE_VERSION`.
pub fn landscape_mask_grid(
    probabilities: &LandscapeProbabilities,
    parts: &[LandscapePart],
    version: u32,
) -> GrayImage {
    let (gw, gh) = probabilities.sky.dimensions();
    let with_sky = parts.contains(&LandscapePart::Sky);
    let ground: Vec<LandscapePart> = LandscapePart::ALL
        .into_iter()
        .filter(|p| *p != LandscapePart::Sky && parts.contains(p))
        .collect();
    let mut out = GrayImage::new(gw, gh);
    if ground.is_empty() && !with_sky {
        return out;
    }
    let sky_coarse = resize_gray_triangle(probabilities.part(LandscapePart::Sky), gw, gh);
    // The fitted sky (version 2: within reach of the network's).
    let fitted: Vec<u8> = if version >= 2 {
        probabilities
            .sky
            .iter()
            .zip(sky_coarse.iter())
            .map(|(&r, &c)| r.min((c as u16 * FITTED_SKY_REACH).min(255) as u8))
            .collect()
    } else {
        probabilities.sky.as_raw().clone()
    };
    if !ground.is_empty() {
        let (fw, fh) = probabilities.parts[0].dimensions();
        let mut sum = GrayImage::new(fw, fh);
        for part in &ground {
            for (s, &v) in sum.iter_mut().zip(probabilities.part(*part).iter()) {
                *s = s.saturating_add(v);
            }
        }
        let sum = resize_gray_triangle(&sum, gw, gh);
        for (((o, &p), &c), &r) in out
            .iter_mut()
            .zip(sum.iter())
            .zip(sky_coarse.iter())
            .zip(fitted.iter())
        {
            // The parts' share of the non-sky probability (at most 1) …
            let not_sky = (255 - c).max(p).max(1) as f32;
            let share = p as f32 / not_sky;
            // … of what the fitted sky leaves.
            *o = (share * (255 - r) as f32).round() as u8;
        }
    }
    if with_sky {
        for (o, &r) in out.iter_mut().zip(fitted.iter()) {
            *o = o.saturating_add(r);
        }
    }
    out
}

/// The share of the photo (0 … 1) each part covers (probability above one
/// half), in the order of `LandscapePart::ALL`: which parts the photo has.
pub fn landscape_shares(probabilities: &LandscapeProbabilities) -> Vec<f32> {
    probabilities
        .parts
        .iter()
        .map(|p| {
            let n = p.as_raw().len().max(1);
            p.as_raw().iter().filter(|&&v| v > 127).count() as f32 / n as f32
        })
        .collect()
}

/// `landscape_probabilities` then `landscape_mask`: parts of a photo in one
/// call (exports, the CLI).
pub fn run_landscape_model(
    image: &DynamicImage,
    model: &LandscapeModel,
    parts: &[LandscapePart],
    version: u32,
    linear: bool,
    run_options: Option<&ort::session::RunOptions>,
) -> Result<GrayImage> {
    let probabilities = landscape_probabilities(image, model, linear, run_options)?;
    Ok(landscape_mask(
        (image.width(), image.height()),
        &probabilities,
        parts,
        version,
    ))
}

/// The parts `keys` name (`LandscapePart::key`); an unknown key is an error.
pub fn landscape_parts(keys: &[impl AsRef<str>]) -> Result<Vec<LandscapePart>> {
    keys.iter()
        .map(|k| {
            LandscapePart::parse(k.as_ref())
                .ok_or_else(|| anyhow::anyhow!("unknown landscape part {}", k.as_ref()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_round_trip_their_keys_in_channel_order() {
        for (i, part) in LandscapePart::ALL.into_iter().enumerate() {
            assert_eq!(part.index(), i);
            assert_eq!(LandscapePart::parse(part.key()), Some(part));
        }
        assert_eq!(LandscapePart::parse("lava"), None);
    }

    fn plane(v: u8) -> GrayImage {
        GrayImage::from_pixel(4, 2, image::Luma([v]))
    }

    fn probabilities(parts: [u8; 7], sky: u8) -> LandscapeProbabilities {
        LandscapeProbabilities {
            parts: parts.into_iter().map(plane).collect(),
            sky: plane(sky),
        }
    }

    #[test]
    fn without_sky_a_mask_of_several_parts_is_their_summed_probability() {
        let p = probabilities([100, 90, 60, 0, 0, 200, 0], 0);
        use LandscapePart::*;
        assert_eq!(landscape_mask((4, 2), &p, &[Water], 1), plane(90));
        assert_eq!(
            landscape_mask((4, 2), &p, &[Water, Mountains], 1),
            plane(190)
        );
        // Rounding may push a sum past 255: it stays full.
        assert_eq!(
            landscape_mask((4, 2), &p, &[Architecture, Mountains], 1),
            plane(255)
        );
        assert_eq!(landscape_mask((4, 2), &p, &[], 1), plane(0));
        assert_eq!(
            landscape_mask((8, 4), &p, &[Vegetation], 1).dimensions(),
            (8, 4)
        );
        assert_eq!(landscape_shares(&p), [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        assert!(landscape_parts(&["water", "lava"]).is_err());
        assert_eq!(landscape_parts(&["water", "sky"]).unwrap(), [Water, Sky]);
    }

    #[test]
    fn parts_end_where_the_fitted_sky_begins() {
        use LandscapePart::*;
        // At the network's soft edge: half sky, half mountain.
        let edge = probabilities([127, 0, 0, 0, 0, 0, 128], 0);
        // The fitted edge says ground here: the mountain takes it all.
        assert_eq!(landscape_mask((4, 2), &edge, &[Mountains], 1), plane(255));
        // The fitted edge says sky: the mountain's halo is gone.
        let halo = probabilities([127, 0, 0, 0, 0, 0, 128], 255);
        assert_eq!(landscape_mask((4, 2), &halo, &[Mountains], 1), plane(0));
        assert_eq!(landscape_mask((4, 2), &halo, &[Sky], 1), plane(255));
        // Sky and the part beside it make the whole photo.
        let between = probabilities([127, 0, 0, 0, 0, 0, 128], 100);
        assert_eq!(
            landscape_mask((4, 2), &between, &[Mountains], 1),
            plane(155)
        );
        assert_eq!(
            landscape_mask((4, 2), &between, &[Mountains, Sky], 1),
            plane(255)
        );
        // A part sharing the ground with another keeps its share.
        let shared = probabilities([60, 60, 0, 0, 0, 0, 135], 155);
        assert_eq!(landscape_mask((4, 2), &shared, &[Water], 1), plane(50));
    }

    #[test]
    fn version_two_keeps_the_sky_where_the_network_sees_some() {
        use LandscapePart::*;
        // The fitted edge calls sky what the network is sure is ground (a
        // snowfield beside the sky): version 1 gives it to the sky and cuts
        // the mountain off it, version 2 keeps it the mountain's.
        let snow = probabilities([255, 0, 0, 0, 0, 0, 0], 255);
        assert_eq!(landscape_mask_grid(&snow, &[Sky], 1), plane(255));
        assert_eq!(landscape_mask_grid(&snow, &[Mountains], 1), plane(0));
        assert_eq!(landscape_mask_grid(&snow, &[Sky], 2), plane(0));
        assert_eq!(landscape_mask_grid(&snow, &[Mountains], 2), plane(255));
        // Up to a tenth of the network's sky probability scales the fitted
        // sky down: 13 of 255 allows 130 of it.
        let faint = probabilities([242, 0, 0, 0, 0, 0, 13], 200);
        assert_eq!(landscape_mask_grid(&faint, &[Sky], 2), plane(130));
        assert_eq!(
            landscape_mask_grid(&faint, &[Mountains, Sky], 2),
            plane(255)
        );
        // From a tenth on, the fitted edge decides as in version 1 (its
        // band within the network's soft edge).
        for p in [
            probabilities([200, 0, 0, 0, 0, 0, 26], 255),
            probabilities([127, 0, 0, 0, 0, 0, 128], 100),
            probabilities([60, 60, 0, 0, 0, 0, 135], 155),
        ] {
            for parts in [&[Sky][..], &[Mountains], &[Water, Sky]] {
                assert_eq!(
                    landscape_mask_grid(&p, parts, 2),
                    landscape_mask_grid(&p, parts, 1)
                );
            }
        }
        // Where the fitted edge says ground, nothing changes either.
        let ground = probabilities([0, 0, 0, 0, 0, 255, 0], 0);
        assert_eq!(landscape_mask_grid(&ground, &[Architecture], 2), plane(255));
        assert_eq!(landscape_mask_grid(&ground, &[Sky], 2), plane(0));
    }

    /// Tool: landscape part masks as the app computes them, for evaluation.
    /// `LANDSCAPE_MODEL` (the ONNX file), `LANDSCAPE_FILES` (`;`-separated
    /// 8-bit photos), `LANDSCAPE_OUT` (writes new files `<stem>.<part>.png`;
    /// an existing one stops the tool), `LANDSCAPE_VERSION` (default the
    /// current one).
    #[test]
    #[ignore = "tool: landscape masks for evaluation"]
    fn print_landscape_masks() {
        let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} not set"));
        let model = LandscapeModel::onnx(Path::new(&var("LANDSCAPE_MODEL"))).unwrap();
        let out = std::path::PathBuf::from(var("LANDSCAPE_OUT"));
        for file in var("LANDSCAPE_FILES").split(';') {
            let image = image::open(file).unwrap();
            let stem = Path::new(file).file_stem().unwrap().to_string_lossy();
            let start = std::time::Instant::now();
            let p = landscape_probabilities(&image, &model, false, None).unwrap();
            let version = std::env::var("LANDSCAPE_VERSION")
                .map_or(LANDSCAPE_VERSION, |v| v.parse().expect("a version"));
            let net = start.elapsed().as_millis();
            for part in LandscapePart::ALL {
                let t = std::time::Instant::now();
                // A new file only: never over an input or an earlier result
                // (two inputs with one stem stop here).
                let path = out.join(format!("{stem}.{}.png", part.key()));
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                landscape_mask((image.width(), image.height()), &p, &[part], version)
                    .write_to(&mut std::io::BufWriter::new(file), image::ImageFormat::Png)
                    .unwrap();
                eprintln!("{stem} {} {} ms", part.key(), t.elapsed().as_millis());
            }
            eprintln!("{stem} network {net} ms");
        }
    }
}
