//! The identity of a photo in a library: its source file, and for a virtual
//! copy the copy's id.
//!
//! Stored and sent as one string (albums, stacks, settings, the webview's
//! paths): the source path for the primary photo, `<source>?vc=<copy id>` for
//! a virtual copy, whose editor sidecar is `<file>.<copy id>.rrdata` next to
//! the source. [`PhotoRef::parse`] and [`PhotoRef::format`] are the one codec
//! of that form: `format(parse(text)) == text` for every string, so a path
//! read and written again is unchanged byte for byte.
//!
//! The copy id is what follows the *last* `?vc=`, so a virtual copy of a file
//! whose name contains `?vc=` keeps its source. A primary photo whose file
//! name contains `?vc=` cannot be told from a copy in this form and reads as
//! one. The id is not validated here: stored forms hold editor ids (six hex
//! digits, [`CopyId::is_editor_id`]) and older or test ids alike.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

/// What separates the source path from the copy id.
pub const COPY_MARKER: &str = "?vc=";

/// A virtual copy's id, as stored after `?vc=` and in its sidecar's name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CopyId(String);

impl CopyId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the id is one the editor makes: six lower-case hex digits.
    pub fn is_editor_id(&self) -> bool {
        is_editor_id(&self.0)
    }
}

impl fmt::Display for CopyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `id` is a virtual copy id as the editor makes them (six
/// lower-case hex digits).
pub fn is_editor_id(id: &str) -> bool {
    id.len() == 6 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A photo: the primary photo of a source file, or one of its virtual copies.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PhotoRef {
    source: String,
    copy: Option<CopyId>,
}

impl PhotoRef {
    /// The photo a stored path names (never fails; see the module docs).
    pub fn parse(text: &str) -> Self {
        let (source, copy) = split(text);
        Self {
            source: source.to_owned(),
            copy: copy.map(CopyId::new),
        }
    }

    /// The primary photo of `source`.
    pub fn primary(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            copy: None,
        }
    }

    /// The photo of `source` with `copy` (`None` = the primary one).
    pub fn new(source: impl Into<String>, copy: Option<CopyId>) -> Self {
        Self {
            source: source.into(),
            copy,
        }
    }

    /// The photo of the file at `source` (a non-UTF-8 path lossily, as the
    /// app has always named it) with the copy `copy`.
    pub fn of_path(source: &Path, copy: Option<&str>) -> Self {
        Self::new(source.to_string_lossy(), copy.map(CopyId::new))
    }

    /// The source file's path as stored.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn source_path(&self) -> &Path {
        Path::new(&self.source)
    }

    pub fn copy(&self) -> Option<&CopyId> {
        self.copy.as_ref()
    }

    pub fn copy_id(&self) -> Option<&str> {
        self.copy.as_ref().map(CopyId::as_str)
    }

    pub fn is_copy(&self) -> bool {
        self.copy.is_some()
    }

    /// The same photo (same copy) of another source file, e.g. after a rename.
    pub fn with_source(&self, source: impl Into<String>) -> Self {
        Self::new(source, self.copy.clone())
    }

    /// This photo with another copy (`None` = the primary one).
    pub fn with_copy(&self, copy: Option<&str>) -> Self {
        Self::new(self.source.clone(), copy.map(CopyId::new))
    }

    /// The editor sidecar of this photo: `<file>.rrdata`, or
    /// `<file>.<copy id>.rrdata` for a virtual copy, next to the source.
    pub fn sidecar_path(&self) -> PathBuf {
        self.source_path().with_file_name(self.sidecar_name())
    }

    /// The editor sidecar's file name (`<file>.rrdata`, or
    /// `<file>.<copy id>.rrdata` for a virtual copy).
    pub fn sidecar_name(&self) -> OsString {
        sidecar_name(self.source_path(), self.copy_id())
    }

    /// The stored form (`<source>` or `<source>?vc=<copy id>`).
    pub fn format(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for PhotoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)?;
        if let Some(id) = &self.copy {
            write!(f, "{COPY_MARKER}{id}")?;
        }
        Ok(())
    }
}

/// The source part and copy id of a stored path, borrowed ([`PhotoRef::parse`]
/// without allocating).
pub fn split(text: &str) -> (&str, Option<&str>) {
    match text.rsplit_once(COPY_MARKER) {
        Some((source, id)) => (source, Some(id)),
        None => (text, None),
    }
}

/// What an editor sidecar's extension is.
pub const SIDECAR_EXTENSION: &str = "rrdata";

/// What an editor sidecar's file name ends with (the extension and its dot).
pub const SIDECAR_SUFFIX: &str = ".rrdata";

