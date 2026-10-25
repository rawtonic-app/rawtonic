// SPDX-License-Identifier: AGPL-3.0-or-later
//! Multi-scale colour noise reduction, set by the colour noise measured in
//! the photo itself.
//!
//! The chroma of the image (in square-root encoding, where shot noise has
//! about the same spread at every brightness: red minus green and blue minus
//! green) is split into a Laplacian pyramid. At each level the colour noise
//! is measured: the RMS of the level's detail in its smoothest blocks, where
//! the photo has no colour detail of its own (clipped and black blocks have
//! no noise to measure and are skipped). Demosaiced noise is not even in
//! that encoding (read noise lifts the shadows and midtones), so the colour
//! noise is also measured per brightness band at the finest level (a noise
//! profile relative to the photo's noise) and each pixel's detail is
//! compared with the noise at its own brightness. Each level's detail is
//! then kept by a soft threshold at a multiple of that level's noise, set by
//! the recipe's Color amount, Detail and Smoothness, and applied to the
//! detail's local energy (3 × 3), so single noisy pixels go and the tails of
//! edges stay.
//! Two guards keep detail of the coarse levels where it belongs to an edge:
//! - where the level's luminance detail stands well above the luminance
//!   noise, the part of its chroma detail that follows the luminance detail
//!   (a local regression over 5 × 5) stays, the rest is thresholded like
//!   anywhere else; colour edges along brightness edges stay crisp, colour
//!   noise next to brightness texture goes;
//! - where the coarser level has a colour edge (its chroma gradient, weighted
//!   by how much the gradients around it share one direction, well above
//!   the level's colour noise), the whole detail stays: colour edges without
//!   a luminance step. Round noise blotches have gradients in every direction
//!   and do not count as edges. This guard's threshold follows the level's
//!   own threshold, so more Color or Smoothness (or less Detail) also
//!   smooths more next to weak colour edges.
//!
//! The finest level keeps its detail next to colour edges of the rebuilt
//! level 1 (edges that survived the coarse levels, not noise blotches).
//!
//! Rebuilding the two finest coarse levels and the full resolution, each
//! coarser sample's share of a finer sample is also weighted by how close
//! its rebuilt chroma is to the finer sample's own (an edge-aware, joint
//! bilateral expansion): a colour edge stays on its side instead of spreading
//! over the expansion's reach (about 2 px at each level), and in flat areas
//! the rebuilt samples agree, so the weights are even. Above Amount 25 the
//! coarsest level (1/32 of the resolution) is smoothed too, by a bilateral
//! filter that keeps strong colour edges, so Color 100 also removes colour
//! variation larger than the detail levels reach.
//! Colour edges and colour texture above the noise stay, blotches of every
//! size within it go. Luminance is kept exactly. The photo's base image is
//! developed without the decode-time chroma clean-up for this filter
//! (`image_loader::BaseImageOptions::for_color_noise`): that clean-up spread
//! colour edges over about 4.5 px before any of this (notes:
//! reference/color-noise-reference.md).
//!
//! The pyramid is built and measured here on the CPU when a render source
//! is uploaded (`ChromaPyramid::build`, once per source, from the whole
//! photo). Level 1 is rebuilt for new slider values from the coarse levels
//! (`reconstruct`; display frames do the same on the GPU from `prepared`,
//! crates/gpu/src/shaders/color_noise.wgsl); the renderer does the finest
//! level per pixel (shader.wgsl, `apply_color_noise_multiscale`) from level 1
//! and its reconstruction. `apply_to_linear` is the same filter entirely on
//! the CPU, for tests and the measurement harness
//! (crates/gpu/examples/color_nr_harness.rs).
//!
//! The idea (a chroma pyramid thresholded by noise measured per level) was
//! described for RAWmakase's adaptive colour noise reduction; the
//! implementation, the measurement and every constant here are Rawtonic's
//! own: the guards and the profile were chosen on the synthetic harness
//! (edge bleed, colour texture) together with Rawtonic's own measurements of
//! colour noise on a training split of public high-ISO RAW files, and
//! checked on held-out files and harness seeds (notes:
//! reference/color-noise-reference.md).

use image::DynamicImage;
use rayon::prelude::*;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

/// Recipe key of the colour noise reduction's version: from
/// [`MULTISCALE_VERSION`] on this filter ([`Tuning::for_version`]), 1 the
/// renderer's cross-bilateral filter. Recipes without the key keep the
/// filter they rendered with before: the cross-bilateral one when they set
/// `colorNoiseReduction`, the newest ([`VERSION`]) when they leave it to the
/// RAW default (photos never edited; the editor starts them the same way).
pub const VERSION_KEY: &str = "colorNoiseVersion";
/// The newest version: 10 (2026-10-24) keeps it within its neighbourhood's
/// plain reconstruction only, closer ([`Tuning::range_plain`]); 9 (2026-10-20)
/// keeps each pixel's filtered chroma
/// within the range of its neighbourhood's filtered reconstruction
/// ([`Tuning::range_clamp`]); 8 (2026-10-20) judges whether such a speck stands
/// out in brightness by its luminance after the luminance noise reduction
/// ([`Tuning::speck_luma_after_nr`]); 7 (2026-10-18) gives an isolated colour speck (a
/// pixel far from its 3 × 3 median chroma while its luminance is not) its
/// neighbourhood's median chroma as its finest detail ([`Tuning::speck_swap`]);
/// 6 (2026-10-17) measures the finest detail against
/// the same edge-aware expansion of level 1 the reconstruction takes, so a
/// colour edge no longer gets a halo and dark sides of colour edges no
/// longer blotchy rims ([`Tuning::finest_detail_aware`]); 5 (2026-10-17)
/// decides at full resolution which side
/// of a colour edge a pixel's aware expansion takes by its 3 × 3 median when
/// its own chroma is an outlier (fewer isolated colour specks;
/// [`Tuning::guide`]); 4 (2026-10-17) builds the pyramid's levels from
/// linear mean colours, so dark noisy colours keep their hue
/// ([`Tuning::linear_means`]); 3 (2026-10-16) keeps more 8–30 px colour
/// texture where those scales of the photo are quiet ([`Tuning::quiet`],
/// [`Tuning::coarse_noise_fall`]); 2 is the same filter before that.
pub const VERSION: u64 = 10;
/// The first version that is this filter (on a base image without the
/// decode-time chroma clean-up; every later version shares that base).
pub const MULTISCALE_VERSION: u64 = 2;

/// The recipe's colour noise version (1 = the cross-bilateral filter),
/// never newer than this renderer knows.
pub fn version(adjustments: &serde_json::Value, is_raw: bool) -> u64 {
    match adjustments
        .get(VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
    {
        Some(version) => version.clamp(1, VERSION),
        None if is_raw && adjustments.get("colorNoiseReduction").is_none() => VERSION,
        None => 1,
    }
}

/// Whether a recipe's colour noise reduction is this filter.
pub fn multiscale(adjustments: &serde_json::Value, is_raw: bool) -> bool {
    version(adjustments, is_raw) >= MULTISCALE_VERSION
}

/// Detail levels with their own threshold; the level below them (1/32 of
/// the resolution) is kept as it is.
pub const LEVELS: usize = 5;
/// Side of the blocks the noise is measured in, at every level.
const BLOCK: usize = 8;
/// Share of the measurable blocks, from the smoothest, whose detail is
/// taken as the noise: colour detail only adds to a block's variation.
const SMOOTHEST: f32 = 0.2;
/// The fewest measurable blocks a level's noise is measured from.
const MIN_BLOCKS: usize = 16;
/// A level without enough measurable blocks gets this share of the finer
/// level's noise (demosaiced colour noise on the synthetic harness: 0.45–0.7).
const COARSER_FALLBACK: f32 = 0.6;
/// Pixels darker than this linear luminance (crushed blacks) or with a
/// channel above `MEASURABLE_PEAK` (clipping) are not measured.
const MEASURABLE_BLACK: f32 = 0.0005;
const MEASURABLE_PEAK: f32 = 0.9;
/// Brightness bins of the noise profile (square-root luminance 0–1).
pub const PROFILE_BINS: usize = 8;
/// The fewest measurable blocks a profile bin is measured from.
const MIN_PROFILE_BLOCKS: usize = 64;
/// Limits of a bin's noise relative to the photo's noise.
const PROFILE_RANGE: (f32, f32) = (0.5, 4.0);
/// Luminance weights of the renderer (shader.wgsl `LUMA_COEFF`).
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// The recipe's colour noise settings, each 0–1: Color amount, Detail and
/// Smoothness (Lightroom's defaults 25, 50, 50).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub amount: f32,
    pub detail: f32,
    pub smoothness: f32,
}

/// What decides which side of a colour edge an edge-aware expansion takes
/// for a finer sample ([`Tuning::guide`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Guide {
    /// The sample's own chroma (versions 2 and 3).
    Own,
    /// Its chroma smoothed over 3 × 3 (weights 1-2-1).
    Mean,
    /// The median of the 3 × 3 chroma, per channel.
    Median,
    /// Its own chroma unless that is farther than this multiple of the
    /// level's colour noise from the 3 × 3 median, then the median.
    Switch(f32),
}

/// The constants of the filter, chosen on the measurement harness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// Threshold of each level (finest first) in multiples of its measured
    /// noise at Amount 25, Detail and Smoothness 50 (see `quiet` for the
    /// coarsest levels of quieter photos).
    pub multiples: [f32; LEVELS],
    /// How sharply the gain moves from removed to kept around the
    /// threshold: the gain is x^s / (1 + x^s) with x = |detail|² / threshold².
    pub steepness: f32,
    /// Threshold growth from Amount 25 to 100 (× (1 + growth)).
    pub amount_growth: f32,
    /// Detail scales level `l`'s threshold by 2^((0.5 − detail) · exp).
    pub detail_exponents: [f32; LEVELS],
    /// Smoothness scales level `l`'s threshold by 2^((smoothness − 0.5) · exp).
    pub smoothness_exponents: [f32; LEVELS],
    /// The part of a level's chroma detail that follows its luminance
    /// detail is kept where the luminance detail exceeds this multiple of
    /// the level's luminance noise (0: off).
    pub luma_guard: f32,
    /// A level's detail is kept where the coarser level's coherent chroma
    /// gradient exceeds this multiple of the level's noise (0: off), at
    /// Amount 25, Detail and Smoothness 50.
    pub edge_guard: f32,
    /// The chroma-edge guard's threshold grows with the level's threshold
    /// relative to its value at Amount 25, Detail and Smoothness 50, to this
    /// power.
    pub edge_follow: f32,
    /// The finest level's detail is kept where the rebuilt level 1's chroma
    /// gradient exceeds this multiple of the finest level's noise (0: off);
    /// its threshold grows with the finest threshold relative to its value
    /// at Amount 25, Detail and Smoothness 50, to the power `finest_follow`.
    pub finest_edge_guard: f32,
    pub finest_follow: f32,
    /// In very noisy photos the finest guard kept each pixel's own noisy
    /// colour along the borders of coarser colour blotches, giving them
    /// hard, speckled edges (Capture One stays calm there). Its threshold
    /// therefore grows while the finest colour noise (`noise[0]`) rises from
    /// `finest_edge_noise[0]` to `[1]`, until the guard keeps
    /// `finest_edge_floor` of its strength (notes:
    /// reference/color-noise-reference.md, round 5).
    pub finest_edge_noise: [f32; 2],
    pub finest_edge_floor: f32,
    /// Rebuilding the levels finer than `aware_levels` + 1 (1/2 and 1/4 of
    /// the resolution by default), each coarser sample's share of a finer
    /// sample is weighted by how close its rebuilt chroma is to the finer
    /// sample's own: exp(−d² / (aware · noise · profile)²), d the chroma
    /// distance, noise the finer level's (0: plain expansion). A colour edge
    /// stays on its side instead of spreading over the expansion's reach;
    /// in flat areas the rebuilt samples agree and the weights are even.
    pub aware: f32,
    pub aware_levels: usize,
    /// The aware ranges (rebuild and finest) grow with the level's threshold
    /// relative to its value at Amount 25, Detail and Smoothness 50, to this
    /// power. 0 by default: growing them at more Color only bought colour
    /// noise far below one 8-bit step (below the noise visibility floor) at
    /// the cost of visibly softer colour edges (notes:
    /// reference/color-noise-reference.md, Round 4).
    pub aware_follow: f32,
    /// A finer sample whose chroma is farther than this from every coarser
    /// sample's (squared distance times the aware range's inverse square)
    /// is taken for noise and expanded plainly: a lone noisy sample would
    /// otherwise pick the coarser samples nearest to its noise.
    pub aware_far: f32,
    /// The same for the finest level, expanding the rebuilt level 1 to full
    /// resolution, in multiples of the finest level's noise (renderer
    /// `ms_expand_aware`).
    pub finest_aware: f32,
    /// Above Amount 25 the coarsest level (1/32 of the resolution) is
    /// smoothed too: a bilateral filter of `base_blur` samples' spread at
    /// Amount 100 (growing linearly from 25) and a range of `base_range`
    /// times the coarsest level's noise; colour variation larger than the
    /// detail levels reach goes, strong colour edges stay.
    pub base_blur: f32,
    pub base_range: f32,
    /// On levels from `quiet_from` on (1/8 and 1/16 of the resolution), the
    /// multiple falls with the level's noise: `quiet[1]` + `quiet[2]` ·
    /// log2(noise / `quiet[0]`), between `quiet[1]` and `multiples[l]`
    /// (`quiet[0]` 0: off). Real colour patches of 8–30 px (moss on bark, a
    /// blue flower) stand only 2–4× above the noise of these levels, so the
    /// full multiples removed them, also at ISO 3200; where these levels'
    /// noise is low its residue stays invisible (flat-area 4–16 px colour
    /// noise +0.01–0.03 Oklab ×100), and the noisiest photos (ISO 25600 and
    /// up) keep the full multiples, which their noise blotches need (notes:
    /// reference/color-noise-reference.md, round 7). Version 3.
    pub quiet: [f32; 3],
    pub quiet_from: usize,
    /// From level `coarse_noise_from` on (1/8 of the resolution), a level's
    /// colour noise is taken as at most the finer level's over
    /// `coarse_noise_fall` (0: off). The true colour noise of high-ISO
    /// photos (noisy minus ground-truth frames of the same scenes) falls
    /// 1.7–1.9× per level there, but the smoothest blocks of these coarse
    /// levels still hold the scene's own colour variation in textured
    /// photos, which raised their measured noise up to 1.6× and removed real
    /// colour patches with it (notes: reference/color-noise-reference.md,
    /// round 7). Version 3.
    pub coarse_noise_fall: f32,
    pub coarse_noise_from: usize,
    /// Each pyramid level holds the chroma of its linear mean colour
    /// ([`ChromaPyramid::linear_means`]) instead of the mean of its pixels'
    /// square-root chroma. Square roots averaged pull a channel that is
    /// weak against its noise towards zero, so dark noisy colours lost
    /// their weakest channel once the noise was removed (a black cat's fur
    /// turned blue when its shadows were lifted; red ÷ green 0.67 → 0.54 on
    /// a flat synthetic patch of that colour; notes:
    /// reference/color-noise-reference.md, round 8). Version 4.
    pub linear_means: bool,
    /// The guide of the edge-aware expansions: with each sample's own noisy
    /// chroma, neighbours took opposite sides of a colour edge by chance,
    /// leaving 2 px stair-steps along colour edges and isolated off-colour
    /// pixels (notes: reference/color-noise-reference.md, round 8).
    /// Versions 2–4 take the sample's own chroma; a robust guide traded
    /// colour-edge contrast for fewer specks in every form measured (round
    /// 9). Version 5 takes the switch (k = 1) at full resolution only, the
    /// cheapest form: isolated specks −22 % for colour edges −0.2 % (55
    /// files, rounds 9–10); the coarse levels' guide cost ~4 % edges.
    /// `guide_coarse`: rebuilding the coarse levels; `guide_finest`: the
    /// full resolution (renderer); `guide_edge`: the finest detail kept only
    /// by the finest edge guard is the guide's, not the pixel's own.
    pub guide: Guide,
    pub guide_coarse: bool,
    pub guide_finest: bool,
    pub guide_edge: bool,
    /// On confident colour edges (where the edge guards keep the detail:
    /// the coarser level's coherent chroma gradient when rebuilding the
    /// coarse levels, the rebuilt level 1's gradient at full resolution)
    /// the guide moves back to the sample's own chroma by the guards' gain.
    pub guide_gate: bool,
    /// The full-resolution detail (energy- and edge-kept) is the guide's
    /// chroma instead of the pixel's own: with [`Guide::Switch`] only
    /// pixels far from their 3 × 3 median (isolated off-colour pixels)
    /// change.
    pub guide_detail: bool,
    /// The full-resolution detail is measured against the same edge-aware
    /// expansion of level 1 (same weights) as the reconstruction, as the
    /// coarse levels' detail already is. Against the plain expansion
    /// (versions 2–5) the pixel next to a colour edge kept the other side's
    /// share of level 1 as "detail": added to an aware reconstruction that
    /// took its own side, it overshot past its own colour (a halo of the
    /// complementary colour, strongest as saturated rims on the dark side of
    /// an edge in the square-root encoding) and a pixel whose noisy chroma
    /// picked the wrong side snapped to that side's colour (blotches along
    /// edges, coloured thin dark lines). Measured on the edge-fringe and
    /// lateral-CA sets (bright-edge excess chroma lower on 36 of 37 files;
    /// Lumix GX850 13.6 → 9.7 %) and on 55 high-ISO files (fewer isolated
    /// specks, closer to RawNIND ground truth); colour transitions are
    /// 0.1–0.45 px wider than version 5's, which snapped them to a side
    /// (notes: reference/color-noise-reference.md, round 11). Version 6.
    pub finest_detail_aware: bool,
    /// A pixel whose chroma lies more than this many finest noise σ from
    /// the median of its 3 × 3 neighbourhood while its square-root
    /// luminance lies within [`SPECK_LUMA`] ± ½ σ of theirs (fading out
    /// over that σ; an off-colour
    /// pixel, not a small bright or dark feature: a coloured point that
    /// stands out of its surroundings by more than the noise, also a blue
    /// one whose luminance weight is small, keeps its colour) keeps the
    /// median chroma as its detail (0 = off, multiples of 0.25 up to 3.75).
    /// Such pixels were the isolated specks left after version 6, mostly the
    /// pixel's own input outlier kept by the energy gain or the finest edge
    /// guard next to luminance edges: on 55 high-ISO RAWs −24 % specks at
    /// Color 25 (−17 % at 50) for colour edges ≥ 0.998 of version 6 in every
    /// ISO band and RawNIND ground-truth error equal or lower (notes:
    /// reference/color-noise-reference.md, rounds 12–13). Version 7.
    pub speck_swap: f32,
    /// The speck swap's luminance gate reads the pixel's luminance after the
    /// luminance noise reduction (renderer) instead of before it: a pixel
    /// darker or brighter than its neighbours only by noise, which the
    /// luminance noise reduction brings back to them, is an off-colour
    /// speck; a point the luminance noise reduction keeps standing out (a
    /// small light, real fine detail) keeps its colour. Before it, such
    /// pixels kept their noisy chroma: most isolated colour specks left
    /// after version 7 (on 55 high-ISO RAWs at Color 50 8.0 → 5.8 per
    /// Mpx, Capture One 5.4, Camera Raw 4.7) for colour edges ≥ 0.9998 of
    /// version 7 in every ISO band and RawNIND ground-truth error unchanged
    /// (notes: reference/color-noise-reference.md, round 14). Without
    /// luminance noise reduction the luminance is the input's, as in
    /// version 7 (the CPU reference has none). Version 8.
    pub speck_luma_after_nr: bool,
    /// The filtered chroma of each pixel (reconstruction plus kept detail)
    /// is kept within the range, per component, of the plain expansion of
    /// the rebuilt level 1 at the pixel and its 8 neighbours (and of its own
    /// edge-aware reconstruction), widened by this many finest noise σ
    /// (0 = off, multiples of 0.25 up to 3.75). Most isolated colour specks
    /// left after version 8 were not lone input outliers but pixels whose
    /// noisy chroma the energy gain or the finest edge guard kept next to
    /// luminance and colour edges (dark pixels by a bright border, rims of
    /// colour edges), where the speck swap's luminance gate keeps them: a
    /// pixel may now go beyond what its neighbourhood's reconstruction holds
    /// only by about the noise. A colour edge stays (its pixels lie between
    /// the two sides' reconstructions), as do thin coloured lines up to the
    /// widened range. It fades with the filter like the speck swap
    /// ([`Tuning::speck_strength`]) and for pixels far beyond the range
    /// ([`RANGE_CLAMP_FAR`]). Version 9.
    pub range_clamp: f32,
    /// The range of [`Tuning::range_clamp`] leaves out the pixel's own
    /// edge-aware reconstruction (only the plain expansions at the pixel and
    /// its 8 neighbours), so a pixel's chroma is checked against what its
    /// neighbourhood holds also where its own reconstruction took a side by
    /// its noisy chroma, and the margin is [`RANGE_CLAMP_PLAIN`]. Most colour
    /// specks left at Color 25 by version 9 lay in flat areas whose rebuilt
    /// level 1 still holds some colour noise: there the finest edge guard
    /// keeps noisy detail and the aware expansion takes a level-1 sample by
    /// the pixel's own noise, both within the range while the pixel's own
    /// reconstruction was part of it. On 55 high-ISO RAWs colour specks
    /// 7.5 → 5.0 / 4.2 → 3.5 / 4.1 → 3.1 per Mpx at Color 25/50/100
    /// (Capture One's default 5.4, Camera Raw 5.4/4.7/4.6) for colour edges
    /// ≥ 0.9988 of version 9 in every ISO band and RawNIND ground-truth
    /// error equal or lower in every cell; fine colour detail −1 % at ISO
    /// 1600–2500 and 6400. Only a smaller margin with the own
    /// reconstruction in the range (1.0 σ) cost more edges for fewer specks
    /// (notes: reference/color-noise-reference.md, round 16). Version 10.
    pub range_plain: bool,
}

