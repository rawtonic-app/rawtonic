// SPDX-License-Identifier: AGPL-3.0-or-later
//! Surface texture prepared once per analysis. Broad scene depth is fitted at
//! inference resolution; the optional facial mesh supplies smooth normals and
//! local relief without magnifying depth-map stair steps.
use super::{DepthSurface, surface_sample};
use image::{ImageBuffer, Rgba};
#[path = "relight_camera.rs"]
mod camera;
#[path = "relight_face_topology.rs"]
mod topology;
type Surface = ImageBuffer<Rgba<f32>, Vec<f32>>;
#[cfg(any(test, feature = "test-support"))]
#[path = "relight_face_fixture.rs"]
mod fixture;
/// Analytic renderer fixture only; runtime uses image-specific inferred points.
#[cfg(any(test, feature = "test-support"))]
pub fn test_face_geometry() -> Vec<[f32; 4]> {
    fixture::POSITIONS
        .iter()
        .map(|p| [0.5 + p[0] * 0.035, 0.48 - p[1] * 0.035, p[2] * 0.035, 1.])
        .collect()
}
const OVAL: [usize; 36] = [
    10, 338, 297, 332, 284, 251, 389, 356, 454, 323, 361, 288, 397, 365, 379, 378, 400, 377, 152,
    148, 176, 149, 150, 136, 172, 58, 132, 93, 234, 127, 162, 21, 54, 103, 67, 109,
];

fn sample(image: &Surface, uv: [f32; 2]) -> [f32; 4] {
    let x = (uv[0] * image.width() as f32 - 0.5).clamp(0., (image.width() - 1) as f32);
    let y = (uv[1] * image.height() as f32 - 0.5).clamp(0., (image.height() - 1) as f32);
    let x0 = x as u32;
    let y0 = y as u32;
    let a = image.get_pixel(x0, y0).0;
    let b = image.get_pixel((x0 + 1).min(image.width() - 1), y0).0;
    let c = image.get_pixel(x0, (y0 + 1).min(image.height() - 1)).0;
    let d = image
        .get_pixel(
            (x0 + 1).min(image.width() - 1),
            (y0 + 1).min(image.height() - 1),
        )
        .0;
    std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * x.fract();
        let bottom = c[i] + (d[i] - c[i]) * x.fract();
        top + (bottom - top) * y.fract()
    })
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn unit(v: [f32; 3]) -> [f32; 3] {
    let length = v.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-8);
    v.map(|v| v / length)
}

pub fn surface_texture(depth: &DepthSurface) -> Surface {
    surface_textures(depth).0
}

/// Keep the coarse scene separate: resampling its depth into the face atlas
/// would invent intermediate surfaces across an occlusion boundary.
pub fn surface_textures(depth: &DepthSurface) -> (Surface, Option<Surface>) {
    if let Some(camera) = depth.camera {
        let scene = self::camera::scene_texture(depth, camera);
        if depth.faces.is_empty() {
            return (scene, None);
        }
        return (
            self::camera::refine_faces(depth, camera, &scene),
            Some(scene),
        );
    }
    (legacy_texture(depth), None)
}

