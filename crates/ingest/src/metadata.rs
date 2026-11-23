//! Opt-in import metadata. Applied to registered, verified copies before
//! their ingest item is done; resumptions repeat this idempotent operation.
use crate::IngestError;
use crate::error::ensure;
use anyhow::Result;
use rawtonic_catalog::Library;
use rawtonic_metadata::{IptcKind, MAX_KEYWORDS, Metadata, iptc_field};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportMetadata {
    pub creator: String,
    pub copyright: String,
    pub description: String,
    pub iptc: BTreeMap<String, String>,
    pub keywords: Vec<String>,
    pub include_creator_contact: bool,
    /// Field keys, defaulting to fill-empty. Empty preset values never clear.
    pub overwrite: BTreeSet<String>,
    /// Star ratings (1–5) given to single photographs in the import dialog,
    /// by source path as the import lists it. They replace the photograph's
    /// own rating on the imported copy; nothing is written to the source.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub ratings: BTreeMap<PathBuf, u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataPreset {
    /// Empty for an unsaved preset (a watch started from a new, unsaved
    /// preset keeps its values without an identity).
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub metadata: ImportMetadata,
}

impl ImportMetadata {
    pub fn validate(&self) -> Result<(), IngestError> {
        for rating in self.ratings.values() {
            ensure!(
                (1..=5).contains(rating),
                "Import rating {rating} is not 1 to 5"
            );
        }
        let supported_iptc = |key: &str| {
            matches!(key, "usageTerms" | "credit")
                || iptc_field(key).is_some_and(|(_, _, kind)| kind == IptcKind::Contact)
        };
        for key in self.iptc.keys() {
            ensure!(supported_iptc(key), "Unsupported import IPTC field {key}");
        }
        for key in &self.overwrite {
            ensure!(
                matches!(key.as_str(), "creator" | "copyright" | "description")
                    || supported_iptc(key),
                "Unsupported import metadata field {key}"
            );
        }
        Ok(Metadata {
            creator: self.creator.clone(),
            copyright: self.copyright.clone(),
            description: self.description.clone(),
            iptc: self.iptc.clone(),
            keywords: self.keywords.clone(),
            ..Metadata::default()
        }
        .validate()?)
    }

    pub fn apply(&self, metadata: &mut Metadata, year: i32) {
        for (key, target, value) in [
            ("creator", &mut metadata.creator, self.creator.clone()),
            (
                "copyright",
                &mut metadata.copyright,
                self.copyright.replace("{year}", &year.to_string()),
            ),
            (
                "description",
                &mut metadata.description,
                self.description.clone(),
            ),
        ] {
            if !value.trim().is_empty()
                && (target.trim().is_empty() || self.overwrite.contains(key))
            {
                *target = value.trim().into();
            }
        }
        for (key, value) in &self.iptc {
            let contact = iptc_field(key).is_some_and(|(_, _, kind)| kind == IptcKind::Contact);
            if (!contact || self.include_creator_contact)
                && !value.trim().is_empty()
                && (metadata.iptc.get(key).is_none_or(|v| v.trim().is_empty())
                    || self.overwrite.contains(key))
            {
                metadata.set_iptc(key, value);
            }
        }
        for keyword in &self.keywords {
            let keyword = keyword.trim();
            if !keyword.is_empty() && !metadata.keywords.iter().any(|k| k == keyword) {
                metadata.keywords.push(keyword.into());
            }
        }
    }

    /// `contents` of a captured photograph with this preset merged in, as
    /// the import merges it into a registered copy: the file's own
    /// metadata (read from the verified copy `photo`), then this preset by
    /// the import's rules (fill-empty, `overwrite`, `{year}`). `contents`
    /// holds what the capture was given (rating, keywords) or, with
    /// `adopted`, the metadata of its card sidecar, whose rating and flag
    /// stay. The note says the keywords could not all be added (`merge`).
    pub(crate) fn for_registration(
        &self,
        photo: &Path,
        source: &Path,
        header: Option<rawtonic_naming::Capture>,
        contents: Option<&rawtonic_catalog::Contents>,
        adopted: bool,
    ) -> Result<(rawtonic_catalog::Contents, Option<String>)> {
        let embedded = Self::embedded(photo, source, header)?;
        let mut contents = contents.cloned().unwrap_or_default();
        let given = contents.metadata.take().unwrap_or_default();
        let typed_rating = if adopted { 0 } else { given.rating };
        let mut metadata = given;
        let note = self.merge(source, embedded, &mut metadata, adopted);
        if typed_rating > 0 {
            metadata.rating = typed_rating;
        }
        contents.metadata = Some(metadata);
        Ok((contents, note))
    }