impl Default for Tuning {
    /// The newest version's constants ([`VERSION`]).
    fn default() -> Self {
        Self::for_version(VERSION)
    }
}

impl Tuning {
    /// The constants of colour noise version `version` (2 or newer; older
    /// versions are not this filter and get version 2's).
    pub fn for_version(version: u64) -> Self {
        let v2 = Self::version2();
        if version <= 2 {
            return v2;
        }
        let v3 = Self {
            quiet: [0.012, 2.5, 2.75],
            quiet_from: 3,
            coarse_noise_fall: 1.7,
            coarse_noise_from: 3,
            ..v2
        };
        if version == 3 {
            return v3;
        }
        let v4 = Self {
            linear_means: true,
            ..v3
        };
        if version == 4 {
            return v4;
        }
        let v5 = Self {
            guide: Guide::Switch(1.0),
            guide_finest: true,
            ..v4
        };
        if version == 5 {
            return v5;
        }
        let v6 = Self {
            finest_detail_aware: true,
            ..v5
        };
        if version == 6 {
            return v6;
        }
        let v7 = Self {
            speck_swap: SPECK_SWAP,
            ..v6
        };
        if version == 7 {
            return v7;
        }
        let v8 = Self {
            speck_luma_after_nr: true,
            ..v7
        };
        if version == 8 {
            return v8;
        }
        let v9 = Self {
            range_clamp: RANGE_CLAMP,
            ..v8
        };
        if version == 9 {
            return v9;
        }
        Self {
            range_clamp: RANGE_CLAMP_PLAIN,
            range_plain: true,
            ..v9
        }
    }

    /// Version 2 (2026-10-13 … 2026-10-16).
    fn version2() -> Self {
        Self {
            multiples: [7.0, 8.0, 10.0, 8.0, 7.0],
            steepness: 3.0,
            amount_growth: 2.0,
            detail_exponents: [8.0, 5.0, 2.0, 0.5, 0.0],
            smoothness_exponents: [0.0, 1.0, 3.0, 5.0, 6.0],
            luma_guard: 10.0,
            edge_guard: 12.0,
            edge_follow: 0.25,
            finest_edge_guard: 1.0,
            finest_follow: 0.25,
            finest_edge_noise: [0.025, 0.04],
            finest_edge_floor: 0.5,
            aware: 0.3,
            aware_levels: 2,
            aware_follow: 0.0,
            aware_far: AWARE_FAR,
            finest_aware: 1.0,
            base_blur: 2.0,
            base_range: 10.0,
            quiet: [0.0, 0.0, 0.0],
            quiet_from: LEVELS,
            coarse_noise_fall: 0.0,
            coarse_noise_from: LEVELS,
            linear_means: false,
            guide: Guide::Own,
            guide_coarse: false,
            guide_finest: false,
            guide_edge: false,
            guide_gate: false,
            guide_detail: false,
            finest_detail_aware: false,
            speck_swap: 0.0,
            speck_luma_after_nr: false,
            range_clamp: 0.0,
            range_plain: false,
        }
    }

    /// The renderer's code of the finest guide (`color_noise_guide`): 0 its
    /// own chroma, 1 mean, 2 median, 3 switch, + 4 when the edge-kept detail
    /// is the guide's, + 8 with the edge gate, + 16 when all detail is the
    /// guide's; + 32 with the detail against the aware expansion
    /// ([`Tuning::finest_detail_aware`]); + 64 + 128 × 4 K with the speck
    /// swap K ([`Tuning::speck_swap`]); + 2048 when its luminance gate reads
    /// the luminance after the luminance noise reduction
    /// ([`Tuning::speck_luma_after_nr`]); + 4096 × 4 T with the range clamp
    /// T ([`Tuning::range_clamp`]); + 65536 when its range leaves out the
    /// pixel's own reconstruction ([`Tuning::range_plain`]); and the switch's
    /// squared multiple.
    pub fn finest_guide_code(&self) -> (f32, f32) {
        let aware = if self.finest_detail_aware { 32.0 } else { 0.0 }
            + 4096.0 * self.range_clamp_steps()
            + if self.range_plain { 65536.0 } else { 0.0 }
            + if self.speck_swap > 0.0 {
                64.0 + 128.0 * (self.speck_swap * 4.0).round().min(15.0)
                    + if self.speck_luma_after_nr {
                        2048.0
                    } else {
                        0.0
                    }
            } else {
                0.0
            };
        if !self.guide_finest {
            return (aware, 0.0);
        }
        let (code, k2) = match self.guide {
            Guide::Own => return (aware, 0.0),
            Guide::Mean => (1.0, 0.0),
            Guide::Median => (2.0, 0.0),
            Guide::Switch(k) => (3.0, k * k),
        };
        (
            code + if self.guide_edge { 4.0 } else { 0.0 }
                + if self.guide_gate { 8.0 } else { 0.0 }
                + if self.guide_detail { 16.0 } else { 0.0 }
                + aware,
            k2,
        )
    }

    /// The colour noise each level is filtered by, from the pyramid's
    /// `measured` noise (`coarse_noise_fall`).
    pub fn level_noise(&self, measured: &[f32; LEVELS]) -> [f32; LEVELS] {
        let mut noise = *measured;
        if self.coarse_noise_fall > 0.0 {
            for l in self.coarse_noise_from.max(1)..LEVELS {
                noise[l] = noise[l].min(noise[l - 1] / self.coarse_noise_fall);
            }
        }
        noise
    }

    /// Level `l`'s multiple at its noise `noise` (`quiet`).
    pub fn multiple(&self, l: usize, noise: f32) -> f32 {
        let [reference, low, growth] = self.quiet;
        if l < self.quiet_from || reference <= 0.0 || noise <= 0.0 {
            return self.multiples[l];
        }
        (low + growth * (noise / reference).log2())
            .clamp(low.min(self.multiples[l]), self.multiples[l])
    }

    /// Each level's threshold (in square-root chroma units) for `settings`
    /// and the measured `noise`; all zero at Amount 0.
    pub fn thresholds(&self, settings: Settings, noise: &[f32; LEVELS]) -> [f32; LEVELS] {
        let amount = settings.amount.clamp(0.0, 1.0);
        if amount <= 0.0 {
            return [0.0; LEVELS];
        }
        // Like Lightroom's Color slider, most of the effect is reached by
        // 25; above, the thresholds grow slowly.
        let response = (amount.min(0.25) / 0.25).sqrt()
            * (1.0 + self.amount_growth * (amount - 0.25).max(0.0) / 0.75);
        let detail = settings.detail.clamp(0.0, 1.0);
        let smoothness = settings.smoothness.clamp(0.0, 1.0);
        std::array::from_fn(|l| {
            noise[l]
                * self.multiple(l, noise[l])
                * response
                * (0.5 - detail).mul_add(self.detail_exponents[l], 0.0).exp2()
                * (smoothness - 0.5)
                    .mul_add(self.smoothness_exponents[l], 0.0)
                    .exp2()
        })
    }

    /// The squared factor on level `l`'s chroma-edge guard threshold when
    /// the level's threshold is `threshold` (`edge_follow`); 1 at Amount 25,
    /// Detail and Smoothness 50.
    pub fn edge_scale2(&self, l: usize, threshold: f32, noise: &[f32; LEVELS]) -> f32 {
        let base = noise[l] * self.multiple(l, noise[l]);
        if base <= 0.0 || threshold <= 0.0 {
            return 1.0;
        }
        (threshold / base).powf(2.0 * self.edge_follow)
    }

    /// The spread (in samples) of the coarsest level's smoothing for
    /// `settings`: 0 up to Amount 25, `base_blur` at 100.
    pub fn base_sigma(&self, settings: Settings) -> f32 {
        self.base_blur * ((settings.amount.clamp(0.0, 1.0) - 0.25).max(0.0) / 0.75)
    }

    /// The inverse squared range of the aware expansion rebuilding level
    /// `l` − 1 from level `l` (before the noise profile at the sample's
    /// brightness); 0 where the plain expansion is used.
    pub fn aware_inv2(&self, l: usize, thresholds: &[f32; LEVELS], noise: &[f32; LEVELS]) -> f32 {
        let sigma = self.aware * noise[l - 1] * self.aware_scale(l - 1, thresholds, noise);
        if l > self.aware_levels || sigma <= 0.0 {
            return 0.0;
        }
        1.0 / (sigma * sigma)
    }

    /// The factor on level `l`'s aware range: its threshold relative to
    /// the one at Amount 25, Detail and Smoothness 50, to `aware_follow`.
    fn aware_scale(&self, l: usize, thresholds: &[f32; LEVELS], noise: &[f32; LEVELS]) -> f32 {
        let base = noise[l] * self.multiple(l, noise[l]);
        if base <= 0.0 || thresholds[l] <= 0.0 {
            return 1.0;
        }
        (thresholds[l] / base).powf(self.aware_follow)
    }

    /// The share of [`Tuning::speck_swap`] applied: the finest level's
    /// threshold over its value at Amount 25 and Detail 50, at most 1.
    /// Below Color 25 or above Detail 50 the swap weakens with the rest of
    /// the filter and is gone at Color 0 (Smoothness leaves the finest
    /// threshold, and so the swap, unchanged).
    pub fn speck_strength(&self, thresholds: &[f32; LEVELS], noise: &[f32; LEVELS]) -> f32 {
        let base = noise[0] * self.multiple(0, noise[0]);
        if self.speck_swap <= 0.0 || base <= 0.0 {
            return 0.0;
        }
        (thresholds[0] / base).clamp(0.0, 1.0)
    }

    /// [`Tuning::range_clamp`] in steps of 0.25 σ (at most 15).
    fn range_clamp_steps(&self) -> f32 {
        (self.range_clamp * 4.0).round().clamp(0.0, 15.0)
    }

    /// The renderer's squared range of the finest level's aware expansion;
    /// 0 for the plain expansion.
    pub fn finest_aware2(&self, thresholds: &[f32; LEVELS], noise: &[f32; LEVELS]) -> f32 {
        (self.finest_aware * noise[0] * self.aware_scale(0, thresholds, noise)).powi(2)
    }

    /// The finest level's squared chroma-edge guard threshold for the
    /// finest threshold `thresholds[0]`; 0 without the guard.
    pub fn finest_edge2(&self, thresholds: &[f32; LEVELS], noise: &[f32; LEVELS]) -> f32 {
        let base = noise[0] * self.multiples[0];
        let scale2 = if base > 0.0 && thresholds[0] > 0.0 {
            (thresholds[0] / base).powf(2.0 * self.finest_follow)
        } else {
            1.0
        };
        let strength = self.finest_edge_strength(noise[0]);
        (self.finest_edge_guard * noise[0]).powi(2) * scale2 / (strength * strength)
    }

    /// The finest guard's strength at finest colour noise `noise0`: 1 up to
    /// `finest_edge_noise[0]`, `finest_edge_floor` from `[1]`.
    fn finest_edge_strength(&self, noise0: f32) -> f32 {
        let [lo, hi] = self.finest_edge_noise;
        if hi <= lo {
            return 1.0;
        }
        let t = ((noise0 - lo) / (hi - lo)).clamp(0.0, 1.0);
        let fade = t * t * (3.0 - 2.0 * t);
        (1.0 - fade * (1.0 - self.finest_edge_floor)).max(1e-3)
    }
}

/// The squared chroma gradient of a level-1 plane (central differences,
/// borders clamped) at its sample nearest to finest position (x, y), as the
/// renderer's `ms_level1_gradient2` (on the rebuilt level 1: colour edges
/// that survived the coarse levels, not noise blotches).
#[inline]
pub fn level1_gradient2(level1: &Plane, x: usize, y: usize) -> f32 {
    let (k, j) = (
        (x / 2).min(level1.width - 1),
        (y / 2).min(level1.height - 1),
    );
    let l = level1.at(k.saturating_sub(1), j);
    let r = level1.at((k + 1).min(level1.width - 1), j);
    let u = level1.at(k, j.saturating_sub(1));
    let d = level1.at(k, (j + 1).min(level1.height - 1));
    let gx = [0.5 * (r[0] - l[0]), 0.5 * (r[1] - l[1])];
    let gy = [0.5 * (d[0] - u[0]), 0.5 * (d[1] - u[1])];
    gx[0] * gx[0] + gx[1] * gx[1] + gy[0] * gy[0] + gy[1] * gy[1]
}

/// Share of a level's detail that is kept: x^s / (1 + x^s) with x =
/// `size2` / `threshold2`; everything is kept without a threshold.
#[inline]
pub fn gain(size2: f32, threshold2: f32, steepness: f32) -> f32 {
    if threshold2 <= 0.0 {
        return 1.0;
    }
    if size2 <= 0.0 {
        return 0.0;
    }
    let x = threshold2 / size2;
    // The default steepness without powf (a third of the rebuild's time).
    let p = if steepness == 3.0 {
        x * x * x
    } else {
        x.powf(steepness)
    };
    1.0 / (1.0 + p)
}

/// Channels of the pyramid: red − green, blue − green and luminance, all in
/// square-root encoding.
pub const CH: usize = 3;
pub type Px = [f32; CH];

/// A chroma plane (red − green, blue − green and the luminance that guards
/// edges, in square-root encoding).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub data: Vec<Px>,
}

impl Plane {
    #[inline]
    fn at(&self, x: usize, y: usize) -> Px {
        self.data[y * self.width + x]
    }
}

/// Signed square root, as the renderer's `nr_encode`.
#[inline]
pub fn encode(v: f32) -> f32 {
    if v < 0.0 { -(-v).sqrt() } else { v.sqrt() }
}

#[inline]
fn decode(p: f32) -> f32 {
    p * p.abs()
}

/// Chroma of a linear RGB pixel.
#[inline]
pub fn chroma(rgb: [f32; 3]) -> [f32; 2] {
    let p = rgb.map(encode);
    [p[0] - p[1], p[2] - p[1]]
}

/// Chroma and luminance of a linear RGB pixel, as the pyramid holds them.
#[inline]
pub fn chroma_luma(rgb: [f32; 3]) -> Px {
    let p = rgb.map(encode);
    [
        p[0] - p[1],
        p[2] - p[1],
        LUMA[0] * p[0] + LUMA[1] * p[1] + LUMA[2] * p[2],
    ]
}

#[inline]
fn measurable(rgb: [f32; 3]) -> bool {
    let luma = LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2];
    luma >= MEASURABLE_BLACK && rgb.iter().all(|v| *v < MEASURABLE_PEAK)
}

#[inline]
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear RGB pixels of a render source (RAW sources are linear, others
/// sRGB-encoded, as the renderer reads them).
struct Source<'a> {
    data: Cow<'a, [f32]>,
    channels: usize,
    width: usize,
    height: usize,
    srgb: bool,
}

impl<'a> Source<'a> {
    fn new(image: &'a DynamicImage, linear: bool) -> Self {
        let (width, height) = (image.width() as usize, image.height() as usize);
        let (data, channels) = match image {
            DynamicImage::ImageRgb32F(buffer) => (Cow::Borrowed(buffer.as_raw().as_slice()), 3),
            DynamicImage::ImageRgba32F(buffer) => (Cow::Borrowed(buffer.as_raw().as_slice()), 4),
            other => (Cow::Owned(other.to_rgb32f().into_raw()), 3),
        };
        Self {
            data,
            channels,
            width,
            height,
            srgb: !linear,
        }
    }

    #[inline]
    fn pixel(&self, x: usize, y: usize) -> [f32; 3] {
        let i = (y * self.width + x) * self.channels;
        let rgb = [self.data[i], self.data[i + 1], self.data[i + 2]];
        if self.srgb {
            rgb.map(srgb_to_linear)
        } else {
            rgb
        }
    }
}

const KERNEL: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

#[inline]
fn mirror(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    let i = if i < 0 { -i } else { i };
    let i = if i >= n { 2 * n - 2 - i } else { i };
    i.clamp(0, n - 1) as usize
}

/// [`expand_at`] of `rebuilt` and of `coarse` (same size) at finer position
/// (x, y), each sample's weight also scaled by exp(−d² · `inv2`), d the
/// distance of its rebuilt chroma from `guide` (the finer sample's chroma):
/// the expansion keeps to the guide's side of a colour edge. The distances
/// are taken relative to the nearest sample's, so the weights cannot all
/// vanish (renderer `ms_expand_aware`, color_noise.wgsl
/// `reconstruct_level`). Per position, as the reference of
/// [`expand_aware_row`].
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn expand_aware(
    rebuilt: &Plane,
    coarse: &Plane,
    x: usize,
    y: usize,
    guide: [f32; 2],
    inv2: f32,
    far: f32,
    profile: Option<&[f32; PROFILE_BINS]>,
) -> (Px, Px) {
    let (xs, wx) = expand_taps(x, coarse.width);
    let (ys, wy) = expand_taps(y, coarse.height);
    // One pass over the taps: their values, distances and the plain
    // expansions.
    let mut taps = [(0.0f32, [0.0f32; CH], [0.0f32; CH], 0.0f32); 9];
    let mut count = 0;
    let (mut plain_r, mut plain_c) = ([0.0f32; CH], [0.0f32; CH]);
    let mut nearest = f32::INFINITY;
    let mut farthest = 0.0f32;
    for j in 0..3 {
        for i in 0..3 {
            let ws = wx[i] * wy[j];
            if ws == 0.0 {
                continue;
            }
            let rv = rebuilt.at(xs[i], ys[j]);
            let cv = coarse.at(xs[i], ys[j]);
            let (da, db) = (rv[0] - guide[0], rv[1] - guide[1]);
            let d2 = da * da + db * db;
            nearest = nearest.min(d2);
            farthest = farthest.max(d2);
            for ch in 0..CH {
                plain_r[ch] += ws * rv[ch];
                plain_c[ch] += ws * cv[ch];
            }
            taps[count] = (ws, rv, cv, d2);
            count += 1;
        }
    }
    // With a noise profile the range follows the colour noise at the
    // brightness of the plain expansion.
    let inv2 = match profile {
        Some(profile) => {
            let p = profile_at(profile, plain_c[2]);
            inv2 / (p * p)
        }
        None => inv2,
    };
    // A sample far from every coarser one is taken for noise; where the
    // weights would differ by less than AWARE_FLAT (flat areas) the plain
    // expansion is the same.
    if nearest * inv2 > far || (farthest - nearest) * inv2 < AWARE_FLAT {
        return (plain_r, plain_c);
    }
    let mut r = [0.0f32; CH];
    let mut c = [0.0f32; CH];
    let mut sw = 0.0f32;
    for &(ws, rv, cv, d2) in &taps[..count] {
        let w = ws * exp_neg((d2 - nearest) * inv2);
        for ch in 0..CH {
            r[ch] += w * rv[ch];
            c[ch] += w * cv[ch];
        }
        sw += w;
    }
    (r.map(|v| v / sw), c.map(|v| v / sw))
}

