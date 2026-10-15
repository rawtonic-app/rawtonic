// SPDX-License-Identifier: AGPL-3.0-or-later
//! The subject of a version-five analysis: its matte, estimated surface
//! normals of people and the photo's own lighting on them. Removing that
//! lighting lets a new light light the material (I·L/S = A·L) instead of the
//! already lit photo, so a light on a face's shadow side brightens it.
//! notes: reference/relight-g70-close.md
//!
//! Normals use the renderer's frame: x right, y down, z towards the camera.
//! The lighting is fitted on one material only — facial skin, located by
//! the face landmarks — where albedo is nearly uniform, so the fit separates
//! light from material; a fit over mixed materials cannot (notes:
//! reference/relight-intrinsics.md). The model is physical and small: an
//! ambient term and one directional light (Lambert), both non-negative. A
//! free second-order fit is ill-conditioned on a face turned to the camera
//! (constant and depth terms trade off): 8-bit rounding of the same photo
//! turned its light from the left to the right.
use image::{ImageBuffer, Rgb, Rgba};

pub type SubjectImage = ImageBuffer<Rgba<u16>, Vec<u16>>;
pub type Normals = ImageBuffer<Rgb<f32>, Vec<f32>>;

/// The photo's own shading at a normal: `[ambient, light, dx, dy, dz, …]`
/// (`SourceLight::coefficients`), ambient plus a Lambert light.
pub fn shading(coefficients: &[f32; 9], n: [f32; 3]) -> f32 {
    let lambert =
        (n[0] * coefficients[2] + n[1] * coefficients[3] + n[2] * coefficients[4]).max(0.);
    coefficients[0] + coefficients[1] * lambert
}

/// Lowest source shading the renderer divides by: deeper shadows hold too
/// little signal to be lifted without amplifying noise.
pub const MIN_SHADING: f32 = 0.12;

/// Packs the subject for the recipe as a two-part atlas, twice as wide as
/// the frame's texture: left, x and y of the people normal (z is positive,
/// the surface faces the camera), the subject matte and the weight of the
/// people normal (0 where no person was analyzed); right, the share of the
/// photo that is the original light's skin highlight (`specular`) and the
/// background reach (`background_reach`, 1 without one).
pub fn encode_subject(
    normals: &Normals,
    matte: &image::GrayImage,
    people: &image::GrayImage,
    reach: Option<&image::GrayImage>,
    specular: Option<&image::GrayImage>,
) -> SubjectImage {
    let (w, h) = matte.dimensions();
    let code = |v: f32| ((v.clamp(-1., 1.) * 0.5 + 0.5) * 65535.).round() as u16;
    let wide = |gray: Option<&image::GrayImage>, x: u32, y: u32, absent: u16| {
        gray.map_or(absent, |g| g.get_pixel(x, y)[0] as u16 * 257)
    };
    ImageBuffer::from_fn(w * 2, h, |x, y| {
        if x < w {
            let n = normals.get_pixel(x, y).0;
            Rgba([
                code(n[0]),
                code(n[1]),
                matte.get_pixel(x, y)[0] as u16 * 257,
                people.get_pixel(x, y)[0] as u16 * 257,
            ])
        } else {
            let x = x - w;
            Rgba([wide(specular, x, y, 0), wide(reach, x, y, 65535), 0, 65535])
        }
    })
}

/// The GPU values of an atlas texel (`encode_subject`): left [nx, ny, matte,
/// weight], right [specular share, reach, 0, 1].
pub fn decode_texel(atlas: &SubjectImage, x: u32, y: u32) -> [f32; 4] {
    let p = atlas.get_pixel(x, y).0;
    if x < atlas.width() / 2 {
        let n = decode_normal(p);
        [n[0], n[1], p[2] as f32 / 65535., p[3] as f32 / 65535.]
    } else {
        p.map(|v| v as f32 / 65535.)
    }
}