/// The file name of `source`'s editor sidecar for `copy` (`None` = the
/// primary photo's): `<file>.rrdata` or `<file>.<copy id>.rrdata`. The one
/// naming rule beside [`PhotoRef::sidecar_path`]: built like the name, so a
/// non-UTF-8 file name stays as it is.
pub fn sidecar_name(source: &Path, copy: Option<&str>) -> OsString {
    let mut name = source.file_name().unwrap_or_default().to_os_string();
    if let Some(id) = copy {
        name.push(".");
        name.push(id);
    }
    name.push(SIDECAR_SUFFIX);
    name
}

/// The source file name and copy id of an editor sidecar's file name:
/// `<file>.rrdata` → `(<file>, None)`, `<file>.<editor id>.rrdata` →
/// `(<file>, Some(id))`; `None` for a name that is not a sidecar's. A
/// middle part that is not an editor id belongs to the file name.
pub fn sidecar_name_parts(name: &str) -> Option<(&str, Option<&str>)> {
    let base = name.strip_suffix(SIDECAR_SUFFIX)?;
    match base.rsplit_once('.') {
        Some((file, id)) if is_editor_id(id) => Some((file, Some(id))),
        _ => Some((base, None)),
    }
}

/// The photograph an editor sidecar lies beside: `/p/a.raf.rrdata` →
/// `/p/a.raf`, `/p/a.raf.0f1e2d.rrdata` → `/p/a.raf`; `None` for a path
/// that is not an editor sidecar's. The inverse of [`PhotoRef::sidecar_path`]
/// when the file it names is there.
pub fn source_path_of_sidecar(sidecar: &Path) -> Option<PathBuf> {
    let name = sidecar.file_name()?.to_str()?;
    Some(sidecar.with_file_name(sidecar_name_parts(name)?.0))
}

/// The source file's path of a stored path.
pub fn source_of(text: &str) -> &str {
    split(text).0
}

/// Whether a stored path names a virtual copy.
pub fn is_copy(text: &str) -> bool {
    text.contains(COPY_MARKER)
}

