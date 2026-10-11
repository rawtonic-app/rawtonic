// SPDX-License-Identifier: AGPL-3.0-or-later
//! Masks of people's parts, after Lightroom's People masks: the entire
//! person, face skin, body skin, eyebrows, eye sclera, iris and pupil, lips,
//! teeth, hair and clothes, for everyone or one person.
//!
//! Region parts (skin, hair, clothes, the entire person) come from the
//! parse: per pixel a weighted mean of the face crops covering it and the
//! whole-photo parse, at a working size of at most 2048 pixels, fitted to
//! the photo's edges by a guided filter, then scaled up. Feature parts
//! (eyes, brows, lips, teeth) are drawn from the landmarks at full size and
//! cut out of the face skin.

use image::imageops::{self, FilterType};
use image::{GrayImage, RgbImage};
use rayon::prelude::*;

use crate::geometry::{Polygon, guided_filter, luma, smoothstep};
use crate::landmarks::{
    LEFT_BROW, LEFT_EYE, LEFT_IRIS, LIPS_INNER, LIPS_OUTER, RIGHT_BROW, RIGHT_EYE, RIGHT_IRIS,
};
use crate::parse::{Class, ParseCrop};
use crate::{Face, FaceAnalysis};

/// Long edge of the working size for region parts.
const WORK: u32 = 2048;
/// Weight of the whole-photo parse against a face crop's (1 inside it).
const GLOBAL_WEIGHT: f32 = 0.05;
/// Black and white point of the region maps after the guided filter.
const LEVELS: (f32, f32) = (0.1, 0.9);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    EntirePerson,
    FaceSkin,
    BodySkin,
    Eyebrows,
    Sclera,
    Iris,
    Lips,
    Teeth,
    Hair,
    Clothes,
}

impl Part {
    pub const ALL: [Part; 10] = [
        Part::EntirePerson,
        Part::FaceSkin,
        Part::BodySkin,
        Part::Eyebrows,
        Part::Sclera,
        Part::Iris,
        Part::Lips,
        Part::Teeth,
        Part::Hair,
        Part::Clothes,
    ];

    /// The name stored in recipes.
    pub fn name(self) -> &'static str {
        match self {
            Part::EntirePerson => "entirePerson",
            Part::FaceSkin => "faceSkin",
            Part::BodySkin => "bodySkin",
            Part::Eyebrows => "eyebrows",
            Part::Sclera => "sclera",
            Part::Iris => "iris",
            Part::Lips => "lips",
            Part::Teeth => "teeth",
            Part::Hair => "hair",
            Part::Clothes => "clothes",
        }
    }

    pub fn from_name(name: &str) -> Option<Part> {
        Part::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The parse class a region part is made of.
    fn class(self) -> Option<Class> {
        match self {
            Part::EntirePerson => Some(Class::Background),
            Part::FaceSkin => Some(Class::FaceSkin),
            Part::BodySkin => Some(Class::BodySkin),
            Part::Hair => Some(Class::Hair),
            Part::Clothes => Some(Class::Clothes),
            _ => None,
        }
    }

    /// Belongs to the head: taken from the face crops only when one person
    /// is asked for.
    fn of_head(self) -> bool {
        matches!(self, Part::FaceSkin | Part::Hair)
    }
}

/// How much (x, y) belongs to face `index` rather than the other faces: a
/// soft nearest-face assignment in units of face size, stretched downwards
/// because a body continues below its face.
fn ownership(faces: &[Face], index: usize, x: f32, y: f32) -> f32 {
    if faces.len() < 2 {
        return 1.0;
    }
    let mut weights = faces.iter().map(|f| {
        let [cx, cy] = f.center();
        let [_, _, w, h] = f.detection.bbox;
        let dx = (x - cx) / w.max(1.0);
        let mut dy = (y - cy) / h.max(1.0);
        if dy > 0.0 {
            dy *= 0.4;
        }
        (-4.0 * (dx * dx + dy * dy).sqrt()).exp()
    });
    let all: Vec<f32> = weights.by_ref().collect();
    let sum: f32 = all.iter().sum();
    if sum > 0.0 { all[index] / sum } else { 0.0 }
}