/// e^−x for x ≥ 0 (relative error below 3e-5; at least e^−87, where the
/// weights it makes no longer count): 2^−x·log2 e from the exponent bits and
/// the Taylor polynomial of 2^−f for the fraction, without branches so the
/// aware weights vectorize. The renderer's `exp` is the GPU's.
#[inline(always)]
fn exp_neg(x: f32) -> f32 {
    let t = x.clamp(0.0, 87.0) * std::f32::consts::LOG2_E;
    let n = t.floor();
    let f = t - n;
    // 2^−f on [0, 1): minimax-style polynomial of 2^−f = e^(−f ln 2).
    let p = 1.0
        + f * (-std::f32::consts::LN_2
            + f * (0.240_226_5
                + f * (-0.055_504_11
                    + f * (0.009_618_129 + f * (-0.001_333_355 + f * 0.000_154_035_3)))));
    p * f32::from_bits(((127 - n as i32) as u32) << 23)
}

/// Reusable buffers of [`expand_aware_row`] (one per worker).
#[derive(Default)]
pub struct AwareRow {
    /// The coarse rows of the taps as padded columns (index m = column
    /// m − 1, clamped): rebuilt chroma, coarse chroma and luminance.
    rows: [Vec<f32>; 5],
    guide: [Vec<f32>; 2],
    out: [Vec<f32>; 4],
    /// [`expand_aware`]'s reconstruction (`.0`) and coarse (`.1`) chroma
    /// at each position of the row.
    pub rebuilt: Vec<[f32; 2]>,
    pub coarse: Vec<[f32; 2]>,
}

/// Positions of one parity taken together by [`aware_pass`].
const AWARE_BLOCK: usize = 8;

/// [`expand_aware`] for the whole finer row `y` (`width` positions, guide
/// chroma `guide(x)`), into `scratch.rebuilt` and `scratch.coarse`. Even
/// and odd positions are taken apart, so each tap is a run of columns, and
/// in blocks of fixed size: the passes vectorize across positions.
#[allow(clippy::too_many_arguments)]
pub fn expand_aware_row(
    rebuilt: &Plane,
    coarse: &Plane,
    y: usize,
    width: usize,
    guide: impl Fn(usize) -> [f32; 2],
    inv2: f32,
    far: f32,
    profile: Option<&[f32; PROFILE_BINS]>,
    scratch: &mut AwareRow,
) {
    let (ys, wy) = expand_taps(y, coarse.height);
    let cw = coarse.width;
    // Room for the last block's taps beyond the clamped edge column.
    let stride = cw + 3 + AWARE_BLOCK;
    let ny = if y.is_multiple_of(2) { 3 } else { 2 };
    for row in scratch.rows.iter_mut() {
        row.resize(3 * stride, 0.0);
    }
    for (j, &yy) in ys.iter().take(ny).enumerate() {
        let r = &rebuilt.data[yy * cw..(yy + 1) * cw];
        let c = &coarse.data[yy * cw..(yy + 1) * cw];
        for m in 0..stride {
            let col = m.saturating_sub(1).min(cw - 1);
            let at = j * stride + m;
            scratch.rows[0][at] = r[col][0];
            scratch.rows[1][at] = r[col][1];
            scratch.rows[2][at] = c[col][0];
            scratch.rows[3][at] = c[col][1];
            scratch.rows[4][at] = c[col][2];
        }
    }
    scratch.rebuilt.resize(width, [0.0; 2]);
    scratch.coarse.resize(width, [0.0; 2]);
    for parity in 0..2 {
        let n = (width + 1 - parity) / 2;
        if n == 0 {
            continue;
        }
        let total = n.div_ceil(AWARE_BLOCK) * AWARE_BLOCK;
        for g in scratch.guide.iter_mut() {
            g.clear();
            g.resize(total, 0.0);
        }
        for k in 0..n {
            let [a, b] = guide(2 * k + parity);
            scratch.guide[0][k] = a;
            scratch.guide[1][k] = b;
        }
        for o in scratch.out.iter_mut() {
            o.resize(total, 0.0);
        }
        // Even positions take columns k − 1, k, k + 1 (padded k … k + 2),
        // odd ones k, k + 1 (padded k + 1, k + 2).
        let even = [0.125, 0.75, 0.125];
        let odd = [0.5, 0.5, 0.0];
        let args = (stride, total, wy, inv2, far, profile);
        match (parity, ny) {
            (0, 3) => aware_pass::<3, 3>(scratch, 0, even, args),
            (0, _) => aware_pass::<3, 2>(scratch, 0, even, args),
            (_, 3) => aware_pass::<2, 3>(scratch, 1, odd, args),
            _ => aware_pass::<2, 2>(scratch, 1, odd, args),
        }
        for k in 0..n {
            let x = 2 * k + parity;
            scratch.rebuilt[x] = [scratch.out[0][k], scratch.out[1][k]];
            scratch.coarse[x] = [scratch.out[2][k], scratch.out[3][k]];
        }
    }
}

type PassArgs<'a> = (
    usize,
    usize,
    [f32; 3],
    f32,
    f32,
    Option<&'a [f32; PROFILE_BINS]>,
);

#[inline(always)]
fn aware_pass<const NX: usize, const NY: usize>(
    scratch: &mut AwareRow,
    m0: usize,
    wx: [f32; 3],
    (stride, total, wy, inv2, far, profile): PassArgs,
) {
    const B: usize = AWARE_BLOCK;
    const W: usize = AWARE_BLOCK + 2;
    let AwareRow {
        rows, guide, out, ..
    } = scratch;
    for k0 in (0..total).step_by(B) {
        // Tap (i, j) of position k0 + b is element b + i of `taps[j]`.
        let taps = |row: &[f32]| -> [[f32; W]; NY] {
            std::array::from_fn(|j| {
                let at = j * stride + m0 + k0;
                row[at..at + W].try_into().unwrap()
            })
        };
        let (ra, rb, ca, cb) = (
            taps(&rows[0]),
            taps(&rows[1]),
            taps(&rows[2]),
            taps(&rows[3]),
        );
        let g0: [f32; B] = guide[0][k0..k0 + B].try_into().unwrap();
        let g1: [f32; B] = guide[1][k0..k0 + B].try_into().unwrap();
        // With a noise profile the range follows the colour noise at the
        // brightness of the plain expansion.
        let mut inv2s = [inv2; B];
        if let Some(profile) = profile {
            let cl = taps(&rows[4]);
            let mut l = [0.0f32; B];
            for j in 0..NY {
                for i in 0..NX {
                    let ws = wx[i] * wy[j];
                    for b in 0..B {
                        l[b] += ws * cl[j][b + i];
                    }
                }
            }
            for b in 0..B {
                let p = profile_at(profile, l[b]);
                inv2s[b] = inv2 / (p * p);
            }
        }
        // Tap by tap, each over the block's positions (the inner loops
        // vectorize; per position the sums keep the order of
        // `expand_aware`).
        let mut d2 = [[[0.0f32; B]; NX]; NY];
        let mut nearest = [f32::INFINITY; B];
        let mut farthest = [0.0f32; B];
        let mut p = [[0.0f32; B]; 4];
        for j in 0..NY {
            for i in 0..NX {
                let ws = wx[i] * wy[j];
                for b in 0..B {
                    let (a, c) = (ra[j][b + i], rb[j][b + i]);
                    let (da, dc) = (a - g0[b], c - g1[b]);
                    let d = da * da + dc * dc;
                    d2[j][i][b] = d;
                    nearest[b] = nearest[b].min(d);
                    farthest[b] = farthest[b].max(d);
                    p[0][b] += ws * a;
                    p[1][b] += ws * c;
                    p[2][b] += ws * ca[j][b + i];
                    p[3][b] += ws * cb[j][b + i];
                }
            }
        }
        let mut q = [[0.0f32; B]; 4];
        let mut sw = [0.0f32; B];
        for j in 0..NY {
            for i in 0..NX {
                let ws = wx[i] * wy[j];
                for b in 0..B {
                    let w = ws * exp_neg((d2[j][i][b] - nearest[b]) * inv2s[b]);
                    q[0][b] += w * ra[j][b + i];
                    q[1][b] += w * rb[j][b + i];
                    q[2][b] += w * ca[j][b + i];
                    q[3][b] += w * cb[j][b + i];
                    sw[b] += w;
                }
            }
        }
        let mut o = [[0.0f32; B]; 4];
        for b in 0..B {
            // A sample far from every coarser one is taken for noise;
            // where the weights would differ by less than AWARE_FLAT (flat
            // areas) the plain expansion is the same.
            let plain =
                nearest[b] * inv2s[b] > far || (farthest[b] - nearest[b]) * inv2s[b] < AWARE_FLAT;
            for c in 0..4 {
                o[c][b] = if plain { p[c][b] } else { q[c][b] / sw[b] };
            }
        }
        for (dst, src) in out.iter_mut().zip(&o) {
            dst[k0..k0 + B].copy_from_slice(src);
        }
    }
}

/// Below this spread of the aware exponents (weights within 0.1 % of
/// each other) the aware expansion is taken as the plain one.
pub const AWARE_FLAT: f32 = 1e-3;

/// Default of [`Tuning::aware_far`]: a sample 5 aware ranges from every
/// coarser one (chosen on the reference files between 14 and 40: lower
/// leaves colour edges softer, higher more 1–4 px mottling; notes:
/// reference/color-noise-reference.md).
pub const AWARE_FAR: f32 = 25.0;

/// Version 7's [`Tuning::speck_swap`] in finest noise σ and the luminance
/// difference from the 3 × 3 median allowed for a swap, in the same σ (notes:
/// reference/color-noise-reference.md, round 13).
pub const SPECK_SWAP: f32 = 2.0;
pub const SPECK_LUMA: f32 = 1.0;

/// Version 9's [`Tuning::range_clamp`] in finest noise σ (notes:
/// reference/color-noise-reference.md, round 15).
pub const RANGE_CLAMP: f32 = 1.5;
/// Version 10's [`Tuning::range_clamp`] with [`Tuning::range_plain`] (notes:
/// reference/color-noise-reference.md, round 16).
pub const RANGE_CLAMP_PLAIN: f32 = 1.25;
/// Beyond the range by this many finest noise σ (from, to) the clamp fades
/// out: a coloured point that far from its neighbourhood (a small light, a
/// real fine detail) keeps its colour, as with the speck swap's luminance
/// gate.
pub const RANGE_CLAMP_FAR: [f32; 2] = [4.0, 8.0];

/// Taps and weights of the expansion of a coarser level at finer position
/// `x` (`n` coarser samples): 1/8, 3/4, 1/8 around an even position, 1/2,
/// 1/2 between two samples at an odd one. Shared with the renderer.
#[inline]
fn expand_taps(x: usize, n: usize) -> ([usize; 3], [f32; 3]) {
    let last = n as isize - 1;
    let clamp = |i: isize| i.clamp(0, last) as usize;
    let k = (x / 2) as isize;
    if x.is_multiple_of(2) {
        ([clamp(k - 1), clamp(k), clamp(k + 1)], [0.125, 0.75, 0.125])
    } else {
        ([clamp(k), clamp(k + 1), clamp(k + 1)], [0.5, 0.5, 0.0])
    }
}

/// The expansion of `plane` at finer position (x, y).
#[inline]
pub fn expand_at(plane: &Plane, x: usize, y: usize) -> Px {
    let (xs, wx) = expand_taps(x, plane.width);
    let (ys, wy) = expand_taps(y, plane.height);
    let mut out = [0.0f32; CH];
    for (j, &yy) in ys.iter().enumerate() {
        if wy[j] == 0.0 {
            continue;
        }
        let mut row = [0.0f32; CH];
        for (i, &xx) in xs.iter().enumerate() {
            if wx[i] == 0.0 {
                continue;
            }
            let v = plane.at(xx, yy);
            for c in 0..CH {
                row[c] += wx[i] * v[c];
            }
        }
        for c in 0..CH {
            out[c] += wy[j] * row[c];
        }
    }
    out
}

/// `plane` filtered by the 5-tap binomial kernel and halved (rounded up).
fn reduce(plane: &Plane) -> Plane {
    let (w, h) = (plane.width, plane.height);
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    let mut horizontal = vec![[0.0f32; CH]; w2 * h];
    horizontal
        .par_chunks_mut(w2)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                let mut sum = [0.0f32; CH];
                for (k, weight) in KERNEL.iter().enumerate() {
                    let s = plane.at(mirror(2 * x as isize + k as isize - 2, w), y);
                    for c in 0..CH {
                        sum[c] += weight * s[c];
                    }
                }
                *v = sum;
            }
        });
    let mut data = vec![[0.0f32; CH]; w2 * h2];
    data.par_chunks_mut(w2).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            let mut sum = [0.0f32; CH];
            for (k, weight) in KERNEL.iter().enumerate() {
                let s = horizontal[mirror(2 * y as isize + k as isize - 2, h) * w2 + x];
                for c in 0..CH {
                    sum[c] += weight * s[c];
                }
            }
            *v = sum;
        }
    });
    Plane {
        width: w2,
        height: h2,
        data,
    }
}

/// Measurability halved: a coarser pixel is measurable when the finer
/// pixels it covers are.
fn reduce_mask(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    (0..w2 * h2)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (2 * (i % w2), 2 * (i / w2));
            mask[y * w + x]
                && mask[y * w + (x + 1).min(w - 1)]
                && mask[(y + 1).min(h - 1) * w + x]
                && mask[(y + 1).min(h - 1) * w + (x + 1).min(w - 1)]
        })
        .collect()
}

/// Rows of the first pyramid level computed together from the source.
const BAND_ROWS: usize = 32;

/// One source row reduced horizontally by the pyramid kernel: `out[x]` from
/// `line[2x - 2 ..= 2x + 2]`, mirrored at the ends.
#[inline]
fn reduce_row(line: &[Px], out: &mut [Px]) {
    let w = line.len();
    for (x, v) in out.iter_mut().enumerate() {
        let mut sum = [0.0f32; CH];
        if 2 * x >= 2 && 2 * x + 2 < w {
            // Inside the row: the taps without mirroring, in the same order.
            for (weight, s) in KERNEL.iter().zip(&line[2 * x - 2..2 * x + 3]) {
                for c in 0..CH {
                    sum[c] += weight * s[c];
                }
            }
        } else {
            for (k, weight) in KERNEL.iter().enumerate() {
                let s = line[mirror(2 * x as isize + k as isize - 2, w)];
                for c in 0..CH {
                    sum[c] += weight * s[c];
                }
            }
        }
        *v = sum;
    }
}

/// Level 1 of the chroma pyramid straight from the source (the
/// full-resolution chroma plane is never stored) and its measurability,
/// and with `means` level 1 of the linear colour too
/// ([`ChromaPyramid::build_with`]) from the same reads of the source. Each
/// value is the same sums in the same order as one pass per plane (the
/// reference `first_level` of the tests); rows inside the frame skip the
/// mirroring. The pyramid of a 24 MP RAW builds in 54 instead of 72 ms
/// (notes: parity-gaps/G43, task BX3).
fn first_levels(source: &Source, means: bool) -> (Plane, Vec<bool>, Option<Plane>) {
    let (w, h) = (source.width, source.height);
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    let mut data = vec![[0.0f32; CH]; w2 * h2];
    let mut mask = vec![false; w2 * h2];
    let mut mean_data = means.then(|| vec![[0.0f32; CH]; w2 * h2]);
    let band =
        |band: usize, out: &mut [Px], out_mask: &mut [bool], mut out_means: Option<&mut [Px]>| {
            let y0 = band * BAND_ROWS;
            let rows = out.len() / w2;
            // Source rows 2·y0 − 2 … 2·(y0 + rows) + 1, horizontally reduced.
            let first = 2 * y0 as isize - 2;
            let count = 2 * rows + 3;
            let mut horizontal = vec![[0.0f32; CH]; count * w2];
            let mut horizontal_means = vec![[0.0f32; CH]; if means { count * w2 } else { 0 }];
            let mut line = vec![[0.0f32; CH]; w];
            let mut line_means = vec![[0.0f32; CH]; if means { w } else { 0 }];
            for r in 0..count {
                let y = mirror(first + r as isize, h);
                if means {
                    for (x, (c, m)) in line.iter_mut().zip(line_means.iter_mut()).enumerate() {
                        let rgb = source.pixel(x, y);
                        *c = chroma_luma(rgb);
                        *m = rgb;
                    }
                    reduce_row(&line_means, &mut horizontal_means[r * w2..(r + 1) * w2]);
                } else {
                    for (x, c) in line.iter_mut().enumerate() {
                        *c = chroma_luma(source.pixel(x, y));
                    }
                }
                reduce_row(&line, &mut horizontal[r * w2..(r + 1) * w2]);
            }
            let vertical = |horizontal: &[Px], out: &mut [Px]| {
                for row in 0..rows {
                    for x in 0..w2 {
                        let mut sum = [0.0f32; CH];
                        for (k, weight) in KERNEL.iter().enumerate() {
                            let s = horizontal[(2 * row + k) * w2 + x];
                            for c in 0..CH {
                                sum[c] += weight * s[c];
                            }
                        }
                        out[row * w2 + x] = sum;
                    }
                }
            };
            vertical(&horizontal, out);
            if let Some(out_means) = out_means.as_deref_mut() {
                vertical(&horizontal_means, out_means);
            }
            for row in 0..rows {
                for x in 0..w2 {
                    let (sx, sy) = (2 * x, 2 * (y0 + row));
                    out_mask[row * w2 + x] =
                        [(0, 0), (1, 0), (0, 1), (1, 1)].iter().all(|(dx, dy)| {
                            measurable(source.pixel((sx + dx).min(w - 1), (sy + dy).min(h - 1)))
                        });
                }
            }
        };
    let chunk = w2 * BAND_ROWS;
    match mean_data.as_mut() {
        Some(mean_data) => data
            .par_chunks_mut(chunk)
            .zip(mask.par_chunks_mut(chunk))
            .zip(mean_data.par_chunks_mut(chunk))
            .enumerate()
            .for_each(|(i, ((out, out_mask), out_means))| band(i, out, out_mask, Some(out_means))),
        None => data
            .par_chunks_mut(chunk)
            .zip(mask.par_chunks_mut(chunk))
            .enumerate()
            .for_each(|(i, (out, out_mask))| band(i, out, out_mask, None)),
    }
    let plane = |data| Plane {
        width: w2,
        height: h2,
        data,
    };
    (plane(data), mask, mean_data.map(plane))
}

/// Level 1 of the chroma pyramid straight from the source (the
/// full-resolution chroma plane is never stored) and its measurability:
/// the reference of [`first_levels`].
#[cfg(test)]
fn first_level(source: &Source, map: impl Fn([f32; 3]) -> Px + Sync) -> (Plane, Vec<bool>) {
    let (w, h) = (source.width, source.height);
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    let mut data = vec![[0.0f32; CH]; w2 * h2];
    let mut mask = vec![false; w2 * h2];
    data.par_chunks_mut(w2 * BAND_ROWS)
        .zip(mask.par_chunks_mut(w2 * BAND_ROWS))
        .enumerate()
        .for_each(|(band, (out, out_mask))| {
            let y0 = band * BAND_ROWS;
            let rows = out.len() / w2;
            // Source rows 2·y0 − 2 … 2·(y0 + rows) + 1, horizontally reduced.
            let first = 2 * y0 as isize - 2;
            let count = 2 * rows + 3;
            let mut horizontal = vec![[0.0f32; CH]; count * w2];
            let mut line = vec![[0.0f32; CH]; w];
            for r in 0..count {
                let y = mirror(first + r as isize, h);
                for (x, c) in line.iter_mut().enumerate() {
                    *c = map(source.pixel(x, y));
                }
                for x in 0..w2 {
                    let mut sum = [0.0f32; CH];
                    for (k, weight) in KERNEL.iter().enumerate() {
                        let s = line[mirror(2 * x as isize + k as isize - 2, w)];
                        for c in 0..CH {
                            sum[c] += weight * s[c];
                        }
                    }
                    horizontal[r * w2 + x] = sum;
                }
            }
            for row in 0..rows {
                for x in 0..w2 {
                    let mut sum = [0.0f32; CH];
                    for (k, weight) in KERNEL.iter().enumerate() {
                        let s = horizontal[(2 * row + k) * w2 + x];
                        for c in 0..CH {
                            sum[c] += weight * s[c];
                        }
                    }
                    out[row * w2 + x] = sum;
                    let (sx, sy) = (2 * x, 2 * (y0 + row));
                    out_mask[row * w2 + x] =
                        [(0, 0), (1, 0), (0, 1), (1, 1)].iter().all(|(dx, dy)| {
                            measurable(source.pixel((sx + dx).min(w - 1), (sy + dy).min(h - 1)))
                        });
                }
            }
        });
    (
        Plane {
            width: w2,
            height: h2,
            data,
        },
        mask,
    )
}

