//! Camera geometry of a panorama: every frame is a pinhole camera (lens
//! corrections applied before) that rotates about its entrance pupil. Pairs
//! are registered with RANSAC homographies, the focal length is estimated
//! from them, rotations are chained along a maximum spanning tree and then
//! refined together with the focal length by bundle adjustment on all
//! matches; finally the panorama is levelled (the cameras' right vectors
//! span the horizon plane).
use nalgebra::{DMatrix, DVector, Matrix3, Rotation3, Vector2, Vector3};
use rayon::prelude::*;

/// A frame's camera: world → camera rotation, shared focal length (pixels),
/// principal point at the image centre.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub rotation: Rotation3<f64>,
    pub focal: f64,
    pub cx: f64,
    pub cy: f64,
}

impl Camera {
    /// World ray of a pixel.
    pub fn ray(&self, x: f64, y: f64) -> Vector3<f64> {
        self.rotation.inverse()
            * Vector3::new((x - self.cx) / self.focal, (y - self.cy) / self.focal, 1.0)
    }

    /// Pixel of a world ray, `None` behind the camera.
    pub fn project(&self, ray: &Vector3<f64>) -> Option<(f64, f64)> {
        let c = self.rotation * ray;
        (c.z > 1e-9).then(|| {
            (
                self.focal * c.x / c.z + self.cx,
                self.focal * c.y / c.z + self.cy,
            )
        })
    }
}

/// Matched points between frames `a` and `b` (pixels of each frame).
#[derive(Clone, Debug)]
pub struct PairMatches {
    pub a: usize,
    pub b: usize,
    pub points: Vec<(Vector2<f64>, Vector2<f64>)>,
    pub homography: Matrix3<f64>,
}

fn normalize(points: &[Vector2<f64>]) -> (Vec<Vector2<f64>>, Matrix3<f64>) {
    let n = points.len() as f64;
    let c = points.iter().fold(Vector2::zeros(), |s, p| s + p) / n;
    let d = points.iter().map(|p| (p - c).norm()).sum::<f64>() / n;
    let s = if d > 1e-12 {
        std::f64::consts::SQRT_2 / d
    } else {
        1.0
    };
    let t = Matrix3::new(s, 0.0, -s * c.x, 0.0, s, -s * c.y, 0.0, 0.0, 1.0);
    (points.iter().map(|p| (p - c) * s).collect(), t)
}

/// Homography a → b from ≥ 4 correspondences (normalised DLT).
pub fn fit_homography(pairs: &[(Vector2<f64>, Vector2<f64>)]) -> Option<Matrix3<f64>> {
    if pairs.len() < 4 {
        return None;
    }
    let (pa, ta) = normalize(&pairs.iter().map(|p| p.0).collect::<Vec<_>>());
    let (pb, tb) = normalize(&pairs.iter().map(|p| p.1).collect::<Vec<_>>());
    let mut a = DMatrix::<f64>::zeros(2 * pairs.len(), 9);
    for (i, (p, q)) in pa.iter().zip(&pb).enumerate() {
        let (x, y, u, v) = (p.x, p.y, q.x, q.y);
        a.row_mut(2 * i)
            .copy_from_slice(&[-x, -y, -1.0, 0.0, 0.0, 0.0, u * x, u * y, u]);
        a.row_mut(2 * i + 1)
            .copy_from_slice(&[0.0, 0.0, 0.0, -x, -y, -1.0, v * x, v * y, v]);
    }
    let ata = a.transpose() * &a;
    let eig = ata.symmetric_eigen();
    let (idx, _) = eig
        .eigenvalues
        .iter()
        .enumerate()
        .min_by(|x, y| x.1.total_cmp(y.1))?;
    let h = eig.eigenvectors.column(idx);
    let hn = Matrix3::new(h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], h[8]);
    let hm = tb.try_inverse()? * hn * ta;
    (hm[(2, 2)].abs() > 1e-12).then(|| hm / hm[(2, 2)])
}

pub fn apply_h(h: &Matrix3<f64>, p: &Vector2<f64>) -> Option<Vector2<f64>> {
    let v = h * Vector3::new(p.x, p.y, 1.0);
    (v.z.abs() > 1e-12).then(|| Vector2::new(v.x / v.z, v.y / v.z))
}

/// Deterministic pseudo-random numbers for RANSAC.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

