//! Which XMP sidecar belongs to which photograph.
//!
//! A RAW owns the stem sidecar (`IMG_1234.xmp`), as in Lightroom Classic and
//! Camera Raw. A developed image (JPEG, HEIF, TIFF, …) that shares its stem
//! with another file (a RAW+JPEG or ARW+HIF pair) never reads or writes that
//! sidecar: its own is `IMG_1234.JPG.xmp`, the full-name form darktable and
//! card ingest use too. A developed image alone keeps the stem form, and an
//! existing unambiguous full-name sidecar is its own. Ownership is decided from
//! the files on disk, so it holds whether or not the partner is in a library.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Formats whose files are developed images rather than RAWs: a sidecar
/// named after the stem alone belongs to the RAW of a pair, not to them.
const DEVELOPED_IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "heic", "heif", "hif", "png", "webp", "avif", "jxl", "tif", "tiff",
];

/// Whether `extension` is a developed image's (JPEG, HEIF, TIFF, …), not
/// a RAW's.
pub fn is_developed_image(extension: &str) -> bool {
    DEVELOPED_IMAGE_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
}

type Stems = HashMap<String, Vec<PathBuf>>;

/// Looks sidecars up, listing each folder at most once (one batch: an
/// import, a check of every sidecar, a folder listing). Drop it after the
/// batch; a folder's files are not watched for changes.
#[derive(Default)]
pub struct Sidecars {
    /// Folder → lower-cased stem → the names of regular files (not XMP).
    folders: RefCell<HashMap<PathBuf, Rc<Stems>>>,
}

impl Sidecars {
    pub fn new() -> Self {
        Self::default()
    }

    /// The sidecar `photo` has on disk, if any: never another file's.
    pub fn existing(&self, photo: &Path) -> Option<PathBuf> {
        self.path(photo).filter(|path| path.exists())
    }

    /// The sidecar's stored spelling, or the name a new packet gets. A
    /// packet that could be a full name and a different file's stem is
    /// left in place until the user resolves the filenames.
    pub fn path(&self, photo: &Path) -> Option<PathBuf> {
        let path = if is_developed(photo) {
            let full = full_form(photo);
            match existing_spelling(&full) {
                Some(full) => full,
                None if self.has_partner(photo) => full,
                None => spelled(&stem_form(photo)),
            }
        } else {
            spelled(&stem_form(photo))
        };
        (!self.is_ambiguous(&path)).then_some(path)
    }

    /// Can this packet be both a photo's full name and another's stem?
    pub fn is_ambiguous(&self, sidecar: &Path) -> bool {
        sidecar.file_stem().is_some_and(|stem| {
            let full_name_owner = sidecar.with_file_name(stem);
            full_name_owner.extension().is_some()
                && full_name_owner.is_file()
                && !self.same_stem(sidecar).is_empty()
        })
    }

    /// An existing packet whose meaning would change if this filename were
    /// added or removed. A move or rename must leave this decision to the
    /// photographer, even when the packet itself is left untouched.
    pub fn ambiguous_packet(&self, photo: &Path) -> Option<PathBuf> {
        let short = photo.with_file_name(photo.file_stem()?);
        if short.extension().is_some()
            && short.is_file()
            && let Some(packet) = existing_spelling(&stem_form(photo))
        {
            return Some(packet);
        }
        existing_spelling(&full_form(photo)).filter(|packet| !self.same_stem(packet).is_empty())
    }

    /// Regular files (excluding XMP) whose stem matches, ignoring ASCII
    /// case. Paths use the spelling on disk.
    pub fn same_stem(&self, path: &Path) -> Vec<PathBuf> {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            return Vec::new();
        };
        let folder = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        self.stems(folder)
            .get(&stem.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    /// Whether another file (not an XMP sidecar) shares the photo's stem.
    pub fn has_partner(&self, photo: &Path) -> bool {
        self.same_stem(photo).len() > usize::from(photo.is_file())
    }

    fn stems(&self, folder: &Path) -> Rc<Stems> {
        if let Some(stems) = self.folders.borrow().get(folder) {
            return stems.clone();
        }
        let mut stems = HashMap::new();
        for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let path = Path::new(&name);
            let sidecar = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                ["xmp", "rrdata", "rrexif"]
                    .iter()
                    .any(|ext| e.eq_ignore_ascii_case(ext))
            });
            if !sidecar && let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                stems
                    .entry(stem.to_ascii_lowercase())
                    .or_insert_with(Vec::new)
                    .push(entry.path());
            }
        }
        let stems = Rc::new(stems);
        self.folders
            .borrow_mut()
            .insert(folder.to_path_buf(), stems.clone());
        stems
    }
}

/// The sidecar `photo` has on disk, if any (`Sidecars::existing`).
pub fn existing_sidecar(photo: &Path) -> Option<PathBuf> {
    Sidecars::new().existing(photo)
}

/// The sidecar `photo` has, or the name a new one gets (`Sidecars::path`).
pub fn sidecar_path(photo: &Path) -> Option<PathBuf> {
    Sidecars::new().path(photo)
}

/// The stem form a RAW owns (`IMG_1234.xmp`), whether or not it exists.
pub fn stem_sidecar(photo: &Path) -> PathBuf {
    spelled(&stem_form(photo))
}

