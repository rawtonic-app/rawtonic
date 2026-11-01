// SPDX-License-Identifier: AGPL-3.0-or-later
//! Select Subject keeps the subject in focus. BiRefNet marks every
//! foreground object; a photographer's subject is the one the lens was
//! focused on. When the network's mask holds separate objects and one of
//! them is clearly out of focus next to the sharpest (a second flower in
//! the bokeh, a blurred foreground leaf), it is left out, with its soft
//! edge. One object is never cut, however soft its parts (a portrait's
//! shoulders behind the eyes), and objects about as sharp as the subject
//! all stay.
//!
//! Defocus is measured at edges, independent of their contrast (Zhuo &
//! Sim 2011, "Defocus map estimation from a single image"): an edge
//! blurred by a Gaussian of σ loses gradient by a known ratio when blurred
//! again by σ₀, so σ = σ₀ / √(R² − 1). Each object gets the median σ of the
//! edges on and in it. notes: benchmark-results/sm-select-subject-2026-11-01/REPORT.md

use std::collections::VecDeque;

use image::{DynamicImage, GrayImage};
use rayon::prelude::*;

use crate::{display_rgb8, resize_fit_triangle};

/// The long side the edges are measured at (at most).
const WORK_SIZE: u32 = 1280;
/// Pre-blur against noise, then the re-blur of the ratio (pixels at
/// `WORK_SIZE`).
const PRE_BLUR: f32 = 0.5;
const RE_BLUR: f32 = 1.0;
/// Edges weaker than this (gradient per pixel of sRGB-encoded luma, 0–1)
/// are not measured.
const EDGE_STRENGTH: f32 = 0.04;
/// An object needs this many measured edges and this share of the mask
/// grid to be judged; smaller ones stay as they are.
const MIN_EDGES: usize = 40;
const MIN_AREA: f64 = 0.001;
/// The sharpest object among those at least this share of the largest
/// sets the focus.
const REFERENCE_SHARE: f64 = 0.1;
/// An object is out of focus when its σ is more than this many times the
/// focus σ and at least `OUT_OF_FOCUS_MARGIN` pixels more.
const OUT_OF_FOCUS_RATIO: f32 = 2.2;
const OUT_OF_FOCUS_MARGIN: f32 = 0.8;

/// `probability` (the network's mask, any size) without the separate
/// objects that are clearly out of focus on `image` (the photo it was
/// computed on, linear light when `linear`). Unchanged when nothing is left
/// out.
pub fn keep_subject_in_focus(
    probability: GrayImage,
    image: &DynamicImage,
    linear: bool,
) -> GrayImage {
    let (w, h) = probability.dimensions();
    let (w, h) = (w as usize, h as usize);
    let (labels, count) = objects(probability.as_raw(), w, h);
    // Only objects large enough to be judged can be compared.
    let mut area = vec![0usize; count];
    for &l in labels.iter().filter(|&&l| l > 0) {
        area[l as usize - 1] += 1;
    }
    let judged_size = |a: &usize| *a as f64 / (w * h) as f64 >= MIN_AREA;
    if area.iter().filter(|a| judged_size(a)).count() < 2 {
        return probability;
    }
    let edges = edge_defocus(image, linear);
    let dropped = out_of_focus(&labels, &area, w, h, &edges);
    if dropped.iter().all(|d| !d) {
        return probability;
    }
    let owner = assign_soft_edges(probability.as_raw(), labels, w, h);
    let data = probability
        .as_raw()
        .iter()
        .zip(&owner)
        .map(|(&p, &o)| {
            if o > 0 && dropped[o as usize - 1] {
                0
            } else {
                p
            }
        })
        .collect();
    GrayImage::from_raw(w as u32, h as u32, data).expect("mask size")
}

