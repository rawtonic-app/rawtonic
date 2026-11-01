//! The multi-photo cue of Find Dust: sensor dust sits at the same sensor
//! position in every photo of a camera body, scene structure does not.
//!
//! A photo's candidates (down to the most sensitive setting) are looked up
//! in the other photos of the series at the same place in the raw (each
//! photo's EXIF orientation undone). Another photo **sees** the spot when it
//! is darker there in the shape of dust (round, even rim, soft) by a fair
//! share of the darkening; it is **clean** there when it would show such a
//! spot (flat, not too dark or bright, the darkening well above its noise,
//! taken at about the same aperture or a smaller one, where dust shows at
//! least as strongly) and is not darker. A photo whose surroundings of the
//! spot look the same (the same view: a tripod series, bracketing) is no
//! second witness and is left out for that spot. Seen elsewhere, a spot is
//! judged as at a sensitivity `SERIES_STEP` higher; clean in at least two
//! others and seen in none, `SERIES_STEP` lower (`Candidate::evidence`).
//! Dust another photo finds at its default that this photo shows only
//! faintly (no candidate of its own: too faint for the single-photo gates,
//! or among specks) is added when this photo sees it.
//!
//! A shadow moves a little with the lens (its exit pupil) away from the
//! frame's centre, so the search reaches further out there.

use super::{
    Candidate, DustAnalysis, Gates, HEAL_MARGIN, Integral, Measures, STEPS, WorkCopy, mean,
    measure_any, min_contrast, noise, plane, ring, shadow_reach,
};
use rayon::prelude::*;

/// One photo of a series: its work copy and analysis (`work_copy`,
/// `analyse_work`), base image size, EXIF orientation and aperture.
pub struct SeriesPhoto<'a> {
    pub work: &'a WorkCopy,
    pub analysis: &'a DustAnalysis,
    pub size: (u32, u32),
    pub orientation: u16,
    /// The f-number, when the metadata has it.
    pub f_number: Option<f32>,
}

/// Darkening another photo must show of the spot's (at its brightness) to
/// see it, and may show at most to be clean there.
const SEEN_SHARE: f32 = 0.4;
const CLEAN_SHARE: f32 = 0.25;
/// Significance (box-mean noise steps) a sighting needs, and the darkening
/// a clean verdict needs to expect (below that, noise could hide it).
const SEEN_SIGNIFICANCE: f32 = 4.0;
const CLEAN_EXPECTED: f32 = 6.0;
/// A clean photo's aperture may be at most this much wider (f-number
/// ratio): dust fades quickly as the aperture opens.
const CLEAN_APERTURE: f32 = 0.7;
/// Photos whose surroundings of the spot correlate at least this share of
/// what two copies of one view would reach show the same view there; the
/// view is told only where that reachable correlation is at least
/// `SAME_VIEW_STRUCTURE` (the surroundings have structure beyond noise).
const SAME_VIEW: f64 = 0.5;
const SAME_VIEW_STRUCTURE: f64 = 0.2;
/// Clean photos needed (and no sighting) to judge a spot lower.
const CLEAN_NEEDED: u16 = 2;
/// Reach of the search for the same shadow per distance from the frame's
/// centre (a lens change moves it radially).
const RADIAL_REACH: f64 = 0.01;

struct Prepared<'a> {
    photo: &'a SeriesPhoto<'a>,
    integral: Integral,
    sigma: f32,
}

impl<'a> Prepared<'a> {
    fn new(photo: &'a SeriesPhoto<'a>) -> Self {
        let w = photo.work;
        let integral = Integral::new(&w.values, w.width, w.height);
        let sigma = noise(&w.values, w.width, w.height, &integral);
        Self {
            photo,
            integral,
            sigma,
        }
    }

    fn factor(&self) -> f64 {
        self.photo.work.factor as f64
    }

    /// Normalised raw position of a work pixel.
    fn raw(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let (w, h) = self.photo.size;
        let base = ((x + 0.5) * self.factor(), (y + 0.5) * self.factor());
        super::unoriented(self.photo.orientation, w, h, base)
    }