/// Whether the photo is a rendered image. TIFFs may contain camera RAW data.
pub fn is_developed(photo: &Path) -> bool {
    photo
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            is_developed_image(ext)
                && (!(ext.eq_ignore_ascii_case("tif") || ext.eq_ignore_ascii_case("tiff"))
                    || !rawtonic_raw_format::classify_cached(photo).is_raw())
        })
}

fn stem_form(photo: &Path) -> PathBuf {
    photo.with_extension("xmp")
}

fn full_form(photo: &Path) -> PathBuf {
    let mut name = photo.file_name().unwrap_or_default().to_os_string();
    name.push(".xmp");
    photo.with_file_name(name)
}

fn upper(lower: &Path) -> PathBuf {
    lower.with_extension("XMP")
}

/// `lower` (`….xmp`) or its `.XMP` spelling, whichever is there.
fn existing_spelling(lower: &Path) -> Option<PathBuf> {
    let upper = upper(lower);
    match (lower.exists(), upper.exists()) {
        (false, false) => None,
        (false, true) => Some(upper),
        (true, false) => Some(lower.to_path_buf()),
        (true, true) => Some(stored_spelling(lower, &upper).unwrap_or(lower.to_path_buf())),
    }
}

/// The existing spelling of `lower`, else `lower` itself (new sidecars use
/// `.xmp`). On a case-insensitive volume both spellings find the one
/// sidecar; on Windows its name as stored is used then, because replacing
/// it under another spelling renames it there.
fn spelled(lower: &Path) -> PathBuf {
    existing_spelling(lower).unwrap_or_else(|| lower.to_path_buf())
}

/// The name `a` and `b` have on disk when they are one file under two
/// spellings.
fn stored_spelling(a: &Path, b: &Path) -> Option<PathBuf> {
    let name = rawtonic_fs::stored_name(a)?;
    let stored = a.with_file_name(&name);
    // Only another case of the same name, and the file `b` finds too.
    (name.to_string_lossy().len() == a.file_name()?.to_string_lossy().len()
        && name
            .to_string_lossy()
            .eq_ignore_ascii_case(&a.file_name()?.to_string_lossy())
        && rawtonic_fs::stored_name(b).as_ref() == Some(&name))
    .then_some(stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(names: &[&str]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for name in names {
            fs::write(d.path().join(name), name).unwrap();
        }
        d
    }

    #[test]
    fn a_raw_owns_the_stem_sidecar_and_its_jpeg_partner_never_reads_it() {
        let d = folder(&["IMG_1234.CR3", "IMG_1234.JPG", "IMG_1234.xmp"]);
        let raw = d.path().join("IMG_1234.CR3");
        let jpg = d.path().join("IMG_1234.JPG");
        assert_eq!(existing_sidecar(&raw), Some(d.path().join("IMG_1234.xmp")));
        assert_eq!(sidecar_path(&raw), Some(d.path().join("IMG_1234.xmp")));
        assert_eq!(existing_sidecar(&jpg), None);
        assert_eq!(sidecar_path(&jpg), Some(d.path().join("IMG_1234.JPG.xmp")));
    }

    #[test]
    fn a_partner_hif_gets_its_own_sidecar_even_before_the_raw_has_one() {
        let d = folder(&["DSC0001.ARW", "DSC0001.HIF"]);
        let hif = d.path().join("DSC0001.HIF");
        assert_eq!(sidecar_path(&hif), Some(d.path().join("DSC0001.HIF.xmp")));
        assert_eq!(
            sidecar_path(&d.path().join("DSC0001.ARW")),
            Some(d.path().join("DSC0001.xmp"))
        );
    }

    #[test]
    fn a_lone_jpeg_keeps_the_stem_sidecar_and_a_full_name_one_always_wins() {
        let d = folder(&["A.JPG", "A.xmp", "B.JPG", "B.xmp", "B.JPG.xmp"]);
        let a = d.path().join("A.JPG");
        assert_eq!(existing_sidecar(&a), Some(d.path().join("A.xmp")));
        assert_eq!(sidecar_path(&a), Some(d.path().join("A.xmp")));
        let b = d.path().join("B.JPG");
        assert_eq!(existing_sidecar(&b), Some(d.path().join("B.JPG.xmp")));
        assert_eq!(
            sidecar_path(&d.path().join("C.JPG")),
            Some(d.path().join("C.xmp"))
        );
    }

    #[test]
    fn partners_match_ignoring_case_and_sidecars_are_no_partners() {
        let d = folder(&[
            "img_1.cr3",
            "IMG_1.JPG",
            "IMG_2.JPG",
            "IMG_2.xmp",
            "IMG_2.JPG.rrdata",
        ]);
        assert_eq!(
            sidecar_path(&d.path().join("IMG_1.JPG")),
            Some(d.path().join("IMG_1.JPG.xmp"))
        );
        assert_eq!(
            existing_sidecar(&d.path().join("IMG_2.JPG")),
            Some(d.path().join("IMG_2.xmp"))
        );
    }

    #[test]
    fn a_missing_jpeg_beside_its_raw_still_has_a_partner() {
        let d = folder(&["A.NEF"]);
        assert!(Sidecars::new().has_partner(&d.path().join("A.JPG")));
        assert!(!Sidecars::new().has_partner(&d.path().join("A.NEF")));
    }
}
