//! One make and model for every file of a camera (moved from
//! `rawtonic_engine::develop_defaults`, CID1): rawler's camera catalogue
//! read as an alias table, from the names a camera writes into its files to
//! the catalogue's clean names. Camera defaults and the library's capture
//! facts name a camera with it, so a body's RAW, its DNG conversions and
//! its JPEG/HEIF files are one camera (notes:
//! benchmark-results/smr5-2026-11-24).

use std::collections::HashMap;
use std::sync::OnceLock;

/// One camera's make and model for all its files: the clean names of
/// rawler's camera catalog for any make and model the catalog knows (the
/// names a camera writes into its RAW's TIFF/EXIF tags, which its JPEG and
/// HEIF and its DNG conversions carry too; regional names stay their own),
/// the names as given otherwise (a camera without RAW support, a phone).
/// Exact names only, ignoring case and surrounding spaces, so two bodies
/// with similar names never meet; a camera added to the catalog later
/// keeps its identity, as its files' names map to the same clean names.
pub fn canonical_names(make: &str, model: &str) -> (String, String) {
    catalog_names()
        .one
        .get(&folded(make, model))
        .cloned()
        .unwrap_or_else(|| (make.trim().to_string(), model.trim().to_string()))
}

/// Every make and model the catalog maps to the camera named `make` /
/// `model` (its clean names, `canonical_names`), in name order; none for a
/// camera the catalog does not know.
pub fn file_names(make: &str, model: &str) -> &'static [(String, String)] {
    catalog_names()
        .all
        .get(&folded(make, model))
        .map_or(&[], Vec::as_slice)
}

/// The make and model pairs files of the camera named `make` / `model`
/// (its one name) state in their metadata, folded: `file_names` without
/// the decoder's own lookup names for files it recognises by their size
/// (headerless CHDK `… CRW` files carry no EXIF names).
pub fn stated_names(make: &str, model: &str) -> Vec<&'static (String, String)> {
    let names = catalog_names();
    file_names(make, model)
        .iter()
        .filter(|name| !names.by_size.contains(*name))
        .collect()
}

/// The one name (`canonical_names`) of the camera whose files give `model`
/// as their model, whatever make they state (Lightroom's camera name is the
/// model alone); none when no catalogued camera or more than one does.
pub fn canonical_of_model(model: &str) -> Option<(String, String)> {
    let model = model.trim().to_lowercase();
    let names = catalog_names()
        .one
        .iter()
        .filter(|((_, file_model), _)| *file_model == model)
        .map(|(_, one)| one);
    let one = most_common(
        names
            .clone()
            .map(|(make, model)| format!("{make}\u{1f}{model}")),
    )?;
    let (make, model) = one.split_once('\u{1f}')?;
    // One camera: its names differ at most in case.
    names
        .into_iter()
        .all(|(m, n)| m.eq_ignore_ascii_case(make) && n.eq_ignore_ascii_case(model))
        .then(|| (make.to_string(), model.to_string()))
}

/// The one make (`canonical_names`) of the cameras whose files state
/// `make` (`NIKON CORPORATION` → `Nikon`); none when no catalogued camera
/// does or they disagree beyond case.
pub fn canonical_of_make(make: &str) -> Option<String> {
    let make = make.trim().to_lowercase();
    let makes: Vec<&String> = catalog_names()
        .one
        .iter()
        .filter(|((file_make, _), _)| *file_make == make)
        .map(|(_, one)| &one.0)
        .collect();
    let one = most_common(makes.iter().map(|m| m.to_string()))?;
    makes
        .iter()
        .all(|m| m.eq_ignore_ascii_case(&one))
        .then_some(one)
}

/// The spelling most names have (the catalogue spells a few makes both
/// ways), the first in byte order among equals: the same every run.
fn most_common(names: impl Iterator<Item = String>) -> Option<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for name in names {
        *counts.entry(name).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(name, _)| name)
}

/// Whether a model test (`holds`, given folded names) that the library
/// applies to a camera's one model or any model its files state (as camera
/// rules match there) selects exactly the files a per-file test of the
/// stated model selects (as Lightroom's camera rules do), for every
/// catalogued camera: false when it holds for some of a camera's stated
/// models but not all (`ends with 1s` for Olympus `STYLUS1` and
/// `STYLUS1,1s`, one camera here), or for its one model only (`starts with
/// EOS` holds for `EOS 6D`, never for a file's `Canon EOS 6D`).
pub fn model_test_agrees_per_file(holds: impl Fn(&str) -> bool) -> bool {
    let names = catalog_names();
    names.all.iter().all(|((_, one_model), files)| {
        let stated: Vec<bool> = files
            .iter()
            .filter(|name| !names.by_size.contains(*name))
            .map(|(_, model)| holds(model))
            .collect();
        if stated.is_empty() {
            return true;
        }
        let any = stated.iter().any(|h| *h);
        let every = stated.iter().all(|h| *h);
        (any == every) && (any || !holds(one_model))
    })
}

/// Whether `model` is the one model name (`canonical_names`) of a
/// catalogued camera, ignoring case.
pub fn is_canonical_model(model: &str) -> bool {
    let model = model.trim().to_lowercase();
    catalog_names().all.keys().any(|(_, one)| *one == model)
}

