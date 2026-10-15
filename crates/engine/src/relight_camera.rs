// SPDX-License-Identifier: AGPL-3.0-or-later
//! Perspective surface fitting. Inverse depth is affine on a camera-space
//! plane, so its fitted derivatives and the same depth drive normals/positions.
use super::{OVAL, Surface, cross, sample, sub, topology, unit};
use crate::relight::{Camera, DepthSurface};
use image::{ImageBuffer, Rgba};
use rayon::prelude::*;

fn fit(depth: &DepthSurface, camera: Camera, x: u32, y: u32) -> [f32; 4] {
    let (w, h) = depth.image.dimensions();
    let center = camera.inverse(depth.image.get_pixel(x, y)[0]);
    let mut sums = [0.0_f64; 9];
    for dy in -2i32..=2 {
        for dx in -2i32..=2 {
            let (px, py) = (x as i32 + dx, y as i32 + dy);
            if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                continue;
            }
            let delta =
                (camera.inverse(depth.image.get_pixel(px as u32, py as u32)[0]) - center) as f64;
            let range = (1. - (delta / (center as f64 * 0.04)).powi(2)).max(0.);
            let (a, b) = (dx as f64, dy as f64);
            let weight = range * range * (-(a * a + b * b) / 4.5).exp();
            let terms = [1., a, b, a * a, a * b, b * b, delta, a * delta, b * delta];
            for (s, t) in sums.iter_mut().zip(terms) {
                *s += weight * t;
            }
        }
    }
    let [sw, sx, sy, sxx, sxy, syy, sz, sxz, syz] = sums;
    let xx = sxx - sx * sx / sw;
    let yy = syy - sy * sy / sw;
    let xy = sxy - sx * sy / sw;
    let xz = sxz - sx * sz / sw;
    let yz = syz - sy * sz / sw;
    let det = xx * yy - xy * xy;
    let (a, b) = if det > 1e-9 {
        ((xz * yy - yz * xy) / det, (yz * xx - xz * xy) / det)
    } else {
        (
            if xx > 1e-9 { xz / xx } else { 0. },
            if yy > 1e-9 { yz / yy } else { 0. },
        )
    };
    // Steep inference transitions are often silhouettes, not visible walls.
    // Exclude them as slope donors; the next pass extends same-depth slopes
    // from stable neighbours, and leaves unsupported thin edges neutral.
    let mut jump = 0.0_f32;
    for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let px = (x as i32 + dx).clamp(0, w as i32 - 1) as u32;
        let py = (y as i32 + dy).clamp(0, h as i32 - 1) as u32;
        jump = jump.max((camera.inverse(depth.image.get_pixel(px, py)[0]) - center).abs() / center);
    }
    let reliability = (1. - (jump / 0.04).powi(2)).max(0.).powi(2);
    [
        a as f32 * w as f32,
        b as f32 * h as f32,
        center,
        reliability,
    ]
}

pub(super) fn scene_texture(depth: &DepthSurface, camera: Camera) -> Surface {
    let (w, h) = depth.image.dimensions();
    let mut pixels = vec![[0.; 4]; (w * h) as usize];
    pixels
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, p)| *p = fit(depth, camera, i as u32 % w, i as u32 / w));
    let fitted: Surface =
        ImageBuffer::from_raw(w, h, pixels.into_iter().flatten().collect()).unwrap();
    // Regularize uncertain coarse scene slopes before adding the finer face.
    // Keep inverse distance unchanged for projection and light falloff.
    let mut pixels = vec![[0.; 4]; (w * h) as usize];
    pixels.par_iter_mut().enumerate().for_each(|(i, out)| {
        let (x, y) = (i as i32 % w as i32, i as i32 / w as i32);
        let center = fitted.get_pixel(x as u32, y as u32).0;
        let mut sum = [0.; 2];
        let mut total = 0.;
        for dy in -6i32..=6 {
            for dx in -6i32..=6 {
                let (px, py) = (x + dx, y + dy);
                if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                    continue;
                }
                let p = fitted.get_pixel(px as u32, py as u32).0;
                let range = (1. - ((p[2] - center[2]) / (center[2] * 0.08)).powi(2)).max(0.);
                let weight = p[3] * range * range * (-(dx * dx + dy * dy) as f32 / 18.).exp();
                sum[0] += p[0] * weight;
                sum[1] += p[1] * weight;
                total += weight;
            }
        }
        *out = if total > 1e-6 {
            [sum[0] / total, sum[1] / total, center[2], 0.]
        } else {
            [0., 0., center[2], 0.]
        };
    });
    if depth.smooth_scene {
        smooth_slopes(&mut pixels, w, h);
    }
    ImageBuffer::from_raw(w, h, pixels.into_iter().flatten().collect()).unwrap()
}