/// The smoothest-blocks RMS of a level's detail: `detail(x, y)` at the
/// level's pixels, `measurable(x, y)` at the coarser level's (each block of
/// `BLOCK` pixels covers `BLOCK / 2` of them). Blocks are taken every
/// `stride` blocks. `None` with fewer than `MIN_BLOCKS` measurable blocks.
fn smoothest_rms(
    width: usize,
    height: usize,
    stride: usize,
    detail: impl Fn(usize, usize) -> [f32; 2] + Sync,
    block_measurable: impl Fn(usize, usize) -> bool + Sync,
) -> Option<f32> {
    let (bw, bh) = (width / BLOCK, height / BLOCK);
    let (sw, sh) = (bw.div_ceil(stride), bh.div_ceil(stride));
    let mut energies: Vec<f32> = (0..sw * sh)
        .into_par_iter()
        .filter_map(|i| {
            let (bx, by) = ((i % sw) * stride, (i / sw) * stride);
            if !block_measurable(bx, by) {
                return None;
            }
            let mut sum = 0.0f32;
            for y in by * BLOCK..(by + 1) * BLOCK {
                for x in bx * BLOCK..(bx + 1) * BLOCK {
                    let d = detail(x, y);
                    sum += d[0] * d[0] + d[1] * d[1];
                }
            }
            Some(sum / (BLOCK * BLOCK) as f32)
        })
        .collect();
    if energies.len() < MIN_BLOCKS {
        return None;
    }
    let k = ((energies.len() as f32 * SMOOTHEST) as usize).min(energies.len() - 1);
    let (_, e, _) = energies.select_nth_unstable_by(k, f32::total_cmp);
    Some(e.sqrt())
}

/// The noise in each brightness bin relative to the photo's noise `global`:
/// the smoothest-blocks RMS (as [`smoothest_rms`]) of the blocks whose mean
/// square-root luminance (from `detail_luma`) falls into the bin, divided by
/// `global`. Bins with too few blocks take the nearest measured bin's
/// value; all 1 when no bin could be measured.
fn noise_profile(
    width: usize,
    height: usize,
    stride: usize,
    global: f32,
    detail_luma: impl Fn(usize, usize) -> ([f32; 2], f32) + Sync,
    block_measurable: impl Fn(usize, usize) -> bool + Sync,
) -> [f32; PROFILE_BINS] {
    let mut profile = [1.0f32; PROFILE_BINS];
    if global <= 0.0 {
        return profile;
    }
    let (bw, bh) = (width / BLOCK, height / BLOCK);
    let (sw, sh) = (bw.div_ceil(stride), bh.div_ceil(stride));
    let blocks: Vec<(usize, f32)> = (0..sw * sh)
        .into_par_iter()
        .filter_map(|i| {
            let (bx, by) = ((i % sw) * stride, (i / sw) * stride);
            if !block_measurable(bx, by) {
                return None;
            }
            let (mut sum, mut y) = (0.0f32, 0.0f32);
            for yy in by * BLOCK..(by + 1) * BLOCK {
                for xx in bx * BLOCK..(bx + 1) * BLOCK {
                    let (d, l) = detail_luma(xx, yy);
                    sum += d[0] * d[0] + d[1] * d[1];
                    y += l;
                }
            }
            let n = (BLOCK * BLOCK) as f32;
            let bin = ((y / n).clamp(0.0, 0.999_999) * PROFILE_BINS as f32) as usize;
            Some((bin, sum / n))
        })
        .collect();
    let mut measured = [None; PROFILE_BINS];
    for (bin, slot) in measured.iter_mut().enumerate() {
        let mut energies: Vec<f32> = blocks
            .iter()
            .filter(|(b, _)| *b == bin)
            .map(|(_, e)| *e)
            .collect();
        if energies.len() < MIN_PROFILE_BLOCKS {
            continue;
        }
        let k = ((energies.len() as f32 * SMOOTHEST) as usize).min(energies.len() - 1);
        let (_, e, _) = energies.select_nth_unstable_by(k, f32::total_cmp);
        *slot = Some((e.sqrt() / global).clamp(PROFILE_RANGE.0, PROFILE_RANGE.1));
    }
    if measured.iter().all(Option::is_none) {
        return profile;
    }
    for (bin, value) in profile.iter_mut().enumerate() {
        // The nearest measured bin (the darker one on a tie).
        *value = (0..PROFILE_BINS)
            .filter_map(|b| measured[b].map(|v| (b.abs_diff(bin), v)))
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, v)| v)
            .unwrap();
    }
    profile
}

/// A noise profile's value at square-root luminance `y`: linear between
/// the bin centres, constant beyond the outer ones.
#[inline]
pub fn profile_at(profile: &[f32; PROFILE_BINS], y: f32) -> f32 {
    let t = (y * PROFILE_BINS as f32 - 0.5).clamp(0.0, (PROFILE_BINS - 1) as f32);
    let i = (t as usize).min(PROFILE_BINS - 2);
    let f = t - i as f32;
    profile[i] + f * (profile[i + 1] - profile[i])
}

/// Block stride keeping the measured blocks of a level near this count.
const MEASURED_BLOCKS: usize = 60_000;

fn block_stride(width: usize, height: usize) -> usize {
    let blocks = (width / BLOCK) * (height / BLOCK);
    let mut stride = 1;
    while blocks / (stride * stride) > MEASURED_BLOCKS {
        stride += 1;
    }
    stride
}

/// The chroma pyramid of a render source: levels 1–5 (half resolution
/// down to 1/32) and the colour noise measured at levels 0–4.
#[derive(Debug)]
pub struct ChromaPyramid {
    pub width: usize,
    pub height: usize,
    /// Gaussian levels 1…LEVELS (index 0 = half resolution).
    pub levels: Vec<Plane>,
    /// Colour noise (RMS of the level's detail in square-root chroma units)
    /// at levels 0…LEVELS − 1 as measured; 0 where nothing could be
    /// measured. The filter uses [`ChromaPyramid::noise_for`] its tuning.
    pub noise: [f32; LEVELS],
    /// Luminance noise measured the same way.
    pub luma_noise: [f32; LEVELS],
    /// Colour noise by brightness relative to `noise` (measured at the
    /// finest level, used at every level): demosaiced noise is not even in
    /// square-root encoding (read noise lifts the shadows and midtones),
    /// and the smoothest blocks the photo's noise is measured in are its
    /// cleanest brightness, so midtones would get too low thresholds.
    pub profile: [f32; PROFILE_BINS],
    /// The finest level's luminance noise by brightness (same bins),
    /// relative to its median over the bins: where the photo's luminance
    /// noise departs from the shot-noise model the renderer's luminance
    /// noise reduction assumes (read noise in the shadows).
    pub luma_profile: [f32; PROFILE_BINS],
    /// Whether each level's chroma is that of its linear mean colour
    /// ([`Tuning::linear_means`], version 4) rather than the mean of its
    /// pixels' square-root chroma (versions 2 and 3).
    pub linear_means: bool,
    /// The last tuning's prepared levels ([`ChromaPyramid::prepared`]).
    prepared: Mutex<Option<Arc<Prepared>>>,
}

impl Clone for ChromaPyramid {
    fn clone(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            levels: self.levels.clone(),
            noise: self.noise,
            luma_noise: self.luma_noise,
            profile: self.profile,
            luma_profile: self.luma_profile,
            linear_means: self.linear_means,
            prepared: Mutex::new(self.cached_prepared()),
        }
    }
}

impl ChromaPyramid {
    /// Builds the pyramid of `image` (linear RGB when `linear`, otherwise
    /// sRGB-encoded) and measures its colour noise. `None` for images too
    /// small to filter.
    pub fn build(image: &DynamicImage, linear: bool) -> Option<Self> {
        Self::build_with(image, linear, false)
    }

    /// [`ChromaPyramid::build`] for the filter of `tuning`
    /// ([`Tuning::linear_means`]).
    pub fn build_for(image: &DynamicImage, linear: bool, tuning: &Tuning) -> Option<Self> {
        Self::build_with(image, linear, tuning.linear_means)
    }

    /// `linear_means`: each level's chroma from its linear mean colour
    /// ([`ChromaPyramid::linear_means`]).
    pub fn build_with(image: &DynamicImage, linear: bool, linear_means: bool) -> Option<Self> {
        let source = Source::new(image, linear);
        let (w, h) = (source.width, source.height);
        if w < 2 * BLOCK || h < 2 * BLOCK {
            return None;
        }
        let (g1, mask1, means) = first_levels(&source, linear_means);
        let mut levels = vec![g1];
        let mut masks = vec![mask1];
        for _ in 1..LEVELS {
            let previous = levels.last().unwrap();
            let next = reduce(previous);
            let mask = reduce_mask(masks.last().unwrap(), previous.width, previous.height);
            levels.push(next);
            masks.push(mask);
        }
        if let Some(mut means) = means {
            // Each level's chroma from the level's linear mean colour, so a
            // level without its finer detail keeps the mean colour of the
            // pixels it covers; its luminance channel stays the mean of the
            // encoded luminance (the luminance noise and profile, which the
            // luminance noise reduction uses, are unchanged).
            for (l, level) in levels.iter_mut().enumerate() {
                if l > 0 {
                    means = reduce(&means);
                }
                level
                    .data
                    .par_iter_mut()
                    .zip(means.data.par_iter())
                    .for_each(|(v, rgb)| {
                        let [a, b] = chroma(*rgb);
                        v[0] = a;
                        v[1] = b;
                    });
            }
        }

        // Chroma (channels 0, 1) and luminance (channel 2) noise per level.
        let measure = |channels: std::ops::Range<usize>| -> [f32; LEVELS] {
            let pick = |d: Px| -> [f32; 2] {
                let mut out = [0.0f32; 2];
                for (o, c) in out.iter_mut().zip(channels.clone()) {
                    *o = d[c];
                }
                out
            };
            let mut measured = [None; LEVELS];
            // Level 0: the source minus level 1 expanded; a block is
            // measurable when the level-1 pixels it covers are.
            let g1 = &levels[0];
            measured[0] = smoothest_rms(
                w,
                h,
                block_stride(w, h),
                |x, y| {
                    let c = chroma_luma(source.pixel(x, y));
                    let e = expand_at(g1, x, y);
                    pick(std::array::from_fn(|k| c[k] - e[k]))
                },
                |bx, by| block_covered(&masks[0], g1.width, g1.height, bx, by),
            );
            for l in 1..LEVELS {
                let (fine, coarse) = (&levels[l - 1], &levels[l]);
                measured[l] = smoothest_rms(
                    fine.width,
                    fine.height,
                    block_stride(fine.width, fine.height),
                    |x, y| {
                        let c = fine.at(x, y);
                        let e = expand_at(coarse, x, y);
                        pick(std::array::from_fn(|k| c[k] - e[k]))
                    },
                    |bx, by| block_covered(&masks[l], coarse.width, coarse.height, bx, by),
                );
            }
            let mut noise = [0.0f32; LEVELS];
            for l in 0..LEVELS {
                noise[l] = match (measured[l], l) {
                    (Some(n), 0) => n,
                    // Demosaiced colour noise is never larger at a coarser
                    // level: more than the finer level's is colour detail.
                    (Some(n), _) => n.min(noise[l - 1]),
                    (None, 0) => 0.0,
                    (None, _) => noise[l - 1] * COARSER_FALLBACK,
                };
            }
            noise
        };
        let noise = measure(0..2);
        let luma_noise = measure(2..3);
        let g1 = &levels[0];
        let profile = noise_profile(
            w,
            h,
            block_stride(w, h),
            noise[0],
            |x, y| {
                let c = chroma_luma(source.pixel(x, y));
                let e = expand_at(g1, x, y);
                ([c[0] - e[0], c[1] - e[1]], c[2])
            },
            |bx, by| block_covered(&masks[0], g1.width, g1.height, bx, by),
        );
        let mut luma_profile = noise_profile(
            w,
            h,
            block_stride(w, h),
            luma_noise[0],
            |x, y| {
                let c = chroma_luma(source.pixel(x, y));
                ([c[2] - expand_at(g1, x, y)[2], 0.0], c[2])
            },
            |bx, by| block_covered(&masks[0], g1.width, g1.height, bx, by),
        );
        let mut sorted = luma_profile;
        sorted.sort_by(f32::total_cmp);
        let middle = 0.5 * (sorted[PROFILE_BINS / 2 - 1] + sorted[PROFILE_BINS / 2]);
        if middle > 0.0 {
            luma_profile.iter_mut().for_each(|v| *v /= middle);
        }
        Some(Self {
            width: w,
            height: h,
            levels,
            noise,
            luma_noise,
            profile,
            luma_profile,
            linear_means,
            prepared: Mutex::new(None),
        })
    }

    /// The colour noise each level is filtered by with `tuning`
    /// ([`Tuning::level_noise`] of the measured `noise`).
    pub fn noise_for(&self, tuning: &Tuning) -> [f32; LEVELS] {
        tuning.level_noise(&self.noise)
    }

    /// Level 1 rebuilt with each level's detail kept by its threshold
    /// (`thresholds` from [`Tuning::thresholds`]) on the coarsest level
    /// smoothed by `base_sigma` ([`Tuning::base_sigma`]); level 1 itself
    /// when all thresholds of the coarse levels are zero.
    pub fn reconstruct(
        &self,
        thresholds: &[f32; LEVELS],
        base_sigma: f32,
        tuning: &Tuning,
    ) -> Plane {
        if thresholds[1..].iter().all(|t| *t <= 0.0) {
            return self.levels[0].clone();
        }
        let prepared = self.prepared(tuning);
        let steepness = tuning.steepness;
        let noise = self.noise_for(tuning);
        // Levels LEVELS − 1 … 1 are rebuilt on the coarsest one.
        let mut rebuilt = self.coarsest(base_sigma, tuning);
        for l in (1..LEVELS).rev() {
            let fine = &self.levels[l - 1];
            let coarse = &self.levels[l];
            let level = &prepared.levels[l - 1];
            let t2 = thresholds[l] * thresholds[l];
            let edge2 = tuning.edge_scale2(l, thresholds[l], &noise);
            let aware_inv2 = tuning.aware_inv2(l, thresholds, &noise);
            let mut data = vec![[0.0f32; CH]; fine.width * fine.height];
            data.par_chunks_mut(fine.width).enumerate().for_each_init(
                AwareRow::default,
                |scratch, (y, row)| {
                    let line = &fine.data[y * fine.width..(y + 1) * fine.width];
                    if aware_inv2 > 0.0 {
                        // The detail against the same weighted expansion
                        // of the unfiltered coarser level.
                        expand_aware_row(
                            &rebuilt,
                            coarse,
                            y,
                            fine.width,
                            |x| {
                                if level.guide.is_empty() {
                                    [line[x][0], line[x][1]]
                                } else {
                                    level.guide[y * fine.width + x]
                                }
                            },
                            aware_inv2,
                            tuning.aware_far,
                            Some(&self.profile),
                            scratch,
                        );
                    }
                    for (x, v) in row.iter_mut().enumerate() {
                        let i = y * fine.width + x;
                        let k = level_keep(level.size2[i], t2, level.edge[i], edge2, steepness);
                        *v = if aware_inv2 > 0.0 {
                            let (r, c, f) = (scratch.rebuilt[x], scratch.coarse[x], line[x]);
                            rebuilt_detail(
                                [r[0], r[1], 0.0],
                                [f[0] - c[0], f[1] - c[1]],
                                level.follow[i],
                                k,
                            )
                        } else {
                            rebuilt_detail(
                                expand_at(&rebuilt, x, y),
                                level.detail[i],
                                level.follow[i],
                                k,
                            )
                        };
                    }
                },
            );
            rebuilt = Plane {
                width: fine.width,
                height: fine.height,
                data,
            };
        }
        rebuilt
    }

    /// The coarsest level, smoothed by a bilateral filter of `sigma`
    /// samples (none at 0) whose range is `base_range` times its noise.
    pub fn coarsest(&self, sigma: f32, tuning: &Tuning) -> Plane {
        let base = &self.levels[LEVELS - 1];
        let range = tuning.base_range * self.noise_for(tuning)[LEVELS - 1];
        if sigma <= 0.0 || range <= 0.0 {
            return base.clone();
        }
        let r = (2.0 * sigma).ceil() as isize;
        let (w, h) = (base.width, base.height);
        let inv_s = 1.0 / (2.0 * sigma * sigma);
        let inv_r = 1.0 / (2.0 * range * range);
        let mut data = vec![[0.0f32; CH]; w * h];
        data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                let c = base.at(x, y);
                let (mut sa, mut sb, mut sw) = (0.0f32, 0.0f32, 0.0f32);
                for dy in -r..=r {
                    for dx in -r..=r {
                        let q = base.at(mirror(x as isize + dx, w), mirror(y as isize + dy, h));
                        let (da, db) = (q[0] - c[0], q[1] - c[1]);
                        let wq = (-((dx * dx + dy * dy) as f32) * inv_s
                            - (da * da + db * db) * inv_r)
                            .exp();
                        sa += wq * q[0];
                        sb += wq * q[1];
                        sw += wq;
                    }
                }
                *v = [sa / sw, sb / sw, c[2]];
            }
        });
        Plane {
            width: w,
            height: h,
            data,
        }
    }

    /// What does not depend on the sliders, prepared once per source and
    /// tuning (the last one is kept): each coarse level's detail, local
    /// energy and guards.
    pub fn prepared(&self, tuning: &Tuning) -> Arc<Prepared> {
        if let Some(prepared) = self.cached_prepared()
            && prepared.tuning == *tuning
        {
            return prepared;
        }
        let computed = Arc::new(self.prepare(tuning));
        *self
            .prepared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&computed));
        computed
    }

    fn cached_prepared(&self) -> Option<Arc<Prepared>> {
        self.prepared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Each coarse level's detail, its local energy and its guards.
    fn prepare(&self, tuning: &Tuning) -> Prepared {
        let noise = self.noise_for(tuning);
        let levels = (1..LEVELS)
            .map(|l| {
                let fine = &self.levels[l - 1];
                let coarse = &self.levels[l];
                let (w, h) = (fine.width, fine.height);
                let luma2 = (tuning.luma_guard * self.luma_noise[l]).powi(2);
                let edge2 = (tuning.edge_guard * noise[l]).powi(2);
                let gradient = (edge2 > 0.0).then(|| coherent_gradient(coarse));
                let steepness = tuning.steepness;
                let mut detail = vec![[0.0f32; 2]; w * h];
                let mut luma = vec![0.0f32; w * h];
                let mut edge = vec![0.0f32; w * h];
                let mut relative2 = vec![1.0f32; w * h];
                detail
                    .par_iter_mut()
                    .zip(luma.par_iter_mut())
                    .zip(edge.par_iter_mut())
                    .zip(relative2.par_iter_mut())
                    .enumerate()
                    .for_each(|(i, (((d, dl), e), p2))| {
                        let (x, y) = (i % w, i / w);
                        let f = fine.at(x, y);
                        let c = expand_at(coarse, x, y);
                        *d = [f[0] - c[0], f[1] - c[1]];
                        *dl = f[2] - c[2];
                        if let Some(gradient) = &gradient {
                            *e = expand_at(gradient, x, y)[0] / edge2;
                        }
                        // The colour noise at the coarser level's brightness.
                        let p = profile_at(&self.profile, c[2]);
                        *p2 = p * p;
                    });
                // The size of the detail: its local energy (3 × 3, weights
                // 1-2-1), so isolated noise peaks go and an edge's tails stay;
                // in units of the colour noise at the pixel's brightness.
                let mut size2 = local_energy(&detail, w, h);
                size2
                    .par_iter_mut()
                    .zip(relative2.par_iter())
                    .for_each(|(s, p2)| *s /= p2);
                // Colour edges mostly come with a luminance edge: where the
                // level's luminance detail stands out of its noise, the
                // chroma detail that follows it (a local regression of the
                // chroma on the luminance detail, regularised by the
                // luminance noise) stays.
                let regular = 25.0 * self.luma_noise[l].powi(2);
                let follow = (0..w * h)
                    .into_par_iter()
                    .map(|i| {
                        if luma2 <= 0.0 {
                            return [0.0; 2];
                        }
                        let (x, y) = (i % w, i / w);
                        let (mut sa, mut sb, mut sl) = (0.0f32, 0.0f32, 0.0f32);
                        for yy in y.saturating_sub(2)..(y + 3).min(h) {
                            for xx in x.saturating_sub(2)..(x + 3).min(w) {
                                let j = yy * w + xx;
                                sa += detail[j][0] * luma[j];
                                sb += detail[j][1] * luma[j];
                                sl += luma[j] * luma[j];
                            }
                        }
                        let dl = luma[i];
                        let share =
                            gain(dl * dl, luma2, steepness) * dl / (sl + regular).max(1e-30);
                        [sa * share, sb * share]
                    })
                    .collect();
                let guide = if tuning.guide_coarse && tuning.guide != Guide::Own {
                    let mut guide = guide_plane(
                        w,
                        h,
                        |x, y| {
                            let f = fine.at(x, y);
                            [f[0], f[1]]
                        },
                        tuning.guide,
                        noise[l - 1] * noise[l - 1],
                    );
                    if tuning.guide_gate && gradient.is_some() {
                        // The chroma-edge guard's gain at Amount 25.
                        guide.par_iter_mut().enumerate().for_each(|(i, g)| {
                            let k = gain(edge[i], 1.0, steepness);
                            let f = fine.data[i];
                            *g = [g[0] + k * (f[0] - g[0]), g[1] + k * (f[1] - g[1])];
                        });
                    }
                    guide
                } else {
                    Vec::new()
                };
                PreparedLevel {
                    width: w,
                    height: h,
                    detail,
                    size2,
                    edge,
                    follow,
                    guide,
                }
            })
            .collect();
        Prepared {
            tuning: *tuning,
            levels,
        }
    }
}