    /// Whether this holds nothing but ratings: then only the rated
    /// photographs are touched, the others are imported as they come.
    fn ratings_only(&self) -> bool {
        self == &Self {
            ratings: self.ratings.clone(),
            ..Self::default()
        }
    }

    /// Whether the photograph imported from `source` gets anything from
    /// this.
    pub(crate) fn touches(&self, source: &Path) -> bool {
        !self.ratings_only() || self.ratings.contains_key(source)
    }

    /// What the photograph `photo` (copied from `source`) brings itself: its
    /// embedded metadata and the year `{year}` stands for. Reads the file
    /// only; `header` is its header as registration read it.
    pub(crate) fn embedded(
        photo: &Path,
        source: &Path,
        header: Option<rawtonic_naming::Capture>,
    ) -> Result<Embedded> {
        let (metadata, embedded_year) = crate::capture::import_metadata(photo, header)?;
        let year = embedded_year.unwrap_or_else(|| {
            let local = std::fs::metadata(source)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|time| jiff::Timestamp::try_from(time).ok())
                .map(|time| time.to_zoned_datetime(rawtonic_runtime::local_time::zone()))
                .unwrap_or_else(rawtonic_runtime::local_time::now);
            i32::from(local.year())
        });
        Ok(Embedded { metadata, year })
    }

    /// Merges `embedded` and this preset into the photograph's `metadata`.
    /// Returns a note for the import result when the keywords could not be
    /// added: the photograph would hold more than `MAX_KEYWORDS`. Its own
    /// keywords are then kept as they were and the rest of the preset is
    /// applied: a deterministic per-photo result, never an error, so the
    /// photograph is imported and a resumed job does not stop at it again.
    pub(crate) fn merge(
        &self,
        source: &Path,
        embedded: Embedded,
        metadata: &mut Metadata,
        initialized: bool,
    ) -> Option<String> {
        let Embedded {
            metadata: embedded,
            year,
        } = embedded;
        // Sidecar/catalog values take precedence; bring in embedded values only
        // where absent, so a fill-empty preset cannot replace embedded rights.
        for (target, value) in [
            (&mut metadata.creator, embedded.creator),
            (&mut metadata.copyright, embedded.copyright),
            (&mut metadata.description, embedded.description),
            (&mut metadata.title, embedded.title),
            (&mut metadata.label, embedded.label),
        ] {
            if target.trim().is_empty() {
                *target = value;
            }
        }
        // A preset must not make Add ineligible for the later camera-rating
        // pass before adopting that rating. Explicit sidecar zero/reject wins.
        if !initialized {
            metadata.rating = embedded.rating;
            metadata.flag = embedded.flag;
        }
        for (key, value) in embedded.iptc {
            if metadata.iptc.get(&key).is_none_or(|v| v.trim().is_empty()) {
                metadata.set_iptc(&key, &value);
            }
        }
        let own_keywords = metadata.keywords.clone();
        for keyword in embedded.keywords {
            if !metadata.keywords.contains(&keyword) {
                metadata.keywords.push(keyword);
            }
        }
        self.apply(metadata, year);
        // Given to this photograph in the import dialog: the photographer's
        // latest call, over the camera's and any sidecar's.
        if let Some(rating) = self.ratings.get(source) {
            metadata.rating = *rating;
        }
        (metadata.keywords.len() > MAX_KEYWORDS).then(|| {
            metadata.keywords = own_keywords;
            format!(
                "Import keywords not applied: the photograph would have more than {MAX_KEYWORDS} keywords"
            )
        })
    }
}

/// What a photograph brings to an import preset (`ImportMetadata::embedded`).
pub(crate) struct Embedded {
    metadata: Metadata,
    year: i32,
}