    /// Work pixel of a normalised raw position.
    fn work_at(&self, (u, v): (f64, f64)) -> (f64, f64) {
        let (w, h) = self.photo.size;
        let (x, y) = crate::lightroom_retouch::orient_point(self.photo.orientation, u, v);
        (
            x * f64::from(w) / self.factor() - 0.5,
            y * f64::from(h) / self.factor() - 0.5,
        )
    }

    fn value(&self, x: isize, y: isize) -> Option<f32> {
        let w = self.photo.work;
        (x >= 0 && y >= 0 && (x as usize) < w.width && (y as usize) < w.height)
            .then(|| w.values[y as usize * w.width + x as usize])
    }

    /// The unrotated raw size: photos of one series must share it.
    fn raw_size(&self) -> (u32, u32) {
        let (w, h) = self.photo.size;
        if (5..=8).contains(&self.photo.orientation) {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// Noise of a box mean of half-size `a`.
    fn box_noise(&self, a: isize) -> f32 {
        self.sigma / (2 * a + 1) as f32
    }
}

/// What a photo shows of a spot of relative darkening `contrast` and box
/// half-size `a` near `at` (work pixels), searched within `reach` more.
enum Sighting {
    /// It could not show it (textured, too dark or bright, too noisy).
    Unknown,
    Seen((isize, isize), Measures),
    Clean,
}

fn look(p: &Prepared, at: (f64, f64), a: isize, contrast: f32, reach: f64) -> Sighting {
    let w = p.photo.work;
    let (cx, cy) = (at.0.round() as isize, at.1.round() as isize);
    let t = a + 1 + reach.round() as isize;
    let m = 2 * a + 1;
    let mut best: Option<(f64, (isize, isize))> = None;
    for y in (cy - t).max(m)..=(cy + t).min(w.height as isize - 1 - m) {
        for x in (cx - t).max(m)..=(cx + t).min(w.width as isize - 1 - m) {
            let d = mean(ring(&p.integral, x, y, a, m)) - mean(p.integral.square(x, y, a));
            if best.is_none_or(|b| d > b.0) {
                best = Some((d, (x, y)));
            }
        }
    }
    let Some((_, spot)) = best else {
        return Sighting::Unknown;
    };
    let Some(measures) = measure_any(&w.values, &p.integral, spot, a, p.sigma) else {
        return Sighting::Unknown;
    };
    let loose = Gates::DEFAULT.loosened(1.0);
    let level = measures.level;
    let expected = contrast * level;
    let informative = (loose.min_level..=loose.max_level).contains(&level)
        && measures.texture <= loose.texture_per_contrast * expected
        && measures.texture <= loose.max_texture * level;
    if !informative {
        return Sighting::Unknown;
    }
    let d = measures.darkening;
    let noise = p.box_noise(a);
    // Seen in the shape single-photo dust needs at the default: a streak
    // of cloud or a line looks only loosely like dust.
    let strict = Gates::DEFAULT;
    let shaped = measures.rim <= strict.rim_spread * d
        && measures.quarter >= strict.min_quarter * d
        && measures.peak <= strict.max_peak * d;
    if d >= SEEN_SHARE * expected && d >= SEEN_SIGNIFICANCE * noise && shaped {
        Sighting::Seen(spot, measures)
    } else if d < CLEAN_SHARE * expected && expected >= CLEAN_EXPECTED * noise {
        Sighting::Clean
    } else {
        Sighting::Unknown
    }
}

/// Whether `p` at `at` and `q` at the same raw place show the same view:
/// the correlation of their surroundings (the spot itself left out) around
/// a plane each.
fn same_view(p: &Prepared, at: (f64, f64), q: &Prepared, a: isize) -> bool {
    let m = 2 * a + 1;
    let r = (3 * m).max(8);
    let stride = ((2 * r + 1) / 30).max(1) as usize;
    let mut ps = Vec::new();
    let mut qs = Vec::new();
    for dy in (-r..=r).step_by(stride) {
        for dx in (-r..=r).step_by(stride) {
            if dx.abs().max(dy.abs()) <= m {
                continue;
            }
            let (x, y) = (at.0 + dx as f64, at.1 + dy as f64);
            let Some(vp) = p.value(x.round() as isize, y.round() as isize) else {
                continue;
            };
            let (qx, qy) = q.work_at(p.raw((x, y)));
            let Some(vq) = q.value(qx.round() as isize, qy.round() as isize) else {
                continue;
            };
            ps.push((dx as f32, dy as f32, vp));
            qs.push((dx as f32, dy as f32, vq));
        }
    }
    if ps.len() < 16 {
        return false;
    }
    let (Some(fp), Some(fq)) = (plane(&ps), plane(&qs)) else {
        return false;
    };
    let residual = |f: (f64, f64, f64), s: &(f32, f32, f32)| {
        f64::from(s.2) - (f.0 * f64::from(s.0) + f.1 * f64::from(s.1) + f.2)
    };
    let (mut pq, mut pp, mut qq) = (0.0, 0.0, 0.0);
    for (sp, sq) in ps.iter().zip(&qs) {
        let (rp, rq) = (residual(fp, sp), residual(fq, sq));
        pq += rp * rq;
        pp += rp * rp;
        qq += rq * rq;
    }
    #[cfg(test)]
    if std::env::var("DUST_SERIES_ANYVIEW").is_ok() {
        return false;
    }
    if pp <= 0.0 || qq <= 0.0 {
        return false;
    }
    // The correlation two noisy copies of one view reach: their structure
    // (spread beyond photon noise) over their whole spread. Where both are
    // all noise (flat sky) the view cannot be told and is not the same.
    let n = ps.len() as f64;
    let structure = |sum: f64, sigma: f32| ((sum / n) - f64::from(sigma).powi(2)).max(0.0);
    let reachable = (structure(pp, p.sigma) * structure(qq, q.sigma) / (pp / n * (qq / n))).sqrt();
    reachable >= SAME_VIEW_STRUCTURE && pq / (pp * qq).sqrt() > SAME_VIEW * reachable
}

/// Whether two photos show the same view as a whole (a burst, a tripod
/// series, one frame saved in several raw modes): the correlation of their
/// band-passed frames (8-pixel means less their 40-pixel surroundings) at
/// the same raw places, against what two noisy copies of one view reach.
fn same_frame(p: &Prepared, q: &Prepared) -> bool {
    let w = p.photo.work;
    let (fine, coarse) = (4isize, 20isize);
    let mut pairs = Vec::new();
    for y in (coarse..w.height as isize - coarse).step_by(8) {
        for x in (coarse..w.width as isize - coarse).step_by(8) {
            let band = |r: &Prepared, x: isize, y: isize| {
                mean(r.integral.square(x, y, fine)) - mean(r.integral.square(x, y, coarse))
            };
            let (qx, qy) = q.work_at(p.raw((x as f64, y as f64)));
            let (qx, qy) = (qx.round() as isize, qy.round() as isize);
            let qw = q.photo.work;
            if qx < coarse
                || qy < coarse
                || qx >= qw.width as isize - coarse
                || qy >= qw.height as isize - coarse
            {
                continue;
            }
            pairs.push((band(p, x, y), band(q, qx, qy)));
        }
    }
    if pairs.len() < 64 {
        return false;
    }
    let n = pairs.len() as f64;
    let (mp, mq) = (
        pairs.iter().map(|v| v.0).sum::<f64>() / n,
        pairs.iter().map(|v| v.1).sum::<f64>() / n,
    );
    let (mut pq, mut pp, mut qq) = (0.0, 0.0, 0.0);
    for (a, b) in &pairs {
        pq += (a - mp) * (b - mq);
        pp += (a - mp) * (a - mp);
        qq += (b - mq) * (b - mq);
    }
    pp > 0.0 && qq > 0.0 && pq / (pp * qq).sqrt() > SAME_VIEW
}

/// Search reach for the same shadow at work pixel `at` of `p`.
fn reach(p: &Prepared, at: (f64, f64)) -> f64 {
    let w = p.photo.work;
    let (dx, dy) = (at.0 - w.width as f64 / 2.0, at.1 - w.height as f64 / 2.0);
    RADIAL_REACH * dx.hypot(dy)
}

/// What the other photos of a series said about a photo's candidates
/// (`SeriesTarget::gather`, one photo at a time).
#[derive(Debug, Clone, Default)]
pub struct SeriesEvidence {
    seen: Vec<u16>,
    clean: Vec<u16>,
    /// Dust found in other photos that this one shows only faintly.
    added: Vec<(Candidate, u16)>,
}

/// The photo Find Dust runs on, ready to gather its series' evidence.
pub struct SeriesTarget<'a> {
    target: Prepared<'a>,
}

impl<'a> SeriesTarget<'a> {
    pub fn new(photo: &'a SeriesPhoto<'a>) -> Self {
        Self {
            target: Prepared::new(photo),
        }
    }