/// The share of a coarse level's detail kept whole: by its threshold or
/// by the chroma-edge guard (`edge` and `edge2` from [`PreparedLevel::edge`]
/// and [`Tuning::edge_scale2`]).
#[inline]
pub fn level_keep(size2: f32, threshold2: f32, edge: f32, edge2: f32, steepness: f32) -> f32 {
    gain(size2, threshold2, steepness).max(gain(edge, edge2, steepness))
}

/// A rebuilt level's pixel: the coarser level expanded (`r`), the share
/// `keep` of the detail and the rest of the luminance-following part.
#[inline]
pub fn rebuilt_detail(r: Px, detail: [f32; 2], follow: [f32; 2], keep: f32) -> Px {
    let rest = 1.0 - keep;
    [
        r[0] + keep * detail[0] + rest * follow[0],
        r[1] + keep * detail[1] + rest * follow[1],
        0.0,
    ]
}

/// A pyramid's coarse levels ready for new thresholds (levels 1…LEVELS − 1,
/// index 0 = level 1). Rebuilding level l: expand(rebuilt l + 1) + k ·
/// detail + (1 − k) · follow, k = [`level_keep`].
#[derive(Debug)]
pub struct Prepared {
    pub tuning: Tuning,
    pub levels: Vec<PreparedLevel>,
}

#[derive(Debug)]
pub struct PreparedLevel {
    pub width: usize,
    pub height: usize,
    /// The level's chroma detail (level minus the coarser one expanded).
    pub detail: Vec<[f32; 2]>,
    /// Its local energy (3 × 3, weights 1-2-1) over the squared noise
    /// profile at the pixel's brightness.
    pub size2: Vec<f32>,
    /// The coarser level's coherent squared chroma gradient over the
    /// squared chroma-edge guard threshold at Amount 25.
    pub edge: Vec<f32>,
    /// The part of the detail that follows the luminance detail, where the
    /// luminance guard holds (zero elsewhere).
    pub follow: Vec<[f32; 2]>,
    /// The guide of the aware expansion rebuilding this level
    /// ([`Tuning::guide`] with `guide_coarse`); empty for the level's own
    /// chroma.
    pub guide: Vec<[f32; 2]>,
}

/// Median of nine values (Devillard's 19-exchange network; the renderer's
/// `ms_median9` runs the same exchanges, so both agree exactly).
#[inline]
fn median9(mut p: [f32; 9]) -> f32 {
    #[inline(always)]
    fn sort(p: &mut [f32; 9], a: usize, b: usize) {
        let (lo, hi) = (p[a].min(p[b]), p[a].max(p[b]));
        p[a] = lo;
        p[b] = hi;
    }
    for (a, b) in [
        (1, 2),
        (4, 5),
        (7, 8),
        (0, 1),
        (3, 4),
        (6, 7),
        (1, 2),
        (4, 5),
        (7, 8),
        (0, 3),
        (5, 8),
        (4, 7),
        (3, 6),
        (1, 4),
        (2, 5),
        (4, 7),
        (4, 2),
        (6, 4),
        (4, 2),
    ] {
        sort(&mut p, a, b);
    }
    p[4]
}

/// The guide of one sample from its 3 × 3 chroma `n` (row by row, centre
/// `n[4]`) for `guide` ([`Guide`]); `noise2` is the squared colour noise the
/// switch compares with.
#[inline]
pub fn guide_of(n: &[[f32; 2]; 9], guide: Guide, noise2: f32) -> [f32; 2] {
    match guide {
        Guide::Own => n[4],
        Guide::Mean => {
            const W: [f32; 9] = [1.0, 2.0, 1.0, 2.0, 4.0, 2.0, 1.0, 2.0, 1.0];
            let mut sum = [0.0f32; 2];
            for (v, w) in n.iter().zip(W) {
                sum[0] += w * v[0];
                sum[1] += w * v[1];
            }
            [sum[0] / 16.0, sum[1] / 16.0]
        }
        Guide::Median | Guide::Switch(_) => {
            let median = [
                median9(std::array::from_fn(|i| n[i][0])),
                median9(std::array::from_fn(|i| n[i][1])),
            ];
            match guide {
                Guide::Switch(k) => {
                    let (da, db) = (n[4][0] - median[0], n[4][1] - median[1]);
                    if da * da + db * db > k * k * noise2 {
                        median
                    } else {
                        n[4]
                    }
                }
                _ => median,
            }
        }
    }
}

/// [`guide_of`] for every sample of a `width` × `height` plane of chroma
/// `at(x, y)` (borders clamped, as the renderer's loads are).
pub fn guide_plane(
    width: usize,
    height: usize,
    at: impl Fn(usize, usize) -> [f32; 2] + Sync,
    guide: Guide,
    noise2: f32,
) -> Vec<[f32; 2]> {
    (0..width * height)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % width, i / width);
            let n: [[f32; 2]; 9] = std::array::from_fn(|k| {
                let (dx, dy) = ((k % 3) as isize - 1, (k / 3) as isize - 1);
                at(
                    (x as isize + dx).clamp(0, width as isize - 1) as usize,
                    (y as isize + dy).clamp(0, height as isize - 1) as usize,
                )
            });
            guide_of(&n, guide, noise2)
        })
        .collect()
}

/// The chroma each pixel's finest detail is measured from with
/// [`Tuning::speck_swap`] `k` (in noise units, `noise2` the squared noise):
/// the 3 × 3 median chroma for an isolated off-colour pixel (chroma more
/// than `k` σ from that median, luminance within [`SPECK_LUMA`] σ, faded
/// out over ± ½ σ around it), its own
/// otherwise (renderer `apply_color_noise_multiscale`, code bit 64).
pub fn speck_plane(
    width: usize,
    height: usize,
    at: impl Fn(usize, usize) -> Px + Sync,
    k: f32,
    noise2: f32,
    strength: f32,
) -> Vec<[f32; 2]> {
    let sigma = noise2.sqrt();
    let limit = k * sigma;
    (0..width * height)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % width, i / width);
            let n: [Px; 9] = std::array::from_fn(|k| {
                let (dx, dy) = ((k % 3) as isize - 1, (k / 3) as isize - 1);
                at(
                    (x as isize + dx).clamp(0, width as isize - 1) as usize,
                    (y as isize + dy).clamp(0, height as isize - 1) as usize,
                )
            });
            let median: Px = std::array::from_fn(|c| median9(std::array::from_fn(|i| n[i][c])));
            let c0 = n[4];
            let (da, db) = (c0[0] - median[0], c0[1] - median[1]);
            let dc = (da * da + db * db).sqrt();
            // Swapped fully within half a σ of SPECK_LUMA σ in luminance,
            // fading to none half a σ beyond it: a hard cut would flip large
            // chroma outliers between two colours on a tiny luminance change.
            let swap =
                strength * (SPECK_LUMA + 0.5 - (c0[2] - median[2]).abs() / sigma).clamp(0.0, 1.0);
            if dc > limit && swap > 0.0 {
                [
                    c0[0] + swap * (median[0] - c0[0]),
                    c0[1] + swap * (median[1] - c0[1]),
                ]
            } else {
                [c0[0], c0[1]]
            }
        })
        .collect()
}

/// The squared chroma gradient (central differences) of a level times the
/// coherence of the chroma structure tensor over 5 × 5 (1 where all
/// gradients share one direction, 0 where they point every way): edges
/// count, round blotches do not.
fn coherent_gradient(plane: &Plane) -> Plane {
    const RADIUS: usize = 2;
    let (w, h) = (plane.width, plane.height);
    let tensor: Vec<[f32; 3]> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let l = plane.at(x.saturating_sub(1), y);
            let r = plane.at((x + 1).min(w - 1), y);
            let u = plane.at(x, y.saturating_sub(1));
            let d = plane.at(x, (y + 1).min(h - 1));
            let gx = [0.5 * (r[0] - l[0]), 0.5 * (r[1] - l[1])];
            let gy = [0.5 * (d[0] - u[0]), 0.5 * (d[1] - u[1])];
            [
                gx[0] * gx[0] + gx[1] * gx[1],
                gx[0] * gy[0] + gx[1] * gy[1],
                gy[0] * gy[0] + gy[1] * gy[1],
            ]
        })
        .collect();
    let data = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let (mut xx, mut xy, mut yy) = (0.0f32, 0.0f32, 0.0f32);
            for ty in y.saturating_sub(RADIUS)..(y + RADIUS + 1).min(h) {
                for tx in x.saturating_sub(RADIUS)..(x + RADIUS + 1).min(w) {
                    let t = tensor[ty * w + tx];
                    xx += t[0];
                    xy += t[1];
                    yy += t[2];
                }
            }
            let trace = xx + yy;
            let coherence = if trace > 0.0 {
                ((xx - yy).powi(2) + 4.0 * xy * xy) / (trace * trace)
            } else {
                0.0
            };
            [(tensor[i][0] + tensor[i][2]) * coherence, 0.0, 0.0]
        })
        .collect();
    Plane {
        width: w,
        height: h,
        data,
    }
}

/// Squared size of each detail value: the 1-2-1 weighted mean of the
/// squared detail over its 3 × 3 neighbourhood (borders clamped), as the
/// renderer computes it at the finest level.
fn local_energy(detail: &[[f32; 2]], width: usize, height: usize) -> Vec<f32> {
    let own: Vec<f32> = detail.iter().map(|d| d[0] * d[0] + d[1] * d[1]).collect();
    const W: [f32; 3] = [0.25, 0.5, 0.25];
    (0..width * height)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % width, i / width);
            let mut sum = 0.0;
            for (j, wy) in W.iter().enumerate() {
                let yy = (y + j).saturating_sub(1).min(height - 1);
                for (k, wx) in W.iter().enumerate() {
                    let xx = (x + k).saturating_sub(1).min(width - 1);
                    sum += wy * wx * own[yy * width + xx];
                }
            }
            sum
        })
        .collect()
}

/// Whether every coarser pixel under block (bx, by) of the finer level is
/// measurable.
fn block_covered(mask: &[bool], width: usize, height: usize, bx: usize, by: usize) -> bool {
    let half = BLOCK / 2;
    (by * half..((by + 1) * half).min(height))
        .all(|y| (bx * half..((bx + 1) * half).min(width)).all(|x| mask[y * width + x]))
}

/// The squared noise of a finest-level pixel relative to the photo's: the
/// colour profile at the pixel's own square-root luminance (the renderer's
/// `finest_energy` does the same).
#[inline]
pub fn finest_relative2(profile: &[f32; PROFILE_BINS], rgb: [f32; 3]) -> f32 {
    let p = profile_at(profile, chroma_luma(rgb)[2]);
    p * p
}

/// The filtered colour of one linear pixel: its chroma `c0` is replaced by
/// `expand(rebuilt) + gain · (c0 − expand(level 1))` and its luminance kept,
/// as the renderer does it.
#[inline]
pub fn filtered_pixel(rgb: [f32; 3], level1: Px, rebuilt: Px, g: f32) -> [f32; 3] {
    filtered_pixel_from(rgb, level1, rebuilt, g, chroma(rgb))
}

/// [`filtered_pixel`] with the detail taken from chroma `c` instead of the
/// pixel's own ([`Tuning::speck_swap`]).
#[inline]
pub fn filtered_pixel_from(
    rgb: [f32; 3],
    level1: Px,
    rebuilt: Px,
    g: f32,
    c0: [f32; 2],
) -> [f32; 3] {
    let p = rgb.map(encode);
    let lp = LUMA[0] * p[0] + LUMA[1] * p[1] + LUMA[2] * p[2];
    let d = [c0[0] - level1[0], c0[1] - level1[1]];
    let a = rebuilt[0] + g * d[0];
    let b = rebuilt[1] + g * d[1];
    with_chroma(rgb, lp, a, b)
}

/// [`filtered_pixel_from`] with the filtered chroma passed through `keep`
/// ([`keep_in_range`]).
#[inline]
pub fn filtered_pixel_in_range(
    rgb: [f32; 3],
    level1: Px,
    rebuilt: Px,
    g: f32,
    c0: [f32; 2],
    keep: impl Fn([f32; 2]) -> [f32; 2],
) -> [f32; 3] {
    let p = rgb.map(encode);
    let lp = LUMA[0] * p[0] + LUMA[1] * p[1] + LUMA[2] * p[2];
    let ab = keep(std::array::from_fn(|c| {
        rebuilt[c] + g * (c0[c] - level1[c])
    }));
    with_chroma(rgb, lp, ab[0], ab[1])
}

/// The range a pixel's filtered chroma is kept in ([`Tuning::range_clamp`]):
/// per component the lowest and highest of the plain expansion of
/// `rebuilt` at (x, y) and its 8 neighbours (clamped to the `w` × `h`
/// image) and of the pixel's own reconstruction `own` (its edge-aware one;
/// with [`Tuning::range_plain`] its plain one, which adds nothing; renderer
/// `ms_rebuilt_range`).
pub fn rebuilt_range(
    rebuilt: &Plane,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    own: Px,
) -> ([f32; 2], [f32; 2]) {
    let (mut lo, mut hi) = ([own[0], own[1]], [own[0], own[1]]);
    for dy in -1..=1isize {
        for dx in -1..=1isize {
            let e = expand_at(
                rebuilt,
                (x as isize + dx).clamp(0, w as isize - 1) as usize,
                (y as isize + dy).clamp(0, h as isize - 1) as usize,
            );
            for c in 0..2 {
                lo[c] = lo[c].min(e[c]);
                hi[c] = hi[c].max(e[c]);
            }
        }
    }
    (lo, hi)
}

/// Chroma `ab` kept within `range` widened by `margin` ([`Tuning::range_clamp`]),
/// by `strength`, fading out while its farthest component lies
/// [`RANGE_CLAMP_FAR`] noise `sigma` beyond the range (renderer
/// `apply_color_noise_multiscale`, code bits 12–15).
pub fn keep_in_range(
    ab: [f32; 2],
    range: ([f32; 2], [f32; 2]),
    margin: f32,
    sigma: f32,
    strength: f32,
) -> [f32; 2] {
    let (lo, hi) = range;
    let beyond = (0..2)
        .map(|c| (lo[c] - ab[c]).max(ab[c] - hi[c]).max(0.0))
        .fold(0.0f32, f32::max);
    let [f0, f1] = RANGE_CLAMP_FAR;
    let t = ((beyond / sigma - f0) / (f1 - f0)).clamp(0.0, 1.0);
    let share = strength * (1.0 - t * t * (3.0 - 2.0 * t));
    std::array::from_fn(|c| {
        let kept = ab[c].clamp(lo[c] - margin, hi[c] + margin);
        ab[c] * (1.0 - share) + kept * share
    })
}

/// The filtered chroma `ab` of pixel (x, y) of a `w` × `h` image kept in its
/// range
/// ([`Tuning::range_clamp`], [`rebuilt_range`], [`keep_in_range`]): the
/// range holds the pixel's own edge-aware reconstruction `aware`, or with
/// [`Tuning::range_plain`] its plain one (one of the 3 × 3 already). Chroma
/// within `margin` of that lies in the widened range already and stays
/// (renderer `apply_color_noise_multiscale`).
#[allow(clippy::too_many_arguments)]
pub fn pixel_in_range(
    ab: [f32; 2],
    rebuilt: &Plane,
    (x, y, w, h): (usize, usize, usize, usize),
    aware: Px,
    tuning: &Tuning,
    margin: f32,
    sigma: f32,
    strength: f32,
) -> [f32; 2] {
    let own = if tuning.range_plain {
        expand_at(rebuilt, x, y)
    } else {
        aware
    };
    if (ab[0] - own[0]).abs().max((ab[1] - own[1]).abs()) <= margin {
        return ab;
    }
    let range = rebuilt_range(rebuilt, x, y, w, h, own);
    keep_in_range(ab, range, margin, sigma, strength)
}

/// [`filtered_pixel`] with the edge-kept detail from the guide
/// ([`Tuning::guide_edge`]): the detail kept by the energy gain `g` is the
/// pixel's own, the further share `edge` kept only by the finest edge guard
/// is that of the guide chroma `guide` (renderer
/// `apply_color_noise_multiscale`).
pub fn filtered_pixel_guided(
    rgb: [f32; 3],
    level1: Px,
    rebuilt: Px,
    g: f32,
    edge: f32,
    guide: [f32; 2],
) -> [f32; 3] {
    let p = rgb.map(encode);
    let lp = LUMA[0] * p[0] + LUMA[1] * p[1] + LUMA[2] * p[2];
    let c0 = [p[0] - p[1], p[2] - p[1]];
    let a = rebuilt[0] + g * (c0[0] - level1[0]) + edge * (guide[0] - level1[0]);
    let b = rebuilt[1] + g * (c0[1] - level1[1]) + edge * (guide[1] - level1[1]);
    with_chroma(rgb, lp, a, b)
}

/// The pixel `rgb` with square-root chroma (a, b) on its encoded luminance
/// `lp`, its linear luminance kept.
#[inline]
fn with_chroma(rgb: [f32; 3], lp: f32, a: f32, b: f32) -> [f32; 3] {
    // Chroma with zero luminance from the two differences.
    let pg = -(LUMA[0] * a + LUMA[2] * b);
    let q = [lp + pg + a, lp + pg, lp + pg + b];
    let mut out = q.map(decode);
    let luma = LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2];
    let new_luma = LUMA[0] * out[0] + LUMA[1] * out[1] + LUMA[2] * out[2];
    if new_luma > 1e-6 {
        let k = luma / new_luma;
        out = out.map(|v| v * k);
    }
    out
}

/// The whole filter on the CPU: `image` (linear RGB, 3 channels) with its
/// colour noise reduced by `settings`; the image unchanged at Amount 0.
pub fn apply_to_linear(
    image: &image::Rgb32FImage,
    settings: Settings,
    tuning: &Tuning,
) -> image::Rgb32FImage {
    let dynamic = DynamicImage::ImageRgb32F(image.clone());
    let Some(pyramid) = ChromaPyramid::build_for(&dynamic, true, tuning) else {
        return image.clone();
    };
    let thresholds = tuning.thresholds(settings, &pyramid.noise_for(tuning));
    if thresholds.iter().all(|t| *t <= 0.0) {
        return image.clone();
    }
    apply_with(
        image,
        &pyramid,
        &thresholds,
        tuning.base_sigma(settings),
        tuning,
    )
}