/// RANSAC homography: inlier indices (reprojection error < `threshold`).
pub fn ransac_homography(
    pairs: &[(Vector2<f64>, Vector2<f64>)],
    threshold: f64,
    iterations: usize,
) -> Option<(Matrix3<f64>, Vec<usize>)> {
    if pairs.len() < 8 {
        return None;
    }
    let t2 = threshold * threshold;
    let inliers_of = |h: &Matrix3<f64>| -> Vec<usize> {
        pairs
            .iter()
            .enumerate()
            .filter(|(_, (p, q))| apply_h(h, p).is_some_and(|r| (r - q).norm_squared() < t2))
            .map(|(i, _)| i)
            .collect()
    };
    let best = (0..iterations)
        .into_par_iter()
        .map(|it| {
            let mut rng = Lcg(0x5DEECE66D ^ (it as u64 * 0x9E3779B97F4A7C15));
            let mut idx = [0usize; 4];
            for k in 0..4 {
                loop {
                    let c = rng.next(pairs.len());
                    if !idx[..k].contains(&c) {
                        idx[k] = c;
                        break;
                    }
                }
            }
            let sample: Vec<_> = idx.iter().map(|&i| pairs[i]).collect();
            match fit_homography(&sample) {
                Some(h) => (inliers_of(&h).len(), Some(h)),
                None => (0, None),
            }
        })
        .max_by_key(|(n, _)| *n)?;
    let mut h = best.1?;
    let mut inliers = inliers_of(&h);
    for _ in 0..3 {
        let sel: Vec<_> = inliers.iter().map(|&i| pairs[i]).collect();
        let Some(refit) = fit_homography(&sel) else {
            break;
        };
        let next = inliers_of(&refit);
        if next.len() < inliers.len() {
            break;
        }
        h = refit;
        inliers = next;
    }
    Some((h, inliers))
}

/// Focal length estimates from a homography between two rotated views
/// (principal points at the origin), after Szeliski & Shum.
fn focal_from_homography(h: &Matrix3<f64>) -> Option<f64> {
    let h = h / h[(2, 2)];
    let (h0, h1, h2, h3, h4, h5, h6, h7) = (
        h[(0, 0)],
        h[(0, 1)],
        h[(0, 2)],
        h[(1, 0)],
        h[(1, 1)],
        h[(1, 2)],
        h[(2, 0)],
        h[(2, 1)],
    );
    let mut f1 = None;
    let d1 = h6 * h7;
    let d2 = (h7 - h6) * (h7 + h6);
    let v1 = -(h0 * h1 + h3 * h4) / d1;
    let v2 = (h0 * h0 + h3 * h3 - h1 * h1 - h4 * h4) / d2;
    let f1sq = if d1.abs() > d2.abs() { v1 } else { v2 };
    if f1sq > 0.0 && d1.abs().max(d2.abs()) > 1e-12 {
        f1 = Some(f1sq.sqrt());
    }
    let mut f0 = None;
    let d1 = h0 * h3 + h1 * h4;
    let d2 = h0 * h0 + h1 * h1 - h3 * h3 - h4 * h4;
    let v1 = -h2 * h5 / d1;
    let v2 = (h5 * h5 - h2 * h2) / d2;
    let f0sq = if d1.abs() > d2.abs() { v1 } else { v2 };
    if f0sq > 0.0 && d1.abs().max(d2.abs()) > 1e-12 {
        f0 = Some(f0sq.sqrt());
    }
    match (f0, f1) {
        (Some(a), Some(b)) => Some((a * b).sqrt()),
        _ => None,
    }
}

/// The focal length (pixels) from the pairs' homographies; `fallback` when
/// none gives an estimate.
pub fn estimate_focal(pairs: &[PairMatches], centers: &[(f64, f64)], fallback: f64) -> f64 {
    let mut estimates: Vec<f64> = pairs
        .iter()
        .filter_map(|p| {
            let (ca, cb) = (centers[p.a], centers[p.b]);
            let ta = Matrix3::new(1.0, 0.0, -ca.0, 0.0, 1.0, -ca.1, 0.0, 0.0, 1.0);
            let tb = Matrix3::new(1.0, 0.0, cb.0, 0.0, 1.0, cb.1, 0.0, 0.0, 1.0);
            // h centred: b' = Tb⁻¹ H Ta⁻¹ a'
            let hc = tb.try_inverse()? * p.homography * ta.try_inverse()?;
            focal_from_homography(&hc)
        })
        .filter(|f| f.is_finite() && *f > fallback * 0.1 && *f < fallback * 10.0)
        .collect();
    if estimates.is_empty() {
        return fallback;
    }
    estimates.sort_by(|a, b| a.total_cmp(b));
    estimates[estimates.len() / 2]
}