    /// No evidence yet.
    pub fn evidence(&self) -> SeriesEvidence {
        let n = self.target.photo.analysis.candidates.len();
        SeriesEvidence {
            seen: vec![0; n],
            clean: vec![0; n],
            added: Vec::new(),
        }
    }

    /// Adds what `other` (another photo of the same camera and frame) says.
    /// A photo with another raw size says nothing.
    pub fn gather(&self, other: &SeriesPhoto, into: &mut SeriesEvidence) {
        let p = &self.target;
        let q = Prepared::new(other);
        if p.raw_size() != q.raw_size() {
            return;
        }
        // Sightings in the same view tell real darkening from noise, not
        // dust from the scene; a clean verdict holds in any view.
        let whole_view = same_frame(p, &q);
        #[cfg(test)]
        if std::env::var("DUST_SERIES_DEBUG").is_ok() {
            eprintln!("same frame: {whole_view}");
        }
        let aperture_ok = match (p.photo.f_number, q.photo.f_number) {
            (Some(fp), Some(fq)) => fq >= CLEAN_APERTURE * fp,
            _ => false,
        };
        // This photo's candidates, looked up there.
        let verdicts: Vec<(bool, bool)> = p
            .photo
            .analysis
            .candidates
            .par_iter()
            .map(|c| {
                let at = (c.centre.0 / p.factor() - 0.5, c.centre.1 / p.factor() - 0.5);
                let a = ((c.size / p.factor()).round() as isize).max(1);
                let there = q.work_at(p.raw(at));
                #[cfg(test)]
                if std::env::var("DUST_SERIES_DEBUG").is_ok() {
                    debug_look(&q, there, a, c.contrast, reach(&q, there), p, at);
                }
                match look(&q, there, a, c.contrast, reach(&q, there)) {
                    Sighting::Unknown => (false, false),
                    Sighting::Seen(..) if whole_view => (false, false),
                    _ if same_view(p, at, &q, a) => (false, false),
                    Sighting::Seen(..) => (true, false),
                    Sighting::Clean => (false, aperture_ok),
                }
            })
            .collect();
        for (k, (seen, clean)) in verdicts.into_iter().enumerate() {
            into.seen[k] += u16::from(seen);
            into.clean[k] += u16::from(clean);
        }
        // Dust that photo finds at its default, looked up here when this
        // photo has no candidate there.
        if whole_view {
            return;
        }
        let own = &p.photo.analysis.candidates;
        let at_default = min_contrast(50.0);
        let found: Vec<Candidate> = other
            .analysis
            .candidates
            .par_iter()
            .filter(|c| c.evidence == 0 && c.looseness == 0.0 && c.contrast >= at_default)
            .filter_map(|c| {
                let qat = (c.centre.0 / q.factor() - 0.5, c.centre.1 / q.factor() - 0.5);
                let a = ((c.size / q.factor()).round() as isize).max(1);
                let here = p.work_at(q.raw(qat));
                let base = ((here.0 + 0.5) * p.factor(), (here.1 + 0.5) * p.factor());
                let taken = own.iter().any(|o| {
                    (o.centre.0 - base.0).hypot(o.centre.1 - base.1) < o.radius.max(c.radius)
                });
                if taken {
                    return None;
                }
                #[cfg(test)]
                if std::env::var("DUST_SERIES_DEBUG").is_ok() {
                    eprint!("extra from {:.0},{:.0}: ", c.centre.0, c.centre.1);
                    debug_look(p, here, a, c.contrast, reach(p, here), &q, qat);
                }
                let Sighting::Seen(spot, m) = look(p, here, a, c.contrast, reach(p, here)) else {
                    return None;
                };
                let spot_f = (spot.0 as f64, spot.1 as f64);
                if same_view(p, spot_f, &q, a) {
                    return None;
                }
                let gates = Gates::DEFAULT;
                let contrast = m.contrast();
                if !(min_contrast(100.0)..=gates.max_contrast).contains(&contrast) {
                    return None;
                }
                let level =
                    (0..=STEPS).find(|&k| m.passes(&gates.loosened(k as f32 / STEPS as f32)))?;
                let w = p.photo.work;
                let shadow = shadow_reach(
                    &w.values,
                    (w.width, w.height),
                    (spot.0 as usize, spot.1 as usize),
                    a as usize,
                    m.level,
                );
                Some(Candidate {
                    centre: ((spot_f.0 + 0.5) * p.factor(), (spot_f.1 + 0.5) * p.factor()),
                    radius: (HEAL_MARGIN * (shadow + 1.0) * p.factor()).max(4.0),
                    contrast,
                    looseness: level as f32 / STEPS as f32,
                    size: a as f64 * p.factor(),
                    evidence: 1,
                })
            })
            .collect();
        for c in found {
            match into.added.iter_mut().find(|(o, _)| {
                (o.centre.0 - c.centre.0).hypot(o.centre.1 - c.centre.1) < o.radius.max(c.radius)
            }) {
                Some((_, n)) => *n += 1,
                None => into.added.push((c, 1)),
            }
        }
    }

