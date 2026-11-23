//! Automatic lens detection: the Lensfun lens a photo was taken with, from
//! what its EXIF map says about the lens — the EXIF lens name, the lens's
//! focal range and widest aperture (LensSpecification, the name) and the
//! lenses its maker-note lens ID fits (`LensCandidates`, filled by the
//! EXIF reader where the ID is shared, e.g. one Canon LensType for several
//! third-party lenses). A lens whose facts contradict the photo's is never
//! picked, and where several lenses remain and nothing tells them apart
//! there is no pick: the candidates are offered instead of a guess.
//! notes: benchmark-results/lns1-2026-11-23/REPORT.md
use crate::lens_correction::{
    Lens, LensDatabase, body_crop_factor, camera_mounts, find_lens_by_camera_mount,
    find_lens_by_fuzzy_model, lens_result, lenses_for_maker, normalized_lens_name,
    without_model_code,
};
use std::collections::HashMap;

/// EXIF map key: the lenses the maker note's lens ID fits, full names
/// (maker first) separated by `LENS_CANDIDATES_SEPARATOR`; one name where
/// the maker note names one lens but the EXIF lens name does not.
pub const LENS_CANDIDATES: &str = "LensCandidates";
pub const LENS_CANDIDATES_SEPARATOR: &str = "; ";
/// EXIF map key, set once the reader has looked into the maker note for
/// the lens (`needs_maker_note_lens`).
pub const MAKER_NOTE_LENS_READ: &str = "MakerNoteLensRead";

/// What the photo says about its lens.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LensFacts {
    /// The camera's maker (EXIF Make).
    pub maker: String,
    /// The EXIF lens name (LensModel).
    pub lens_name: String,
    /// The camera model (EXIF Model).
    pub camera_model: String,
    /// The lens's focal range in mm (the name, else LensSpecification).
    pub focal_range: Option<(f32, f32)>,
    /// The lens's widest aperture at its shortest focal length.
    pub max_aperture: Option<f32>,
    /// The lenses the maker-note lens ID fits (maker first).
    pub candidates: Vec<String>,
    /// The frame's crop factor, for choosing among calibrations of one lens.
    pub frame_crop: Option<f32>,
}