/// Wide depth-aware smoothing of the scene slopes (version five): a
/// separable Gaussian over 6 % of the short side whose weights fall with
/// the relative inverse-depth difference, so one surface's direction does
/// not cross onto another at a different distance.
fn smooth_slopes(pixels: &mut [[f32; 4]], w: u32, h: u32) {
    let sigma = (w.min(h) as f32 * 0.06).max(2.);
    let radius = (sigma * 2.5) as i32;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|d| (-(d * d) as f32 / (2. * sigma * sigma)).exp())
        .collect();
    let pass = |pixels: &[[f32; 4]], horizontal: bool| -> Vec<[f32; 4]> {
        (0..(w * h) as usize)
            .into_par_iter()
            .map(|i| {
                let (x, y) = ((i as u32 % w) as i32, (i as u32 / w) as i32);
                let center = pixels[i];
                let mut sum = [0.; 2];
                let mut total = 0.;
                for (k, d) in (-radius..=radius).enumerate() {
                    let (px, py) = if horizontal { (x + d, y) } else { (x, y + d) };
                    if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                        continue;
                    }
                    let p = pixels[(py as u32 * w + px as u32) as usize];
                    let range = (1. - ((p[2] - center[2]) / (center[2] * 0.08)).powi(2)).max(0.);
                    let weight = kernel[k] * range * range;
                    sum[0] += p[0] * weight;
                    sum[1] += p[1] * weight;
                    total += weight;
                }
                if total > 1e-6 {
                    [sum[0] / total, sum[1] / total, center[2], center[3]]
                } else {
                    center
                }
            })
            .collect()
    };
    let horizontal = pass(pixels, true);
    pixels.copy_from_slice(&pass(&horizontal, false));
}