/// A digest of the decoder's camera naming data (every catalogue entry's
/// file names, mode and clean names): the camera names a file gives through
/// the decoder change with it, so names kept from another catalogue are not
/// used.
pub fn naming_fingerprint() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| {
        let mut cameras: Vec<_> = rawler::global_loader()
            .get_cameras()
            .iter()
            .map(|((make, model, mode), camera)| {
                [
                    make.as_str(),
                    model.as_str(),
                    mode.as_str(),
                    camera.clean_make.as_str(),
                    camera.clean_model.as_str(),
                ]
                .join("\u{1f}")
            })
            .collect();
        cameras.sort_unstable();
        rawtonic_fs::sha256_hex(cameras.join("\u{1e}"))
    })
}

fn folded(make: &str, model: &str) -> (String, String) {
    (make.trim().to_lowercase(), model.trim().to_lowercase())
}

struct CatalogNames {
    /// A file's make and model (folded) → the camera's one name.
    one: HashMap<(String, String), (String, String)>,
    /// The camera's one name (folded) → the file names giving it.
    all: HashMap<(String, String), Vec<(String, String)>>,
    /// File names (folded) the decoder only gives files it recognises by
    /// their size, never stated by a file.
    by_size: std::collections::HashSet<(String, String)>,
}

fn catalog_names() -> &'static CatalogNames {
    static CATALOG: OnceLock<CatalogNames> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut one = HashMap::new();
        let mut by_size = std::collections::HashSet::new();
        let mut stated = std::collections::HashSet::new();
        for ((make, model, _mode), camera) in rawler::global_loader().get_cameras() {
            if camera.filesize > 0 {
                by_size.insert(folded(make, model));
            } else {
                stated.insert(folded(make, model));
            }
        }
        by_size.retain(|name| !stated.contains(name));
        // The decoder looks a file's names up with mode "" first, then by
        // its own mode: the "" entry names a file as the decoder does; a
        // name only some modes list takes the first mode's in sorted order,
        // the same every run.
        let mut cameras: Vec<_> = rawler::global_loader().get_cameras().iter().collect();
        cameras.sort_by(|a, b| a.0.cmp(b.0));
        for ((make, model, _mode), camera) in cameras {
            if camera.clean_make.trim().is_empty() && camera.clean_model.trim().is_empty() {
                continue;
            }
            one.entry(folded(make, model)).or_insert_with(|| {
                (
                    camera.clean_make.trim().to_string(),
                    camera.clean_model.trim().to_string(),
                )
            });
        }
        let mut all: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
        for ((make, model), (one_make, one_model)) in &one {
            all.entry(folded(one_make, one_model))
                .or_default()
                .push((make.clone(), model.clone()));
        }
        for names in all.values_mut() {
            names.sort();
        }
        CatalogNames { one, all, by_size }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model test agrees per file unless it splits a camera's stated
    /// models or holds for its one model alone.
    #[test]
    fn model_tests_that_agree_per_file() {
        assert!(model_test_agrees_per_file(|m| m.starts_with("nikon d")));
        assert!(model_test_agrees_per_file(|m| m.starts_with("canon eos")));
        // `EOS 6D` starts with it, the files' `Canon EOS 6D` does not.
        assert!(!model_test_agrees_per_file(|m| m.starts_with("eos")));
        // Olympus `STYLUS1` and `STYLUS1,1s` are one camera here.
        assert!(!model_test_agrees_per_file(|m| m.ends_with("1s")));
    }

    /// A camera's RAW and its JPEG name it alike (a JPEG's EXIF states the
    /// names the RAW's tags do); a name the catalogue does not know stays.
    #[test]
    fn a_bodys_file_names_give_one_name() {
        assert_eq!(
            canonical_names("NIKON CORPORATION", "NIKON D750"),
            ("Nikon".into(), "D750".into())
        );
        assert_eq!(
            canonical_names("Canon", "Canon EOS 6D"),
            ("Canon".into(), "EOS 6D".into())
        );
        assert_eq!(
            canonical_names(" sony ", "ilce-7m3"),
            ("Sony".into(), "ILCE-7M3".into())
        );
        assert_eq!(
            canonical_names("Apple", "iPhone 99"),
            ("Apple".into(), "iPhone 99".into())
        );
        assert_eq!(
            canonical_of_model("NIKON D750"),
            Some(("Nikon".into(), "D750".into()))
        );
        assert_eq!(canonical_of_model("Canon EOS 6D").unwrap().1, "EOS 6D");
        assert_eq!(canonical_of_model("No Such Camera"), None);
        assert_eq!(canonical_of_model("D750"), None);
        assert!(is_canonical_model("d750"));
        assert!(!is_canonical_model("NIKON D750"));
        assert_eq!(
            canonical_of_make("NIKON CORPORATION").as_deref(),
            Some("Nikon")
        );
        assert_eq!(canonical_of_make("SONY").as_deref(), Some("Sony"));
        // A CHDK file's size-detected name is the decoder's, not a file's.
        let (make, model) = canonical_of_model("Canon PowerShot G7 X").unwrap();
        assert!(file_names(&make, &model).len() > stated_names(&make, &model).len());
        assert!(
            stated_names(&make, &model)
                .iter()
                .all(|(_, file_model)| file_model == "canon powershot g7 x")
        );
        assert!(stated_names("Olympus", "Stylus1").len() > 1);
        assert!(
            file_names("Nikon", "D750")
                .iter()
                .any(|(make, model)| make == "nikon corporation" && model == "nikon d750")
        );
    }
}