/// The share of each skin texel's light that is the original light's
/// highlight (shine), 0..1: brightness beyond 1.5× what the skin's albedo
/// under the fitted shading reflects, where the colour is whiter than the
/// skin's. Removed with the original light; the new light adds its own.
pub fn skin_specular(
    linear: &ImageBuffer<Rgb<f32>, Vec<f32>>,
    normals: &Normals,
    skin: &image::GrayImage,
    light: &SourceLight,
) -> image::GrayImage {
    let (w, h) = skin.dimensions();
    let saturation = |c: [f32; 3]| {
        let max = c[0].max(c[1]).max(c[2]);
        if max <= 1e-6 {
            0.
        } else {
            1. - c[0].min(c[1]).min(c[2]) / max
        }
    };
    let mut skin_saturation: Vec<f32> = skin
        .enumerate_pixels()
        .filter(|(_, _, p)| p[0] > 128)
        .map(|(x, y, _)| saturation(linear.get_pixel(x, y).0))
        .collect();
    if skin_saturation.is_empty() {
        return image::GrayImage::new(w, h);
    }
    skin_saturation.sort_by(f32::total_cmp);
    let reference = skin_saturation[skin_saturation.len() / 2].max(0.05);
    let shine = image::GrayImage::from_fn(w, h, |x, y| {
        if skin.get_pixel(x, y)[0] <= 128 {
            return image::Luma([0]);
        }
        let c = linear.get_pixel(x, y).0;
        let luminance = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let diffuse = light.skin_albedo
            * shading(&light.coefficients, normals.get_pixel(x, y).0).max(MIN_SHADING);
        let excess = ((luminance - 1.5 * diffuse) / luminance.max(1e-6)).clamp(0., 0.85);
        let whiter = ((reference - saturation(c)) / (0.5 * reference)).clamp(0., 1.);
        image::Luma([(excess * whiter * 255.).round() as u8])
    });
    image::imageops::blur(&shine, 1.)
}

/// The normal stored at a subject texel.
pub fn decode_normal(texel: [u16; 4]) -> [f32; 3] {
    let x = texel[0] as f32 / 65535. * 2. - 1.;
    let y = texel[1] as f32 / 65535. * 2. - 1.;
    [x, y, (1. - x * x - y * y).max(0.).sqrt()]
}

const OVAL: [usize; 36] = [
    10, 338, 297, 332, 284, 251, 389, 356, 454, 323, 361, 288, 397, 365, 379, 378, 400, 377, 152,
    148, 176, 149, 150, 136, 172, 58, 132, 93, 234, 127, 162, 21, 54, 103, 67, 109,
];
/// Eyes, brows and lips: not skin, excluded from the fit.
const EXCLUDED: [&[usize]; 5] = [
    &[
        33, 7, 163, 144, 145, 153, 154, 155, 133, 173, 157, 158, 159, 160, 161, 246,
    ],
    &[
        362, 382, 381, 380, 374, 373, 390, 249, 263, 466, 388, 387, 386, 385, 384, 398,
    ],
    &[70, 63, 105, 66, 107, 55, 65, 52, 53, 46],
    &[300, 293, 334, 296, 336, 285, 295, 282, 283, 276],
    &[
        61, 146, 91, 181, 84, 17, 314, 405, 321, 375, 291, 409, 270, 269, 267, 0, 37, 39, 40, 185,
    ],
];

fn inside(polygon: &[[f32; 2]], p: [f32; 2]) -> bool {
    let mut odd = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[j]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            odd = !odd;
        }
        j = i;
    }
    odd
}

fn chroma(c: [f32; 3]) -> [f32; 2] {
    let s = (c[0] + c[1] + c[2]).max(1e-6);
    [c[0] / s, c[1] / s]
}