impl LensFacts {
    pub fn from_exif(exif: &HashMap<String, String>) -> Self {
        let field = |key: &str| {
            exif.get(key)
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        };
        let lens_name = field("LensModel");
        let (spec_focal, spec_aperture) = exif
            .get("LensSpecification")
            .map(|spec| parse_specification(spec))
            .unwrap_or((None, None));
        let (name_focal, name_aperture) = name_spec(&lens_name);
        let candidates = exif
            .get(LENS_CANDIDATES)
            .map(|list| {
                list.split(LENS_CANDIDATES_SEPARATOR)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let number = |key: &str| {
            exif.get(key)
                .and_then(|v| v.split_whitespace().next()?.parse::<f32>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
        };
        let frame_crop = number("FocalLengthIn35mmFilm")
            .zip(number("FocalLength"))
            .map(|(equivalent, focal)| equivalent / focal)
            .filter(|crop| (crop - 1.0).abs() > 1e-3);
        Self {
            maker: field("Make"),
            lens_name,
            camera_model: field("Model"),
            // The name before LensSpecification: an adapter writes the
            // specification of no lens (f/1.1 for an f/1.8 lens), never
            // the lens's own name.
            focal_range: name_focal.or(spec_focal),
            max_aperture: name_aperture.or(spec_aperture),
            candidates,
            frame_crop,
        }
    }

    /// Only the maker, lens name and camera (callers without an EXIF map).
    pub fn from_names(maker: &str, lens_name: &str, camera_model: &str) -> Self {
        let (focal_range, max_aperture) = name_spec(lens_name);
        Self {
            maker: maker.trim().to_string(),
            lens_name: lens_name.trim().trim_matches('"').to_string(),
            camera_model: camera_model.trim().to_string(),
            focal_range,
            max_aperture,
            ..Self::default()
        }
    }
}

/// The detected lens (maker, display name), or none and the lenses it
/// could be.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct LensDetection {
    pub lens: Option<(String, String)>,
    pub candidates: Vec<(String, String)>,
}

impl LensDetection {
    fn picked(lens: (String, String)) -> Self {
        Self {
            lens: Some(lens),
            candidates: Vec::new(),
        }
    }
}

/// Whether a lens name states its aperture ("EF50mm f/1.2L USM", "FE
/// 24-70mm F2.8 GM", "Summicron-M 1:2/50"): a name that does not ("24-70mm",
/// "----") does not identify the lens, so the maker note is read too.
pub fn names_aperture(name: &str) -> bool {
    name_spec(name).1.is_some()
}

/// The focal range and widest aperture a lens name states: "24-70mm
/// f/2.8", "17-55mm F2.8", "EF50mm f/1.2L", Leica's "1:2.8-4/24-90".
pub fn name_spec(name: &str) -> (Option<(f32, f32)>, Option<f32>) {
    use std::sync::OnceLock;
    static FOCAL: OnceLock<regex::Regex> = OnceLock::new();
    static APERTURE: OnceLock<regex::Regex> = OnceLock::new();
    static LEICA: OnceLock<regex::Regex> = OnceLock::new();
    let focal = FOCAL.get_or_init(|| {
        regex::Regex::new(r"(?i)(\d+(?:\.\d+)?)(?:\s*-\s*(\d+(?:\.\d+)?))?\s*mm").unwrap()
    });
    let aperture = APERTURE.get_or_init(|| {
        regex::Regex::new(r"(?:^|[^A-Za-z]|mm)[fF]\s*/?\s*(\d+(?:\.\d+)?)").unwrap()
    });
    let leica = LEICA.get_or_init(|| {
        regex::Regex::new(r"1\s*:\s*(\d+(?:\.\d+)?)(?:\s*-\s*\d+(?:\.\d+)?)?\s*/\s*(\d+(?:\.\d+)?)(?:\s*-\s*(\d+(?:\.\d+)?))?")
            .unwrap()
    });
    let num = |m: Option<regex::Match>| m.and_then(|m| m.as_str().parse::<f32>().ok());
    let range = |a: Option<f32>, b: Option<f32>| {
        let a = a.filter(|v| *v > 0.0)?;
        let b = b.filter(|v| *v >= a).unwrap_or(a);
        Some((a, b))
    };
    let plausible = |f: f32| (0.7..=32.0).contains(&f);
    if let Some(c) = leica.captures(name) {
        let f = num(c.get(1)).filter(|f| plausible(*f));
        return (range(num(c.get(2)), num(c.get(3))), f);
    }
    let focal_range = focal
        .captures(name)
        .and_then(|c| range(num(c.get(1)), num(c.get(2))));
    let wide = aperture
        .captures(name)
        .and_then(|c| num(c.get(1)))
        .filter(|f| plausible(*f));
    (focal_range, wide)
}

/// LensSpecification as the EXIF map writes it: "24-70 mm, f/2.8-4",
/// "50 mm", "24-70 mm" (zero parts are not stated).
fn parse_specification(spec: &str) -> (Option<(f32, f32)>, Option<f32>) {
    let (focal, aperture) = match spec.split_once(',') {
        Some((focal, aperture)) => (focal, Some(aperture)),
        None => (spec, None),
    };
    let numbers = |text: &str| -> Vec<f32> {
        text.split(|c: char| c == '-' || c.is_whitespace() || c == '/')
            .filter_map(|part| part.trim_start_matches(['f', 'F']).parse::<f32>().ok())
            .collect()
    };
    let focal = numbers(focal.trim().trim_end_matches("mm"));
    let focal_range = match focal.as_slice() {
        [a] if *a > 0.0 => Some((*a, *a)),
        [a, b, ..] if *a > 0.0 && *b >= *a => Some((*a, *b)),
        [a, b, ..] if *a > 0.0 && *b == 0.0 => Some((*a, *a)),
        _ => None,
    };
    let wide = aperture
        .and_then(|a| numbers(a).first().copied())
        .filter(|f| (0.7..=32.0).contains(f));
    (focal_range, wide)
}

/// The focal range and widest aperture a Lensfun lens has: its <focal> and
/// <aperture> where stated, else what its name says.
fn lens_spec(lens: &Lens) -> (Option<(f32, f32)>, Option<f32>) {
    let (name_focal, name_aperture) = name_spec(&lens.get_canonical_model_name());
    let (en_focal, en_aperture) = name_spec(&lens.get_full_model_name());
    let stated_focal = lens.focal.as_ref().and_then(|f| {
        let min = f.min.or(f.value)?;
        Some((min, f.max.unwrap_or(min)))
    });
    let stated_aperture = lens.aperture.as_ref().and_then(|a| a.min);
    (
        stated_focal.or(name_focal).or(en_focal),
        stated_aperture.or(name_aperture).or(en_aperture),
    )
}

fn close(a: f32, b: f32, relative: f32, absolute: f32) -> bool {
    (a - b).abs() <= absolute.max(relative * a.abs().max(b.abs()))
}

/// Whether a Lensfun lens can be the photo's: its focal range and widest
/// aperture, where both sides state them, agree (half a millimetre or 3 %;
/// an aperture within a sixth of a stop and rounding).
fn consistent(lens: &Lens, facts: &LensFacts) -> bool {
    let (focal, aperture) = lens_spec(lens);
    let focal_ok = match (facts.focal_range, focal) {
        (Some((a0, a1)), Some((b0, b1))) => close(a0, b0, 0.03, 0.5) && close(a1, b1, 0.03, 0.5),
        _ => true,
    };
    let aperture_ok = match (facts.max_aperture, aperture) {
        (Some(a), Some(b)) => close(a, b, 0.07, 0.05),
        _ => true,
    };
    focal_ok && aperture_ok
}

/// A lens Lensfun lists for one camera body only (its English name is
/// "fixed lens"): found through the camera's mount, never by name.
fn is_fixed_lens(lens: &Lens) -> bool {
    lens.model.iter().any(|name| {
        name.lang.as_deref() == Some("en")
            && name.value.trim().to_lowercase().starts_with("fixed lens")
    })
}

/// The Lensfun lenses (every calibration of it) whose name is a maker-note
/// candidate's: same maker, same name once normalised.
fn lensfun_lenses_named<'a>(db: &'a LensDatabase, full_name: &str) -> Vec<&'a Lens> {
    db.lenses
        .iter()
        .filter(|lens| lens.calibration.is_some() && !is_fixed_lens(lens))
        .filter(|lens| {
            let maker = lens.get_maker();
            let lower = full_name.trim().to_ascii_lowercase();
            if !lower.starts_with(&maker.to_ascii_lowercase()) {
                return false;
            }
            let wanted = normalized_lens_name(full_name, &maker);
            wanted.len() >= 4
                && lens
                    .model
                    .iter()
                    .any(|name| normalized_lens_name(&name.value, &maker) == wanted)
        })
        .collect()
}