/// 8-connected objects of the mask at one half: labels 1..=count, 0 outside.
fn objects(p: &[u8], w: usize, h: usize) -> (Vec<u32>, usize) {
    let mut labels = vec![0u32; w * h];
    let mut count = 0;
    let mut queue = VecDeque::new();
    for start in 0..w * h {
        if p[start] < 128 || labels[start] != 0 {
            continue;
        }
        count += 1;
        labels[start] = count as u32;
        queue.push_back(start);
        while let Some(i) = queue.pop_front() {
            for j in neighbours(i, w, h) {
                if p[j] >= 128 && labels[j] == 0 {
                    labels[j] = count as u32;
                    queue.push_back(j);
                }
            }
        }
    }
    (labels, count)
}

fn neighbours(i: usize, w: usize, h: usize) -> impl Iterator<Item = usize> {
    let (x, y) = ((i % w) as isize, (i / w) as isize);
    (-1isize..=1)
        .flat_map(move |dy| (-1isize..=1).map(move |dx| (x + dx, y + dy)))
        .filter(move |&(nx, ny)| {
            (nx, ny) != (x, y) && nx >= 0 && ny >= 0 && nx < w as isize && ny < h as isize
        })
        .map(move |(nx, ny)| ny as usize * w + nx as usize)
}

/// Measured edges: position as a share of the frame (x, y in 0–1) and σ in
/// pixels at `WORK_SIZE`.
struct Edges {
    width: usize,
    height: usize,
    points: Vec<(usize, usize, f32)>,
}

fn edge_defocus(image: &DynamicImage, linear: bool) -> Edges {
    // Smaller photos are measured as they are (upscaling would blur every edge).
    let fitted = if image.width().max(image.height()) > WORK_SIZE {
        resize_fit_triangle(image, WORK_SIZE, WORK_SIZE)
    } else {
        image.clone()
    };
    let small = display_rgb8(fitted, linear);
    let (w, h) = (small.width() as usize, small.height() as usize);
    let luma: Vec<f32> = small
        .pixels()
        .map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0)
        .collect();
    let g1 = gaussian(&luma, w, h, PRE_BLUR);
    let g2 = gaussian(&g1, w, h, RE_BLUR);
    let (gx, gy) = gradient(&g1, w, h);
    let (hx, hy) = gradient(&g2, w, h);
    let m1: Vec<f32> = gx.iter().zip(&gy).map(|(a, b)| a.hypot(*b)).collect();
    let mut points = Vec::new();
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            let m = m1[i];
            if m < EDGE_STRENGTH {
                continue;
            }
            // Non-maximum suppression across the edge (four directions).
            let angle = gy[i].atan2(gx[i]);
            let d = ((angle / std::f32::consts::FRAC_PI_4).round() as i32).rem_euclid(4);
            let (dx, dy): (isize, isize) = match d {
                0 => (1, 0),
                1 => (1, 1),
                2 => (0, 1),
                _ => (-1, 1),
            };
            let at = |sx: isize, sy: isize| {
                m1[(y as isize + sy) as usize * w + (x as isize + sx) as usize]
            };
            if m < at(dx, dy) || m < at(-dx, -dy) {
                continue;
            }
            let ratio = m / hx[i].hypot(hy[i]).max(1e-6);
            if ratio <= 1.01 {
                continue;
            }
            let sigma = (RE_BLUR / (ratio * ratio - 1.0).sqrt()).min(10.0);
            points.push((x, y, sigma));
        }
    }
    Edges {
        width: w,
        height: h,
        points,
    }
}

