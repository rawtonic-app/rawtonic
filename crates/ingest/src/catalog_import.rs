//! Import from Catalog (G39): brings the work of another Rawtonic catalog
//! (or session) — typically one made by Export as Catalog and edited on
//! another computer — into this catalog.
//!
//! The other library is never written: its database is read through a
//! snapshot in this catalog's `.cache` (migrated there when it is older),
//! its files only read. `review` lists what is new, changed and unchanged
//! (`rawtonic_catalog::catalog_import`); `import` reviews again, copies the
//! edit resources the imported photographs use into this catalog's
//! `resources/`, copies the originals of new photographs when asked — and
//! always those inside the other library's folder, which belong to it —
//! (verified, no clobber, staged and only put in place before the database
//! transaction, taken back when it fails) and applies everything in one
//! transaction.
use crate::catalog_export::folder_names;
use crate::transfer::{ResourceCopier, check, copy_file};
use anyhow::{Context, Result, ensure};
use rawtonic_catalog::{
    Library, Mode,
    catalog_import::{Changed, Choice, Incoming, Plan, Report, Side, Status},
    portable::ResourceUse,
    subset::{read_manifest, snapshot_database},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

/// The other library, read through a snapshot of its database.
pub struct Source {
    /// Holds the snapshot; removed when dropped.
    _temp: tempfile::TempDir,
    pub snapshot: Library,
    /// The other library's folder.
    pub directory: PathBuf,
}

/// Opens the library at `other` for import into `target` (a catalog).
pub fn open_source(target: &Library, other: &Path) -> Result<Source> {
    ensure!(
        target.manifest.mode == Mode::Catalog,
        "Import from Catalog brings photographs into a catalog; a session stays self-contained"
    );
    let other = other
        .canonicalize()
        .with_context(|| format!("{} is not available", other.display()))?;
    ensure!(
        other != target.directory,
        "Choose another catalog than the open one"
    );
    let mut manifest = read_manifest(&other)?;
    let cache = target.directory.join(".cache");
    fs::create_dir_all(&cache)?;
    let temp = tempfile::Builder::new()
        .prefix("import-")
        .tempdir_in(&cache)?;
    snapshot_database(&other, &manifest, temp.path())?;
    manifest.mode = Mode::Catalog;
    rawtonic_metadata::atomic_write(
        &temp.path().join("catalog.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    // Opening migrates the snapshot (an older catalog), never the other.
    let snapshot = Library::open(temp.path())?;
    snapshot.check_integrity()?;
    Ok(Source {
        _temp: temp,
        snapshot,
        directory: other,
    })
}

/// What the review dialog shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub name: String,
    /// New and changed photographs (unchanged ones are only counted).
    pub photos: Vec<Incoming>,
    pub new: usize,
    pub changed: usize,
    pub unchanged: usize,
    /// Photographs inside a third library's folder: not imported (their
    /// latest edit is in that library).
    pub left_out: usize,
}

/// Whether `photo` lies inside a third library's folder (neither the other
/// catalog's nor this one's): that library holds its latest edit, so the
/// other catalog's record of it may be older. Such photographs are left out.
fn owned_elsewhere(target: &Library, source: &Source, photo: &Incoming) -> bool {
    rawtonic_catalog::session_for(&photo.path)
        .is_some_and(|library| library != source.directory && library != target.directory)
}

/// The photographs of the other library this import can take.
fn incoming(target: &Library, source: &Source) -> Result<(Vec<Incoming>, usize)> {
    let all = target.review_import(&source.snapshot, &source.directory)?;
    let count = all.len();
    let kept: Vec<Incoming> = all
        .into_iter()
        .filter(|photo| !owned_elsewhere(target, source, photo))
        .collect();
    let left_out = count - kept.len();
    Ok((kept, left_out))
}

pub fn review(target: &Library, source: &Source) -> Result<Review> {
    let (all, left_out) = incoming(target, source)?;
    let count = |status| all.iter().filter(|p| p.status == status).count();
    Ok(Review {
        left_out,
        name: source.snapshot.manifest.name.clone(),
        new: count(Status::New),
        changed: count(Status::Changed),
        unchanged: count(Status::Unchanged),
        photos: all
            .into_iter()
            .filter(|p| p.status != Status::Unchanged)
            .collect(),
    })
}

/// How new photographs come in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "mode", content = "folder")]
pub enum NewPhotos {
    /// Registered where the other catalog has them.
    Reference,
    /// Copied (verified, no clobber) into this folder, in a folder per
    /// folder they come from.
    Copy(PathBuf),
}

/// The photographer's choices.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub new_photos: NewPhotos,
    /// One choice for every changed photograph; `None`: Replace, except
    /// Keep current for those only this catalog changed.
    pub changed: Option<Choice>,
    /// Per photograph (by its id in the other catalog), over `changed`.
    #[serde(default)]
    pub overrides: HashMap<String, Choice>,
    /// Bring in the new photographs at all.
    #[serde(default = "yes")]
    pub include_new: bool,
    /// The name of the snapshot that keeps an edit Replace replaces (the
    /// app's translation of "Before import from …").
    #[serde(default = "before_import")]
    pub before_snapshot: String,
}

fn before_import() -> String {
    "Before import".into()
}

fn yes() -> bool {
    true
}

/// The default for a changed photograph: Replace only when the other
/// catalog alone changed it since their last common state; Keep current
/// when this catalog did (alone or as well), so newer work here is never
/// replaced unasked.
pub fn default_choice(photo: &Incoming) -> Choice {
    if photo.side == Some(Side::There) {
        Choice::Replace
    } else {
        Choice::Keep
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub phase: &'static str,
    pub done: usize,
    pub total: usize,
}

/// What an import did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    #[serde(flatten)]
    pub report: Report,
    pub unchanged: usize,
    pub originals_copied: usize,
    /// New photographs whose original was not there: added as missing.
    pub offline: usize,
    pub missing_resources: usize,
    /// Photographs inside a third library's folder, not imported.
    pub left_out: usize,
}

/// Imports the library at `other` into `target` as `options` says.
pub fn import(
    target: &Library,
    other: &Path,
    options: &Options,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<Receipt> {
    let source = open_source(target, other)?;
    let (photos, left_out) = incoming(target, &source)?;
    let mut receipt = Receipt {
        unchanged: photos
            .iter()
            .filter(|p| p.status == Status::Unchanged)
            .count(),
        left_out,
        ..Receipt::default()
    };
    let new: Vec<&Incoming> = photos
        .iter()
        .filter(|p| p.status == Status::New && options.include_new)
        .collect();
    let changed: Vec<Changed> = photos
        .iter()
        .filter(|p| p.status == Status::Changed)
        .map(|p| Changed {
            incoming: p.id.clone(),
            target: p.target.clone().expect("a changed photograph has one here"),
            choice: options
                .overrides
                .get(&p.id)
                .copied()
                .or(options.changed)
                .unwrap_or_else(|| default_choice(p)),
            changes: p.changes.clone(),
        })
        .collect();
    let used: Vec<String> = new
        .iter()
        .map(|p| p.id.clone())
        .chain(
            changed
                .iter()
                .filter(|c| c.choice != Choice::Keep)
                .map(|c| c.incoming.clone()),
        )
        .collect();
    // Photographs the same in both: their common state now (before the
    // snapshot keeps only what comes in). A record that cannot be written
    // only makes a later import ask more (changed on both sides).
    let same: Vec<(String, String)> = photos
        .iter()
        .filter(|p| p.status == Status::Unchanged)
        .filter_map(|p| Some((p.id.clone(), p.target.clone()?)))
        .collect();
    if let Err(error) = target.record_import(&source.snapshot, &source.directory, &same) {
        log::warn!("Import from Catalog: common states not recorded: {error:#}");
    }
    if used.is_empty() {
        receipt.report.kept = changed.len();
        return Ok(receipt);
    }
    // Only what comes in: no other photograph's resources are copied.
    source.snapshot.retain_only(&used)?;
    check(cancel)?;
    // The edit resources the imported photographs use, into this catalog's
    // `resources/` (references in a database are its library's own).
    let mut copier = ResourceCopier::new(&target.directory, &source.directory, cancel);
    source
        .snapshot
        .rewrite_resources(|value, used: ResourceUse<'_>| {
            // The other library's presets, styles and export recipes are not
            // imported: their files stay where they are.
            if used.owner.is_none() && !used.current {
                return Ok(value.to_string());
            }
            copier.rewrite(value, used)
        })?;
    receipt.missing_resources = copier.missing.len();
    drop(copier);
    let mut placed = Placed::default();
    let mut plan = Plan {
        new: Vec::new(),
        changed,
        batch_kind: "add",
        before: options.before_snapshot.clone(),
    };
    // A new photograph inside the other library's folder (its own copies)
    // belongs to it: referring to it would edit it in that library's name.
    // It is always copied — to the chosen folder, or by reference into
    // this catalog's own `Photos/`; only photographs outside any library
    // (or already inside this catalog) are referred to where they are.
    // Those inside a third library were left out above.
    let (folder, copy_all) = match &options.new_photos {
        NewPhotos::Copy(folder) => (folder.clone(), true),
        NewPhotos::Reference => (target.directory.join(&target.manifest.capture), false),
    };
    let (to_copy, referenced): (Vec<&Incoming>, Vec<&Incoming>) = new
        .iter()
        .copied()
        .partition(|p| copy_all || p.path.starts_with(&source.directory));
    for photo in referenced {
        if !photo.available {
            receipt.offline += 1;
        }
        plan.new.push((photo.id.clone(), photo.path.clone()));
    }
    if !to_copy.is_empty() {
        if copy_all {
            plan.batch_kind = "ingest";
        }
        fs::create_dir_all(&folder)?;
        let copied = copy_originals(
            target,
            &source,
            &to_copy,
            &folder,
            cancel,
            &mut progress,
            &mut placed,
        )?;
        receipt.originals_copied = copied.len();
        plan.new.extend(copied);
    }
    progress(Progress {
        phase: "applying",
        done: 0,
        total: used.len(),
    });
    match check(cancel).and_then(|_| target.apply_import(&source.snapshot, &plan)) {
        Ok(report) => {
            placed.keep();
            // What came in is now the same here: the new common state.
            let pairs: Vec<(String, String)> = plan
                .new
                .iter()
                .map(|(id, _)| (id.clone(), id.clone()))
                .chain(
                    plan.changed
                        .iter()
                        .filter(|c| c.choice == Choice::Replace)
                        .map(|c| (c.incoming.clone(), c.target.clone())),
                )
                .collect();
            if let Err(error) = target.record_import(&source.snapshot, &source.directory, &pairs) {
                log::warn!("Import from Catalog: common states not recorded: {error:#}");
            }
            receipt.report = report;
            Ok(receipt)
        }
        Err(error) => {
            placed.take_back();
            Err(error)
        }
    }
}

/// Folders of copied originals put in place before the database
/// transaction: taken back into their staging folder (and removed with it)
/// unless the import is kept.
#[derive(Default)]
struct Placed {
    stage: Option<tempfile::TempDir>,
    moves: Vec<(PathBuf, PathBuf)>,
}

impl Placed {
    fn keep(&mut self) {
        self.moves.clear();
    }

    fn take_back(&mut self) {
        for (staged, placed) in self.moves.drain(..).rev() {
            if let Err(error) = rawtonic_fileops::rename_no_clobber(&placed, &staged) {
                log::warn!(
                    "Could not take back {} after a failed import: {error}",
                    placed.display()
                );
            }
        }
    }
}

impl Drop for Placed {
    fn drop(&mut self) {
        self.take_back();
        drop(self.stage.take());
    }
}

/// Copies the originals of `new` (with their XMP and companion JPEG) into
/// `folder/<their folder's name>/`, verified, then puts each folder in
/// place under a free name. Returns (id, new place).
fn copy_originals(
    target: &Library,
    source: &Source,
    new: &[&Incoming],
    folder: &Path,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress),
    placed: &mut Placed,
) -> Result<Vec<(String, PathBuf)>> {
    let folder = folder
        .canonicalize()
        .with_context(|| format!("{} is not available", folder.display()))?;
    ensure!(
        !folder.starts_with(&source.directory),
        "Choose a folder outside the catalog you import from"
    );
    // A folder another library owns would make the copies its photographs
    // (their edits would go there, not here).
    if let Some(library) = rawtonic_catalog::session_for(&folder.join("_"))
        && library != target.directory
    {
        anyhow::bail!(
            "{} belongs to {}; choose a folder outside other projects and catalogs",
            folder.display(),
            library.display()
        );
    }
    let missing: Vec<String> = new
        .iter()
        .filter(|p| !p.available)
        .map(|p| p.path.display().to_string())
        .collect();
    ensure!(
        missing.is_empty(),
        "Originals not available ({}); connect their drive or add them by reference:\n{}",
        missing.len(),
        missing.join("\n")
    );
    let stage = tempfile::Builder::new()
        .prefix(".rawtonic-import-")
        .tempdir_in(&folder)?;
    let stage_root = stage.path().to_path_buf();
    placed.stage = Some(stage);
    let originals: Vec<PathBuf> = new.iter().map(|p| p.path.clone()).collect();
    let names = folder_names(&originals);
    let mut staged = Vec::new();
    for (index, photo) in new.iter().enumerate() {
        check(cancel)?;
        let asset = source.snapshot.asset(&photo.id)?;
        let name = &names[photo.path.parent().context("Original has no folder")?];
        let file = photo.path.file_name().context("Original has no filename")?;
        let to = stage_root.join(name).join(file);
        ensure!(!to.exists(), "Two photographs would share {}", to.display());
        copy_file(&photo.path, &to, cancel)?;
        ensure!(
            fs::metadata(&to)?.len() == asset.size
                && rawtonic_catalog::fingerprint(&to)? == asset.fingerprint,
            "Original has changed since it was registered: {}",
            photo.path.display()
        );
        let mut sidecars = Vec::new();
        let xmp = photo.path.with_extension("xmp");
        if xmp.try_exists()? {
            sidecars.push(xmp);
        }
        if let Some(record) = source.snapshot.companion_record(&asset)?
            && let Some(companion) = record.path.file_name()
        {
            let companion = photo.path.with_file_name(companion);
            if record.matches(&companion) {
                sidecars.push(companion);
            }
        }
        for sidecar in sidecars {
            let beside = to.with_file_name(sidecar.file_name().context("Sidecar has no name")?);
            if !beside.exists() {
                copy_file(&sidecar, &beside, cancel)?;
            }
        }
        staged.push((photo.id.clone(), PathBuf::from(name), PathBuf::from(file)));
        progress(Progress {
            phase: "originals",
            done: index + 1,
            total: new.len(),
        });
    }
    check(cancel)?;
    // Each staged folder under a name free in `folder`.
    let mut place_of = HashMap::new();
    let mut taken = HashSet::new();
    for (_, name, _) in &staged {
        if place_of.contains_key(name) {
            continue;
        }
        let base = name.to_string_lossy().into_owned();
        let mut candidate = base.clone();
        let mut n = 2;
        while folder.join(&candidate).exists() || !taken.insert(candidate.clone()) {
            candidate = format!("{base} ({n})");
            n += 1;
        }
        let from = stage_root.join(name);
        let to = folder.join(&candidate);
        rawtonic_fileops::rename_no_clobber(&from, &to)
            .with_context(|| format!("Cannot put the copies in {}", to.display()))?;
        placed.moves.push((from, to.clone()));
        place_of.insert(name.clone(), to);
    }
    Ok(staged
        .into_iter()
        .map(|(id, name, file)| (id, place_of[&name].join(file)))
        .collect())
}