/// One lens with several calibrations (Lensfun entries of one name for
/// different crop factors): the one measured on the frame closest to the
/// photo's without being smaller (a calibration covers its own frame), else
/// the largest frame.
fn best_calibration<'a>(variants: &[&'a Lens], crop: Option<f32>) -> Option<&'a Lens> {
    let crop_of = |lens: &Lens| lens.cropfactor.unwrap_or(1.0);
    match crop {
        Some(crop) => variants
            .iter()
            .filter(|lens| crop_of(lens) <= crop * 1.03)
            .max_by(|a, b| crop_of(a).total_cmp(&crop_of(b)))
            .or_else(|| {
                variants
                    .iter()
                    .min_by(|a, b| crop_of(a).total_cmp(&crop_of(b)))
            })
            .copied(),
        None => variants
            .iter()
            .min_by(|a, b| crop_of(a).total_cmp(&crop_of(b)))
            .copied(),
    }
}

fn named(db: &LensDatabase, lens: &Lens) -> (String, String) {
    lens_result(db, lens)
}

/// The lens a photo was taken with (`LensDetection`).
pub fn detect_lens(db: &LensDatabase, facts: &LensFacts) -> LensDetection {
    let crop = body_crop_factor(db, &facts.maker, &facts.camera_model, facts.frame_crop);
    if !facts.candidates.is_empty() {
        return from_candidates(db, facts, crop);
    }
    if let Some(found) = from_name(db, facts) {
        return found;
    }
    find_lens_by_camera_mount(db, &facts.maker, &facts.camera_model)
        .map(|lens| LensDetection::picked(named(db, lens)))
        .unwrap_or_default()
}