pub(super) fn refine_faces(depth: &DepthSurface, camera: Camera, base: &Surface) -> Surface {
    let (w, h) = base.dimensions();
    let scale = (1024. / w.max(h) as f32).clamp(1., 2.);
    let (w, h) = (
        (w as f32 * scale).round() as u32,
        (h as f32 * scale).round() as u32,
    );
    let mut output = ImageBuffer::from_fn(w, h, |x, y| {
        Rgba(sample(
            base,
            [(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32],
        ))
    });
    let mut nearest = vec![f32::NEG_INFINITY; (w * h) as usize];
    let world_unit = camera.world_unit();
    for points in &depth.faces {
        let anchors: Vec<_> = points
            .iter()
            .enumerate()
            .filter(|(i, p)| p[3] > 0.8 && !OVAL.contains(i))
            .map(|(_, p)| (sample(base, [p[0], p[1]])[2].recip(), p[2]))
            .collect();
        if anchors.len() < 20 {
            continue;
        }
        // Landmark relief is measured in image units. Its world scale must
        // follow the face's distance, just like its back-projected width.
        let mut distances: Vec<_> = anchors.iter().map(|(d, _)| *d).collect();
        distances.sort_by(f32::total_cmp);
        let face_unit = world_unit * distances[distances.len() / 2] / camera.reference;
        let mut offsets: Vec<_> = anchors.iter().map(|(d, z)| d + z * face_unit).collect();
        offsets.sort_by(f32::total_cmp);
        let offset = offsets[offsets.len() / 2];
        let distances: Vec<_> = points.iter().map(|p| offset - p[2] * face_unit).collect();
        if distances
            .iter()
            .any(|d| !d.is_finite() || *d < camera.reference * 0.02)
        {
            continue;
        }
        let positions: Vec<_> = points
            .iter()
            .zip(&distances)
            .map(|(p, d)| camera.position([p[0], p[1]], *d, 1.))
            .collect();
        let mut normals = vec![[0.; 3]; points.len()];
        for &[a, b, c] in topology::TRIANGLES {
            let n = cross(
                sub(positions[c], positions[a]),
                sub(positions[b], positions[a]),
            );
            for i in [a, b, c] {
                for (dst, v) in normals[i].iter_mut().zip(n) {
                    *dst += v;
                }
            }
        }
        for n in &mut normals {
            *n = unit(*n);
        }
        for &[ia, ib, ic] in topology::TRIANGLES {
            let ids = [ia, ib, ic];
            let p = ids.map(|i| [points[i][0] * w as f32, points[i][1] * h as f32]);
            let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]);
            if area >= -1e-5 {
                continue;
            }
            let lo = std::array::from_fn::<_, 2, _>(|k| {
                p.iter()
                    .map(|p| p[k])
                    .fold(f32::INFINITY, f32::min)
                    .floor()
                    .max(0.) as u32
            });
            let hi = [
                p.iter()
                    .map(|p| p[0])
                    .fold(0., f32::max)
                    .ceil()
                    .min(w as f32) as u32,
                p.iter()
                    .map(|p| p[1])
                    .fold(0., f32::max)
                    .ceil()
                    .min(h as f32) as u32,
            ];
            for y in lo[1]..hi[1] {
                for x in lo[0]..hi[0] {
                    let pixel = [x as f32 + 0.5, y as f32 + 0.5];
                    let b = ((pixel[0] - p[0][0]) * (p[2][1] - p[0][1])
                        - (pixel[1] - p[0][1]) * (p[2][0] - p[0][0]))
                        / area;
                    let c = ((p[1][0] - p[0][0]) * (pixel[1] - p[0][1])
                        - (p[1][1] - p[0][1]) * (pixel[0] - p[0][0]))
                        / area;
                    let weights = [1. - b - c, b, c];
                    if weights.iter().any(|v| *v < -1e-5) {
                        continue;
                    }
                    let inverse: f32 = ids
                        .iter()
                        .zip(weights)
                        .map(|(&i, a)| a / distances[i])
                        .sum();
                    let index = (y * w + x) as usize;
                    if inverse <= nearest[index] {
                        continue;
                    }
                    let interpolate = |f: &dyn Fn(usize) -> f32| {
                        ids.iter()
                            .zip(weights)
                            .map(|(&i, a)| f(i) * a / distances[i])
                            .sum::<f32>()
                            / inverse
                    };
                    let n = unit(std::array::from_fn(|k| interpolate(&|i| normals[i][k])));
                    let uv = [pixel[0] / w as f32, pixel[1] / h as f32];
                    let facing = n[2]
                        - n[0] * (uv[0] - camera.cx) / camera.fx
                        - n[1] * (uv[1] - camera.cy) / camera.fy;
                    if facing < 0.08 {
                        continue;
                    }
                    let coverage =
                        interpolate(&|i| if OVAL.contains(&i) { 0. } else { points[i][3] })
                            .clamp(0., 1.);
                    if coverage <= 0.001 {
                        continue;
                    }
                    let face = [
                        -inverse * n[0] / (camera.fx * facing),
                        -inverse * n[1] / (camera.fy * facing),
                        inverse,
                        1.,
                    ];
                    let prior = sample(base, uv);
                    output.put_pixel(
                        x,
                        y,
                        Rgba(std::array::from_fn(|i| {
                            prior[i] + (face[i] - prior[i]) * coverage
                        })),
                    );
                    nearest[index] = inverse;
                }
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    fn camera() -> Camera {
        Camera {
            fx: 1.2,
            fy: 1.4,
            cx: 0.46,
            cy: 0.53,
            inverse_min: 0.3,
            inverse_max: 2.,
            reference: 1.,
        }
    }
    fn normal(camera: Camera, uv: [f32; 2], surface: [f32; 4], strength: f32) -> [f32; 3] {
        let original = surface[2].recip();
        let d = camera.reference + (original - camera.reference) * strength;
        let a = surface[0] * strength * original * original / (d * d);
        let b = surface[1] * strength * original * original / (d * d);
        unit([
            -camera.fx * a,
            -camera.fy * b,
            d.recip() - a * (uv[0] - camera.cx) - b * (uv[1] - camera.cy),
        ])
    }
    #[test]
    fn inverse_depth_fit_keeps_slanted_planes_separate() {
        let c = camera();
        for (w, h) in [(96, 64), (64, 96)] {
            for diagonal in [false, true] {
                let image = ImageBuffer::from_fn(w, h, |x, y| {
                    image::Luma([if (if diagonal { x + y } else { x }) > w / 2 {
                        35000
                    } else {
                        12000
                    } + x as u16 * 32
                        + y as u16 * 16])
                });
                let depth = DepthSurface {
                    camera: Some(c),
                    image,
                    faces: vec![],
                    refined: true,
                    subject: None,
                    smooth_scene: false,
                };
                let output = crate::relight::surface_texture(&depth);
                for y in 0..h {
                    for x in 0..w {
                        let expected = [
                            (c.inverse_max - c.inverse_min) * 32. / 65535. * w as f32,
                            (c.inverse_max - c.inverse_min) * 16. / 65535. * h as f32,
                        ];
                        let actual = output.get_pixel(x, y).0;
                        assert!(
                            (actual[0] - expected[0]).abs() < 0.0001,
                            "{x},{y}: {actual:?}"
                        );
                        assert!(
                            (actual[1] - expected[1]).abs() < 0.0001,
                            "{x},{y}: {actual:?}"
                        );
                        assert_eq!(actual[2], c.inverse(depth.image.get_pixel(x, y)[0]));
                        assert_eq!(actual[3], 0.);
                    }
                }
            }
        }
    }
    #[test]
    fn perspective_normal_matches_position_tangents_at_every_strength() {
        let c = camera();
        let inverse = |uv: [f32; 2]| 1.1 + 0.25 * (uv[0] - 0.5) - 0.17 * (uv[1] - 0.5);
        for uv in [[0.2, 0.3], [0.5, 0.5], [0.8, 0.7]] {
            for strength in [0., 0.4, 1.] {
                let epsilon = 0.001;
                let position = |u| c.position(u, inverse(u).recip(), strength);
                let tangent_x = sub(
                    position([uv[0] + epsilon, uv[1]]),
                    position([uv[0] - epsilon, uv[1]]),
                );
                let tangent_y = sub(
                    position([uv[0], uv[1] + epsilon]),
                    position([uv[0], uv[1] - epsilon]),
                );
                let expected = unit(cross(tangent_x, tangent_y));
                let actual = normal(c, uv, [0.25, -0.17, inverse(uv), 0.], strength);
                for i in 0..3 {
                    assert!(
                        (actual[i] - expected[i]).abs() < 0.0001,
                        "{actual:?} != {expected:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn facial_shape_is_invariant_to_scene_distance() {
        let c = camera();
        let face = super::super::test_face_geometry();
        let render = |code| {
            crate::relight::surface_texture(&DepthSurface {
                camera: Some(c),
                image: ImageBuffer::from_pixel(128, 128, image::Luma([code])),
                faces: vec![face.clone()],
                refined: true,
                subject: None,
                smooth_scene: false,
            })
        };
        let near = render(60000);
        let far = render(5000);
        let ratio = c.inverse(60000) / c.inverse(5000);
        for (a, b) in near.pixels().zip(far.pixels()) {
            for i in 0..3 {
                assert!((a[i] - ratio * b[i]).abs() < 0.001, "{a:?} {b:?}");
            }
            assert!((a[3] - b[3]).abs() < 0.0001);
        }
    }
    #[test]
    fn facial_relief_and_smooth_normals_share_the_camera() {
        let c = camera();
        let face = super::super::test_face_geometry();
        let input = DepthSurface {
            camera: Some(c),
            image: ImageBuffer::from_pixel(128, 128, image::Luma([27000])),
            faces: vec![face.clone()],
            refined: true,
            subject: None,
            smooth_scene: false,
        };
        let out = crate::relight::surface_texture(&input);
        let at = |i: usize| sample(&out, [face[i][0], face[i][1]]);
        assert!(at(1)[2] > at(50)[2] && at(1)[2] > at(280)[2]);
        assert!(at(1)[3] > 0.99);
        let left = normal(c, [face[50][0], face[50][1]], at(50), 1.);
        let right = normal(c, [face[280][0], face[280][1]], at(280), 1.);
        assert!(left[0] * right[0] < 0., "{left:?} {right:?}");
        assert!(
            out.pixels()
                .all(|p| p.0.iter().all(|v| v.is_finite()) && p[2] > 0.)
        );
    }
}