/// Which objects (index = label − 1) are clearly out of focus.
fn out_of_focus(labels: &[u32], area: &[usize], w: usize, h: usize, edges: &Edges) -> Vec<bool> {
    let count = area.len();
    let mut sigmas: Vec<Vec<f32>> = vec![Vec::new(); count];
    for &(x, y, sigma) in &edges.points {
        let gx = (((x as f64 + 0.5) * w as f64 / edges.width as f64) as usize).min(w - 1);
        let gy = (((y as f64 + 0.5) * h as f64 / edges.height as f64) as usize).min(h - 1);
        // The object on or next to the edge (its outline is an edge too).
        let mut label = 0;
        for ny in gy.saturating_sub(2)..=(gy + 2).min(h - 1) {
            for nx in gx.saturating_sub(2)..=(gx + 2).min(w - 1) {
                label = label.max(labels[ny * w + nx]);
            }
        }
        if label > 0 {
            sigmas[label as usize - 1].push(sigma);
        }
    }
    let total = (w * h) as f64;
    let largest = *area.iter().max().unwrap_or(&0) as f64;
    let judged: Vec<Option<f32>> = sigmas
        .iter_mut()
        .zip(area)
        .map(|(s, &a)| (a as f64 / total >= MIN_AREA && s.len() >= MIN_EDGES).then(|| median(s)))
        .collect();
    let focus = judged
        .iter()
        .zip(area)
        .filter(|(_, a)| **a as f64 >= REFERENCE_SHARE * largest)
        .filter_map(|(s, _)| *s)
        .min_by(f32::total_cmp);
    let Some(focus) = focus else {
        return vec![false; count];
    };
    judged
        .iter()
        .map(|s| {
            s.is_some_and(|s| s > OUT_OF_FOCUS_RATIO * focus && s - focus > OUT_OF_FOCUS_MARGIN)
        })
        .collect()
}

fn median(values: &mut [f32]) -> f32 {
    values.sort_by(f32::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Every pixel of the mask's soft edge (above zero, below one half) goes
/// to the object it reaches first through the mask, so a left-out object
/// takes its own edge with it.
fn assign_soft_edges(p: &[u8], mut owner: Vec<u32>, w: usize, h: usize) -> Vec<u32> {
    let mut queue: VecDeque<usize> = (0..w * h).filter(|&i| owner[i] > 0).collect();
    while let Some(i) = queue.pop_front() {
        let o = owner[i];
        for j in neighbours(i, w, h) {
            if owner[j] == 0 && p[j] > 0 {
                owner[j] = o;
                queue.push_back(j);
            }
        }
    }
    owner
}

/// A separable Gaussian blur with replicated borders.
fn gaussian(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil() as isize;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / sum).collect();
    let clamp = |v: isize, n: usize| v.clamp(0, n as isize - 1) as usize;
    let mut tmp = vec![0.0; w * h];
    tmp.par_chunks_exact_mut(w)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                *v = kernel
                    .iter()
                    .enumerate()
                    .map(|(k, c)| c * src[y * w + clamp(x as isize + k as isize - radius, w)])
                    .sum();
            }
        });
    let mut out = vec![0.0; w * h];
    out.par_chunks_exact_mut(w)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                *v = kernel
                    .iter()
                    .enumerate()
                    .map(|(k, c)| c * tmp[clamp(y as isize + k as isize - radius, h) * w + x])
                    .sum();
            }
        });
    out
}