/// The Lensfun lenses of a maker-note candidate's maker whose focal range
/// and widest aperture are the candidate's (both stated) and the photo's.
fn same_spec_lenses<'a>(db: &'a LensDatabase, full_name: &str, facts: &LensFacts) -> Vec<&'a Lens> {
    let (focal, aperture) = name_spec(full_name);
    if focal.is_none() || aperture.is_none() {
        return Vec::new();
    }
    let own = LensFacts {
        focal_range: focal,
        max_aperture: aperture,
        ..LensFacts::default()
    };
    let lower = full_name.trim().to_ascii_lowercase();
    db.lenses
        .iter()
        .filter(|lens| lens.calibration.is_some() && !is_fixed_lens(lens))
        .filter(|lens| lower.starts_with(&lens.get_maker().to_ascii_lowercase()))
        .filter(|lens| {
            let (f, a) = lens_spec(lens);
            f.is_some() && a.is_some() && consistent(lens, &own) && consistent(lens, facts)
        })
        .collect()
}

/// The maker note's lens candidates that fit the photo's facts: one with a
/// Lensfun profile is picked, several are offered.
fn from_candidates(db: &LensDatabase, facts: &LensFacts, crop: Option<f32>) -> LensDetection {
    let matcher = fuzzy_matcher::skim::SkimMatcherV2::default().ignore_case();
    use fuzzy_matcher::FuzzyMatcher;
    // Each candidate with its Lensfun calibrations (none: a lens without a
    // profile, which still rules out a pick among the others).
    let mut found: Vec<(&str, Vec<&Lens>)> = facts
        .candidates
        .iter()
        .map(|name| {
            let lenses: Vec<&Lens> = lensfun_lenses_named(db, name)
                .into_iter()
                .filter(|lens| consistent(lens, facts))
                .collect();
            (name.as_str(), lenses)
        })
        .collect();
    // A candidate Lensfun spells differently ("AF 17-50mm f/2.8 Di-II LD
    // Aspherical" / "SP AF 17-50mm f/2.8 XR Di II LD Aspherical (IF)"): its
    // maker's lenses of the same focal range and aperture, offered only.
    let mut spelled_otherwise: Vec<&Lens> = Vec::new();
    for (name, lenses) in &found {
        if lenses.is_empty() {
            spelled_otherwise.extend(same_spec_lenses(db, name, facts));
        }
    }
    // An EXIF lens name that fits some candidates but not all narrows them.
    let exif_name = facts.lens_name.trim();
    if found.len() > 1 && exif_name.len() >= 4 {
        let fits: Vec<bool> = found
            .iter()
            .map(|(name, _)| matcher.fuzzy_match(name, exif_name).is_some())
            .collect();
        if fits.iter().any(|f| *f) && fits.iter().any(|f| !*f) {
            found = found
                .into_iter()
                .zip(fits)
                .filter_map(|(entry, fits)| fits.then_some(entry))
                .collect();
        }
    }
    if let [(_, lenses)] = found.as_slice()
        && let Some(lens) = best_calibration(lenses, crop)
    {
        return LensDetection::picked(named(db, lens));
    }
    let mut candidates: Vec<(String, String)> = Vec::new();
    for lenses in found
        .iter()
        .map(|(_, lenses)| lenses.as_slice())
        .chain(std::iter::once(spelled_otherwise.as_slice()))
    {
        for lens in lenses {
            let entry = named(db, lens);
            if !candidates.contains(&entry) {
                candidates.push(entry);
            }
        }
    }
    LensDetection {
        lens: None,
        candidates,
    }
}