/// Nearest rotation to `m` (SVD).
fn nearest_rotation(m: &Matrix3<f64>) -> Rotation3<f64> {
    let svd = m.svd(true, true);
    let (u, vt) = (svd.u.unwrap(), svd.v_t.unwrap());
    let mut r = u * vt;
    if r.determinant() < 0.0 {
        let mut u2 = u;
        u2.column_mut(2).neg_mut();
        r = u2 * vt;
    }
    Rotation3::from_matrix_unchecked(r)
}

/// Initial rotations along a maximum spanning tree of the match graph,
/// rooted at `root` (identity). Frames outside the tree stay `None`.
pub fn chain_rotations(
    n: usize,
    pairs: &[PairMatches],
    focal: f64,
    centers: &[(f64, f64)],
    root: usize,
) -> Vec<Option<Rotation3<f64>>> {
    let mut rotations: Vec<Option<Rotation3<f64>>> = vec![None; n];
    rotations[root] = Some(Rotation3::identity());
    let k = |c: (f64, f64)| Matrix3::new(focal, 0.0, c.0, 0.0, focal, c.1, 0.0, 0.0, 1.0);
    loop {
        // The strongest pair joining the tree to a new frame.
        let next = pairs
            .iter()
            .filter(|p| rotations[p.a].is_some() != rotations[p.b].is_some())
            .max_by_key(|p| p.points.len());
        let Some(p) = next else { break };
        // H_ab = K_b R_b R_aᵀ K_a⁻¹ → R_b R_aᵀ = K_b⁻¹ H K_a
        let Some(ka_inv) = k(centers[p.a]).try_inverse() else {
            break;
        };
        let Some(kb_inv) = k(centers[p.b]).try_inverse() else {
            break;
        };
        let rel = nearest_rotation(&(kb_inv * p.homography * k(centers[p.a])));
        let _ = ka_inv;
        match (rotations[p.a], rotations[p.b]) {
            (Some(ra), None) => rotations[p.b] = Some(rel * ra),
            (None, Some(rb)) => rotations[p.a] = Some(rel.inverse() * rb),
            _ => unreachable!(),
        }
    }
    rotations
}