/// Facial skin of landmarked faces at `width × height`: inside each face
/// oval, outside eyes, brows and lips.
pub fn face_skin_mask(width: u32, height: u32, faces: &[Vec<[f32; 4]>]) -> image::GrayImage {
    let mut mask = image::GrayImage::new(width, height);
    for face in faces.iter().filter(|f| f.len() >= 468) {
        let oval: Vec<[f32; 2]> = OVAL.iter().map(|&i| [face[i][0], face[i][1]]).collect();
        let excluded: Vec<Vec<[f32; 2]>> = EXCLUDED
            .iter()
            .map(|set| set.iter().map(|&i| [face[i][0], face[i][1]]).collect())
            .collect();
        let (lo, hi) = oval.iter().fold(([1f32; 2], [0f32; 2]), |(lo, hi), p| {
            (
                [lo[0].min(p[0]), lo[1].min(p[1])],
                [hi[0].max(p[0]), hi[1].max(p[1])],
            )
        });
        let x0 = ((lo[0] * width as f32).floor().max(0.)) as u32;
        let y0 = ((lo[1] * height as f32).floor().max(0.)) as u32;
        let x1 = ((hi[0] * width as f32).ceil() as u32).min(width);
        let y1 = ((hi[1] * height as f32).ceil() as u32).min(height);
        for y in y0..y1 {
            for x in x0..x1 {
                let uv = [
                    (x as f32 + 0.5) / width as f32,
                    (y as f32 + 0.5) / height as f32,
                ];
                if inside(&oval, uv) && !excluded.iter().any(|p| inside(p, uv)) {
                    mask.put_pixel(x, y, image::Luma([255]));
                }
            }
        }
    }
    mask
}

/// Lighting samples (normal, luminance) on the skin mask: skin chromaticity
/// (loosely: the lit and the shaded side of a face differ in colour when sun
/// and sky light them), neither clipped nor black.
fn skin_samples(
    linear: &ImageBuffer<Rgb<f32>, Vec<f32>>,
    normals: &Normals,
    skin: &image::GrayImage,
) -> Vec<([f32; 3], f32)> {
    let texels: Vec<(u32, u32)> = skin
        .enumerate_pixels()
        .filter(|(_, _, p)| p[0] > 128)
        .map(|(x, y, _)| (x, y))
        .collect();
    if texels.is_empty() {
        return Vec::new();
    }
    let at = |image: &ImageBuffer<Rgb<f32>, Vec<f32>>, x: u32, y: u32| {
        let sx = ((x as f32 + 0.5) / skin.width() as f32 * image.width() as f32) as u32;
        let sy = ((y as f32 + 0.5) / skin.height() as f32 * image.height() as f32) as u32;
        image
            .get_pixel(sx.min(image.width() - 1), sy.min(image.height() - 1))
            .0
    };
    let mut chromas: Vec<[f32; 2]> = texels
        .iter()
        .map(|&(x, y)| chroma(at(linear, x, y)))
        .collect();
    let median = |i: usize, values: &mut Vec<[f32; 2]>| {
        values.sort_by(|a, b| a[i].total_cmp(&b[i]));
        values[values.len() / 2][i]
    };
    let reference = [median(0, &mut chromas), median(1, &mut chromas)];
    // At most about 6000 samples, evenly spread.
    let stride = (texels.len() / 6000).max(1);
    texels
        .iter()
        .step_by(stride)
        .filter_map(|&(x, y)| {
            let c = at(linear, x, y);
            let luminance = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            // Clipped highlights and black hold no shading.
            if !(0.002..0.9).contains(&luminance) || c.iter().any(|v| *v >= 0.98) {
                return None;
            }
            let k = chroma(c);
            ((k[0] - reference[0]).hypot(k[1] - reference[1]) <= 0.07)
                .then(|| (at(normals, x, y), luminance))
        })
        .collect()
}

/// The photo's own lighting on its people.
#[derive(Clone, Copy, Debug)]
pub struct SourceLight {
    /// `[ambient, light, dx, dy, dz, 0, 0, 0, 0]` (`shading`), scaled so
    /// the subject's brightly lit side (98th percentile) has shading 1.
    pub coefficients: [f32; 9],
    /// The facial skin's albedo under that scale (median luminance /
    /// shading of the fitted samples), the reference for plausible material.
    pub skin_albedo: f32,
}

