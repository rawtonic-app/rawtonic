//! Automatic lens detection: the Lensfun lens a photo was taken with, from
//! what its EXIF map says about the lens — the EXIF lens name, the lens's
//! focal range and widest aperture (LensSpecification, the name) and the
//! lenses its maker-note lens ID fits (`LensCandidates`, filled by the
//! EXIF reader where the ID is shared, e.g. one Canon LensType for several
//! third-party lenses). Focal range, aperture, generation and maker model
//! code constrain the pick. If the photo does not distinguish the remaining
//! lenses, their profiles are offered for manual selection.
//! notes: benchmark-results/lns1-2026-11-23/REPORT.md
use crate::lens_correction::{
    Lens, LensDatabase, body_crop_factor, camera_mounts, find_lens_by_camera_mount,
    find_lens_by_fuzzy_model, lens_result, normalized_lens_name, without_model_code,
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

/// Whether a lens name identifies the lens: it states focal length and
/// aperture and has a word beyond them ("EF50mm f/1.2L USM", "FE 85mm
/// F1.8", "Summicron-M 1:2/50"). One that does not ("24-70mm",
/// "24-70mm f/2.8", "55mmF1.4", "----") fits several lenses: the maker note
/// is read too, and no lens is picked by the name alone.
pub fn identifies_lens(name: &str) -> bool {
    let (focal, aperture) = name_spec(name);
    if focal.is_none() || aperture.is_none() {
        return false;
    }
    let (focal_re, aperture_re, leica_re) = spec_patterns();
    let rest = leica_re.replace_all(name, " ");
    let rest = focal_re.replace_all(&rest, " ");
    let rest = aperture_re.replace_all(&rest, " ");
    rest.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.eq_ignore_ascii_case("mm"))
        .map(str::len)
        .sum::<usize>()
        >= 2
}

fn spec_patterns() -> (
    &'static regex::Regex,
    &'static regex::Regex,
    &'static regex::Regex,
) {
    use std::sync::OnceLock;
    static FOCAL: OnceLock<regex::Regex> = OnceLock::new();
    static APERTURE: OnceLock<regex::Regex> = OnceLock::new();
    static LEICA: OnceLock<regex::Regex> = OnceLock::new();
    let focal = FOCAL.get_or_init(|| {
        regex::Regex::new(r"(?i)(\d+(?:\.\d+)?)(?:\s*-\s*(\d+(?:\.\d+)?))?\s*mm").unwrap()
    });
    let aperture = APERTURE.get_or_init(|| {
        regex::Regex::new(r"(?:^|[^A-Za-z]|mm)[fF]\s*/?\s*(\d+(?:\.\d+)?)(?:\s*-\s*\d+(?:\.\d+)?)?")
            .unwrap()
    });
    let leica = LEICA.get_or_init(|| {
        regex::Regex::new(r"1\s*:\s*(\d+(?:\.\d+)?)(?:\s*-\s*\d+(?:\.\d+)?)?\s*/\s*(\d+(?:\.\d+)?)(?:\s*-\s*(\d+(?:\.\d+)?))?")
            .unwrap()
    });
    (focal, aperture, leica)
}

/// The focal range and widest aperture a lens name states: "24-70mm
/// f/2.8", "17-55mm F2.8", "EF50mm f/1.2L", Leica's "1:2.8-4/24-90".
pub fn name_spec(name: &str) -> (Option<(f32, f32)>, Option<f32>) {
    let (focal, aperture, leica) = spec_patterns();
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

/// Product suffixes are identity claims, even when the rest of the name
/// was matched by subsequence or the name used for scoring omitted a code.
#[derive(Default)]
struct NameMarkers<'a> {
    generation: u8,
    format: u8,
    aperture_suffix: Option<&'a str>,
    model_code: Option<&'a str>,
    mount_suffixes: Option<&'a str>,
}

impl<'a> NameMarkers<'a> {
    fn read(name: &'a str) -> Self {
        // Extender generations describe a different optical component.
        let name = name.split(" + ").next().unwrap_or(name);
        let mut markers = Self::default();
        for part in marker_pattern().captures_iter(name) {
            if let Some(generation) = part.name("generation").or_else(|| part.name("number")) {
                markers.generation |= generation_bit(generation.as_str());
            }
            if let Some(format) = part.name("format") {
                markers.format = generation_bit(format.as_str());
            }
            if let Some(suffix) = part.name("suffix") {
                markers.aperture_suffix = Some(suffix.as_str().trim());
            }
            if let Some(code) = model_code(&part) {
                markers.model_code = Some(code);
                markers.mount_suffixes = part.name("mounts").map(|mounts| mounts.as_str());
            }
        }
        markers
    }