/// [`apply_to_linear`] with a built pyramid and thresholds.
pub fn apply_with(
    image: &image::Rgb32FImage,
    pyramid: &ChromaPyramid,
    thresholds: &[f32; LEVELS],
    base_sigma: f32,
    tuning: &Tuning,
) -> image::Rgb32FImage {
    let steepness = tuning.steepness;
    let rebuilt = pyramid.reconstruct(thresholds, base_sigma, tuning);
    let noise = pyramid.noise_for(tuning);
    let aware2 = tuning.finest_aware2(thresholds, &noise);
    let level1 = &pyramid.levels[0];
    let (w, h) = (image.width() as usize, image.height() as usize);
    let src = image.as_raw();
    let pixel = |i: usize| [src[3 * i], src[3 * i + 1], src[3 * i + 2]];
    let detail: Vec<[f32; 2]> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let c = chroma(pixel(i));
            let e = expand_at(level1, i % w, i / w);
            [c[0] - e[0], c[1] - e[1]]
        })
        .collect();
    let mut size2 = local_energy(&detail, w, h);
    size2.par_iter_mut().enumerate().for_each(|(i, s)| {
        *s /= finest_relative2(&pyramid.profile, pixel(i));
    });
    let t2 = thresholds[0] * thresholds[0];
    let edge2 = tuning.finest_edge2(thresholds, &noise);
    // The finest guide as the renderer takes it (`color_noise_guide`).
    let (_, k2) = tuning.finest_guide_code();
    let guide = (tuning.guide_finest && tuning.guide != Guide::Own && aware2 > 0.0).then(|| {
        let mode = match tuning.guide {
            Guide::Switch(_) => Guide::Switch(k2.sqrt()),
            other => other,
        };
        guide_plane(w, h, |x, y| chroma(pixel(y * w + x)), mode, aware2)
    });
    let guide_edge = guide.is_some() && tuning.guide_edge;
    let guide_gate = guide.is_some() && tuning.guide_gate;
    let guide_detail = guide.is_some() && tuning.guide_detail;
    // Version 7 (renderer: code bit 64).
    let strength = tuning.speck_strength(thresholds, &noise);
    let speck = (strength > 0.0 && aware2 > 0.0).then(|| {
        let k = (tuning.speck_swap * 4.0).round().min(15.0) * 0.25;
        speck_plane(
            w,
            h,
            |x, y| chroma_luma(pixel(y * w + x)),
            k,
            aware2,
            strength,
        )
    });
    // Version 9 (renderer: code bits 12–15).
    let range_margin = (strength > 0.0 && aware2 > 0.0 && tuning.range_clamp > 0.0)
        .then(|| tuning.range_clamp_steps() * 0.25 * aware2.sqrt());
    let sigma = aware2.sqrt();
    let mut out = vec![0.0f32; w * h * 3];
    out.par_chunks_mut(w * 3)
        .enumerate()
        .for_each_init(AwareRow::default, |scratch, (y, row)| {
            // Only the reconstruction expanded aware (renderer
            // `ms_expand_aware`); the detail stays against level 1's plain
            // expansion.
            if aware2 > 0.0 {
                expand_aware_row(
                    &rebuilt,
                    level1,
                    y,
                    w,
                    |x| match &guide {
                        Some(guide) if guide_gate && edge2 > 0.0 => {
                            let k = gain(level1_gradient2(&rebuilt, x, y), edge2, steepness);
                            let (g, c) = (guide[y * w + x], chroma(pixel(y * w + x)));
                            [g[0] * (1.0 - k) + c[0] * k, g[1] * (1.0 - k) + c[1] * k]
                        }
                        Some(guide) => guide[y * w + x],
                        None => chroma(pixel(y * w + x)),
                    },
                    1.0 / aware2,
                    tuning.aware_far,
                    None,
                    scratch,
                );
            }
            for x in 0..w {
                let i = y * w + x;
                let rgb = pixel(i);
                let rebuilt_at = if aware2 > 0.0 {
                    let r = scratch.rebuilt[x];
                    [r[0], r[1], 0.0]
                } else {
                    expand_at(&rebuilt, x, y)
                };
                // Version 6: the detail against the same weighted expansion
                // of level 1 as the reconstruction.
                let level1_at = if aware2 > 0.0 && tuning.finest_detail_aware {
                    let c = scratch.coarse[x];
                    [c[0], c[1], 0.0]
                } else {
                    expand_at(level1, x, y)
                };
                if guide_detail && let Some(guide) = &guide {
                    let g = if edge2 > 0.0 {
                        gain(size2[i], t2, steepness).max(gain(
                            level1_gradient2(&rebuilt, x, y),
                            edge2,
                            steepness,
                        ))
                    } else {
                        gain(size2[i], t2, steepness)
                    };
                    let v = filtered_pixel_guided(rgb, level1_at, rebuilt_at, 0.0, g, guide[i]);
                    row[x * 3..x * 3 + 3].copy_from_slice(&v);
                    continue;
                }
                if guide_edge && let Some(guide) = &guide {
                    let g = gain(size2[i], t2, steepness);
                    let edge = if edge2 > 0.0 {
                        gain(level1_gradient2(&rebuilt, x, y), edge2, steepness)
                    } else {
                        0.0
                    };
                    let own = chroma(rgb);
                    let guide = if guide_gate {
                        [
                            guide[i][0] * (1.0 - edge) + own[0] * edge,
                            guide[i][1] * (1.0 - edge) + own[1] * edge,
                        ]
                    } else {
                        guide[i]
                    };
                    let v = filtered_pixel_guided(
                        rgb,
                        level1_at,
                        rebuilt_at,
                        g,
                        (edge - g).max(0.0),
                        guide,
                    );
                    row[x * 3..x * 3 + 3].copy_from_slice(&v);
                    continue;
                }
                let g = if edge2 > 0.0 {
                    gain(size2[i], t2, steepness).max(gain(
                        level1_gradient2(&rebuilt, x, y),
                        edge2,
                        steepness,
                    ))
                } else {
                    gain(size2[i], t2, steepness)
                };
                let c0 = speck.as_ref().map_or_else(|| chroma(rgb), |s| s[i]);
                let v = match range_margin {
                    Some(margin) => {
                        filtered_pixel_in_range(rgb, level1_at, rebuilt_at, g, c0, |ab| {
                            pixel_in_range(
                                ab,
                                &rebuilt,
                                (x, y, w, h),
                                rebuilt_at,
                                tuning,
                                margin,
                                sigma,
                                strength,
                            )
                        })
                    }
                    None => filtered_pixel_from(rgb, level1_at, rebuilt_at, g, c0),
                };
                row[x * 3..x * 3 + 3].copy_from_slice(&v);
            }
        });
    image::Rgb32FImage::from_raw(w as u32, h as u32, out).unwrap()
}

