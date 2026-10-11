//! Ingest of a list of photographs, as Lightroom's and Capture One's import:
//! the sources the user chose or dropped (folders opened up), which of them
//! the library already holds, and the copy itself one file after the other,
//! each reported as soon as it is in the library so the app shows it while
//! the rest still copy.
use crate::{ImportResult, destination_folder, import_one_into};
use anyhow::Result;
use rawtonic_catalog::Library;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

fn hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
}

struct Expansion<'a> {
    accept: &'a dyn Fn(&Path) -> bool,
    found: Vec<PathBuf>,
    seen: HashSet<PathBuf>,
    limit: usize,
}

impl Expansion<'_> {
    /// Takes `file`; `false` once the limit is reached.
    fn take(&mut self, file: &Path) -> bool {
        if !(self.accept)(file) || self.seen.contains(file) {
            return true;
        }
        if self.found.len() >= self.limit {
            return false;
        }
        self.seen.insert(file.to_path_buf());
        self.found.push(file.to_path_buf());
        true
    }

    fn walk(&mut self, folder: &Path) -> bool {
        let Ok(entries) = fs::read_dir(folder) else {
            return true;
        };
        let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for entry in entries {
            if hidden(&entry) {
                continue;
            }
            // Symlinks are not followed into folders (no loops, no surprises).
            let Ok(meta) = fs::symlink_metadata(&entry) else {
                continue;
            };
            let more = if meta.is_dir() {
                self.walk(&entry)
            } else if entry.is_file() {
                self.take(&entry)
            } else {
                true
            };
            if !more {
                return false;
            }
        }
        true
    }
}

/// The files to import from what was chosen or dropped: files as they are,
/// folders with every file inside them (recursively, in name order; hidden
/// files and folders and symlinked folders left out), each only if `accept`
/// takes it, without repeats and at most `limit`. The `bool` tells whether
/// files were left out for the limit.
pub fn expand_sources(
    paths: &[PathBuf],
    accept: impl Fn(&Path) -> bool,
    limit: usize,
) -> (Vec<PathBuf>, bool) {
    let mut expansion = Expansion {
        accept: &accept,
        found: Vec::new(),
        seen: HashSet::new(),
        limit,
    };
    for path in paths {
        let more = if path.is_dir() {
            expansion.walk(path)
        } else if path.is_file() && !hidden(path) {
            expansion.take(path)
        } else {
            true
        };
        if !more {
            return (expansion.found, true);
        }
    }
    (expansion.found, false)
}

/// For each source, whether the library already holds a file with its
/// content (same size and content fingerprint). Cheap: only sources whose
/// size some photograph of the library has are read (two 64 KB samples).
/// A hint for the import dialog; the import itself compares SHA-256.
pub fn known_in_library(library: &Library, sources: &[PathBuf]) -> Result<Vec<bool>> {
    let keys = library.content_keys()?;
    let sizes: HashSet<u64> = keys.iter().map(|(size, _)| *size).collect();
    Ok(sources
        .iter()
        .map(|source| {
            let Ok(meta) = fs::metadata(source) else {
                return false;
            };
            sizes.contains(&meta.len())
                && rawtonic_catalog::fingerprint(source)
                    .is_ok_and(|fp| keys.contains(&(meta.len(), fp)))
        })
        .collect())
}

/// How a batch names and places its copies.
#[derive(Debug, Clone, Copy)]
pub struct BatchOptions<'a> {
    pub template: &'a str,
    /// The first `{seq}` (and `{seq_start}`).
    pub sequence_start: u64,
    /// A second, verified copy of each file here.
    pub backup: Option<&'a Path>,
    /// The folder to copy into (`None`: the capture folder); see
    /// `destination_folder`.
    pub folder: Option<&'a Path>,
    /// The library's top-level folders as the app's folder list shows them
    /// (for a catalog, folders below them are destinations too).
    pub roots: &'a [PathBuf],
}