/// Whether the stored path `text` names a virtual copy of `source`.
pub fn is_copy_of(text: &str, source: &str) -> bool {
    matches!(split(text), (s, Some(_)) if s == source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Vector {
        text: String,
        source: String,
        copy: Option<String>,
    }

    /// The vectors the frontend's `photoRef.ts` tests read too.
    fn vectors() -> Vec<Vector> {
        serde_json::from_str(include_str!("photo_ref_vectors.json")).unwrap()
    }

    #[test]
    fn every_stored_form_parses_and_formats_back_byte_for_byte() {
        let vectors = vectors();
        assert!(vectors.len() >= 15);
        for v in vectors {
            let photo = PhotoRef::parse(&v.text);
            assert_eq!(photo.source(), v.source, "{}", v.text);
            assert_eq!(photo.copy_id(), v.copy.as_deref(), "{}", v.text);
            assert_eq!(photo.format(), v.text);
            assert_eq!(split(&v.text), (v.source.as_str(), v.copy.as_deref()));
            assert_eq!(source_of(&v.text), v.source);
            assert_eq!(is_copy(&v.text), v.copy.is_some());
            assert_eq!(
                PhotoRef::new(v.source.clone(), v.copy.clone().map(CopyId::new)),
                photo
            );
            if let Some(copy) = &v.copy {
                assert!(is_copy_of(&v.text, &v.source));
                assert_eq!(
                    photo.with_source("/q/b.raf").format(),
                    format!("/q/b.raf?vc={copy}")
                );
            }
        }
    }

    #[test]
    fn sidecars_are_named_after_the_file_and_the_copy() {
        let dir = Path::new("/p");
        assert_eq!(
            PhotoRef::parse("/p/a.raf").sidecar_path(),
            dir.join("a.raf.rrdata")
        );
        assert_eq!(
            PhotoRef::parse("/p/a.raf?vc=0f1e2d").sidecar_path(),
            dir.join("a.raf.0f1e2d.rrdata")
        );
        assert_eq!(
            PhotoRef::of_path(&dir.join("a.raf"), Some("0f1e2d")).format(),
            "/p/a.raf?vc=0f1e2d"
        );
        assert_eq!(
            PhotoRef::of_path(&dir.join("a.raf"), None).format(),
            "/p/a.raf"
        );
    }

    /// The sidecar naming rule replaces the `format!` calls of the call
    /// sites (`{name}.rrdata`, `{name}.{copy}.rrdata`): the same name, byte
    /// for byte, for the primary photo, a virtual copy and names that hold
    /// dots or `?vc=`.
    #[test]
    fn sidecar_names_match_the_written_out_form() {
        let sources = [
            "/p/a.raf",
            "/p/a.raf?vc=0f1e2d",
            "/p/a.b.c.rrdata/name",
            "/p/a.raf.jpg",
            "/p/img?vc=abc.raf",
            "/p/2019.07.14 DSC_1234.ARW",
            "/p/Grün .nef",
            "/p/.hidden",
            "a.raf",
        ];
        for source in sources {
            let path = Path::new(source);
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            for copy in [None, Some("0f1e2d"), Some("1"), Some("")] {
                let want = match copy {
                    Some(copy) => format!("{name}.{copy}.rrdata"),
                    None => format!("{name}.rrdata"),
                };
                assert_eq!(sidecar_name(path, copy).to_string_lossy(), want);
                assert_eq!(
                    PhotoRef::primary(source).with_copy(copy),
                    PhotoRef::new(source, copy.map(CopyId::new))
                );
                let photo = PhotoRef::new(source, copy.map(CopyId::new));
                assert_eq!(photo.sidecar_name().to_string_lossy(), want);
                assert_eq!(photo.sidecar_path(), path.with_file_name(&want));
                // Same also for a stored form whose copy the parse finds.
                if let Some(copy) = copy.filter(|_| !source.contains(COPY_MARKER)) {
                    let stored = format!("{source}{COPY_MARKER}{copy}");
                    let parsed = PhotoRef::parse(&stored);
                    assert_eq!(parsed.sidecar_path(), path.with_file_name(&want));
                    assert_eq!(parsed.format(), stored);
                }
            }
        }
        // The name a `Path` gives keeps a non-UTF-8 file name as it is.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let source = Path::new(std::ffi::OsStr::from_bytes(b"/p/a\xff.raf"));
            let name = sidecar_name(source, None);
            assert_eq!(name.as_encoded_bytes(), b"a\xff.raf.rrdata");
        }
    }

    #[test]
    fn a_sidecar_names_its_photograph() {
        let sidecar = |name: &str| Path::new("/p").join(name);
        assert_eq!(
            source_path_of_sidecar(&sidecar("a.raf.rrdata")),
            Some(PathBuf::from("/p/a.raf"))
        );
        assert_eq!(
            source_path_of_sidecar(&sidecar("a.raf.0f1e2d.rrdata")),
            Some(PathBuf::from("/p/a.raf"))
        );
        // Not an editor id's: part of the file name, as everywhere.
        assert_eq!(
            source_path_of_sidecar(&sidecar("a.raf.ABCDEF.rrdata")),
            Some(PathBuf::from("/p/a.raf.ABCDEF"))
        );
        assert_eq!(source_path_of_sidecar(&sidecar("a.raf.xmp")), None);
        // Round trip: a photograph's sidecar names it again.
        for source in ["/p/a.raf", "/p/a.b.jpg", "/p/Grün .nef"] {
            assert_eq!(
                source_path_of_sidecar(&PhotoRef::primary(source).sidecar_path()),
                Some(PathBuf::from(source))
            );
        }
    }

    #[test]
    fn a_copy_of_is_only_a_copy_of_that_exact_source() {
        assert!(is_copy_of("/p/a.raf?vc=1", "/p/a.raf"));
        assert!(!is_copy_of("/p/a.raf", "/p/a.raf"));
        assert!(!is_copy_of("/p/a.raf.jpg?vc=1", "/p/a.raf"));
        assert!(!is_copy_of("/p/b.raf?vc=1", "/p/a.raf"));
        // The source is what precedes the last marker.
        assert!(!is_copy_of("/p/a.raf?vc=b.raf?vc=abc123", "/p/a.raf"));
        assert!(is_copy_of(
            "/p/a.raf?vc=b.raf?vc=abc123",
            "/p/a.raf?vc=b.raf"
        ));
    }

    #[test]
    fn sidecar_names_give_their_file_and_copy() {
        assert_eq!(sidecar_name_parts("a.raf.rrdata"), Some(("a.raf", None)));
        assert_eq!(
            sidecar_name_parts("a.raf.0f1e2d.rrdata"),
            Some(("a.raf", Some("0f1e2d")))
        );
        assert_eq!(
            sidecar_name_parts("a.raf.ABCDEF.rrdata"),
            Some(("a.raf.ABCDEF", None))
        );
        assert_eq!(sidecar_name_parts("0f1e2d.rrdata"), Some(("0f1e2d", None)));
        assert_eq!(
            sidecar_name_parts(".0f1e2d.rrdata"),
            Some(("", Some("0f1e2d")))
        );
        assert_eq!(
            sidecar_name_parts("Grün.0f1e2d.rrdata"),
            Some(("Grün", Some("0f1e2d")))
        );
        assert_eq!(sidecar_name_parts("a.raf"), None);
    }

    #[test]
    fn editor_ids_are_six_lower_case_hex_digits() {
        assert!(CopyId::new("0f1e2d").is_editor_id());
        for id in ["", "0f1e2", "0F1E2D", "0f1e2d0", "copy12"] {
            assert!(!is_editor_id(id), "{id}");
        }
    }
}