/// Full chroma plane of a linear image (tests).
#[cfg(test)]
fn chroma_plane(image: &image::Rgb32FImage) -> Plane {
    Plane {
        width: image.width() as usize,
        height: image.height() as usize,
        data: image.pixels().map(|p| chroma_luma(p.0)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb32FImage;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 40) as f32) / (1u64 << 24) as f32
        }
        fn gaussian(&mut self) -> f32 {
            let u1 = self.next().max(1e-7);
            let u2 = self.next();
            (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
        }
    }

    /// A grey image with chroma noise of `sigma` per square-root channel
    /// on its left half and strong colour texture on its right half.
    fn noisy(width: u32, height: u32, sigma: f32, textured: bool, seed: u64) -> Rgb32FImage {
        let mut rng = Rng(seed);
        Rgb32FImage::from_fn(width, height, |x, y| {
            let base = if textured && x >= width / 2 {
                let t = ((x as f32 * 0.9).sin() * (y as f32 * 0.7).cos()) * 0.12;
                [0.45 + t, 0.45, 0.45 - t]
            } else {
                [0.45, 0.45, 0.45]
            };
            image::Rgb(base.map(|v| {
                let p = v.sqrt() + sigma * rng.gaussian();
                p * p
            }))
        })
    }

    /// RMS of level-0 detail of a whole image (what the estimator should
    /// find on a flat noisy image).
    fn level0_rms(image: &Rgb32FImage) -> f32 {
        let plane = chroma_plane(image);
        let g1 = reduce(&plane);
        let mut sum = 0.0f64;
        for y in 0..plane.height {
            for x in 0..plane.width {
                let c = plane.at(x, y);
                let e = expand_at(&g1, x, y);
                sum += ((c[0] - e[0]).powi(2) + (c[1] - e[1]).powi(2)) as f64;
            }
        }
        (sum / (plane.width * plane.height) as f64).sqrt() as f32
    }

    #[test]
    fn first_levels_are_the_one_pass_per_plane_sums() {
        let bits = |plane: &Plane| -> Vec<u32> {
            plane.data.iter().flatten().map(|v| v.to_bits()).collect()
        };
        // Odd and even sides, bands cut short, rows shorter than the kernel,
        // values beyond the measurable range, alpha and sRGB sources.
        for (seed, (width, height)) in [(77, 53), (64, 70), (3, 9), (5, 2), (130, 67)]
            .into_iter()
            .enumerate()
        {
            let mut image = noisy(width, height, 0.05, true, seed as u64 + 40);
            for (i, p) in image.pixels_mut().enumerate() {
                if i % 7 == 0 {
                    p.0[1] = 1.3;
                }
                if i % 11 == 0 {
                    p.0 = [0.0; 3];
                }
            }
            let rgba =
                DynamicImage::ImageRgba32F(DynamicImage::ImageRgb32F(image.clone()).to_rgba32f());
            for (image, linear) in [
                (DynamicImage::ImageRgb32F(image.clone()), true),
                (rgba, true),
                (DynamicImage::ImageRgb32F(image), false),
            ] {
                let source = Source::new(&image, linear);
                let (chroma, mask) = first_level(&source, chroma_luma);
                let (means, _) = first_level(&source, |rgb| rgb);
                for with_means in [false, true] {
                    let (fused, fused_mask, fused_means) = first_levels(&source, with_means);
                    assert_eq!(bits(&fused), bits(&chroma), "{width}x{height}");
                    assert_eq!((fused.width, fused.height), (chroma.width, chroma.height));
                    assert_eq!(fused_mask, mask);
                    assert_eq!(
                        fused_means.map(|plane| bits(&plane)),
                        with_means.then(|| bits(&means))
                    );
                }
            }
        }
    }

    #[test]
    fn first_level_matches_the_reduced_full_plane() {
        let image = noisy(77, 53, 0.02, true, 3);
        let pyramid =
            ChromaPyramid::build(&DynamicImage::ImageRgb32F(image.clone()), true).unwrap();
        let reduced = reduce(&chroma_plane(&image));
        assert_eq!(pyramid.levels[0].width, reduced.width);
        for (a, b) in pyramid.levels[0].data.iter().zip(&reduced.data) {
            assert!((a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6);
        }
    }

    #[test]
    fn measures_known_noise_within_tolerance() {
        for (sigma, seed) in [(0.004, 1), (0.012, 2), (0.03, 3)] {
            let flat = noisy(512, 384, sigma, false, seed);
            let truth = level0_rms(&flat);
            let pyramid = ChromaPyramid::build(&DynamicImage::ImageRgb32F(flat), true).unwrap();
            let ratio = pyramid.noise[0] / truth;
            assert!((0.85..=1.0).contains(&ratio), "flat σ {sigma}: {ratio}");
            // Texture on half the frame does not raise the estimate.
            let textured = noisy(512, 384, sigma, true, seed + 10);
            let measured = ChromaPyramid::build(&DynamicImage::ImageRgb32F(textured), true)
                .unwrap()
                .noise[0];
            assert!(
                (measured / pyramid.noise[0] - 1.0).abs() < 0.1,
                "textured σ {sigma}: {measured}"
            );
            // White noise of σ per channel: chroma differences have σ√2 per
            // component; the estimate scales with σ.
            assert!(pyramid.noise[0] > sigma && pyramid.noise[0] < 2.0 * sigma);
            // Coarser levels of white noise are much smaller.
            assert!(pyramid.noise[1] < 0.5 * pyramid.noise[0]);
        }
    }

    #[test]
    fn the_profile_follows_noise_by_brightness() {
        // Dark top half (square-root luminance about 0.3) with three times
        // the colour noise of the bright bottom half (about 0.8).
        let mut rng = Rng(21);
        let image = Rgb32FImage::from_fn(384, 384, |_, y| {
            let (level, sigma) = if y < 192 { (0.09, 0.03) } else { (0.64, 0.01) };
            image::Rgb([level; 3].map(|v: f32| {
                let p = v.sqrt() + sigma * rng.gaussian();
                p * p
            }))
        });
        let pyramid = ChromaPyramid::build(&DynamicImage::ImageRgb32F(image), true).unwrap();
        let dark = profile_at(&pyramid.profile, 0.3);
        let bright = profile_at(&pyramid.profile, 0.8);
        assert!((dark / bright - 3.0).abs() < 0.6, "{dark} {bright}");
        // The photo's noise is the cleaner brightness's.
        assert!((bright - 1.0).abs() < 0.2, "{bright}");
        // Flat noise everywhere: a flat profile.
        let flat = ChromaPyramid::build(
            &DynamicImage::ImageRgb32F(noisy(256, 256, 0.01, false, 5)),
            true,
        )
        .unwrap();
        assert!(
            flat.profile.iter().all(|p| (p - 1.0).abs() < 0.2),
            "{:?}",
            flat.profile
        );
    }

    #[test]
    fn the_luminance_profile_follows_luminance_noise_by_brightness() {
        // Dark top half with three times the luminance noise (the same noise
        // in all channels) of the bright bottom half.
        let mut rng = Rng(23);
        let image = Rgb32FImage::from_fn(384, 384, |_, y| {
            let (level, sigma) = if y < 192 {
                (0.09f32, 0.03f32)
            } else {
                (0.64, 0.01)
            };
            let p = level.sqrt() + sigma * rng.gaussian();
            image::Rgb([p * p; 3])
        });
        let pyramid = ChromaPyramid::build(&DynamicImage::ImageRgb32F(image), true).unwrap();
        let dark = profile_at(&pyramid.luma_profile, 0.3);
        let bright = profile_at(&pyramid.luma_profile, 0.8);
        assert!((dark / bright - 3.0).abs() < 0.6, "{dark} {bright}");
        // Relative to its median: the bins around 1.
        let mut sorted = pyramid.luma_profile;
        sorted.sort_by(f32::total_cmp);
        assert!((0.5 * (sorted[3] + sorted[4]) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn clipped_and_black_areas_are_not_measured() {
        let mut image = noisy(256, 256, 0.01, false, 5);
        // A clipped half with huge differences between channels.
        for y in 0..256 {
            for x in 0..128 {
                image.put_pixel(
                    x,
                    y,
                    image::Rgb([if (x + y) % 2 == 0 { 1.0 } else { 0.95 }, 0.2, 1.0]),
                );
            }
        }
        let pyramid = ChromaPyramid::build(&DynamicImage::ImageRgb32F(image), true).unwrap();
        let flat = ChromaPyramid::build(
            &DynamicImage::ImageRgb32F(noisy(128, 256, 0.01, false, 5)),
            true,
        )
        .unwrap();
        assert!((pyramid.noise[0] / flat.noise[0] - 1.0).abs() < 0.15);
    }

    #[test]
    fn amount_zero_passes_the_image_through_bit_exactly() {
        let image = noisy(96, 80, 0.02, true, 7);
        let settings = Settings {
            amount: 0.0,
            detail: 0.5,
            smoothness: 0.5,
        };
        let out = apply_to_linear(&image, settings, &Tuning::default());
        assert_eq!(out.as_raw(), image.as_raw());
        let pyramid = ChromaPyramid::build(&DynamicImage::ImageRgb32F(image), true).unwrap();
        assert_eq!(
            pyramid.reconstruct(&[0.0; LEVELS], 0.0, &Tuning::default()),
            pyramid.levels[0]
        );
    }

    #[test]
    fn the_pyramid_reconstructs_its_input() {
        // Thresholds far below every detail keep it all: the pyramid
        // returns level 1 and each pixel its own colour.
        let image = noisy(130, 70, 0.02, true, 9);
        let pyramid =
            ChromaPyramid::build(&DynamicImage::ImageRgb32F(image.clone()), true).unwrap();
        let tiny = [1e-12; LEVELS];
        let rebuilt = pyramid.reconstruct(&tiny, 0.0, &Tuning::default());
        for (a, b) in rebuilt.data.iter().zip(&pyramid.levels[0].data) {
            assert!((a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5);
        }
        // Without the finest aware expansion (which moves the smooth part
        // towards the pixel's side of an edge) each pixel is its own.
        let plain = Tuning {
            finest_aware: 0.0,
            ..Tuning::default()
        };
        let out = apply_with(&image, &pyramid, &tiny, 0.0, &plain);
        for (a, b) in out.as_raw().iter().zip(image.as_raw()) {
            assert!((a - b).abs() < 1e-4 * b.abs().max(1e-3), "{a} {b}");
        }
    }

    #[test]
    fn removes_noise_and_keeps_luminance() {
        let image = noisy(256, 256, 0.02, false, 11);
        let settings = Settings {
            amount: 0.25,
            detail: 0.5,
            smoothness: 0.5,
        };
        let out = apply_to_linear(&image, settings, &Tuning::default());
        let before = level0_rms(&image);
        let after = level0_rms(&out);
        assert!(after < 0.3 * before, "{before} → {after}");
        for (a, b) in out.pixels().zip(image.pixels()) {
            let la = LUMA[0] * a[0] + LUMA[1] * a[1] + LUMA[2] * a[2];
            let lb = LUMA[0] * b[0] + LUMA[1] * b[1] + LUMA[2] * b[2];
            assert!((la - lb).abs() < 1e-5 * lb.max(1e-3));
        }
    }

    #[test]
    fn the_version_follows_the_recipe() {
        use serde_json::json;
        assert!(multiscale(&json!({"colorNoiseVersion": 2}), false));
        assert!(multiscale(&json!({"colorNoiseVersion": 3}), false));
        assert!(multiscale(&json!({"colorNoiseVersion": 4}), false));
        assert!(multiscale(&json!({"colorNoiseVersion": 5}), false));
        assert!(multiscale(&json!({"colorNoiseVersion": 6}), false));
        assert!(!multiscale(&json!({"colorNoiseVersion": 1}), true));
        // Saved colour noise reduction without a version: as before.
        assert!(!multiscale(&json!({"colorNoiseReduction": 25}), true));
        assert_eq!(version(&json!({"colorNoiseReduction": 25}), true), 1);
        // The RAW default (never edited): this filter, newest version.
        assert!(multiscale(&json!({}), true));
        assert!(multiscale(&serde_json::Value::Null, true));
        assert!(!multiscale(&json!({}), false));
        assert_eq!(version(&json!({}), true), VERSION);
        assert_eq!(VERSION, 10);
        // Saved versions stay; unknown newer ones get the newest.
        assert_eq!(version(&json!({"colorNoiseVersion": 2}), true), 2);
        assert_eq!(version(&json!({"colorNoiseVersion": 3}), true), 3);
        assert_eq!(version(&json!({"colorNoiseVersion": 4}), true), 4);
        assert_eq!(version(&json!({"colorNoiseVersion": 5}), true), 5);
        assert_eq!(version(&json!({"colorNoiseVersion": 7}), true), 7);
        assert_eq!(version(&json!({"colorNoiseVersion": 8}), true), 8);
        assert_eq!(version(&json!({"colorNoiseVersion": 9}), true), 9);
        assert_eq!(version(&json!({"colorNoiseVersion": 11}), true), VERSION);
        assert_eq!(version(&json!({"colorNoiseVersion": 0}), true), 1);
    }

    #[test]
    fn version_2_keeps_its_constants() {
        // Version 2 recipes render exactly as before version 3: no quiet
        // multiples and the measured coarse noise.
        let v2 = Tuning::for_version(2);
        assert_eq!(v2, Tuning::for_version(1));
        assert_eq!(v2.multiples, [7.0, 8.0, 10.0, 8.0, 7.0]);
        let noise = [0.02, 0.012, 0.008, 0.007, 0.006];
        assert_eq!(v2.level_noise(&noise), noise);
        for l in 0..LEVELS {
            for n in [0.0005, 0.003, 0.012, 0.05] {
                assert_eq!(v2.multiple(l, n), v2.multiples[l]);
            }
        }
        let v3 = Tuning::for_version(3);
        assert_eq!(
            Tuning {
                quiet: v2.quiet,
                quiet_from: v2.quiet_from,
                coarse_noise_fall: v2.coarse_noise_fall,
                coarse_noise_from: v2.coarse_noise_from,
                ..v3
            },
            v2
        );
    }

    #[test]
    fn version_3_keeps_its_constants() {
        // Version 3 recipes render exactly as before version 4: square-root
        // means and each sample's own chroma as the aware guide.
        let (v3, v4) = (Tuning::for_version(3), Tuning::for_version(4));
        assert!(!v3.linear_means && v4.linear_means);
        assert_eq!(v3.guide, Guide::Own);
        assert!(!v3.guide_coarse && !v3.guide_finest && !v3.guide_edge);
        assert_eq!(v3.finest_guide_code(), (0.0, 0.0));
        assert_eq!(
            Tuning {
                linear_means: v3.linear_means,
                guide: v3.guide,
                guide_coarse: v3.guide_coarse,
                guide_finest: v3.guide_finest,
                guide_edge: v3.guide_edge,
                ..v4
            },
            v3
        );
    }

    #[test]
    fn version_4_keeps_its_constants() {
        // Version 4 recipes render exactly as before version 5: each
        // sample's own chroma as the full-resolution aware guide.
        let (v4, v5) = (Tuning::for_version(4), Tuning::for_version(5));
        assert_eq!(v4.guide, Guide::Own);
        assert!(!v4.guide_finest);
        assert_eq!(v4.finest_guide_code(), (0.0, 0.0));
        // Version 5: the switch (one noise unit from the 3 × 3 median) at
        // full resolution only, no edge gate, the detail the pixel's own.
        assert_eq!(v5.guide, Guide::Switch(1.0));
        assert!(v5.guide_finest && !v5.guide_coarse && !v5.guide_edge);
        assert!(!v5.guide_gate && !v5.guide_detail);
        assert_eq!(v5.finest_guide_code(), (3.0, 1.0));
        assert_eq!(
            Tuning {
                guide: v4.guide,
                guide_finest: v4.guide_finest,
                ..v5
            },
            v4
        );
    }

    /// Mean linear colour of an image.
    fn mean_rgb(image: &Rgb32FImage) -> [f64; 3] {
        let mut m = [0.0f64; 3];
        for p in image.pixels() {
            for c in 0..3 {
                m[c] += f64::from(p[c]);
            }
        }
        m.map(|v| v / f64::from(image.width() * image.height()))
    }

    /// A flat patch of linear colour `base` with Gaussian noise `sigma`.
    fn flat_noisy(base: [f32; 3], sigma: f32, seed: u64) -> Rgb32FImage {
        let mut rng = Rng(seed);
        Rgb32FImage::from_fn(256, 256, |_, _| {
            image::Rgb(std::array::from_fn(|c| base[c] + sigma * rng.gaussian()))
        })
    }

    #[test]
    fn version_5_keeps_its_constants() {
        // Version 5 recipes render exactly as before version 6: the finest
        // detail against the plain expansion of level 1.
        let (v5, v6) = (Tuning::for_version(5), Tuning::for_version(6));
        assert!(!v5.finest_detail_aware && v6.finest_detail_aware);
        assert_eq!(v5.finest_guide_code(), (3.0, 1.0));
        assert_eq!(v6.finest_guide_code(), (35.0, 1.0));
        assert_eq!(
            Tuning {
                finest_detail_aware: v5.finest_detail_aware,
                ..v6
            },
            v5
        );
        for version in 2..=4 {
            assert!(!Tuning::for_version(version).finest_detail_aware);
        }
    }

    #[test]
    fn version_6_keeps_colour_edges_free_of_halos() {
        // A dark grey area beside a yellow one under colour noise (notes:
        // reference/color-noise-reference.md, round 11). Measured against
        // the plain expansion of level 1, the finest detail of a dark pixel
        // next to the edge held part of the yellow side; added to the aware
        // reconstruction of its own side, it pushed the pixel past the dark
        // grey, away from yellow (a blue rim). Mean chroma per column, in
        // the direction from yellow to dark, beyond the dark plateau.
        let (w, h) = (96u32, 256u32);
        let edge = 49u32;
        let dark = [0.02f32, 0.02, 0.02];
        let yellow = [0.55f32, 0.45, 0.04];
        let mut rng = Rng(5);
        let image = Rgb32FImage::from_fn(w, h, |x, _| {
            let base = if x < edge { dark } else { yellow };
            image::Rgb(std::array::from_fn(|c| {
                (base[c].sqrt() + 0.02 * rng.gaussian()).max(0.0).powi(2)
            }))
        });
        let settings = Settings {
            amount: 0.25,
            detail: 0.5,
            smoothness: 0.5,
        };
        let (cd, cy) = (chroma(dark), chroma(yellow));
        let away = {
            let d = [cd[0] - cy[0], cd[1] - cy[1]];
            let n = d[0].hypot(d[1]);
            [d[0] / n, d[1] / n]
        };
        let overshoot = |tuning: &Tuning| {
            let out = apply_to_linear(&image, settings, tuning);
            let column = |x: u32| {
                let mut m = [0.0f32; 2];
                for y in 0..h {
                    let c = chroma(out.get_pixel(x, y).0);
                    m[0] += c[0] / h as f32;
                    m[1] += c[1] / h as f32;
                }
                m
            };
            let plateau = column(edge - 12);
            (edge - 3..edge)
                .map(|x| {
                    let c = column(x);
                    (c[0] - plateau[0]) * away[0] + (c[1] - plateau[1]) * away[1]
                })
                .fold(f32::MIN, f32::max)
        };
        let (v5, v6) = (
            overshoot(&Tuning::for_version(5)),
            overshoot(&Tuning::for_version(6)),
        );
        println!("overshoot past the dark side: version 5 {v5:.4}, version 6 {v6:.4}");
        assert!(v5 > 0.05 && v6 < 0.1 * v5, "{v5} → {v6}");
    }
    #[test]
    fn version_6_keeps_its_constants() {
        // Version 6 recipes render exactly as before version 7.
        let (v6, v7) = (Tuning::for_version(6), Tuning::for_version(7));
        assert_eq!((v6.speck_swap, v7.speck_swap), (0.0, SPECK_SWAP));
        assert_eq!(v7.finest_guide_code(), (35.0 + 64.0 + 128.0 * 8.0, 1.0));
        assert_eq!(
            Tuning {
                speck_swap: 0.0,
                ..v7
            },
            v6
        );
        for version in 2..=6 {
            assert_eq!(Tuning::for_version(version).speck_swap, 0.0);
        }
    }

    #[test]
    fn version_7_keeps_its_constants() {
        // Version 7 recipes render exactly as before version 8: its speck
        // swap's luminance gate reads the input's luminance.
        let (v7, v8) = (Tuning::for_version(7), Tuning::for_version(8));
        assert_eq!(
            (v7.speck_luma_after_nr, v8.speck_luma_after_nr),
            (false, true)
        );
        assert_eq!(v7.finest_guide_code(), (35.0 + 64.0 + 128.0 * 8.0, 1.0));
        assert_eq!(
            v8.finest_guide_code(),
            (35.0 + 64.0 + 128.0 * 8.0 + 2048.0, 1.0)
        );
        assert_eq!(
            Tuning {
                speck_luma_after_nr: false,
                ..v8
            },
            v7
        );
        for version in 2..=7 {
            assert!(!Tuning::for_version(version).speck_luma_after_nr);
        }
    }

    #[test]
    fn version_8_keeps_its_constants() {
        // Version 8 recipes render exactly as before version 9: no range
        // clamp.
        let (v8, v9) = (Tuning::for_version(8), Tuning::for_version(9));
        assert_eq!((v8.range_clamp, v9.range_clamp), (0.0, RANGE_CLAMP));
        assert_eq!(
            v8.finest_guide_code(),
            (35.0 + 64.0 + 128.0 * 8.0 + 2048.0, 1.0)
        );
        assert_eq!(
            v9.finest_guide_code(),
            (
                35.0 + 64.0 + 128.0 * 8.0 + 2048.0 + 4096.0 * 4.0 * RANGE_CLAMP,
                1.0
            )
        );
        assert_eq!(
            Tuning {
                range_clamp: 0.0,
                ..v9
            },
            v8
        );
        for version in 2..=8 {
            assert_eq!(Tuning::for_version(version).range_clamp, 0.0);
        }
    }

    #[test]
    fn version_9_keeps_its_constants() {
        // Version 9 recipes render exactly as before version 10: the range
        // holds the pixel's own aware reconstruction, margin 1.5 σ.
        let (v9, v10) = (Tuning::for_version(9), Tuning::for_version(10));
        assert_eq!(v10, Tuning::default());
        assert_eq!((v9.range_plain, v10.range_plain), (false, true));
        assert_eq!(
            (v9.range_clamp, v10.range_clamp),
            (RANGE_CLAMP, RANGE_CLAMP_PLAIN)
        );
        let base = 35.0 + 64.0 + 128.0 * 8.0 + 2048.0;
        assert_eq!(
            v10.finest_guide_code(),
            (base + 4096.0 * 4.0 * RANGE_CLAMP_PLAIN + 65536.0, 1.0)
        );
        assert_eq!(
            Tuning {
                range_clamp: RANGE_CLAMP,
                range_plain: false,
                ..v10
            },
            v9
        );
        for version in 2..=9 {
            assert!(!Tuning::for_version(version).range_plain);
        }
    }

    #[test]
    fn version_7_removes_isolated_colour_specks() {
        // A flat grey under colour noise with single off-colour pixels of the
        // same luminance (notes: reference/color-noise-reference.md, round
        // 13), and single darker pixels of the same chromaticity, which are
        // luminance noise and keep their chroma.
        let (w, h) = (128u32, 128u32);
        let grey = 0.2f32;
        let mut rng = Rng(9);
        let mut image = Rgb32FImage::from_fn(w, h, |_, _| {
            image::Rgb(std::array::from_fn(|_| {
                (grey.sqrt() + 0.01 * rng.gaussian()).max(0.0).powi(2)
            }))
        });
        let specks: Vec<(u32, u32)> = (0..40)
            .map(|k| (8 + (k % 8) * 15, 8 + (k / 8) * 22))
            .collect();
        for &(x, y) in &specks {
            // Same square-root luminance as the grey, chroma pushed.
            let (r, b) = (grey.sqrt() + 0.12, grey.sqrt() - 0.10);
            let g = (grey.sqrt() - LUMA[0] * r - LUMA[2] * b) / LUMA[1];
            image.put_pixel(x, y, image::Rgb([r * r, g * g, b * b]));
        }
        let left = |tuning: &Tuning, amount: f32| {
            let settings = Settings {
                amount,
                detail: 0.5,
                smoothness: 0.5,
            };
            let out = apply_to_linear(&image, settings, tuning);
            specks
                .iter()
                .map(|&(x, y)| {
                    let c = chroma(out.get_pixel(x, y).0);
                    c[0].hypot(c[1])
                })
                .sum::<f32>()
                / specks.len() as f32
        };
        let (v6, v7) = (Tuning::for_version(6), Tuning::for_version(7));
        let (at25, swapped25) = (left(&v6, 0.25), left(&v7, 0.25));
        println!("speck chroma left at Color 25: version 6 {at25:.4}, version 7 {swapped25:.4}");
        assert!(swapped25 < 0.5 * at25, "{at25} → {swapped25}");
        // At Color 1 the swap is a fifth as strong: the specks stay.
        let (at1, swapped1) = (left(&v6, 0.01), left(&v7, 0.01));
        println!("speck chroma left at Color 1: version 6 {at1:.4}, version 7 {swapped1:.4}");
        assert!(swapped1 > 0.7 * at1, "{at1} → {swapped1}");
    }

    #[test]
    fn version_9_keeps_filtered_chroma_within_the_neighbourhood() {
        // A noisy grey beside an orange area (a colour edge at x = 64) with
        // single darker off-colour pixels on the grey side next to the edge:
        // they stand out in brightness, so the speck swap leaves them, and
        // the detail kept along the edge kept their chroma (notes:
        // reference/color-noise-reference.md, round 15). (Coloured points
        // far beyond the range keep their colour:
        // version_7_keeps_coloured_points_that_stand_out_in_brightness.)
        let (w, h) = (128u32, 128u32);
        let (grey, orange) = ([0.2f32; 3], [0.40f32, 0.25, 0.15]);
        let mut rng = Rng(21);
        let mut image = Rgb32FImage::from_fn(w, h, |x, _| {
            let base = if x < 64 { grey } else { orange };
            image::Rgb(base.map(|v| (v.sqrt() + 0.01 * rng.gaussian()).max(0.0).powi(2)))
        });
        // Square-root RGB of an off-colour pixel `dark` below the grey.
        let put = |image: &mut Rgb32FImage, (x, y): (u32, u32), dark: f32, push: f32| {
            let p = grey[0].sqrt() - dark;
            let rgb = [p - push, p, p + push];
            image.put_pixel(x, y, image::Rgb(rgb.map(|v| v.max(0.0).powi(2))));
        };
        let specks: Vec<(u32, u32)> = (0..30).map(|k| (62 + k % 2, 4 + k * 4)).collect();
        for &p in &specks {
            put(&mut image, p, 0.08, 0.05);
        }
        let settings = Settings {
            amount: 0.5,
            detail: 0.5,
            smoothness: 0.5,
        };
        let render =
            |version: u64| apply_to_linear(&image, settings, &Tuning::for_version(version));
        let (v8, v9) = (render(8), render(9));
        // Each column's mean chroma over the rows without specks.
        let rows: Vec<u32> = (0..h).filter(|y| y % 4 != 0).collect();
        let column = |out: &Rgb32FImage, x: u32| {
            let s = rows.iter().fold([0.0f32; 2], |s, &y| {
                let c = chroma(out.get_pixel(x, y).0);
                [s[0] + c[0], s[1] + c[1]]
            });
            s.map(|v| v / rows.len() as f32)
        };
        // How far the specks' chroma lies from their column's.
        let off = |out: &Rgb32FImage| {
            specks
                .iter()
                .map(|&(x, y)| {
                    let (c, m) = (chroma(out.get_pixel(x, y).0), column(out, x));
                    (c[0] - m[0]).hypot(c[1] - m[1])
                })
                .sum::<f32>()
                / specks.len() as f32
        };
        // The edge itself: the chroma step from x = 61 to x = 66.
        let step = |out: &Rgb32FImage| {
            let (a, b) = (column(out, 61), column(out, 66));
            (b[0] - a[0]).hypot(b[1] - a[1])
        };
        let (s8, s9) = (off(&v8), off(&v9));
        let (e8, e9) = (step(&v8), step(&v9));
        println!("speck chroma v8 {s8:.4}, v9 {s9:.4}; edge step v8 {e8:.4}, v9 {e9:.4}");
        assert!(s9 < 0.6 * s8, "version 9 left the specks: {s8} -> {s9}");
        assert!((e9 / e8 - 1.0).abs() < 0.01, "edge step {e8} -> {e9}");
    }

    #[test]
    fn version_10_checks_a_pixel_against_its_neighbourhoods_plain_reconstruction() {
        // A flat rebuilt level 1 (grey) and a pixel whose edge-aware
        // reconstruction took a sample 3 noise σ off it (its own noisy chroma
        // picked it) with its filtered chroma close to that: version 9's
        // range held that reconstruction, so the pixel stayed a speck;
        // version 10's range is the neighbourhood's plain reconstruction
        // (notes: reference/color-noise-reference.md, round 16).
        let (w, h) = (16, 16);
        let rebuilt = Plane {
            width: w / 2,
            height: h / 2,
            data: vec![[0.0, 0.0, 0.4]; (w / 2) * (h / 2)],
        };
        let sigma = 0.02;
        let aware = [3.0 * sigma, 0.0, 0.4];
        let ab = [3.3 * sigma, 0.2 * sigma];
        let kept = |version: u64| {
            let tuning = Tuning::for_version(version);
            let margin = tuning.range_clamp_steps() * 0.25 * sigma;
            pixel_in_range(
                ab,
                &rebuilt,
                (7, 7, w, h),
                aware,
                &tuning,
                margin,
                sigma,
                1.0,
            )
        };
        assert_eq!(kept(9), ab);
        let v10 = kept(10);
        assert!((v10[0] - RANGE_CLAMP_PLAIN * sigma).abs() < 1e-6, "{v10:?}");
        assert_eq!(v10[1], ab[1]);
        // Within the margin of the plain reconstruction nothing changes.
        let near = [sigma, -sigma];
        let tuning = Tuning::for_version(10);
        let margin = tuning.range_clamp_steps() * 0.25 * sigma;
        assert_eq!(
            pixel_in_range(
                near,
                &rebuilt,
                (7, 7, w, h),
                aware,
                &tuning,
                margin,
                sigma,
                1.0
            ),
            near
        );
    }

    #[test]
    fn version_7_speck_swap_follows_the_filter_strength() {
        // Full from the defaults on, weaker below Color 25 and above Detail
        // 50, gone at Color 0: no jump between Color 0 and 1.
        let v7 = Tuning::for_version(7);
        let noise = [0.02; LEVELS];
        let strength = |amount: f32, detail: f32| {
            let settings = Settings {
                amount,
                detail,
                smoothness: 0.5,
            };
            v7.speck_strength(&v7.thresholds(settings, &noise), &noise)
        };
        assert_eq!(strength(0.25, 0.5), 1.0);
        assert_eq!(strength(1.0, 0.5), 1.0);
        assert_eq!(strength(0.0, 0.5), 0.0);
        assert!((strength(0.01, 0.5) - 0.2).abs() < 1e-5);
        assert!(strength(0.25, 1.0) < 1.0 && strength(0.25, 1.0) > 0.0);
        assert!(strength(0.25, 0.75) > strength(0.25, 1.0));
        assert_eq!(Tuning::for_version(6).speck_strength(&noise, &noise), 0.0);
    }

    #[test]
    fn version_7_keeps_coloured_points_that_stand_out_in_brightness() {
        // Single blue points on dark grey and dark yellow points on light
        // grey: blue carries little luminance, so their luminance differs
        // from the surroundings by less than their chroma does, but by
        // several times the noise. They are features, not specks.
        let (w, h) = (128u32, 128u32);
        let points: Vec<(u32, u32)> = (0..40)
            .map(|k| (8 + (k % 8) * 15, 8 + (k / 8) * 22))
            .collect();
        let settings = Settings {
            amount: 0.25,
            detail: 0.5,
            smoothness: 0.5,
        };
        for (ground, point) in [(0.1f32, [0.1f32, 0.1, 0.6]), (0.8, [0.8, 0.8, 0.3])] {
            let mut rng = Rng(11);
            let mut image = Rgb32FImage::from_fn(w, h, |_, _| {
                image::Rgb(std::array::from_fn(|_| {
                    (ground + 0.01 * rng.gaussian()).max(0.0).powi(2)
                }))
            });
            for &(x, y) in &points {
                image.put_pixel(x, y, image::Rgb(point.map(|v| v * v)));
            }
            let kept = |tuning: &Tuning| {
                let out = apply_to_linear(&image, settings, tuning);
                points
                    .iter()
                    .map(|&(x, y)| {
                        let c = chroma(out.get_pixel(x, y).0);
                        c[0].hypot(c[1])
                    })
                    .sum::<f32>()
                    / points.len() as f32
            };
            let (v6, v7, v9) = (
                kept(&Tuning::for_version(6)),
                kept(&Tuning::for_version(7)),
                kept(&Tuning::for_version(9)),
            );
            println!(
                "point chroma on {ground}: version 6 {v6:.4}, version 7 {v7:.4}, version 9 {v9:.4}"
            );
            assert!(v7 > 0.95 * v6, "{ground}: {v6} → {v7}");
            // Version 9's range clamp fades out this far beyond the range.
            assert!(v9 > 0.95 * v7, "{ground}: {v7} → {v9}");
        }
    }

    #[test]
    fn dark_noisy_colours_keep_their_mean_with_version_4() {
        // A black cat's bluish fur: red is weak against the noise. Versions
        // 2 and 3 averaged square roots and lost the red (red ÷ green 0.67
        // → 0.54); version 4's levels are the chroma of linear means.
        let image = flat_noisy([0.004, 0.006, 0.010], 0.004, 3);
        let settings = Settings {
            amount: 0.5,
            detail: 0.5,
            smoothness: 0.5,
        };
        let ratio = |m: [f64; 3]| (m[0] / m[1], m[2] / m[1]);
        let (r_in, b_in) = ratio(mean_rgb(&image));
        let filtered = |version| {
            ratio(mean_rgb(&apply_to_linear(
                &image,
                settings,
                &Tuning::for_version(version),
            )))
        };
        let (r3, b3) = filtered(3);
        let (r4, b4) = filtered(4);
        assert!(r3 < 0.85 * r_in && b3 > 1.1 * b_in, "v3 {r3} {b3}");
        assert!(
            (r4 / r_in - 1.0).abs() < 0.05 && (b4 / b_in - 1.0).abs() < 0.08,
            "v4 {r4} {b4} vs {r_in} {b_in}"
        );
        // Brighter colours: exact within 1 %.
        let image = flat_noisy([0.01, 0.02, 0.012], 0.006, 4);
        let (r_in, b_in) = ratio(mean_rgb(&image));
        let (r4, b4) = ratio(mean_rgb(&apply_to_linear(
            &image,
            settings,
            &Tuning::for_version(4),
        )));
        assert!(
            (r4 / r_in - 1.0).abs() < 0.01 && (b4 / b_in - 1.0).abs() < 0.01,
            "{r4} {b4}"
        );
    }

    #[test]
    fn linear_means_change_only_the_chroma_of_the_levels() {
        let image = DynamicImage::ImageRgb32F(noisy(128, 96, 0.02, true, 5));
        let plain = ChromaPyramid::build_with(&image, true, false).unwrap();
        let means = ChromaPyramid::build_for(&image, true, &Tuning::for_version(4)).unwrap();
        assert!(means.linear_means && !plain.linear_means);
        assert!(
            !ChromaPyramid::build_for(&image, true, &Tuning::for_version(3))
                .unwrap()
                .linear_means
        );
        // Luminance, its noise and profile (the luminance noise reduction's)
        // stay; the chroma moves.
        assert_eq!(plain.luma_noise, means.luma_noise);
        assert_eq!(plain.luma_profile, means.luma_profile);
        let mut moved = 0;
        for (a, b) in plain.levels.iter().zip(&means.levels) {
            for (p, q) in a.data.iter().zip(&b.data) {
                assert_eq!(p[2], q[2]);
                moved += usize::from(p[0] != q[0] || p[1] != q[1]);
            }
        }
        assert!(moved > 0);
    }

    #[test]
    fn the_median_network_is_the_median() {
        let mut rng = Rng(9);
        for _ in 0..500 {
            let p: [f32; 9] = std::array::from_fn(|_| rng.gaussian());
            let mut sorted = p;
            sorted.sort_by(f32::total_cmp);
            assert_eq!(median9(p), sorted[4]);
        }
    }

    #[test]
    fn guides_follow_their_mode() {
        // A straight colour step through the 3 × 3: the median keeps the
        // centre's side, the mean mixes both; the switch keeps the centre's
        // own chroma unless it stands out of its neighbourhood.
        let (a, b) = ([0.1f32, 0.0], [-0.1f32, 0.05]);
        let mut n = [a, a, b, a, a, b, a, a, b];
        assert_eq!(guide_of(&n, Guide::Own, 1e-4), a);
        assert_eq!(guide_of(&n, Guide::Median, 1e-4), a);
        let mean = guide_of(&n, Guide::Mean, 1e-4);
        assert!((mean[0] - 0.05).abs() < 1e-6 && (mean[1] - 0.0125).abs() < 1e-6);
        n[4] = [0.104, 0.002];
        assert_eq!(guide_of(&n, Guide::Switch(1.0), 1e-4), n[4]);
        n[4] = [0.3, -0.2];
        assert_eq!(guide_of(&n, Guide::Switch(1.0), 1e-4), a);
        // Versions 2–4 take each sample's own chroma.
        for version in 2..=4 {
            let t = Tuning::for_version(version);
            assert_eq!(t.guide, Guide::Own);
            assert_eq!(t.finest_guide_code(), (0.0, 0.0));
        }
    }

    #[test]
    fn a_switched_guide_keeps_neighbours_on_one_side_of_a_colour_edge() {
        // A diagonal colour step under strong colour noise (notes:
        // reference/color-noise-reference.md, round 9): with each pixel's own
        // chroma as the full-resolution guide, pixels next to the edge took
        // the other side's colour by chance. Count pixels whose filtered
        // chroma is far from all eight neighbours'.
        let (w, h) = (192u32, 192u32);
        let mut rng = Rng(11);
        let image = Rgb32FImage::from_fn(w, h, |x, y| {
            let base = if x + y < w {
                [0.10f32, 0.12, 0.30]
            } else {
                [0.20, 0.30, 0.06]
            };
            image::Rgb(std::array::from_fn(|c| {
                (base[c].sqrt() + 0.03 * rng.gaussian()).max(0.0).powi(2)
            }))
        });
        let settings = Settings {
            amount: 0.25,
            detail: 0.5,
            smoothness: 0.5,
        };
        let isolated = |tuning: &Tuning| {
            let out = apply_to_linear(&image, settings, tuning);
            let c = |x: u32, y: u32| chroma(out.get_pixel(x, y).0);
            let mut n = 0;
            for y in 1..h - 1 {
                for x in 1..w - 1 {
                    let p = c(x, y);
                    let far = (-1i32..=1)
                        .flat_map(|dy| (-1i32..=1).map(move |dx| (dx, dy)))
                        .filter(|d| *d != (0, 0))
                        .all(|(dx, dy)| {
                            let q = c((x as i32 + dx) as u32, (y as i32 + dy) as u32);
                            (p[0] - q[0]).hypot(p[1] - q[1]) > 0.03
                        });
                    n += usize::from(far);
                }
            }
            n
        };
        let own = Tuning::for_version(4);
        let switched = Tuning::for_version(5);
        let (with_own, with_switch) = (isolated(&own), isolated(&switched));
        assert!(with_switch < with_own, "{with_own} → {with_switch}");
    }

    #[test]
    fn coarse_multiples_fall_only_where_their_noise_is_low() {
        let tuning = Tuning::for_version(3);
        let [reference, low, _] = tuning.quiet;
        for l in 0..tuning.quiet_from {
            assert_eq!(tuning.multiple(l, 0.001), tuning.multiples[l]);
        }
        for l in tuning.quiet_from..LEVELS {
            // Quiet photos: the lowest multiple; the noisiest: the full one.
            assert_eq!(tuning.multiple(l, 0.5 * reference), low);
            assert_eq!(tuning.multiple(l, reference), low);
            assert_eq!(tuning.multiple(l, 0.1), tuning.multiples[l]);
            let mid = tuning.multiple(l, 2.0 * reference);
            assert!(low < mid && mid < tuning.multiples[l]);
        }
    }

    #[test]
    fn coarse_noise_falls_at_least_as_measured_noise_does() {
        let tuning = Tuning::for_version(3);
        let noise = tuning.level_noise(&[0.02, 0.012, 0.009, 0.008, 0.007]);
        assert_eq!(noise[..3], [0.02, 0.012, 0.009]);
        for l in tuning.coarse_noise_from..LEVELS {
            assert!(noise[l] <= noise[l - 1] / tuning.coarse_noise_fall + 1e-9);
        }
        // Below the cap the measured noise stays.
        let quiet = [0.02, 0.01, 0.005, 0.002, 0.001];
        assert_eq!(tuning.level_noise(&quiet), quiet);
        let image = noisy(256, 256, 0.02, true, 7);
        let pyramid = ChromaPyramid::build(&DynamicImage::ImageRgb32F(image), true).unwrap();
        let used = pyramid.noise_for(&tuning);
        for l in tuning.coarse_noise_from..LEVELS {
            assert!(used[l] <= used[l - 1] / tuning.coarse_noise_fall + 1e-9);
            assert!(used[l] <= pyramid.noise[l]);
        }
    }

    #[test]
    fn version_3_keeps_more_quiet_coarse_colour_than_version_2() {
        // Faint 8–30 px colour patches under fine noise: version 2's full
        // coarse multiples flatten them more than version 3's.
        let (w, h) = (256u32, 256u32);
        let noise = noisy(w, h, 0.01, false, 5);
        let patch = |x: u32, y: u32| {
            let (cx, cy) = ((x / 24) % 2, (y / 24) % 2);
            if cx == cy { 0.012 } else { -0.012 }
        };
        let image = Rgb32FImage::from_fn(w, h, |x, y| {
            let p = noise.get_pixel(x, y).0;
            let d = patch(x, y);
            image::Rgb([
                (p[0].sqrt() + d).max(0.0).powi(2),
                p[1],
                (p[2].sqrt() - d).max(0.0).powi(2),
            ])
        });
        let settings = Settings {
            amount: 0.25,
            detail: 0.5,
            smoothness: 0.5,
        };
        let kept = |tuning: &Tuning| {
            let out = apply_to_linear(&image, settings, tuning);
            // Correlation of the output's red-minus-green with the patches.
            let mut sum = 0.0f64;
            for (x, y, p) in out.enumerate_pixels() {
                sum += f64::from(chroma(p.0)[0] * patch(x, y).signum());
            }
            sum / f64::from(w * h)
        };
        let (v2, v3) = (kept(&Tuning::for_version(2)), kept(&Tuning::for_version(3)));
        assert!(v3 > v2, "{v2} vs {v3}");
    }

    #[test]
    fn thresholds_follow_the_sliders() {
        let tuning = Tuning::default();
        let noise = [0.01, 0.006, 0.004, 0.003, 0.002];
        let at = |amount, detail, smoothness| {
            tuning.thresholds(
                Settings {
                    amount,
                    detail,
                    smoothness,
                },
                &noise,
            )
        };
        assert_eq!(at(0.0, 0.5, 0.5), [0.0; LEVELS]);
        let default = at(0.25, 0.5, 0.5);
        for l in 0..LEVELS {
            assert!((default[l] - noise[l] * tuning.multiple(l, noise[l])).abs() < 1e-7);
            assert!(at(1.0, 0.5, 0.5)[l] > default[l]);
            assert!(at(0.1, 0.5, 0.5)[l] < default[l]);
        }
        // More Detail lowers the finest thresholds, more Smoothness raises
        // the coarsest.
        assert!(at(0.25, 1.0, 0.5)[0] < default[0]);
        assert!(at(0.25, 0.5, 1.0)[LEVELS - 1] > default[LEVELS - 1]);
        // The chroma-edge guard follows the thresholds.
        let strong = at(1.0, 0.5, 0.5);
        for (l, (&base, &more)) in default.iter().zip(&strong).enumerate() {
            assert!((tuning.edge_scale2(l, base, &noise) - 1.0).abs() < 1e-5);
            assert!(tuning.edge_scale2(l, more, &noise) > 1.0);
        }
        assert!(tuning.edge_scale2(LEVELS - 1, at(0.25, 0.5, 1.0)[LEVELS - 1], &noise) > 1.0);
    }

    #[test]
    fn edges_are_coherent_and_blotches_are_not() {
        // A straight colour step and a round blotch of the same contrast.
        let plane = |f: &dyn Fn(f32, f32) -> f32| Plane {
            width: 40,
            height: 40,
            data: (0..1600)
                .map(|i| {
                    let v = f((i % 40) as f32, (i / 40) as f32);
                    [v, -v, 0.5]
                })
                .collect(),
        };
        let edge = coherent_gradient(&plane(&|x, _| if x < 20.0 { 0.0 } else { 0.1 }));
        let blotch = coherent_gradient(&plane(&|x, y| {
            if (x - 20.0).powi(2) + (y - 20.0).powi(2) < 4.0 {
                0.1
            } else {
                0.0
            }
        }));
        let peak = |p: &Plane| p.data.iter().map(|v| v[0]).fold(0.0f32, f32::max);
        let plain = 2.0 * 0.05f32.powi(2);
        assert!(peak(&edge) > 0.9 * plain, "{}", peak(&edge));
        assert!(peak(&blotch) < 0.5 * peak(&edge), "{}", peak(&blotch));
    }

    #[test]
    fn colour_steps_along_brightness_steps_stay() {
        // Noisy grey with a step in brightness and colour at x = 64: the
        // step's colour stays crisp while the noise beside it goes.
        let mut rng = Rng(31);
        let image = Rgb32FImage::from_fn(128, 128, |x, _| {
            let base: [f32; 3] = if x < 64 {
                [0.10, 0.10, 0.10]
            } else {
                [0.55, 0.40, 0.30]
            };
            image::Rgb(base.map(|v| {
                let p = v.sqrt() + 0.02 * rng.gaussian();
                p * p
            }))
        });
        let settings = Settings {
            amount: 0.25,
            detail: 0.5,
            smoothness: 0.5,
        };
        let out = apply_to_linear(&image, settings, &Tuning::default());
        let red_minus_green = |img: &Rgb32FImage, x: u32| -> f32 {
            (0..128)
                .map(|y| {
                    let p = img.get_pixel(x, y).0;
                    encode(p[0]) - encode(p[1])
                })
                .sum::<f32>()
                / 128.0
        };
        let step = |img: &Rgb32FImage| red_minus_green(img, 65) - red_minus_green(img, 62);
        let truth = encode(0.55) - encode(0.40);
        let plain = Tuning {
            luma_guard: 0.0,
            edge_guard: 0.0,
            finest_edge_guard: 0.0,
            aware: 0.0,
            finest_aware: 0.0,
            ..Tuning::default()
        };
        let unguarded = apply_to_linear(&image, settings, &plain);
        // Each of the guards and the aware expansions keeps the step.
        let aware_only = apply_to_linear(
            &image,
            settings,
            &Tuning {
                aware: Tuning::default().aware,
                finest_aware: Tuning::default().finest_aware,
                ..plain
            },
        );
        // Most of the step stays; with neither guards nor aware expansion
        // the colour bleeds across it.
        assert!(step(&out) > 0.6 * truth, "{} {}", step(&out), truth);
        assert!(step(&out) > 3.0 * step(&unguarded), "{}", step(&unguarded));
        assert!(
            step(&aware_only) > 2.0 * step(&unguarded),
            "{}",
            step(&aware_only)
        );
        let spread = |img: &Rgb32FImage| level0_rms(img);
        assert!(spread(&out) < 0.5 * spread(&image));
    }

    #[test]
    fn aware_expansion_keeps_an_iso_luminant_colour_step() {
        // Two flat colours of the same brightness side by side, noise on
        // both: without the aware expansions the rebuild spreads the step
        // over several pixels, with them it stays nearly as crisp as the
        // truth, and the flats are as clean.
        let mut rng = Rng(5);
        let left = [0.30f32, 0.22, 0.12];
        let right = [0.16f32, 0.24, 0.30];
        let truth = |x: u32| if x < 61 { left } else { right };
        let image = Rgb32FImage::from_fn(128, 96, |x, _| {
            image::Rgb(truth(x).map(|v| {
                let p = v.sqrt() + 0.02 * rng.gaussian();
                p * p
            }))
        });
        let settings = Settings {
            amount: 0.25,
            detail: 0.5,
            smoothness: 0.5,
        };
        let plain = Tuning {
            aware: 0.0,
            finest_aware: 0.0,
            ..Tuning::default()
        };
        let aware = apply_to_linear(&image, settings, &Tuning::default());
        let flat = apply_to_linear(&image, settings, &plain);
        let column = |img: &Rgb32FImage, x: u32| -> f32 {
            (0..96)
                .map(|y| encode(img.get_pixel(x, y).0[0]) - encode(img.get_pixel(x, y).0[1]))
                .sum::<f32>()
                / 96.0
        };
        // The share of the step between the two pixels next to it.
        let truth_step = (left[0].sqrt() - left[1].sqrt()) - (right[0].sqrt() - right[1].sqrt());
        let crisp = |img: &Rgb32FImage| (column(img, 60) - column(img, 61)) / truth_step;
        assert!(
            crisp(&aware) > crisp(&flat) + 0.05,
            "{} {}",
            crisp(&aware),
            crisp(&flat)
        );
        // The flats away from the step are as clean as without.
        let flat_noise = |img: &Rgb32FImage| {
            let values: Vec<f32> = (0..96)
                .flat_map(|y| (8..40).chain(88..120).map(move |x| (x, y)))
                .map(|(x, y)| encode(img.get_pixel(x, y).0[0]) - encode(img.get_pixel(x, y).0[1]))
                .collect();
            let mean = values.iter().sum::<f32>() / values.len() as f32;
            (values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / values.len() as f32).sqrt()
        };
        assert!(
            flat_noise(&aware) < 1.1 * flat_noise(&flat),
            "{} {}",
            flat_noise(&aware),
            flat_noise(&flat)
        );
    }

    #[test]
    fn the_fast_exponential_is_close() {
        for i in 0..3000 {
            let x = i as f32 * 0.01;
            let (fast, exact) = (exp_neg(x), (-x).exp());
            assert!((fast - exact).abs() <= 3e-5 * exact, "{x} {fast} {exact}");
        }
        assert!(exp_neg(1000.0) < 1e-37);
        assert_eq!(exp_neg(-1.0), 1.0);
    }

    #[test]
    fn the_row_expansion_matches_each_position() {
        let mut rng = Rng(11);
        let mut plane = |w: usize, h: usize| Plane {
            width: w,
            height: h,
            data: (0..w * h)
                .map(|_| {
                    let step = if rng.next() < 0.3 { 0.2 } else { 0.0 };
                    [
                        0.05 * rng.gaussian() + step,
                        0.05 * rng.gaussian() - step,
                        rng.next(),
                    ]
                })
                .collect(),
        };
        let profile = std::array::from_fn(|b| 0.5 + 0.1 * b as f32);
        let mut scratch = AwareRow::default();
        for (cw, ch) in [(1, 1), (2, 3), (5, 4), (13, 9)] {
            let (rebuilt, coarse) = (plane(cw, ch), plane(cw, ch));
            for (width, height) in [(2 * cw, 2 * ch), (2 * cw - 1, 2 * ch - 1)] {
                let fine = plane(width, height);
                let guide = |x: usize, y: usize| {
                    let f = fine.at(x, y);
                    [f[0], f[1]]
                };
                for (inv2, profile) in [(400.0, None), (40.0, Some(&profile)), (1e5, None)] {
                    for y in 0..height {
                        expand_aware_row(
                            &rebuilt,
                            &coarse,
                            y,
                            width,
                            |x| guide(x, y),
                            inv2,
                            AWARE_FAR,
                            profile,
                            &mut scratch,
                        );
                        for x in 0..width {
                            let (r, c) = expand_aware(
                                &rebuilt,
                                &coarse,
                                x,
                                y,
                                guide(x, y),
                                inv2,
                                AWARE_FAR,
                                profile,
                            );
                            assert_eq!(scratch.rebuilt[x], [r[0], r[1]], "{cw} {width} {x} {y}");
                            assert_eq!(scratch.coarse[x], [c[0], c[1]], "{cw} {width} {x} {y}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_finest_edge_guard_weakens_only_in_very_noisy_photos() {
        let tuning = Tuning::default();
        let thresholds = [0.1; LEVELS];
        let at = |noise0: f32| {
            let mut noise = [0.01; LEVELS];
            noise[0] = noise0;
            tuning.finest_edge2(&thresholds, &noise) / (tuning.finest_edge_guard * noise0).powi(2)
        };
        // Unchanged below the range, threshold doubled (strength 0.5) above it.
        let plain = |noise0: f32| {
            let noise = {
                let mut n = [0.01; LEVELS];
                n[0] = noise0;
                n
            };
            let base = noise[0] * tuning.multiples[0];
            (thresholds[0] / base).powf(2.0 * tuning.finest_follow)
        };
        assert!((at(0.02) - plain(0.02)).abs() < 1e-6 * plain(0.02));
        assert!((at(0.05) / plain(0.05) - 4.0).abs() < 1e-4);
        let mid = at(0.0325) / plain(0.0325);
        assert!(mid > 1.0 && mid < 4.0, "{mid}");
    }

    #[test]
    fn the_coarsest_level_is_smoothed_only_above_amount_25() {
        let tuning = Tuning::default();
        let at = |amount: f32| Settings {
            amount,
            detail: 0.5,
            smoothness: 0.5,
        };
        assert_eq!(tuning.base_sigma(at(0.25)), 0.0);
        assert_eq!(tuning.base_sigma(at(0.1)), 0.0);
        assert!((tuning.base_sigma(at(1.0)) - tuning.base_blur).abs() < 1e-6);
        let image = noisy(256, 256, 0.02, true, 21);
        let pyramid = ChromaPyramid::build(&DynamicImage::ImageRgb32F(image), true).unwrap();
        assert_eq!(pyramid.coarsest(0.0, &tuning), pyramid.levels[LEVELS - 1]);
        let smoothed = pyramid.coarsest(2.0, &tuning);
        let spread = |p: &Plane| {
            let n = p.data.len() as f32;
            let mean = p.data.iter().map(|v| v[0]).sum::<f32>() / n;
            (p.data.iter().map(|v| (v[0] - mean).powi(2)).sum::<f32>() / n).sqrt()
        };
        assert!(spread(&smoothed) < spread(&pyramid.levels[LEVELS - 1]));
        // Luminance is kept.
        for (a, b) in smoothed.data.iter().zip(&pyramid.levels[LEVELS - 1].data) {
            assert_eq!(a[2], b[2]);
        }
    }

    #[test]
    fn aware_ranges_and_guards_follow_the_amount() {
        let tuning = Tuning {
            aware_follow: 1.0,
            finest_follow: 1.0,
            ..Tuning::default()
        };
        let noise = [0.01; LEVELS];
        let at = |amount: f32| {
            tuning.thresholds(
                Settings {
                    amount,
                    detail: 0.5,
                    smoothness: 0.5,
                },
                &noise,
            )
        };
        let (t25, t100) = (at(0.25), at(1.0));
        assert!(
            (tuning.finest_aware2(&t25, &noise) - (tuning.finest_aware * 0.01).powi(2)).abs()
                < 1e-12
        );
        assert!(tuning.finest_aware2(&t100, &noise) > tuning.finest_aware2(&t25, &noise));
        assert!(tuning.aware_inv2(1, &t100, &noise) < tuning.aware_inv2(1, &t25, &noise));
        assert!(tuning.finest_edge2(&t100, &noise) > tuning.finest_edge2(&t25, &noise));
        // No aware expansion beyond `aware_levels`.
        assert_eq!(
            tuning.aware_inv2(tuning.aware_levels + 1, &t25, &noise),
            0.0
        );
    }
}
