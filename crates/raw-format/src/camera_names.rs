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

/// The one name (`canonical_names`) of the camera whose files give `model`
/// as their model, whatever make they state (Lightroom's camera name is the
/// model alone); none when no catalogued camera or more than one does.
pub fn canonical_of_model(model: &str) -> Option<(String, String)> {
    let model = model.trim().to_lowercase();
    let mut found: Option<&(String, String)> = None;
    for ((_, file_model), one) in &catalog_names().one {
        if *file_model == model {
            match found {
                Some(other)
                    if !one.0.eq_ignore_ascii_case(&other.0)
                        || !one.1.eq_ignore_ascii_case(&other.1) =>
                {
                    return None;
                }
                _ => found = Some(one),
            }
        }
    }
    found.cloned()
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
}

fn catalog_names() -> &'static CatalogNames {
    static CATALOG: OnceLock<CatalogNames> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut one = HashMap::new();
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
        CatalogNames { one, all }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(
            file_names("Nikon", "D750")
                .iter()
                .any(|(make, model)| make == "nikon corporation" && model == "nikon d750")
        );
    }
}