/// The turn of the app's metadata writers (its rating, label, keyword and
/// text edits): taken around the preset's read, change and write of a
/// photograph's metadata, so neither writes back metadata read before the
/// other's write. The guard is held until it is dropped.
pub type WriteTurn<'a> = &'a dyn Fn() -> Box<dyn std::any::Any>;

/// For a library no other writer uses (tests, the CLI).
pub fn no_turn() -> Box<dyn std::any::Any> {
    Box::new(())
}

/// Adds `note` to `result`'s warnings.
pub(crate) fn warn(result: &mut crate::ImportResult, note: String) {
    result.warning = Some(match result.warning.take() {
        Some(earlier) => format!("{earlier}; {note}"),
        None => note,
    });
}

/// Only called for this job's successfully published photographs, never a
/// duplicate already in the library. The catalog's writer and conflict-aware
/// atomic XMP writer keep both sides recoverable if sidecar sync fails.
/// Catalog errors are returned (the job stops and is resumed later); what
/// the preset cannot add to this photograph is a warning on its result.
pub(crate) fn apply_to_copy(
    library: &Library,
    result: &mut crate::ImportResult,
    preset: &ImportMetadata,
    turn: WriteTurn<'_>,
) -> Result<()> {
    if result.error.is_some() || result.skipped || result.companion_of.is_some() {
        return Ok(());
    }
    let Some(id) = result.asset_id.clone() else {
        return Ok(());
    };
    if !preset.touches(&result.source) {
        return Ok(());
    }
    let id = id.as_str();
    let asset = library.asset(id)?;
    // The file is read before the turn, which is held only for the catalog.
    let embedded = ImportMetadata::embedded(&asset.path, &result.source, result.header.take())?;
    let _turn = turn();
    let mut metadata = library.metadata(id)?;
    let note = preset.merge(
        &result.source,
        embedded,
        &mut metadata,
        library.metadata_initialized(id)?,
    );
    library.set_metadata(id, &metadata)?;
    if let Some(note) = note {
        warn(result, note);
    }
    match library.sync_xmp(id) {
        Ok(()) => result.sidecar = rawtonic_metadata::sidecar_path(&asset.path),
        Err(error) => warn(
            result,
            format!("Import metadata is saved in the catalog; XMP was not synced: {error}"),
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fill_empty_overwrite_year_contacts_and_keywords() {
        let mut preset = ImportMetadata {
            creator: "New".into(),
            copyright: "© {year} / {year}".into(),
            keywords: vec!["old".into(), "new".into(), "new".into()],
            ..ImportMetadata::default()
        };
        preset
            .iptc
            .insert("creatorEmail".into(), "new@example.org".into());
        preset.iptc.insert("usageTerms".into(), "New terms".into());
        let mut old = Metadata {
            creator: "Old".into(),
            keywords: vec!["old".into()],
            ..Metadata::default()
        };
        old.set_iptc("usageTerms", "Existing terms");
        old.set_iptc("creatorEmail", "old@example.org");
        preset.apply(&mut old, 2001);
        assert_eq!(old.creator, "Old");
        assert_eq!(old.copyright, "© 2001 / 2001");
        assert_eq!(old.keywords, ["old", "new"]);
        assert_eq!(old.iptc["usageTerms"], "Existing terms");
        assert_eq!(old.iptc["creatorEmail"], "old@example.org");
        preset
            .overwrite
            .extend(["creator".into(), "usageTerms".into(), "creatorEmail".into()]);
        preset.apply(&mut old, 2002);
        assert_eq!(old.creator, "New");
        assert_eq!(old.iptc["usageTerms"], "New terms");
        assert_eq!(
            old.iptc["creatorEmail"], "old@example.org",
            "contact opt-in is independent of overwrite"
        );
        preset.include_creator_contact = true;
        preset.apply(&mut old, 2002);
        assert_eq!(old.iptc["creatorEmail"], "new@example.org");
        preset.creator.clear();
        preset.apply(&mut old, 2002);
        assert_eq!(old.creator, "New", "blank presets do not clear");
        assert!(
            !serde_json::from_str::<ImportMetadata>("{}")
                .unwrap()
                .include_creator_contact
        );
    }
}