    fn agrees_with(&self, other: &Self) -> bool {
        generations_agree(self.generation, other.generation)
            && generations_agree(self.format, other.format)
            && agrees(self.aperture_suffix, [other.aperture_suffix])
            && self.code_agrees([other])
    }

    fn fits_profile(&self, lens: &Lens) -> bool {
        if self.generation == 0
            && self.format == 0
            && self.aperture_suffix.is_none()
            && self.model_code.is_none()
        {
            return true;
        }
        // An alternate spelling can supply a code or generation omitted by
        // another spelling. Omitting it never hides a conflicting marker.
        let names: Vec<_> = lens
            .model
            .iter()
            .map(|name| NameMarkers::read(&name.value))
            .collect();
        let generations = names.iter().fold(0, |all, name| all | name.generation);
        let formats = names.iter().fold(0, |all, name| all | name.format);
        generations_agree(self.generation, generations)
            && generations_agree(self.format, formats)
            && agrees(
                self.aperture_suffix,
                names.iter().map(|name| name.aperture_suffix),
            )
            && self.code_agrees(&names)
    }

    fn code_agrees<'b>(&self, names: impl IntoIterator<Item = &'b NameMarkers<'b>>) -> bool {
        let Some(wanted) = self.model_code else {
            return true;
        };
        let mut codes = names
            .into_iter()
            .filter_map(|name| Some((name.model_code?, name.mount_suffixes)))
            .peekable();
        codes.peek().is_none() || codes.any(|(code, mounts)| {
            code.eq_ignore_ascii_case(wanted)
                // A profile may spell out the suffixes it covers: "B070 Z/RF/X/E".
                || mounts.is_some_and(|mounts| {
                    wanted.get(..code.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(code))
                        && mounts.split('/').any(|suffix| suffix.eq_ignore_ascii_case(&wanted[code.len()..]))
                })
        })
    }
}

fn generation_bit(generation: &str) -> u8 {
    match generation.to_ascii_lowercase().as_str() {
        "i" | "1" => 1,
        "ii" | "2" => 2,
        "iii" | "3" => 4,
        _ => 8, // IV
    }
}

fn generations_agree(wanted: u8, known: u8) -> bool {
    // A shared calibration may explicitly cover several generations (I/II).
    known == 0 || wanted & known == wanted
}

fn agrees<'a>(wanted: Option<&str>, known: impl IntoIterator<Item = Option<&'a str>>) -> bool {
    let Some(wanted) = wanted else { return true };
    let mut known = known.into_iter().flatten().peekable();
    known.peek().is_none() || known.any(|marker| wanted.eq_ignore_ascii_case(marker))
}

fn marker_pattern() -> &'static regex::Regex {
    static MARKERS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    MARKERS.get_or_init(|| {
        regex::Regex::new(concat!(
            r"(?xi)",
            // Aperture suffixes (f/2.8L, F2.8G). Capture the number separately
            // so the family name still includes the aperture.
            r"(?<aperture>f\s*/?\s*\d+(?:\.\d+)?(?:\s*-\s*\d+(?:\.\d+)?)?)(?<suffix>[ldge]|\s+l)\b",
            // Di II/III describes a lens format, not the generation.
            r"|\b(?<format_name>di\s*-?\s*(?<format>iii|ii|i))\b",
            r"|\b(?:(?:mark|mk|g)\s*(?<number>[1-4])|(?:mark\s*|mk\s*)?(?<generation>iii|ii|iv|i))\b",
            // Keep the Sigma series name when removing its numeric code.
            r"|(?<series>\|\s*(?:\[[acs]\]\s*)?(?:contemporary|art|sports|[acs])\s+(?:[acs]\s+)?)(?<series_code>\d{3})\b",
            r"|\b(?:model\s+)?(?<code>[a-z]{1,4}\d{3,}[a-z]{0,4}|[ab]\d{2})\b(?:\s+(?<mounts>[a-z]{1,2}(?:/[a-z]{1,2})+))?",
        ))
        .unwrap()
    })
}