/// One file of a batch finished: in the library (or skipped as a duplicate,
/// or failed, see `result`).
#[derive(Debug)]
pub struct FileDone<'a> {
    /// Its place in the batch (0-based); files finish in this order.
    pub index: usize,
    pub total: usize,
    pub result: &'a ImportResult,
    /// Bytes copied by the batch so far.
    pub bytes: u64,
}

/// Imports `sources` one after the other (in their order, which `{seq}`
/// numbers), calling `on_file` as soon as each is done, so it can be shown
/// while the rest copy. A cancel stops before the next file; the file it
/// interrupted is neither published nor reported (`verified_copy` never
/// leaves a partial copy). Fails at once only when the destination folder
/// is not one an import may copy into.
pub fn import_batch(
    library: &Library,
    sources: &[PathBuf],
    options: &BatchOptions<'_>,
    cancel: &AtomicBool,
    mut on_file: impl FnMut(FileDone<'_>),
) -> Result<Vec<ImportResult>> {
    let destination = destination_folder(library, options.folder, options.roots)?;
    let total = sources.len();
    let mut results = Vec::with_capacity(total);
    let mut bytes = 0u64;
    for (index, source) in sources.iter().enumerate() {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let result = import_one_into(
            library,
            source,
            &destination,
            options.template,
            options.sequence_start + index as u64,
            options.sequence_start,
            options.backup,
            cancel,
        )
        .unwrap_or_else(|error| ImportResult {
            source: source.clone(),
            destination: None,
            backup: None,
            asset_id: None,
            error: Some(error.to_string()),
            skipped: false,
        });
        if cancel.load(Ordering::Acquire) && result.error.is_some() {
            break;
        }
        if !result.skipped && result.error.is_none() {
            bytes += fs::metadata(source).map(|m| m.len()).unwrap_or(0);
        }
        on_file(FileDone {
            index,
            total,
            result: &result,
            bytes,
        });
        results.push(result);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::Mode;

    fn library(dir: &Path) -> Library {
        Library::create(&dir.join("job"), "Job", Mode::Session).unwrap()
    }
    fn photos(dir: &Path, count: usize) -> Vec<PathBuf> {
        let card = dir.join("card");
        fs::create_dir_all(&card).unwrap();
        (0..count)
            .map(|i| {
                let path = card.join(format!("DSC_{i:04}.ARW"));
                fs::write(&path, vec![i as u8 + 1; 300_000 + i]).unwrap();
                path
            })
            .collect()
    }
    fn options(template: &str) -> BatchOptions<'_> {
        BatchOptions {
            template,
            sequence_start: 1,
            backup: None,
            folder: None,
            roots: &[],
        }
    }
    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Each file is reported once it is in the library, in batch order:
    /// when its event arrives its copy exists and is registered.
    #[test]
    fn reports_each_file_in_order_as_soon_as_it_is_in_the_library() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let sources = photos(d.path(), 4);
        let mut events = Vec::new();
        let results = import_batch(
            &lib,
            &sources,
            &options("{orig}"),
            &AtomicBool::new(false),
            |done| {
                let destination = done.result.destination.clone().unwrap();
                assert!(destination.is_file(), "copy published before its event");
                assert!(lib.asset_at(&destination).unwrap().is_some(), "registered");
                events.push((
                    done.index,
                    done.total,
                    done.result.source.clone(),
                    done.bytes,
                ));
            },
        )
        .unwrap();
        assert_eq!(results.len(), 4);
        assert_eq!(
            events.iter().map(|e| e.0).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert!(events.iter().all(|e| e.1 == 4));
        assert_eq!(
            events.iter().map(|e| e.2.clone()).collect::<Vec<_>>(),
            sources
        );
        assert!(events.windows(2).all(|w| w[1].3 > w[0].3), "bytes grow");
    }

    /// A cancel during the batch stops before the next file: the files done
    /// stay (reported and registered), nothing after is copied, no partial
    /// file is left and the originals are untouched.
    #[test]
    fn cancel_stops_before_the_next_file_and_leaves_no_partial_copy() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let sources = photos(d.path(), 5);
        let before: Vec<Vec<u8>> = sources.iter().map(|s| fs::read(s).unwrap()).collect();
        let cancel = AtomicBool::new(false);
        let mut reported = Vec::new();
        let results = import_batch(&lib, &sources, &options("{orig}"), &cancel, |done| {
            reported.push(done.index);
            if done.index == 1 {
                cancel.store(true, Ordering::Release);
            }
        })
        .unwrap();
        assert_eq!(reported, vec![0, 1]);
        assert_eq!(results.len(), 2);
        let capture = lib.directory.join(&lib.manifest.capture);
        assert_eq!(names(&capture), vec!["DSC_0000.ARW", "DSC_0001.ARW"]);
        assert_eq!(lib.assets(None).unwrap().len(), 2);
        for (source, bytes) in sources.iter().zip(before) {
            assert_eq!(fs::read(source).unwrap(), bytes);
        }
    }

    /// A cancel that interrupts a copy: that file is neither published nor
    /// reported as failed.
    #[test]
    fn the_copy_a_cancel_interrupts_is_not_reported() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let sources = photos(d.path(), 2);
        let cancel = AtomicBool::new(false);
        let mut reported = 0;
        // Cancelled while the first file is in progress (its source is
        // read for its duplicate check before the copy starts).
        let results = std::thread::scope(|scope| {
            scope.spawn(|| cancel.store(true, Ordering::Release));
            import_batch(&lib, &sources, &options("{orig}"), &cancel, |_| {
                reported += 1
            })
            .unwrap()
        });
        assert!(results.len() <= 1);
        assert_eq!(reported, results.len());
        assert!(results.iter().all(|r| r.error.is_none()));
        let capture = lib.directory.join(&lib.manifest.capture);
        assert_eq!(names(&capture).len(), results.len(), "no partial copies");
    }

    /// Copies go into a folder of the library when one is given (a drop onto
    /// a folder), never into one outside it, the Trash, a hidden folder or
    /// the library folder itself.
    #[test]
    fn copies_into_a_library_folder_only() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let sources = photos(d.path(), 1);
        let selects = lib.directory.join("Selects");
        fs::create_dir_all(&selects).unwrap();
        let mut opts = options("{orig}");
        opts.folder = Some(&selects);
        let results = import_batch(&lib, &sources, &opts, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(
            results[0].destination.as_deref(),
            Some(selects.join("DSC_0000.ARW").as_path())
        );
        let outside = d.path().join("elsewhere");
        fs::create_dir_all(&outside).unwrap();
        let trash = lib.directory.join("Trash");
        fs::create_dir_all(&trash).unwrap();
        let cache = lib.directory.join(".cache");
        fs::create_dir_all(&cache).unwrap();
        for folder in [&outside, &trash, &cache, &lib.directory.clone()] {
            opts.folder = Some(folder);
            assert!(import_batch(&lib, &sources, &opts, &AtomicBool::new(false), |_| {}).is_err());
            assert!(!names(folder).contains(&"DSC_0000.ARW".to_string()));
        }
    }

    /// A catalog's folder list also shows folders outside its directory (on
    /// another drive): those, and folders below them, are destinations too
    /// (as Lightroom's "Import into this folder…"); a folder the list does
    /// not show, a hidden one or the app's own folders are not. An existing
    /// file there is never replaced.
    #[test]
    fn a_catalog_copies_into_the_folders_its_list_shows() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Main.catalog"), "Main", Mode::Catalog).unwrap();
        // A folder the catalog holds photographs of, elsewhere.
        let shoot = d.path().join("Drive/Shoots/2026");
        fs::create_dir_all(shoot.join("Day 2")).unwrap();
        fs::create_dir_all(shoot.join(".thumbs")).unwrap();
        fs::write(shoot.join("KEEP.ARW"), b"registered").unwrap();
        lib.add_asset(&shoot.join("KEEP.ARW")).unwrap();
        // A top-level folder of the app's folder list, without photographs yet.
        let root = d.path().join("Drive/Clients");
        fs::create_dir_all(root.join("Acme")).unwrap();
        let unlisted = d.path().join("Drive/Private");
        fs::create_dir_all(&unlisted).unwrap();
        let sources = photos(d.path(), 4);
        // A file of that name already there is kept as it is.
        fs::write(shoot.join("Day 2/DSC_0001.ARW"), b"someone else's").unwrap();

        let roots = [root.clone()];
        let mut opts = options("{orig}");
        opts.roots = &roots;
        for (i, (folder, expected)) in [
            (shoot.clone(), shoot.join("DSC_0000.ARW")),
            (shoot.join("Day 2"), shoot.join("Day 2/DSC_0001_1.ARW")),
            (root.join("Acme"), root.join("Acme/DSC_0002.ARW")),
        ]
        .into_iter()
        .enumerate()
        {
            let opts = BatchOptions {
                folder: Some(&folder),
                ..opts
            };
            let results = import_batch(
                &lib,
                &sources[i..=i],
                &opts,
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
            let result = &results[0];
            assert!(result.error.is_none() && !result.skipped, "{result:?}");
            assert_eq!(result.destination.as_deref(), Some(expected.as_path()));
            assert_eq!(fs::read(&expected).unwrap(), fs::read(&sources[i]).unwrap());
            assert!(lib.asset_at(&expected).unwrap().is_some(), "registered");
        }
        assert_eq!(
            fs::read(shoot.join("Day 2/DSC_0001.ARW")).unwrap(),
            b"someone else's"
        );
        let copy = sources[3].clone();
        for folder in [
            unlisted.clone(),
            shoot.join(".thumbs"),
            lib.directory.join("Trash"),
            lib.directory.join(".cache"),
        ] {
            fs::create_dir_all(&folder).unwrap();
            let opts = BatchOptions {
                folder: Some(&folder),
                ..opts
            };
            assert!(
                import_batch(
                    &lib,
                    std::slice::from_ref(&copy),
                    &opts,
                    &AtomicBool::new(false),
                    |_| {}
                )
                .is_err(),
                "{}",
                folder.display()
            );
            assert!(!names(&folder).contains(&"DSC_0003.ARW".to_string()));
        }
        // A session copies into its own folders only.
        let session = library(&d.path().join("s"));
        opts.folder = Some(&root);
        assert!(
            import_batch(
                &session,
                &sources[1..],
                &opts,
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
        );
    }

    #[test]
    fn expands_folders_recursively_with_the_filter() {
        let d = tempfile::tempdir().unwrap();
        let card = d.path().join("DCIM");
        fs::create_dir_all(card.join("100CANON")).unwrap();
        fs::create_dir_all(card.join(".hidden")).unwrap();
        fs::write(card.join("100CANON/B.CR3"), b"b").unwrap();
        fs::write(card.join("100CANON/A.CR3"), b"a").unwrap();
        fs::write(card.join("100CANON/notes.txt"), b"x").unwrap();
        fs::write(card.join("100CANON/._A.CR3"), b"x").unwrap();
        fs::write(card.join(".hidden/C.CR3"), b"c").unwrap();
        let single = d.path().join("Z.CR3");
        fs::write(&single, b"z").unwrap();
        let accept = |p: &Path| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("cr3"));
        let (found, truncated) =
            expand_sources(&[single.clone(), card.clone(), single.clone()], accept, 100);
        assert!(!truncated);
        assert_eq!(
            found,
            vec![
                single.clone(),
                card.join("100CANON/A.CR3"),
                card.join("100CANON/B.CR3")
            ]
        );
        let (found, truncated) = expand_sources(&[card], accept, 1);
        assert!(truncated);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn marks_files_the_library_already_holds() {
        let d = tempfile::tempdir().unwrap();
        let lib = library(d.path());
        let sources = photos(d.path(), 2);
        import_batch(
            &lib,
            &sources[..1],
            &options("{orig}"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(known_in_library(&lib, &sources).unwrap(), vec![true, false]);
    }
}