/// Central differences (zero at the border).
fn gradient(src: &[f32], w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let mut gx = vec![0.0; w * h];
    let mut gy = vec![0.0; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            gx[i] = (src[i + 1] - src[i - 1]) / 2.0;
            gy[i] = (src[i + w] - src[i - w]) / 2.0;
        }
    }
    (gx, gy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    const N: u32 = 256;

    fn disc(cx: f32, cy: f32, r: f32) -> impl Fn(u32, u32) -> bool {
        move |x, y| (x as f32 - cx).hypot(y as f32 - cy) <= r
    }

    /// A photo of textured discs on grey (`blurred` = how much each is
    /// blurred) and the network's mask of them.
    fn scene(discs: &[(f32, f32, f32, f32)]) -> (GrayImage, DynamicImage) {
        let mut photo = vec![0.5f32; (N * N) as usize];
        for &(cx, cy, r, blur) in discs {
            let inside = disc(cx, cy, r);
            let mut layer = vec![0.5f32; (N * N) as usize];
            let mut alpha = vec![0.0f32; (N * N) as usize];
            for y in 0..N {
                for x in 0..N {
                    if inside(x, y) {
                        let i = (y * N + x) as usize;
                        // Patches give the object edges of its own.
                        layer[i] = if (x / 24 + y / 30) % 2 == 0 {
                            0.9
                        } else {
                            0.15
                        };
                        alpha[i] = 1.0;
                    }
                }
            }
            if blur > 0.0 {
                layer = gaussian(&layer, N as usize, N as usize, blur);
                alpha = gaussian(&alpha, N as usize, N as usize, blur);
            }
            for i in 0..photo.len() {
                photo[i] = photo[i] * (1.0 - alpha[i]) + layer[i] * alpha[i].min(1.0);
            }
        }
        let rgb = RgbImage::from_fn(N, N, |x, y| {
            let v = (photo[(y * N + x) as usize] * 255.0).round() as u8;
            Rgb([v, v, v])
        });
        let mask = GrayImage::from_fn(N, N, |x, y| {
            let on = discs.iter().any(|&(cx, cy, r, _)| disc(cx, cy, r)(x, y));
            image::Luma([if on { 255 } else { 0 }])
        });
        (mask, DynamicImage::ImageRgb8(rgb))
    }

    fn kept(mask: &GrayImage, at: (u32, u32)) -> bool {
        mask.get_pixel(at.0, at.1)[0] > 0
    }

    #[test]
    fn a_separate_object_far_out_of_focus_is_left_out() {
        let (mask, photo) = scene(&[(70.0, 128.0, 45.0, 0.0), (190.0, 128.0, 45.0, 4.0)]);
        let out = keep_subject_in_focus(mask, &photo, false);
        assert!(kept(&out, (70, 128)));
        assert!(!kept(&out, (190, 128)));
    }

    #[test]
    fn a_left_out_object_takes_its_soft_edge_and_the_kept_one_stays_as_it_was() {
        // The network's soft edges (20 px ramps) of the two objects touch
        // between them.
        let (left, right) = ((75.0, 128.0), (180.0, 128.0));
        let (_, photo) = scene(&[(left.0, left.1, 45.0, 0.0), (right.0, right.1, 45.0, 4.0)]);
        let ramp = |x: u32, y: u32, (cx, cy): (f32, f32)| {
            let d = (x as f32 - cx).hypot(y as f32 - cy);
            ((55.0 - d) / 20.0).clamp(0.0, 1.0)
        };
        let mask = GrayImage::from_fn(N, N, |x, y| {
            let v = ramp(x, y, left).max(ramp(x, y, right));
            image::Luma([(v * 255.0).round() as u8])
        });
        let out = keep_subject_in_focus(mask.clone(), &photo, false);
        assert_ne!(out, mask);
        let dist = |x: u32, y: u32, (cx, cy): (f32, f32)| (x as f32 - cx).hypot(y as f32 - cy);
        for y in 0..N {
            for x in 0..N {
                let (l, r) = (dist(x, y, left), dist(x, y, right));
                let (before, after) = (mask.get_pixel(x, y)[0], out.get_pixel(x, y)[0]);
                if l + 2.0 < r {
                    assert_eq!(after, before, "kept object changed at {x},{y}");
                } else if r + 2.0 < l {
                    assert_eq!(after, 0, "left-out object remains at {x},{y}");
                }
            }
        }
    }

    #[test]
    fn objects_about_as_sharp_all_stay() {
        let (mask, photo) = scene(&[(70.0, 128.0, 45.0, 0.0), (190.0, 128.0, 45.0, 0.6)]);
        let out = keep_subject_in_focus(mask.clone(), &photo, false);
        assert_eq!(out, mask);
    }

    #[test]
    fn one_object_is_never_cut() {
        // A single blurred object: nothing sharper to compare with.
        let (mask, photo) = scene(&[(128.0, 128.0, 60.0, 4.0)]);
        let out = keep_subject_in_focus(mask.clone(), &photo, false);
        assert_eq!(out, mask);
    }
}