/// Probability of `class` at photo point (x, y), for everyone or one person.
fn class_probability(
    analysis: &FaceAnalysis,
    class: Class,
    head_only: bool,
    person: Option<usize>,
    x: f32,
    y: f32,
) -> f32 {
    let p = match (person, head_only) {
        (Some(i), true) => {
            let (p, w) = analysis.faces[i].parse.sample(class, x, y);
            p * w
        }
        _ => {
            let (mut num, mut den) = (0.0, 0.0);
            for face in &analysis.faces {
                let (p, w) = face.parse.sample(class, x, y);
                num += p * w;
                den += w;
            }
            let (g, _) = analysis.global.sample(class, x, y);
            (num + GLOBAL_WEIGHT * g) / (den + GLOBAL_WEIGHT)
        }
    };
    let p = if class == Class::Background {
        1.0 - p
    } else {
        p
    };
    match person {
        Some(i) => p * ownership(&analysis.faces, i, x, y),
        None => p,
    }
}

/// A region map at the working size, fitted to the photo's edges.
fn region_map(
    analysis: &FaceAnalysis,
    parts: &[Part],
    person: Option<usize>,
    guide: &[f32],
    (ww, wh): (u32, u32),
) -> Vec<f32> {
    let sx = analysis.width as f32 / ww as f32;
    let sy = analysis.height as f32 / wh as f32;
    let raw: Vec<f32> = (0..(ww * wh) as usize)
        .into_par_iter()
        .map(|k| {
            let x = ((k % ww as usize) as f32 + 0.5) * sx;
            let y = ((k / ww as usize) as f32 + 0.5) * sy;
            parts
                .iter()
                .filter_map(|part| {
                    part.class()
                        .map(|c| class_probability(analysis, c, part.of_head(), person, x, y))
                })
                .fold(0.0, f32::max)
        })
        .collect();
    let r = (ww.max(wh) / 400).max(2) as usize;
    // The filter lets the guide's texture and edges through as faint
    // values on either side (stubble on a cheek, a shirt's outline): levels
    // cut them, as for the sky mask.
    guided_filter(guide, &raw, ww as usize, wh as usize, r, 2e-3)
        .into_iter()
        .map(|v| ((v - LEVELS.0) / (LEVELS.1 - LEVELS.0)).clamp(0.0, 1.0))
        .collect()
}

/// The landmark outlines of one face, ready for coverage tests.
struct Features {
    eyes: [Polygon; 2],
    brows: [Polygon; 2],
    lips_outer: Polygon,
    lips_inner: Polygon,
    /// Centre and radius of each iris.
    irises: [([f32; 2], f32); 2],
    /// Edge softness, about 2 % of the eye distance.
    feather: f32,
    /// Skin lightness between the brows, for telling brow hair from skin.
    skin_luma: f32,
    /// Lightness percentiles inside the mouth (10th, 90th), for the teeth.
    mouth_luma: (f32, f32),
    /// Bounding box of everything above, grown by the feather.
    bounds: [f32; 4],
}

fn outline(points: &[[f32; 2]], indices: &[usize]) -> Polygon {
    Polygon::new(indices.iter().map(|&i| points[i]).collect())
}

fn iris(points: &[[f32; 2]], indices: &[usize; 5]) -> ([f32; 2], f32) {
    let c = points[indices[0]];
    let r = indices[1..]
        .iter()
        .map(|&i| ((points[i][0] - c[0]).powi(2) + (points[i][1] - c[1]).powi(2)).sqrt())
        .sum::<f32>()
        / 4.0;
    (c, r)
}