fn model_code<'a>(part: &regex::Captures<'a>) -> Option<&'a str> {
    part.name("code")
        .or_else(|| part.name("series_code"))
        .map(|code| code.as_str())
        // Mount prefixes can be attached to the focal length (RF100mm).
        .filter(|code| !code.to_ascii_lowercase().ends_with("mm"))
}

fn family_name(name: &str, maker: &str) -> String {
    let name = marker_pattern().replace_all(name, |part: &regex::Captures<'_>| {
        if part.name("code").is_some() && model_code(part).is_none() {
            return part[0].to_string();
        }
        ["aperture", "format_name", "series"]
            .iter()
            .filter_map(|label| part.name(label).map(|value| value.as_str()))
            .collect::<String>()
    });
    normalized_lens_name(&name, maker)
}

/// An otherwise exact spelling can omit a leading brand word (Lumix) and
/// write the same generation as "II", "MkII" or "Mark 2".
fn matches_spelling(name: &str, profile: &str, maker: &str) -> bool {
    let normalize = |name: &str| {
        let name = marker_pattern().replace_all(name, |part: &regex::Captures<'_>| {
            part.name("number")
                .or_else(|| part.name("generation"))
                .map_or_else(
                    || part[0].to_string(),
                    |generation| format!(" mark{} ", generation_bit(generation.as_str())),
                )
        });
        normalized_lens_name(&name, maker)
    };
    let wanted = normalize(name);
    let mut profile = profile.trim();
    loop {
        if normalize(profile) == wanted {
            return true;
        }
        let Some((prefix, rest)) = profile.split_once(char::is_whitespace) else {
            return false;
        };
        if prefix.len() < 2
            || !prefix.bytes().all(|byte| byte.is_ascii_alphabetic())
            || NameMarkers::read(prefix).generation != 0
            || ["mark", "mk"]
                .iter()
                .any(|word| prefix.eq_ignore_ascii_case(word))
        {
            return false;
        }
        profile = rest.trim_start();
    }
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
        let found = from_candidates(db, facts, crop);
        // Nothing the maker note names is in Lensfun as a lens (a compact's
        // own lens is listed for its camera): the name and the camera's
        // mount as without it.
        if found.lens.is_some() || !found.candidates.is_empty() {
            return found;
        }
    }
    if let Some(found) = from_name(db, facts, &facts.lens_name) {
        return found;
    }
    find_lens_by_camera_mount(db, &facts.maker, &facts.camera_model)
        .map(|lens| LensDetection::picked(named(db, lens)))
        .unwrap_or_default()
}

/// Lenses offered for the photographer to choose: each physical lens once
/// (maker and canonical name), with its calibration for the photo's frame
/// (`best_calibration`).
fn offer(db: &LensDatabase, lenses: &[&Lens], crop: Option<f32>) -> Vec<(String, String)> {
    let mut candidates: Vec<(String, String)> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for lens in lenses {
        let identity = (lens.get_maker(), lens.get_canonical_model_name());
        if seen.contains(&identity) {
            continue;
        }
        let variants: Vec<&Lens> = lenses
            .iter()
            .copied()
            .filter(|other| (other.get_maker(), other.get_canonical_model_name()) == identity)
            .collect();
        seen.push(identity);
        if let Some(lens) = best_calibration(&variants, crop) {
            let entry = named(db, lens);
            if !candidates.contains(&entry) {
                candidates.push(entry);
            }
        }
    }
    candidates
}