/// The photo's lighting on its people, fitted on their skin (`skin`, a mask
/// of any size over the frame: `face_skin_mask`, or a parse's skin). `None`
/// without enough skin.
pub fn fit(
    linear: &ImageBuffer<Rgb<f32>, Vec<f32>>,
    normals: &Normals,
    subject: &[[f32; 3]],
    skin: &image::GrayImage,
) -> Option<SourceLight> {
    let samples = skin_samples(linear, normals, skin);
    if samples.len() < 200 {
        return None;
    }
    // Directions over the whole sphere: rim and back light included.
    let count = 2048;
    let directions: Vec<[f32; 3]> = (0..count)
        .map(|i| {
            let y = 1. - 2. * (i as f32 + 0.5) / count as f32;
            let r = (1. - y * y).sqrt();
            let a = i as f32 * 2.399_963;
            [r * a.cos(), y, r * a.sin()]
        })
        .collect();
    // Weighted least squares of ambient and light for one direction, both
    // kept non-negative; returns (ambient, light, weighted squared error).
    let solve = |d: [f32; 3], weights: &[f64]| -> (f64, f64, f64) {
        let (mut sw, mut sr, mut srr, mut sl, mut srl) = (0f64, 0f64, 0f64, 0f64, 0f64);
        for ((n, l), w) in samples.iter().zip(weights) {
            let r = (n[0] * d[0] + n[1] * d[1] + n[2] * d[2]).max(0.) as f64;
            let l = *l as f64;
            sw += w;
            sr += w * r;
            srr += w * r * r;
            sl += w * l;
            srl += w * r * l;
        }
        let det = sw * srr - sr * sr;
        let (mut a, mut k) = if det > 1e-12 {
            ((srr * sl - sr * srl) / det, (sw * srl - sr * sl) / det)
        } else {
            (sl / sw, 0.)
        };
        if k < 0. {
            (a, k) = (sl / sw, 0.);
        }
        if a < 0. {
            (a, k) = (0., if srr > 1e-12 { srl / srr } else { 0. });
        }
        let error = samples
            .iter()
            .zip(weights)
            .map(|((n, l), w)| {
                let r = (n[0] * d[0] + n[1] * d[1] + n[2] * d[2]).max(0.) as f64;
                w * (a + k * r - *l as f64).powi(2)
            })
            .sum();
        (a, k, error)
    };
    let mut weights = vec![1f64; samples.len()];
    let mut best = ([0f32, 0., 1.], 0f64, 0f64);
    for _ in 0..3 {
        let (d, a, k, _) = directions
            .iter()
            .map(|d| {
                let (a, k, e) = solve(*d, &weights);
                (*d, a, k, e)
            })
            .min_by(|x, y| x.3.total_cmp(&y.3))?;
        best = (d, a, k);
        // Robust weights from this fit's residuals.
        let residuals: Vec<f64> = samples
            .iter()
            .map(|(n, l)| {
                let r = (n[0] * d[0] + n[1] * d[1] + n[2] * d[2]).max(0.) as f64;
                a + k * r - *l as f64
            })
            .collect();
        let mut sorted: Vec<f64> = residuals.iter().map(|r| r.abs()).collect();
        sorted.sort_by(f64::total_cmp);
        let scale = sorted[sorted.len() / 2] * 1.5 + 1e-9;
        for (w, r) in weights.iter_mut().zip(&residuals) {
            *w = 1. / (r.abs() / scale).max(1.);
        }
    }
    let (d, a, k) = best;
    let c: [f64; 9] = [a, k, d[0] as f64, d[1] as f64, d[2] as f64, 0., 0., 0., 0.];
    let c = c.map(|v| v as f32);
    let mut values: Vec<f32> = subject.iter().map(|n| shading(&c, *n)).collect();
    if values.len() < 100 {
        values = samples.iter().map(|(n, _)| shading(&c, *n)).collect();
    }
    values.sort_by(f32::total_cmp);
    let peak = values[(values.len() as f32 * 0.98) as usize].max(1e-6);
    // A back-lit subject is mostly in shadow: that is a fit (the division
    // stops at MIN_SHADING). No light at all is not.
    if peak <= 1e-5 || c.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let mut coefficients = c;
    coefficients[0] /= peak;
    coefficients[1] /= peak;
    let mut albedo: Vec<f32> = samples
        .iter()
        .map(|(n, l)| l / shading(&coefficients, *n).max(MIN_SHADING))
        .collect();
    albedo.sort_by(f32::total_cmp);
    Some(SourceLight {
        coefficients,
        skin_albedo: albedo[albedo.len() / 2],
    })
}