    /// The target's analysis with the evidence: each candidate's
    /// `evidence` set, and the dust found through the other photos added
    /// (but in the face boxes `faces`, as `analyse` leaves them out).
    pub fn apply(&self, evidence: &SeriesEvidence, faces: &[[f64; 4]]) -> DustAnalysis {
        let analysis = self.target.photo.analysis;
        let mut candidates: Vec<Candidate> = analysis
            .candidates
            .iter()
            .enumerate()
            .map(|(k, c)| {
                let (seen, clean) = (
                    evidence.seen.get(k).copied().unwrap_or(0),
                    evidence.clean.get(k).copied().unwrap_or(0),
                );
                // Dust shows wherever it can: seen in fewer photos than
                // show the place clean, a sighting was chance.
                let evidence = if seen > 0 && seen >= clean {
                    1
                } else if clean >= CLEAN_NEEDED {
                    -1
                } else {
                    0
                };
                Candidate { evidence, ..*c }
            })
            .collect();
        candidates.extend(
            evidence
                .added
                .iter()
                .map(|(c, _)| *c)
                .filter(|c| !super::in_face(faces, c.centre)),
        );
        DustAnalysis {
            width: analysis.width,
            height: analysis.height,
            candidates,
        }
    }
}

#[cfg(test)]
fn debug_look(
    q: &Prepared,
    there: (f64, f64),
    a: isize,
    contrast: f32,
    reach: f64,
    p: &Prepared,
    at: (f64, f64),
) {
    let w = q.photo.work;
    let (cx, cy) = (there.0.round() as isize, there.1.round() as isize);
    let t = a + 1 + reach.round() as isize;
    let m = 2 * a + 1;
    let mut best: Option<(f64, (isize, isize))> = None;
    for y in (cy - t).max(m)..=(cy + t).min(w.height as isize - 1 - m) {
        for x in (cx - t).max(m)..=(cx + t).min(w.width as isize - 1 - m) {
            let d = mean(ring(&q.integral, x, y, a, m)) - mean(q.integral.square(x, y, a));
            if best.is_none_or(|b| d > b.0) {
                best = Some((d, (x, y)));
            }
        }
    }
    let Some((_, spot)) = best else { return };
    let ms = measure_any(&w.values, &q.integral, spot, a, q.sigma);
    let verdict = match look(q, there, a, contrast, reach) {
        Sighting::Unknown => "unknown",
        Sighting::Seen(..) => "seen",
        Sighting::Clean => "clean",
    };
    eprintln!(
        "at {:.0},{:.0} a{a} c{contrast:.4} -> {:?} noise {:.5} same {} {verdict} {:?}",
        at.0,
        at.1,
        spot,
        q.box_noise(a),
        same_view(p, at, q, a),
        ms.map(|m| (m.level, m.darkening, m.texture, m.rim, m.quarter, m.peak))
    );
}