fn pixel(image: &RgbImage, x: f32, y: f32) -> Option<[u8; 3]> {
    let (w, h) = image.dimensions();
    (x >= 0.0 && y >= 0.0 && (x as u32) < w && (y as u32) < h)
        .then(|| image.get_pixel(x as u32, y as u32).0)
}

impl Features {
    fn new(points: &[[f32; 2]], image: &RgbImage) -> Self {
        let eyes = [outline(points, &RIGHT_EYE), outline(points, &LEFT_EYE)];
        let brows = [outline(points, &RIGHT_BROW), outline(points, &LEFT_BROW)];
        let lips_outer = outline(points, &LIPS_OUTER);
        let lips_inner = outline(points, &LIPS_INNER);
        let irises = [iris(points, &RIGHT_IRIS), iris(points, &LEFT_IRIS)];
        let (a, b) = (eyes[0].centroid(), eyes[1].centroid());
        let eye_distance = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
        let feather = (eye_distance * 0.02).max(1.0);

        // Median lightness of a small patch between the brows (landmark 9)
        // and on the forehead (151).
        let mut samples = Vec::new();
        let step = (eye_distance * 0.02).max(1.0);
        for &i in &[9usize, 151] {
            let [cx, cy] = points[i];
            for dy in -2..=2 {
                for dx in -2..=2 {
                    if let Some(p) = pixel(image, cx + dx as f32 * step, cy + dy as f32 * step) {
                        samples.push(luma(p));
                    }
                }
            }
        }
        samples.sort_by(f32::total_cmp);
        let skin_luma = samples.get(samples.len() / 2).copied().unwrap_or(0.5);

        let mut mouth = Vec::new();
        let [x0, y0] = lips_inner.min;
        let [x1, y1] = lips_inner.max;
        let mut y = y0.floor();
        while y <= y1 {
            let mut x = x0.floor();
            while x <= x1 {
                if lips_inner.signed_distance(x + 0.5, y + 0.5) < 0.0
                    && let Some(p) = pixel(image, x, y)
                {
                    mouth.push(luma(p));
                }
                x += 1.0;
            }
            y += 1.0;
        }
        mouth.sort_by(f32::total_cmp);
        let pct = |q: f32| {
            mouth
                .get(((mouth.len() as f32 - 1.0) * q).round() as usize)
                .copied()
                .unwrap_or(0.0)
        };
        let mouth_luma = (pct(0.1), pct(0.9));

        let mut bounds = [
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        ];
        for poly in eyes.iter().chain(brows.iter()).chain([&lips_outer]) {
            bounds[0] = bounds[0].min(poly.min[0]);
            bounds[1] = bounds[1].min(poly.min[1]);
            bounds[2] = bounds[2].max(poly.max[0]);
            bounds[3] = bounds[3].max(poly.max[1]);
        }
        let reach = feather * 4.0;
        bounds = [
            bounds[0] - reach,
            bounds[1] - reach,
            bounds[2] + reach,
            bounds[3] + reach,
        ];
        Self {
            eyes,
            brows,
            lips_outer,
            lips_inner,
            irises,
            feather,
            skin_luma,
            mouth_luma,
            bounds,
        }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.bounds[0] && y >= self.bounds[1] && x <= self.bounds[2] && y <= self.bounds[3]
    }

    fn eye(&self, x: f32, y: f32, grow: f32) -> f32 {
        let f = self.feather;
        self.eyes[0]
            .coverage(x, y, grow, f)
            .max(self.eyes[1].coverage(x, y, grow, f))
    }

    fn iris(&self, x: f32, y: f32) -> f32 {
        self.irises
            .iter()
            .map(|(c, r)| {
                let d = ((x - c[0]).powi(2) + (y - c[1]).powi(2)).sqrt() - r;
                (0.5 - d / self.feather).clamp(0.0, 1.0)
            })
            .fold(0.0, f32::max)
    }