/// Takes the people weight back where the photo is far brighter than its
/// fitted shading allows: material over 3× the skin's albedo is background
/// caught in a soft matte (sky above hair) or a highlight, where dividing by
/// the shading would multiply the brightness rather than lift a shadow.
pub fn limit_to_consistent(
    linear: &ImageBuffer<Rgb<f32>, Vec<f32>>,
    normals: &Normals,
    people: &mut image::GrayImage,
    light: &SourceLight,
) {
    let reference = light.skin_albedo.max(1e-4);
    for (x, y, w) in people.enumerate_pixels_mut() {
        if w[0] == 0 {
            continue;
        }
        let c = linear.get_pixel(x, y).0;
        let luminance = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let s = shading(&light.coefficients, normals.get_pixel(x, y).0).max(MIN_SHADING);
        let ratio = luminance / s / reference;
        let keep = 1. - ((ratio - 3.) / 3.).clamp(0., 1.);
        let keep = keep * keep * (3. - 2. * keep);
        w[0] = (w[0] as f32 * keep).round() as u8;
    }
}

/// How much of a light set up for the subject reaches the background at
/// each texel (0..1): full on surfaces at the subject's distance (a table,
/// a wall right behind it), none below 0.75× its inverse depth (sky,
/// distant land, a backdrop metres away; measured on the public test set,
/// notes: reference/relight-g70-close.md).
/// Depth near the silhouette is blurred into the subject's, so the
/// background behind and around the matte takes the depth of the clear
/// background nearby.
pub fn background_reach(
    inverse: &ImageBuffer<image::Luma<f32>, Vec<f32>>,
    matte: &image::GrayImage,
    subject_inverse: f32,
) -> image::GrayImage {
    let (w, h) = matte.dimensions();
    let at = |x: u32, y: u32| {
        let (iw, ih) = inverse.dimensions();
        let ix = ((x as f32 + 0.5) / w as f32 * iw as f32) as u32;
        let iy = ((y as f32 + 0.5) / h as f32 * ih as f32) as u32;
        inverse.get_pixel(ix.min(iw - 1), iy.min(ih - 1))[0]
    };
    // Exclude the matte and a band around it (4 % of the short side).
    let band = (w.min(h) as f32 * 0.04).max(2.) as i32;
    let covered = image::imageops::blur(
        &image::GrayImage::from_fn(w, h, |x, y| {
            image::Luma([if matte.get_pixel(x, y)[0] > 25 {
                255
            } else {
                0
            }])
        }),
        band as f32 / 2.,
    );
    let mut value = vec![0f32; (w * h) as usize];
    let mut weight = vec![0f32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            if covered.get_pixel(x, y)[0] < 8 {
                let i = (y * w + x) as usize;
                value[i] = at(x, y);
                weight[i] = 1.;
            }
        }
    }
    // Fill the excluded area from the clear background (push-pull).
    let mut filled = value.clone();
    let mut known = weight.clone();
    let mut sigma = band as f32;
    while known.iter().any(|k| *k < 0.5) && sigma < w.max(h) as f32 * 2. {
        let num = image::imageops::blur(
            &ImageBuffer::<image::Luma<f32>, _>::from_fn(w, h, |x, y| {
                let i = (y * w + x) as usize;
                image::Luma([filled[i] * known[i]])
            }),
            sigma,
        );
        let den = image::imageops::blur(
            &ImageBuffer::<image::Luma<f32>, _>::from_fn(w, h, |x, y| {
                image::Luma([known[(y * w + x) as usize]])
            }),
            sigma,
        );
        for i in 0..filled.len() {
            if known[i] < 0.5 {
                let d = den.as_raw()[i];
                if d > 1e-4 {
                    filled[i] = num.as_raw()[i] / d;
                    known[i] = 1.;
                }
            }
        }
        sigma *= 2.;
    }
    let subject = subject_inverse.max(1e-6);
    image::GrayImage::from_fn(w, h, |x, y| {
        let i = (y * w + x) as usize;
        let depth = if known[i] >= 0.5 { filled[i] } else { at(x, y) };
        let t = ((depth / subject - 0.75) / 0.2).clamp(0., 1.);
        image::Luma([(t * t * (3. - 2. * t) * 255.).round() as u8])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere(
        size: u32,
        light: [f32; 3],
        albedo: f32,
    ) -> (ImageBuffer<Rgb<f32>, Vec<f32>>, Normals) {
        let normal = |x: u32, y: u32| {
            let u = (x as f32 + 0.5) / size as f32 * 2. - 1.;
            let v = (y as f32 + 0.5) / size as f32 * 2. - 1.;
            let z = (1. - u * u - v * v).max(0.).sqrt();
            [u, v, z]
        };
        let normals = ImageBuffer::from_fn(size, size, |x, y| Rgb(normal(x, y)));
        let image = ImageBuffer::from_fn(size, size, |x, y| {
            let n = normal(x, y);
            let lambert = (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]).max(0.);
            let l = albedo * (0.15 + 0.85 * lambert);
            Rgb([l * 1.3, l, l * 0.8])
        });
        (image, normals)
    }

    /// A synthetic face: landmarks on a disc, skin confident everywhere.
    fn disc_face() -> Vec<[f32; 4]> {
        (0..468)
            .map(|i| {
                let a = i as f32 / 468. * std::f32::consts::TAU * 7.;
                let r = 0.35 * ((i % 13) as f32 / 13.);
                [0.5 + r * a.cos(), 0.5 + r * a.sin(), 0., 1.]
            })
            .collect()
    }

    #[test]
    fn fitted_lighting_follows_the_light_and_removes_it() {
        let light = {
            let l = [-0.6f32, -0.3, 0.74];
            let n = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
            l.map(|v| v / n)
        };
        let (image, normals) = sphere(128, light, 0.5);
        let mut face = disc_face();
        // Put the oval on a circle so the whole disc counts as the face.
        for (k, &i) in OVAL.iter().enumerate() {
            let a = k as f32 / OVAL.len() as f32 * std::f32::consts::TAU;
            face[i] = [0.5 + 0.4 * a.cos(), 0.5 + 0.4 * a.sin(), 0., 1.];
        }
        for set in EXCLUDED {
            for &i in set {
                face[i] = [0.02, 0.02, 0., 0.];
            }
        }
        let subject: Vec<[f32; 3]> = normals
            .pixels()
            .map(|p| p.0)
            .filter(|n| n[2] > 0.2)
            .collect();
        let fitted = fit(
            &image,
            &normals,
            &subject,
            &face_skin_mask(128, 128, &[face]),
        )
        .expect("fit");
        let c = fitted.coefficients;
        assert!(
            (fitted.skin_albedo - 0.5).abs() < 0.1,
            "albedo {}",
            fitted.skin_albedo
        );
        // The lit side is brighter than the shadow side, in the light's direction.
        let lit = shading(&c, light);
        let away = shading(&c, [-light[0], -light[1], light[2].abs() * 0.2]);
        assert!(lit > 0.9 && away < 0.5, "lit {lit} away {away}");
        // Albedo (image / shading) is nearly uniform across the lit sphere.
        let mut ratios = Vec::new();
        for y in 16..112 {
            for x in 16..112 {
                let n = normals.get_pixel(x, y).0;
                let s = shading(&c, n).max(MIN_SHADING);
                if n[2] > 0.3 && s > 0.3 {
                    ratios.push(image.get_pixel(x, y)[1] / s);
                }
            }
        }
        ratios.sort_by(f32::total_cmp);
        let spread = ratios[ratios.len() * 9 / 10] / ratios[ratios.len() / 10];
        assert!(spread < 1.25, "albedo spread {spread}");
    }

    #[test]
    fn bright_background_in_the_matte_loses_the_people_weight() {
        let (mut image, normals) = sphere(64, [0., 0., 1.], 0.4);
        // A bright patch where the fitted lighting is dark: sky in the matte.
        let c = SourceLight {
            coefficients: [0.2, 0.8, 0., 0., 1., 0., 0., 0., 0.],
            skin_albedo: 0.42,
        };
        for y in 0..6 {
            for x in 0..6 {
                image.put_pixel(x, y, Rgb([0.9, 0.9, 0.9]));
            }
        }
        let mut people = image::GrayImage::from_pixel(64, 64, image::Luma([255]));
        limit_to_consistent(&image, &normals, &mut people, &c);
        assert_eq!(people.get_pixel(2, 2)[0], 0);
        assert!(people.get_pixel(32, 32)[0] > 250);
    }

    #[test]
    fn background_reach_ignores_depth_blurred_into_the_silhouette() {
        // Subject at inverse 2 in the middle, far background at 0.5, and a
        // blurred ramp of depth around the silhouette.
        let inverse = ImageBuffer::from_fn(100, 100, |x, _| {
            let d = (x as f32 - 50.).abs();
            image::Luma([if d < 20. {
                2.
            } else if d < 30. {
                2. - (d - 20.) * 0.15
            } else {
                0.5
            }])
        });
        let matte = image::GrayImage::from_fn(100, 100, |x, _| {
            image::Luma([if (x as f32 - 50.).abs() < 20. { 255 } else { 0 }])
        });
        let reach = background_reach(&inverse, &matte, 2.);
        // Right beside the silhouette the ramp would read as near; it is not.
        assert!(
            reach.get_pixel(72, 50)[0] < 30,
            "{}",
            reach.get_pixel(72, 50)[0]
        );
        assert_eq!(reach.get_pixel(95, 50)[0], 0);
        // A wall at the subject's distance is reached.
        let wall = ImageBuffer::from_pixel(100, 100, image::Luma([1.9f32]));
        assert!(background_reach(&wall, &matte, 2.).get_pixel(95, 50)[0] > 250);
    }

    #[test]
    fn skin_shine_is_found_where_skin_is_too_bright_and_white() {
        let (mut image, normals) = sphere(64, [0., 0., 1.], 0.4);
        let light = SourceLight {
            coefficients: [0.15, 0.85, 0., 0., 1., 0., 0., 0., 0.],
            skin_albedo: 0.42,
        };
        // A white highlight on the forehead.
        for y in 20..24 {
            for x in 30..34 {
                image.put_pixel(x, y, Rgb([0.95, 0.93, 0.9]));
            }
        }
        let skin = image::GrayImage::from_pixel(64, 64, image::Luma([255]));
        let shine = skin_specular(&image, &normals, &skin, &light);
        assert!(
            shine.get_pixel(32, 22)[0] > 60,
            "{}",
            shine.get_pixel(32, 22)[0]
        );
        assert!(shine.get_pixel(32, 44)[0] < 5);
    }

    #[test]
    fn too_little_skin_gives_no_fit() {
        let (image, normals) = sphere(32, [0., 0., 1.], 0.5);
        assert!(fit(&image, &normals, &[], &image::GrayImage::new(32, 32)).is_none());
    }

    #[test]
    fn subject_texels_round_trip() {
        let normals = ImageBuffer::from_pixel(2, 1, Rgb([0.3f32, -0.4, 0.866]));
        let matte = image::GrayImage::from_pixel(2, 1, image::Luma([255]));
        let people = image::GrayImage::from_pixel(2, 1, image::Luma([128]));
        let encoded = encode_subject(&normals, &matte, &people, None, None);
        assert_eq!(encoded.dimensions(), (4, 1));
        let n = decode_normal(encoded.get_pixel(0, 0).0);
        assert_eq!(decode_texel(&encoded, 2, 0), [0., 1., 0., 1.]);
        assert!(
            (n[0] - 0.3).abs() < 1e-4 && (n[1] + 0.4).abs() < 1e-4 && (n[2] - 0.866).abs() < 1e-3
        );
        assert_eq!(encoded.get_pixel(0, 0)[2], 65535);
    }
}