/// The mounts whose lenses fit the photo's camera: its Lensfun mounts and
/// the mounts they take (EF-S takes EF); None when Lensfun lacks the camera.
fn mounts_for_camera(db: &LensDatabase, facts: &LensFacts) -> Option<Vec<String>> {
    let own = camera_mounts(db, &facts.maker, &facts.camera_model);
    if own.is_empty() {
        return None;
    }
    let mut mounts: Vec<String> = own.iter().map(|m| m.to_ascii_lowercase()).collect();
    for mount in &db.mounts {
        if own.iter().any(|m| {
            mount
                .name
                .iter()
                .any(|n| m.eq_ignore_ascii_case(n.value.trim()))
        }) {
            mounts.extend(mount.compat.iter().map(|c| c.trim().to_ascii_lowercase()));
        }
    }
    Some(mounts)
}

fn fits_mount(lens: &Lens, mounts: Option<&[String]>) -> bool {
    mounts.is_none_or(|mounts| {
        lens.mount
            .iter()
            .any(|m| mounts.contains(&m.trim().to_ascii_lowercase()))
    })
}

/// The lens the EXIF name names: the best fuzzy match (the camera maker's
/// lenses first) where the name states focal length and aperture and the
/// lens agrees with the photo's facts (not its mount: an adapter reports
/// the adapted lens's name, and Lensfun's mount lists are incomplete). A name
/// that states no focal length identifies nothing by itself (the camera's
/// fixed lens is looked up by its mount); one that states no aperture
/// ("24-70mm") picks nothing: the lenses it fits are offered.
fn from_name(db: &LensDatabase, facts: &LensFacts) -> Option<LensDetection> {
    let name = facts.lens_name.trim();
    if name.is_empty() || name_spec(name).0.is_none() {
        return None;
    }
    let mounts = mounts_for_camera(db, facts);
    let mounts = mounts.as_deref();
    if names_aperture(name) {
        let best = |query: &str| find_lens_by_fuzzy_model(db, &facts.maker, query);
        let hit = best(name).or_else(|| without_model_code(name).and_then(best));
        if let Some((maker, model)) = &hit
            && let Some((_, lens)) =
                crate::lens_correction::find_lens_by_display_name(db, maker, model)
            && !is_fixed_lens(lens)
            && consistent(lens, facts)
        {
            return Some(LensDetection::picked((maker.clone(), model.clone())));
        }
    }
    // A name without an aperture, or a best match that contradicts the
    // facts: the lenses the name fits and the facts allow, offered.
    let matcher = fuzzy_matcher::skim::SkimMatcherV2::default().ignore_case();
    use fuzzy_matcher::FuzzyMatcher;
    let query = without_model_code(name).unwrap_or(name);
    let fitting: Vec<&Lens> = db
        .lenses
        .iter()
        .filter(|lens| lens.calibration.is_some() && !is_fixed_lens(lens))
        .filter(|lens| lens_spec(lens).0.is_some() && consistent(lens, facts))
        .filter(|lens| {
            matcher
                .fuzzy_match(&lens.get_full_model_name(), query)
                .or_else(|| matcher.fuzzy_match(&lens.get_canonical_model_name(), query))
                .is_some()
        })
        .collect();
    // The camera's own mount first; adapted lenses only when none fits it.
    let on_mount: Vec<&Lens> = fitting
        .iter()
        .copied()
        .filter(|lens| fits_mount(lens, mounts))
        .collect();
    let offered = if on_mount.is_empty() {
        fitting
    } else {
        on_mount
    };
    let mut candidates: Vec<(String, String)> = Vec::new();
    for lens in offered {
        let maker_lenses = lenses_for_maker(db, &lens.get_maker());
        let entry = (lens.get_maker(), lens.get_display_name(&maker_lenses));
        if !candidates.contains(&entry) {
            candidates.push(entry);
        }
    }
    (!candidates.is_empty()).then_some(LensDetection {
        lens: None,
        candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lens_correction::{embedded_lens_db, find_best_lens_match};

    fn exif(fields: &[(&str, &str)]) -> HashMap<String, String> {
        fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn detect(fields: &[(&str, &str)]) -> LensDetection {
        detect_lens(&embedded_lens_db(), &LensFacts::from_exif(&exif(fields)))
    }

    /// D800 + AF-S 24-70 f/2.8G: no EXIF LensModel, the name from the
    /// Nikon lens data (G71S d800; the map as the EXIF reader writes it).
    #[test]
    fn nikon_lens_named_by_the_maker_note() {
        let found = detect(&[
            ("Make", "NIKON CORPORATION"),
            ("Model", "NIKON D800"),
            ("LensMake", "Nikon"),
            ("LensModel", "AF-S Zoom-Nikkor 24-70mm f/2.8G ED"),
            ("FocalLength", "24"),
            ("FocalLengthIn35mmFilm", "24 mm"),
            ("MaxApertureValue", "3 EV"),
            (MAKER_NOTE_LENS_READ, "1"),
        ]);
        assert_eq!(
            found.lens,
            Some(("Nikon".into(), "Nikkor AF-S 24-70mm f/2.8G ED".into()))
        );
    }

    /// EOS-1Ds Mark III + EF 50 f/1.2L: no EXIF LensModel, Canon LensType
    /// 241 (G71S office208).
    #[test]
    fn canon_lens_named_by_its_lens_type() {
        let found = detect(&[
            ("Make", "Canon"),
            ("Model", "Canon EOS-1Ds Mark III"),
            ("LensMake", "Canon"),
            ("LensModel", "EF 50mm f/1.2L USM"),
            ("FocalLength", "50"),
            ("FNumber", "f/1.2"),
        ]);
        assert_eq!(
            found.lens,
            Some(("Canon".into(), "EF 50mm f/1.2L USM".into()))
        );
    }

    /// EOS 650D + Sigma 24-70 f/2.8 IF EX DG HSM: EXIF "24-70mm", Canon
    /// LensType 137 (24-70 mm, f/2.83) shared with Tamron's SP 24-70 VC USD
    /// (G71S eos650d): no pick, both offered — never Canon's 24-70 f/4L.
    #[test]
    fn shared_lens_type_is_offered_not_guessed() {
        let found = detect(&[
            ("Make", "Canon"),
            ("Model", "Canon EOS 650D"),
            ("LensModel", "24-70mm"),
            ("LensSpecification", "24-70 mm"),
            ("FocalLength", "24"),
            ("FNumber", "f/5"),
            (
                LENS_CANDIDATES,
                "Sigma 24-70mm f/2.8 IF EX DG HSM; Tamron SP 24-70mm f/2.8 Di VC USD",
            ),
        ]);
        assert_eq!(found.lens, None);
        let makers: Vec<&str> = found.candidates.iter().map(|(m, _)| m.as_str()).collect();
        assert!(
            makers.contains(&"Sigma") && makers.contains(&"Tamron"),
            "{found:?}"
        );
        assert!(!makers.contains(&"Canon"), "{found:?}");
    }

    /// The same photo by names alone (an EXIF map cached before the maker
    /// note was read): a name without an aperture picks nothing.
    #[test]
    fn a_name_without_aperture_picks_nothing() {
        let db = embedded_lens_db();
        assert_eq!(
            find_best_lens_match(&db, "Canon", "24-70mm", "Canon EOS 650D"),
            None
        );
        let found = detect(&[
            ("Make", "Canon"),
            ("Model", "Canon EOS 650D"),
            ("LensModel", "24-70mm"),
            ("LensSpecification", "24-70 mm"),
        ]);
        assert_eq!(found.lens, None);
        assert!(!found.candidates.is_empty());
    }

    /// One maker-note lens for a name without an aperture (EOS 6D Mark II,
    /// EXIF "18-35mm"): that lens.
    #[test]
    fn one_maker_note_lens_is_picked() {
        let found = detect(&[
            ("Make", "Canon"),
            ("Model", "Canon EOS 6D Mark II"),
            ("LensModel", "18-35mm"),
            ("LensSpecification", "18-35 mm"),
            (LENS_CANDIDATES, "Sigma 18-35mm f/1.8 DC HSM | A"),
        ]);
        assert_eq!(found.lens.map(|(maker, _)| maker), Some("Sigma".into()));
    }

    /// Names that state no lens ("----", "FT1", "GR LENS") pick none of
    /// the database's lenses by letters alone.
    #[test]
    fn names_without_focal_length_match_no_lens() {
        for (make, model, lens) in [
            ("SONY", "ILCE-7M2", "----"),
            ("NIKON CORPORATION", "NIKON 1 J5", "FT1"),
            ("RICOH IMAGING COMPANY, LTD.", "GR II", "GR LENS"),
        ] {
            let found = detect(&[("Make", make), ("Model", model), ("LensModel", lens)]);
            assert_eq!(found.lens, None, "{lens}");
        }
    }

    /// A lens whose focal range contradicts the photo's is not picked
    /// (PowerShot SX70 HS, 3.8-247 mm, is not the SX710 HS's lens).
    #[test]
    fn contradicting_focal_range_is_not_picked() {
        let found = detect(&[
            ("Make", "Canon"),
            ("Model", "PowerShot SX70 HS"),
            ("LensModel", "PowerShot SX70 HS"),
            ("LensSpecification", "3.8-247 mm, f/3.4-6.5"),
        ]);
        assert_eq!(found.lens, None);
    }

    /// An adapter's LensSpecification (f/1.1 for an f/1.8 lens) does not
    /// outweigh the lens's own name.
    #[test]
    fn the_name_outweighs_an_adapters_specification() {
        let found = detect(&[
            ("Make", "SIGMA"),
            ("Model", "fp L"),
            ("LensModel", "TAMRON SP 45mm F/1.8 Di VC USD F013"),
            ("LensSpecification", "45 mm, f/1.1"),
        ]);
        assert_eq!(found.lens.map(|(maker, _)| maker), Some("Tamron".into()));
    }

    #[test]
    fn lens_names_state_focal_range_and_aperture() {
        assert_eq!(name_spec("24-70mm"), (Some((24.0, 70.0)), None));
        assert_eq!(
            name_spec("EF50mm f/1.2L USM"),
            (Some((50.0, 50.0)), Some(1.2))
        );
        assert_eq!(name_spec("XF35mmF1.4 R"), (Some((35.0, 35.0)), Some(1.4)));
        assert_eq!(
            name_spec("FE 24-70mm F2.8 GM"),
            (Some((24.0, 70.0)), Some(2.8))
        );
        assert_eq!(
            name_spec("Summicron-M 1:2/50"),
            (Some((50.0, 50.0)), Some(2.0))
        );
        assert_eq!(
            name_spec("Vario-Elmarit-SL 1:2.8-4/24-90 ASPH."),
            (Some((24.0, 90.0)), Some(2.8))
        );
        assert_eq!(
            name_spec("AF-S DX Nikkor 18-140mm"),
            (Some((18.0, 140.0)), None)
        );
        assert!(!names_aperture("----"));
        assert_eq!(parse_specification("24-70 mm"), (Some((24.0, 70.0)), None));
        assert_eq!(
            parse_specification("15-45 mm, f/3.5-6.3"),
            (Some((15.0, 45.0)), Some(3.5))
        );
        assert_eq!(
            parse_specification("8.5-0 mm, f/1.9-0"),
            (Some((8.5, 8.5)), Some(1.9))
        );
    }
}