    fn brow_outline(&self, x: f32, y: f32, grow: f32) -> f32 {
        let f = self.feather * 2.0;
        self.brows[0]
            .coverage(x, y, grow, f)
            .max(self.brows[1].coverage(x, y, grow, f))
    }

    /// What of the face is not skin: eyes with their lashes, brows, lips and
    /// the mouth.
    fn not_skin(&self, x: f32, y: f32) -> f32 {
        let f = self.feather;
        self.eye(x, y, f * 1.5)
            .max(self.brow_outline(x, y, f))
            .max(self.lips_outer.coverage(x, y, f * 0.5, f))
    }

    fn part(&self, part: Part, x: f32, y: f32, rgb: [u8; 3]) -> f32 {
        let f = self.feather;
        match part {
            Part::Sclera => self.eye(x, y, 0.0) * (1.0 - self.iris(x, y)),
            Part::Iris => self.eye(x, y, 0.0) * self.iris(x, y),
            Part::Lips => {
                self.lips_outer.coverage(x, y, 0.0, f)
                    * (1.0 - self.lips_inner.coverage(x, y, 0.0, f))
            }
            Part::Teeth => {
                let (lo, hi) = self.mouth_luma;
                if hi - lo < 0.08 {
                    return 0.0;
                }
                let l = luma(rgb);
                let max = rgb.iter().copied().max().unwrap_or(0) as f32;
                let min = rgb.iter().copied().min().unwrap_or(0) as f32;
                let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };
                self.lips_inner.coverage(x, y, 0.0, f)
                    * smoothstep(lo + 0.35 * (hi - lo), lo + 0.6 * (hi - lo), l)
                    * (1.0 - smoothstep(0.35, 0.55, saturation))
            }
            Part::Eyebrows => {
                let darker = self.skin_luma - luma(rgb);
                self.brow_outline(x, y, f * 2.0) * smoothstep(0.02, 0.10, darker)
            }
            _ => 0.0,
        }
    }
}

/// Whether the face itself is seen at (x, y) rather than something in
/// front of it (sunglasses, a hand, food): the landmarks place eyes and
/// lips even where they are hidden.
fn visible(parse: &ParseCrop, x: f32, y: f32) -> f32 {
    let (face, w) = parse.sample(Class::FaceSkin, x, y);
    if w == 0.0 {
        return 1.0;
    }
    let (hair, _) = parse.sample(Class::Hair, x, y);
    let (body, _) = parse.sample(Class::BodySkin, x, y);
    smoothstep(0.25, 0.6, face + hair + body)
}

fn bilinear(map: &[f32], (w, h): (u32, u32), x: f32, y: f32) -> f32 {
    let x = (x - 0.5).clamp(0.0, (w - 1) as f32);
    let y = (y - 0.5).clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w as usize - 1), (y0 + 1).min(h as usize - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |xx: usize, yy: usize| map[yy * w as usize + xx];
    let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
    let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
    top + (bottom - top) * fy
}