/// The best fuzzy ties and variants of the selected lens's family.
/// Letters before and after a missing generation can affect the fuzzy score,
/// but they cannot tell two generations apart.
fn ambiguous_matches<'a>(
    db: &'a LensDatabase,
    winner: &Lens,
    name: &str,
    facts: &LensFacts,
    allowed: impl Fn(&Lens) -> bool,
) -> Vec<&'a Lens> {
    use fuzzy_matcher::FuzzyMatcher;
    let matcher = fuzzy_matcher::skim::SkimMatcherV2::default().ignore_case();
    let maker = winner.get_maker();
    let families: Vec<_> = winner
        .model
        .iter()
        .map(|model| family_name(&model.value, &maker))
        .collect();
    let scored: Vec<_> = db
        .lenses
        .iter()
        .filter(|lens| lens.get_maker().eq_ignore_ascii_case(&maker))
        .filter(|lens| {
            lens.calibration.is_some()
                && !is_fixed_lens(lens)
                && consistent(lens, facts)
                && allowed(lens)
        })
        .map(|lens| {
            let score = matcher
                .fuzzy_match(&lens.get_full_model_name(), name)
                .max(matcher.fuzzy_match(&lens.get_canonical_model_name(), name));
            let same_family = lens
                .model
                .iter()
                .any(|model| families.contains(&family_name(&model.value, &maker)));
            (score, same_family, lens)
        })
        .collect();
    let top = scored.iter().filter_map(|(score, _, _)| *score).max();
    let winner_model = winner.get_canonical_model_name();
    let has_family_variant = scored.iter().any(|(_, same_family, lens)| {
        *same_family && lens.get_canonical_model_name() != winner_model
    });
    let best: Vec<&Lens> = scored
        .into_iter()
        .filter(|(score, same_family, _)| {
            has_family_variant && *same_family || score.is_some() && *score == top
        })
        .map(|(_, _, lens)| lens)
        .collect();
    let identities = best
        .iter()
        .map(|lens| lens.get_canonical_model_name())
        .collect::<std::collections::BTreeSet<_>>();
    if identities.len() > 1 {
        best
    } else {
        Vec::new()
    }
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
    let photo_markers = NameMarkers::read(exif_name);
    if found.len() > 1 && exif_name.len() >= 4 {
        let fits: Vec<bool> = found
            .iter()
            .map(|(name, _)| {
                matcher.fuzzy_match(name, exif_name).is_some()
                    && photo_markers.agrees_with(&NameMarkers::read(name))
            })
            .collect();
        if fits.iter().any(|f| *f) && fits.iter().any(|f| !*f) {
            found = found
                .into_iter()
                .zip(fits)
                .filter_map(|(entry, fits)| fits.then_some(entry))
                .collect();
        }
    }
    let matching: Vec<_> = found
        .iter()
        .filter(|(name, _)| photo_markers.agrees_with(&NameMarkers::read(name)))
        .collect();
    if let [(name, lenses)] = matching.as_slice() {
        let markers = NameMarkers::read(name);
        let eligible: Vec<_> = lenses
            .iter()
            .copied()
            .filter(|lens| photo_markers.fits_profile(lens) && markers.fits_profile(lens))
            .collect();
        if let Some(lens) = best_calibration(&eligible, crop) {
            return LensDetection::picked(named(db, lens));
        }
        // The one lens the maker note names, without a Lensfun entry of
        // that exact spelling: look it up with both sources' facts.
        if lenses.is_empty() {
            let (focal, aperture) = name_spec(name);
            let by_name = LensFacts {
                focal_range: facts.focal_range.or(focal),
                max_aperture: facts.max_aperture.or(aperture),
                ..facts.clone()
            };
            if let Some(found) = from_name(db, &by_name, name)
                && found.lens.is_some()
            {
                return found;
            }
        }
    }
    let lenses: Vec<&Lens> = found
        .iter()
        .flat_map(|(_, lenses)| lenses.iter().copied())
        .chain(spelled_otherwise.iter().copied())
        .collect();
    let candidates = offer(db, &lenses, crop);
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
fn from_name(db: &LensDatabase, facts: &LensFacts, name: &str) -> Option<LensDetection> {
    let name = name.trim();
    if name.is_empty() || name_spec(name).0.is_none() {
        return None;
    }
    let mounts = mounts_for_camera(db, facts);
    let mounts = mounts.as_deref();
    if identifies_lens(name) {
        let markers = NameMarkers::read(name);
        let photo = (facts.lens_name.trim() != name).then(|| NameMarkers::read(&facts.lens_name));
        let allowed = |lens: &Lens| {
            markers.fits_profile(lens)
                && photo.as_ref().is_none_or(|photo| photo.fits_profile(lens))
        };
        let best = |query| find_lens_by_fuzzy_model(db, &facts.maker, query, allowed);
        let hit = best(name).or_else(|| without_model_code(name).and_then(best));
        if let Some(lens) = hit
            && !is_fixed_lens(lens)
            && consistent(lens, facts)
        {
            let tied = ambiguous_matches(db, lens, name, facts, allowed);
            if !tied.is_empty() {
                let crop =
                    body_crop_factor(db, &facts.maker, &facts.camera_model, facts.frame_crop);
                let exact: Vec<&Lens> = tied
                    .iter()
                    .copied()
                    .filter(|lens| {
                        let maker = lens.get_maker();
                        lens.model
                            .iter()
                            .any(|n| matches_spelling(name, &n.value, &maker))
                    })
                    .collect();
                let identities: std::collections::BTreeSet<String> = exact
                    .iter()
                    .map(|lens| lens.get_canonical_model_name())
                    .collect();
                if identities.len() != 1 {
                    return Some(LensDetection {
                        lens: None,
                        candidates: offer(db, &tied, crop),
                    });
                }
                if !identities.contains(&lens.get_canonical_model_name()) {
                    return best_calibration(&exact, crop)
                        .map(|lens| LensDetection::picked(named(db, lens)));
                }
            }
            return Some(LensDetection::picked(named(db, lens)));
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
                || lens.model.iter().any(|model| {
                    let maker = lens.get_maker();
                    family_name(&model.value, &maker) == family_name(name, &maker)
                })
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
    let crop = body_crop_factor(db, &facts.maker, &facts.camera_model, facts.frame_crop);
    let candidates = offer(db, &offered, crop);
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
            (LENS_CANDIDATES, "Nikon AF-S Zoom-Nikkor 24-70mm f/2.8G ED"),
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
            (LENS_CANDIDATES, "Canon EF 50mm f/1.2L USM"),
            (MAKER_NOTE_LENS_READ, "1"),
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
        // One entry per physical lens: the Sigma's calibration for the
        // 650D's APS-C frame only.
        assert_eq!(
            makers.iter().filter(|m| **m == "Sigma").count(),
            1,
            "{found:?}"
        );
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
        // One entry per physical lens: the Sigma once.
        let sigmas = found
            .candidates
            .iter()
            .filter(|(maker, model)| maker == "Sigma" && model.contains("IF EX DG HSM"))
            .count();
        assert_eq!(sigmas, 1, "{found:?}");
    }

    /// A mount prefix does not tell two generations apart: "EF 24-70mm
    /// f/2.8" fits the f/2.8L and the f/2.8L II equally — offered; the full
    /// name of one of them picks it.
    #[test]
    fn a_name_two_lenses_fit_equally_picks_neither() {
        let found = detect(&[
            ("Make", "Canon"),
            ("Model", "Canon EOS 5D Mark III"),
            ("LensModel", "EF 24-70mm f/2.8"),
        ]);
        assert_eq!(found.lens, None);
        assert!(found.candidates.len() > 1, "{found:?}");
        let found = detect(&[
            ("Make", "Canon"),
            ("Model", "Canon EOS 5D Mark III"),
            ("LensModel", "EF24-70mm f/2.8L II USM"),
        ]);
        assert_eq!(
            found.lens.map(|(_, model)| model.contains("II")),
            Some(true)
        );
        // An exact name beside its successor's ("… GM II") picks its lens.
        let found = detect(&[
            ("Make", "SONY"),
            ("Model", "ILCE-7RM2"),
            ("LensModel", "FE 24-70mm F2.8 GM"),
        ]);
        assert_eq!(
            found.lens,
            Some(("Sony".into(), "FE 24-70mm f/2.8 GM".into()))
        );
    }

    #[test]
    fn a_name_missing_generation_details_offers_both_generations() {
        let found = detect(&[
            ("Make", "Canon"),
            ("Model", "Canon EOS 5D Mark III"),
            ("LensModel", "EF 24-70mm f/2.8 USM"),
        ]);
        assert_eq!(found.lens, None);
        assert_eq!(
            found.candidates,
            [
                ("Canon".into(), "EF 24-70mm f/2.8L USM".into()),
                ("Canon".into(), "EF 24-70mm f/2.8L II USM".into()),
            ]
        );
    }

    #[test]
    fn a_conflicting_model_code_is_only_offered() {
        for maker_note in ["", "Tamron SP 90mm F/2.8 Di VC USD MACRO 1:1 F004"] {
            let found = detect(&[
                ("Make", "Canon"),
                ("Model", "Canon EOS 600D"),
                ("LensModel", "TAMRON SP 90mm F/2.8 Di VC USD MACRO1:1 F017"),
                ("LensSpecification", "90 mm, f/2.8"),
                (LENS_CANDIDATES, maker_note),
            ]);
            assert_eq!(found.lens, None);
            assert!(
                found
                    .candidates
                    .iter()
                    .any(|(_, model)| model.ends_with("F004"))
            );
        }
    }

    #[test]
    fn explicit_identity_markers_must_agree() {
        for (exif, profile) in [
            ("35mm F1.4L Mark II", "35mm f/1.4L III"),
            ("85mm F1.8G", "85mm f/1.8D"),
            ("90mm F2.8 F017", "90mm F2.8 (Model F004)"),
            (
                "150-600mm F5-6.3 DG OS HSM | Contemporary 015",
                "150-600mm F5-6.3 DG OS HSM | Sports 014",
            ),
            ("SP 90mm F/2.8 Di III G2", "SP 90mm F/2.8 Di II G2"),
        ] {
            assert!(!NameMarkers::read(exif).agrees_with(&NameMarkers::read(profile)));
        }
        for (exif, profile) in [
            ("35mm F1.4 L Mark 2", "35mm f/1.4L MkII"),
            ("35mm F1.4 II", "35mm F1.4 (Vers. I/II)"),
            ("RF100mm F2.8", "RF 100mm F2.8"),
            (
                "17-70mm F/2.8 DiIII-A VC RXD B070X",
                "17-70mm F2.8 B070 Z/RF/X/E",
            ),
        ] {
            assert!(NameMarkers::read(exif).agrees_with(&NameMarkers::read(profile)));
        }
    }

    #[test]
    fn a_later_generation_does_not_match_an_explicit_earlier_one() {
        let mut db = embedded_lens_db();
        db.lenses
            .retain(|lens| lens.get_canonical_model_name() == "Canon EF 16-35mm f/2.8L III USM");
        let found = detect_lens(
            &db,
            &LensFacts::from_names("Canon", "EF 16-35mm f/2.8L II USM", "Canon EOS 5D Mark III"),
        );
        assert_eq!(found.lens, None);
        assert_eq!(
            found.candidates,
            [("Canon".into(), "EF 16-35mm f/2.8L III USM".into())]
        );
    }

    /// A maker-note lens whose aperture contradicts the EXIF name's is
    /// offered, not applied.
    #[test]
    fn a_maker_note_lens_contradicting_the_exif_name_is_not_applied() {
        let found = detect(&[
            ("Make", "Canon"),
            ("Model", "Canon EOS 5D Mark III"),
            ("LensModel", "50mm f/1.8"),
            (LENS_CANDIDATES, "Canon EF 50mm f/1.2L USM"),
        ]);
        assert_eq!(found.lens, None);
    }

    /// A compact whose maker note names its own lens (rawler's fixed-lens
    /// key) still gets the lens Lensfun lists for the camera.
    #[test]
    fn a_compacts_own_lens_comes_through_its_camera() {
        let found = detect(&[
            ("Make", "Canon"),
            ("Model", "Canon PowerShot SX60 HS"),
            (LENS_CANDIDATES, "Canon PowerShot SX60 HS"),
            (MAKER_NOTE_LENS_READ, "1"),
        ]);
        assert!(
            found
                .lens
                .as_ref()
                .is_some_and(|(_, model)| model.contains("SX60")),
            "{found:?}"
        );
    }

    /// A name with focal length and aperture but nothing else ("24-70mm
    /// f/2.8") fits Canon's 24-70 f/2.8L and L II alike: no pick.
    #[test]
    fn a_generic_full_name_picks_nothing() {
        let found = detect(&[
            ("Make", "Canon"),
            ("Model", "Canon EOS 5D Mark III"),
            ("LensModel", "24-70mm f/2.8"),
        ]);
        assert_eq!(found.lens, None);
        assert!(found.candidates.len() > 1, "{found:?}");
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
        assert!(!identifies_lens("----"));
        for weak in [
            "24-70mm",
            "24-70mm f/2.8",
            "55mmF1.4",
            "E 60mm F2.8",
            "100-300mm F4.5-5.6",
        ] {
            assert!(!identifies_lens(weak), "{weak}");
        }
        for full in [
            "EF50mm f/1.2L USM",
            "FE 85mm F1.8",
            "XF35mmF1.4 R",
            "Summicron-M 1:2/50",
            "50mm F1.8 II",
        ] {
            assert!(identifies_lens(full), "{full}");
        }
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