/// Bundle adjustment of rotations (all but `fixed`) and the shared focal
/// length on all matches (symmetric transfer error, Huber-weighted,
/// Levenberg-Marquardt with a numeric Jacobian). Returns the RMS error.
pub fn bundle_adjust(
    cameras: &mut [Camera],
    pairs: &[PairMatches],
    fixed: usize,
    active: &[bool],
) -> f64 {
    let idx: Vec<Option<usize>> = {
        let mut k = 0;
        (0..cameras.len())
            .map(|i| {
                (active[i] && i != fixed).then(|| {
                    k += 1;
                    k - 1
                })
            })
            .collect()
    };
    let n_rot = idx.iter().flatten().count();
    let np = 3 * n_rot + 1;
    let pairs: Vec<&PairMatches> = pairs
        .iter()
        .filter(|p| active[p.a] && active[p.b])
        .collect();
    let pack = |cams: &[Camera]| -> DVector<f64> {
        let mut v = DVector::zeros(np);
        for (i, c) in cams.iter().enumerate() {
            if let Some(k) = idx[i] {
                let s = c.rotation.scaled_axis();
                v[3 * k] = s.x;
                v[3 * k + 1] = s.y;
                v[3 * k + 2] = s.z;
            }
        }
        v[np - 1] = cams[fixed].focal.ln();
        v
    };
    let unpack = |v: &DVector<f64>, cams: &mut [Camera]| {
        let f = v[np - 1].exp();
        for (i, c) in cams.iter_mut().enumerate() {
            if let Some(k) = idx[i] {
                c.rotation =
                    Rotation3::from_scaled_axis(Vector3::new(v[3 * k], v[3 * k + 1], v[3 * k + 2]));
            }
            c.focal = f;
        }
    };
    let residuals = |cams: &[Camera]| -> Vec<f64> {
        pairs
            .par_iter()
            .flat_map_iter(|p| {
                let (ca, cb) = (cams[p.a], cams[p.b]);
                p.points.iter().flat_map(move |(pa, pb)| {
                    let to_b = cb
                        .project(&ca.ray(pa.x, pa.y))
                        .unwrap_or((pb.x + 1e3, pb.y + 1e3));
                    let to_a = ca
                        .project(&cb.ray(pb.x, pb.y))
                        .unwrap_or((pa.x + 1e3, pa.y + 1e3));
                    [to_b.0 - pb.x, to_b.1 - pb.y, to_a.0 - pa.x, to_a.1 - pa.y]
                })
            })
            .collect()
    };
    let huber = 2.0;
    let cost = |r: &[f64]| -> f64 {
        r.iter()
            .map(|&e| {
                if e.abs() <= huber {
                    e * e
                } else {
                    2.0 * huber * e.abs() - huber * huber
                }
            })
            .sum()
    };
    let mut params = pack(cameras);
    let mut work: Vec<Camera> = cameras.to_vec();
    let mut r = residuals(&work);
    let mut c = cost(&r);
    let mut lambda = 1e-3;
    for _ in 0..60 {
        // Numeric Jacobian, one column per parameter.
        let cols: Vec<Vec<f64>> = (0..np)
            .into_par_iter()
            .map(|j| {
                let eps = if j == np - 1 { 1e-5 } else { 1e-6 };
                let mut p2 = params.clone();
                p2[j] += eps;
                let mut cams = work.clone();
                unpack(&p2, &mut cams);
                let r2 = residuals(&cams);
                r2.iter().zip(&r).map(|(a, b)| (a - b) / eps).collect()
            })
            .collect();
        let m = r.len();
        let weights: Vec<f64> = r
            .iter()
            .map(|&e| {
                if e.abs() <= huber {
                    1.0
                } else {
                    huber / e.abs()
                }
            })
            .collect();
        let mut jtj = DMatrix::<f64>::zeros(np, np);
        let mut jtr = DVector::<f64>::zeros(np);
        for a in 0..np {
            for b in a..np {
                let s: f64 = (0..m).map(|i| weights[i] * cols[a][i] * cols[b][i]).sum();
                jtj[(a, b)] = s;
                jtj[(b, a)] = s;
            }
            jtr[a] = (0..m).map(|i| weights[i] * cols[a][i] * r[i]).sum();
        }
        let mut improved = false;
        for _ in 0..8 {
            let mut lhs = jtj.clone();
            for d in 0..np {
                lhs[(d, d)] += lambda * jtj[(d, d)].max(1e-9);
            }
            let Some(delta) = lhs.lu().solve(&(-&jtr)) else {
                break;
            };
            let trial = &params + &delta;
            let mut cams = work.clone();
            unpack(&trial, &mut cams);
            let r2 = residuals(&cams);
            let c2 = cost(&r2);
            if c2 < c {
                let rel = (c - c2) / c.max(1e-12);
                params = trial;
                work = cams;
                r = r2;
                c = c2;
                lambda = (lambda * 0.3).max(1e-9);
                improved = rel > 1e-7;
                break;
            }
            lambda *= 10.0;
        }
        if !improved {
            break;
        }
    }
    cameras.copy_from_slice(&work);
    (r.iter().map(|e| e * e).sum::<f64>() / r.len().max(1) as f64).sqrt()
}

/// Levels the panorama: rotates the world so that the cameras' right
/// vectors lie in the horizontal plane and the average viewing direction
/// looks along +z.
pub fn level(cameras: &mut [Camera], active: &[bool]) {
    let mut cov = Matrix3::<f64>::zeros();
    let mut forward = Vector3::<f64>::zeros();
    let mut up_sum = Vector3::<f64>::zeros();
    for (c, _) in cameras.iter().zip(active).filter(|(_, a)| **a) {
        let inv = c.rotation.inverse();
        let x = inv * Vector3::x();
        cov += x * x.transpose();
        forward += inv * Vector3::z();
        up_sum += inv * Vector3::y();
    }
    let eig = cov.symmetric_eigen();
    let (i, _) = eig
        .eigenvalues
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))
        .unwrap();
    let mut up: Vector3<f64> = eig.eigenvectors.column(i).into();
    // Image y points down: keep "up" on the side of the cameras' y axes.
    if up.dot(&up_sum) < 0.0 {
        up = -up;
    }
    // A single row of frames that all look the same way has no defined
    // plane; keep the cameras' own mean y then.
    if eig.eigenvalues[i] > 0.5 * eig.eigenvalues.iter().cloned().fold(0.0, f64::max) {
        up = up_sum.normalize();
    }
    let z = (forward - up * forward.dot(&up)).normalize();
    let x = up.cross(&z).normalize();
    // World' basis: x, up (as y), z. R_world maps old world → new world.
    let m = Matrix3::from_rows(&[x.transpose(), up.transpose(), z.transpose()]);
    let r_world = Rotation3::from_matrix_unchecked(m);
    for c in cameras.iter_mut() {
        c.rotation *= r_world.inverse();
    }
}