/// The mask of `parts` (their union) of everyone or of `person` (an index
/// into `analysis.faces`), at the size of `image`, which must be the photo
/// the analysis was made of.
pub fn part_mask(
    image: &RgbImage,
    analysis: &FaceAnalysis,
    parts: &[Part],
    person: Option<usize>,
) -> GrayImage {
    let (w, h) = image.dimensions();
    let mut out = GrayImage::new(w, h);
    let person = person.filter(|&i| i < analysis.faces.len());
    if parts.is_empty() {
        return out;
    }

    let scale = (WORK as f32 / w.max(h) as f32).min(1.0);
    let work = (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    );
    let small = imageops::resize(image, work.0, work.1, FilterType::Triangle);
    let guide: Vec<f32> = small.pixels().map(|p| luma(p.0)).collect();

    let skin_wanted = parts.contains(&Part::FaceSkin);
    let others: Vec<Part> = parts
        .iter()
        .copied()
        .filter(|p| p.class().is_some() && *p != Part::FaceSkin)
        .collect();
    let skin_map =
        skin_wanted.then(|| region_map(analysis, &[Part::FaceSkin], person, &guide, work));
    let other_map =
        (!others.is_empty()).then(|| region_map(analysis, &others, person, &guide, work));

    let features: Vec<(usize, Features)> = analysis
        .faces
        .iter()
        .enumerate()
        .filter(|(i, _)| person.is_none_or(|p| p == *i))
        .filter_map(|(i, f)| f.landmarks.as_ref().map(|l| (i, Features::new(l, image))))
        .collect();
    let feature_parts: Vec<Part> = parts
        .iter()
        .copied()
        .filter(|p| p.class().is_none())
        .collect();

    let sx = work.0 as f32 / w as f32;
    let sy = work.1 as f32 / h as f32;
    out.par_chunks_mut(w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let fy = y as f32 + 0.5;
            for (x, value) in row.iter_mut().enumerate() {
                let fx = x as f32 + 0.5;
                let mut v = other_map
                    .as_ref()
                    .map_or(0.0, |m| bilinear(m, work, fx * sx, fy * sy));
                let near = features.iter().filter(|(_, f)| f.contains(fx, fy));
                if let Some(m) = &skin_map {
                    let mut skin = bilinear(m, work, fx * sx, fy * sy);
                    for (_, f) in near.clone() {
                        skin *= 1.0 - f.not_skin(fx, fy);
                    }
                    v = v.max(skin);
                }
                if !feature_parts.is_empty() {
                    let rgb = image.get_pixel(x as u32, y as u32).0;
                    for (i, f) in near {
                        let mut part = 0.0f32;
                        for &p in &feature_parts {
                            part = part.max(f.part(p, fx, fy, rgb));
                        }
                        if part > 0.0 {
                            v = v.max(part * visible(&analysis.faces[*i].parse, fx, fy));
                        }
                    }
                }
                *value = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Detection;
    use crate::geometry::Affine;
    use crate::parse::{CLASSES, INPUT, ParseCrop, whole_photo};

    /// A parse crop that says `class` everywhere inside it.
    fn uniform(to_photo: Affine, class: Class) -> ParseCrop {
        let mut probs = vec![0u8; (INPUT * INPUT) as usize * CLASSES];
        for px in probs.as_chunks_mut::<CLASSES>().0 {
            px[class as usize] = 255;
        }
        ParseCrop::new(to_photo, probs)
    }

    fn face_at(cx: f32, cy: f32, size: f32, class: Class) -> Face {
        Face {
            detection: Detection {
                score: 0.9,
                bbox: [cx - size / 2.0, cy - size / 2.0, size, size],
                keypoints: [[0.0; 2]; 5],
            },
            landmarks: None,
            parse: uniform(Affine::crop([cx, cy], size * 2.0, 0.0, INPUT), class),
        }
    }

    #[test]
    fn part_names_round_trip() {
        for part in Part::ALL {
            assert_eq!(Part::from_name(part.name()), Some(part));
        }
        assert_eq!(Part::from_name("nose"), None);
    }

    #[test]
    fn face_crop_outweighs_the_whole_photo_parse() {
        let analysis = FaceAnalysis {
            width: 400,
            height: 200,
            faces: vec![face_at(100.0, 100.0, 60.0, Class::FaceSkin)],
            global: uniform(whole_photo(400, 200), Class::Background),
        };
        let image = RgbImage::from_pixel(400, 200, image::Rgb([128, 128, 128]));
        let mask = part_mask(&image, &analysis, &[Part::FaceSkin], None);
        assert!(mask.get_pixel(100, 100)[0] > 240);
        assert!(mask.get_pixel(300, 100)[0] < 15);
        // Everything but the background: the face crop is a person.
        let person = part_mask(&image, &analysis, &[Part::EntirePerson], None);
        assert!(person.get_pixel(100, 100)[0] > 240);
        assert!(person.get_pixel(300, 100)[0] < 15);
    }

    #[test]
    fn one_person_keeps_only_their_head() {
        let analysis = FaceAnalysis {
            width: 400,
            height: 200,
            faces: vec![
                face_at(100.0, 100.0, 60.0, Class::FaceSkin),
                face_at(300.0, 100.0, 60.0, Class::FaceSkin),
            ],
            global: uniform(whole_photo(400, 200), Class::Background),
        };
        let image = RgbImage::from_pixel(400, 200, image::Rgb([128, 128, 128]));
        let mask = part_mask(&image, &analysis, &[Part::FaceSkin], Some(1));
        assert!(mask.get_pixel(100, 100)[0] < 15);
        assert!(mask.get_pixel(300, 100)[0] > 240);
        // Ownership splits the photo between the two faces.
        assert!(ownership(&analysis.faces, 0, 120.0, 100.0) > 0.9);
        assert!((ownership(&analysis.faces, 0, 200.0, 100.0) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn landmark_features_cut_eyes_out_of_the_skin() {
        // A synthetic face: 478 points on a grid, then the outlines placed
        // where a face's would be.
        let mut points = vec![[200.0f32, 100.0]; crate::landmarks::COUNT];
        let ring = |points: &mut Vec<[f32; 2]>, idx: &[usize], c: [f32; 2], rx: f32, ry: f32| {
            for (k, &i) in idx.iter().enumerate() {
                let a = k as f32 / idx.len() as f32 * std::f32::consts::TAU;
                points[i] = [c[0] + rx * a.cos(), c[1] + ry * a.sin()];
            }
        };
        ring(&mut points, &RIGHT_EYE, [180.0, 90.0], 8.0, 3.0);
        ring(&mut points, &LEFT_EYE, [220.0, 90.0], 8.0, 3.0);
        ring(&mut points, &RIGHT_BROW, [180.0, 80.0], 9.0, 2.0);
        ring(&mut points, &LEFT_BROW, [220.0, 80.0], 9.0, 2.0);
        ring(&mut points, &LIPS_OUTER, [200.0, 125.0], 14.0, 5.0);
        ring(&mut points, &LIPS_INNER, [200.0, 125.0], 10.0, 1.5);
        ring(&mut points, &RIGHT_IRIS[1..], [180.0, 90.0], 2.5, 2.5);
        ring(&mut points, &LEFT_IRIS[1..], [220.0, 90.0], 2.5, 2.5);
        points[RIGHT_IRIS[0]] = [180.0, 90.0];
        points[LEFT_IRIS[0]] = [220.0, 90.0];
        let mut face = face_at(200.0, 100.0, 80.0, Class::FaceSkin);
        face.landmarks = Some(points);
        let analysis = FaceAnalysis {
            width: 400,
            height: 200,
            faces: vec![face],
            global: uniform(whole_photo(400, 200), Class::Background),
        };
        let image = RgbImage::from_pixel(400, 200, image::Rgb([180, 140, 120]));
        let skin = part_mask(&image, &analysis, &[Part::FaceSkin], None);
        assert!(
            skin.get_pixel(200, 105)[0] > 240,
            "cheek between eyes and mouth"
        );
        assert!(skin.get_pixel(180, 90)[0] < 15, "eye");
        assert!(skin.get_pixel(200, 128)[0] < 15, "lips");
        let lips = part_mask(&image, &analysis, &[Part::Lips], None);
        assert!(lips.get_pixel(200, 129)[0] > 200);
        assert!(lips.get_pixel(200, 125)[0] < 60, "inside the mouth");
        let iris = part_mask(&image, &analysis, &[Part::Iris], None);
        assert!(iris.get_pixel(180, 90)[0] > 200);
        let sclera = part_mask(&image, &analysis, &[Part::Sclera], None);
        assert!(sclera.get_pixel(186, 90)[0] > 200);
        assert!(sclera.get_pixel(180, 90)[0] < 60);
    }
}