fn legacy_texture(depth: &DepthSurface) -> Surface {
    let (w, h) = depth.image.dimensions();
    let base = ImageBuffer::from_fn(w, h, |x, y| Rgba(surface_sample(&depth.image, x, y)));
    let base = if depth.refined {
        // Regularize the coarse scene directions inside each depth surface.
        // Keep height untouched and do not carry normals across silhouettes.
        ImageBuffer::from_fn(w, h, |x, y| {
            let center = base.get_pixel(x, y).0;
            let mut sum = [0.; 2];
            let mut total = 0.;
            for dy in -4i32..=4 {
                for dx in -4i32..=4 {
                    let (px, py) = (x as i32 + dx, y as i32 + dy);
                    if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                        continue;
                    }
                    let p = base.get_pixel(px as u32, py as u32).0;
                    let range = (1. - ((p[2] - center[2]) / 0.04).powi(2)).max(0.);
                    let weight = range * range * p[3] / (1. + (dx * dx + dy * dy) as f32);
                    sum[0] += p[0] * weight;
                    sum[1] += p[1] * weight;
                    total += weight;
                }
            }
            // Alpha identifies trustworthy facial relief for local shadows;
            // scene depth alone is not reliable enough for cast silhouettes.
            if total > 1e-6 {
                Rgba([sum[0] / total, sum[1] / total, center[2], 0.])
            } else {
                Rgba([0., 0., center[2], 0.])
            }
        })
    } else {
        base
    };
    if depth.faces.is_empty() {
        return base;
    }
    // Up-sample fitted derivatives, never the inference bitmap before fitting.
    let scale = (1024. / w.max(h) as f32).clamp(1., 2.);
    let (w, h) = (
        (w as f32 * scale).round() as u32,
        (h as f32 * scale).round() as u32,
    );
    let mut output = ImageBuffer::from_fn(w, h, |x, y| {
        Rgba(sample(
            &base,
            [(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32],
        ))
    });
    let short = w.min(h) as f32;
    let aspect = [w as f32 / short, h as f32 / short];
    let mut nearest = vec![f32::NEG_INFINITY; (w * h) as usize];
    for points in &depth.faces {
        let positions: Vec<[f32; 3]> = points
            .iter()
            .map(|p| [p[0] * aspect[0], p[1] * aspect[1], p[2]])
            .collect();
        let mut normals = vec![[0.; 3]; points.len()];
        for &[a, b, c] in topology::TRIANGLES {
            // Canonical topology is y-up; photograph coordinates are y-down.
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
        // Align facial relief to the scene, without pretending relative depth
        // gives a metric camera distance. Robust to a few occluded landmarks.
        let mut offsets: Vec<_> = points
            .iter()
            .enumerate()
            .filter(|(i, p)| p[3] > 0.8 && !OVAL.contains(i))
            .map(|(_, p)| sample(&base, [p[0], p[1]])[2] - p[2] / 0.25)
            .collect();
        if offsets.len() < 20 {
            continue;
        }
        offsets.sort_by(f32::total_cmp);
        let offset = offsets[offsets.len() / 2];
        for &[ia, ib, ic] in topology::TRIANGLES {
            let ids = [ia, ib, ic];
            let p = ids.map(|i| [points[i][0] * w as f32, points[i][1] * h as f32]);
            let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]);
            if area >= -1e-5 {
                continue;
            } // back-facing / degenerate
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
                    let q = [x as f32 + 0.5, y as f32 + 0.5];
                    let b = ((q[0] - p[0][0]) * (p[2][1] - p[0][1])
                        - (q[1] - p[0][1]) * (p[2][0] - p[0][0]))
                        / area;
                    let c = ((p[1][0] - p[0][0]) * (q[1] - p[0][1])
                        - (p[1][1] - p[0][1]) * (q[0] - p[0][0]))
                        / area;
                    let weights = [1. - b - c, b, c];
                    if weights.iter().any(|v| *v < -1e-5) {
                        continue;
                    }
                    let interpolate = |f: &dyn Fn(usize) -> f32| {
                        ids.iter().zip(weights).map(|(&i, a)| f(i) * a).sum::<f32>()
                    };
                    let relief = offset + interpolate(&|i| points[i][2]) / 0.25;
                    let index = (y * w + x) as usize;
                    if relief <= nearest[index] {
                        continue;
                    }
                    let n = unit(std::array::from_fn(|k| interpolate(&|i| normals[i][k])));
                    if n[2] < 0.08 {
                        continue;
                    }
                    let coverage =
                        interpolate(&|i| if OVAL.contains(&i) { 0. } else { points[i][3] })
                            .clamp(0., 1.);
                    if coverage <= 0.001 {
                        continue;
                    }
                    let face = [
                        -n[0] / n[2] * aspect[0] / 0.25,
                        -n[1] / n[2] * aspect[1] / 0.25,
                        relief,
                        1.,
                    ];
                    let prior = sample(
                        &base,
                        [(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32],
                    );
                    output.put_pixel(
                        x,
                        y,
                        Rgba(std::array::from_fn(|i| {
                            prior[i] + (face[i] - prior[i]) * coverage
                        })),
                    );
                    nearest[index] = relief;
                }
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn face_relief_preserves_nose_and_opposed_cheek_normals() {
        let face = test_face_geometry();
        let input = DepthSurface {
            camera: None,
            image: ImageBuffer::from_pixel(128, 128, image::Luma([32768])),
            faces: vec![face.clone()],
            refined: true,
            subject: None,
            smooth_scene: false,
        };
        let output = surface_texture(&input);
        let at = |i: usize| sample(&output, [face[i][0], face[i][1]]);
        let nose = at(1);
        let left = at(50);
        let right = at(280);
        assert!(
            nose[2] > left[2] + 0.1 && nose[2] > right[2] + 0.1,
            "nose/cheeks: {nose:?} {left:?} {right:?}"
        );
        assert!(
            left[0] > 0.5 && right[0] < -0.5,
            "opposed cheeks: {left:?} {right:?}"
        );
        assert!(nose[3] > 0.99 && left[3] > 0.99 && right[3] > 0.99);
        assert_eq!(output.get_pixel(0, 0).0, [0., 0., 32768. / 65535., 0.]);
        assert!(output.as_raw().iter().all(|v| v.is_finite()));
    }
}
