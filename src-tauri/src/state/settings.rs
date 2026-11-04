//! `SettingsStore`: the app settings, read from `settings.json` and migrated
//! once per run, then served from memory. Every change is one
//! read-modify-write under the store's write lock, persisted atomically before
//! readers see it.
//!
//! Only this process writes `settings.json` while the app runs: the GUI is a
//! single instance (`tauri_plugin_single_instance`), the headless runs of the
//! same binary (exports, GPU warm-up) read it at their start and only write
//! back the same migrations the GUI wrote at its own start, and the frontend
//! saves through `save_settings`. A hand edit of the file while the app runs
//! was already overwritten by the next frontend save; it now also stays
//! unseen until the next start.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tauri::{AppHandle, Manager};

use crate::app_settings::{
    AppSettings, get_settings_path, load_settings_file, migrate_settings, persist_settings_file,
};
use crate::state::lock::{lock_recover, read_recover, write_recover};

#[derive(Default)]
pub struct SettingsStore {
    /// `None` until the file was read: a file that could not be read (or
    /// kept aside) is tried again on the next access, as before the store.
    current: RwLock<Option<Arc<AppSettings>>>,
    /// Held for the whole load or read-modify-write (lock order: `writer`,
    /// then `current`).
    writer: Mutex<()>,
    file_loads: AtomicUsize,
}

impl SettingsStore {
    /// The current settings; reads the file only on the first call of the run.
    pub fn get(
        &self,
        path: impl FnOnce() -> Result<PathBuf, String>,
    ) -> Result<Arc<AppSettings>, String> {
        if let Some(settings) = read_recover(&self.current).as_ref() {
            return Ok(settings.clone());
        }
        let _writer = lock_recover(&self.writer);
        self.loaded(&path()?)
    }

    /// Applies `change` to the current settings and saves the result, all
    /// under the write lock, so concurrent changes never lose one another.
    /// The result is migrated like a file read at start-up would be (an older
    /// frontend snapshot reads back as it did when every read was a file
    /// read). On an error neither the file nor the settings change.
    pub fn update<R>(
        &self,
        path: impl FnOnce() -> Result<PathBuf, String>,
        change: impl FnOnce(&mut AppSettings) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut result = None;
        self.update_if(path, |settings| {
            result = Some(change(settings)?);
            Ok(Some(()))
        })?;
        result.ok_or_else(|| "settings change did not run".to_string())
    }

    /// `update` for a change that may not apply: `change` returns `None` when
    /// it changed nothing, and then nothing is written.
    pub fn update_if<R>(
        &self,
        path: impl FnOnce() -> Result<PathBuf, String>,
        change: impl FnOnce(&mut AppSettings) -> Result<Option<R>, String>,
    ) -> Result<Option<R>, String> {
        let _writer = lock_recover(&self.writer);
        let path = path()?;
        let mut next = (*self.loaded(&path)?).clone();
        let Some(result) = change(&mut next)? else {
            return Ok(None);
        };
        migrate_settings(&mut next);
        persist_settings_file(&path, &next)?;
        *write_recover(&self.current) = Some(Arc::new(next));
        Ok(Some(result))
    }

    /// How often the file was read in this run.
    #[cfg(test)]
    pub fn file_loads(&self) -> usize {
        self.file_loads.load(Ordering::Relaxed)
    }

    /// Call with `writer` held.
    fn loaded(&self, path: &Path) -> Result<Arc<AppSettings>, String> {
        if let Some(settings) = read_recover(&self.current).as_ref() {
            return Ok(settings.clone());
        }
        self.file_loads.fetch_add(1, Ordering::Relaxed);
        let settings = Arc::new(load_settings_file(path)?);
        *write_recover(&self.current) = Some(settings.clone());
        Ok(settings)
    }
}

/// The app's settings, or the defaults when the file cannot be read (what
/// every caller used before the store).
pub fn current(app: &AppHandle) -> Arc<AppSettings> {
    try_current(app).unwrap_or_default()
}

/// The app's settings, or why the file could not be read.
pub fn try_current(app: &AppHandle) -> Result<Arc<AppSettings>, String> {
    app.state::<SettingsStore>().get(|| get_settings_path(app))
}

/// One read-modify-write of the app's settings (`SettingsStore::update`).
pub fn update<R>(
    app: &AppHandle,
    change: impl FnOnce(&mut AppSettings) -> Result<R, String>,
) -> Result<R, String> {
    app.state::<SettingsStore>()
        .update(|| get_settings_path(app), change)
}

/// `update` that writes only when `change` returns `Some`
/// (`SettingsStore::update_if`).
pub fn update_if<R>(
    app: &AppHandle,
    change: impl FnOnce(&mut AppSettings) -> Result<Option<R>, String>,
) -> Result<Option<R>, String> {
    app.state::<SettingsStore>()
        .update_if(|| get_settings_path(app), change)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// `AppSettings` has no `PartialEq`: compares what would be written, the
    /// two adjustment sets (`HashSet`, serialised in any order) sorted.
    fn same(a: &AppSettings, b: &AppSettings) -> Result<bool, String> {
        let json = |s: &AppSettings| {
            let mut value = serde_json::to_value(s).map_err(|e| e.to_string())?;
            for key in ["includedAdjustments", "knownAdjustments"] {
                if let Some(serde_json::Value::Array(set)) = value
                    .get_mut("copyPasteSettings")
                    .and_then(|c| c.get_mut(key))
                {
                    set.sort_by_key(|v| v.to_string());
                }
            }
            Ok::<_, String>(value)
        };
        Ok(json(a)? == json(b)?)
    }

    fn at(path: &Path) -> impl FnOnce() -> Result<PathBuf, String> + '_ {
        || Ok(path.to_path_buf())
    }

    /// An older settings file that needs every start-up migration.
    const OLD_FILE: &str = r#"{"lastRootPath":"/Photos","editorNeutralGreyBg":true,"theme":"light","imageCacheSize":7}"#;

    #[test]
    fn the_file_is_read_and_migrated_once_per_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, OLD_FILE).unwrap();
        let store = SettingsStore::default();
        let first = store.get(at(&path)).unwrap();
        assert_eq!(first.root_folders, ["/Photos"]);
        assert_eq!(first.viewer_background.as_deref(), Some("medium"));
        assert!(!first.copy_paste_settings.known_adjustments.is_empty());
        let migrated = fs::read(&path).unwrap();
        assert_ne!(
            migrated,
            OLD_FILE.as_bytes(),
            "the migration is written back"
        );
        // Later reads come from memory: neither a change on disk nor a
        // missing file reaches them, and nothing is written again.
        fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        for _ in 0..100 {
            let again = store.get(at(&path)).unwrap();
            assert!(Arc::ptr_eq(&first, &again));
        }
        assert_eq!(store.file_loads(), 1);
        assert_eq!(fs::read(&path).unwrap(), br#"{"theme":"dark"}"#);
    }

    #[test]
    fn an_unreadable_file_is_tried_again_until_it_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::create_dir(&path).unwrap(); // a directory cannot be read as a file
        let store = SettingsStore::default();
        assert!(store.get(at(&path)).is_err());
        assert!(store.update(at(&path), |_| Ok(())).is_err());
        fs::remove_dir(&path).unwrap();
        fs::write(&path, r#"{"theme":"light"}"#).unwrap();
        assert_eq!(
            store.get(at(&path)).unwrap().theme.as_deref(),
            Some("light")
        );
        assert_eq!(store.file_loads(), 3);
    }

    #[test]
    fn concurrent_updates_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let store = Arc::new(SettingsStore::default());
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let (store, path) = (store.clone(), path.clone());
                std::thread::spawn(move || {
                    for i in 0..25 {
                        store
                            .update(at(&path), |s| {
                                s.root_folders.push(format!("/t{t}/{i}"));
                                Ok(())
                            })
                            .unwrap();
                        // Readers meanwhile see whole settings, never a half update.
                        let seen = store.get(at(&path)).unwrap();
                        assert!(seen.root_folders.iter().any(|r| r == &format!("/t{t}/{i}")));
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(store.get(at(&path)).unwrap().root_folders.len(), 200);
        let on_disk = load_settings_file(&path).unwrap();
        assert_eq!(on_disk.root_folders.len(), 200);
        assert_eq!(store.file_loads(), 1);
    }

    /// The render paths (preview jobs, `generate_preview_for_path`, exports,
    /// thumbnails) read their render options from the store. After any
    /// sequence of saves the store holds exactly what a fresh file read with
    /// the start-up migrations (the former `load_settings`) returns.
    #[test]
    fn the_store_holds_what_a_file_read_returned_before() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, OLD_FILE).unwrap();
        let store = SettingsStore::default();
        let same_as_file = |store: &SettingsStore| {
            let fresh = load_settings_file(&path).unwrap();
            assert!(same(&store.get(at(&path)).unwrap(), &fresh).unwrap());
        };
        same_as_file(&store);
        // A frontend snapshot that still carries the old fields and no root
        // folders: a file read would migrate it again.
        let mut snapshot: AppSettings = serde_json::from_str(OLD_FILE).unwrap();
        snapshot.theme = Some("dark".into());
        snapshot.processing_backend = Some("metal".into());
        store
            .update(at(&path), |s| {
                *s = snapshot;
                Ok(())
            })
            .unwrap();
        same_as_file(&store);
        let now = store.get(at(&path)).unwrap();
        assert_eq!(now.theme.as_deref(), Some("dark"));
        assert_eq!(now.root_folders, ["/Photos"]);
        assert_eq!(now.editor_neutral_grey_bg, None);
        store
            .update(at(&path), |s| {
                s.image_cache_size = Some(3);
                Ok(())
            })
            .unwrap();
        same_as_file(&store);
    }

    #[test]
    fn a_failed_change_or_write_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"theme":"light"}"#).unwrap();
        let store = SettingsStore::default();
        let before = fs::read(&store_loaded(&store, &path)).unwrap();
        let refused: Result<(), _> = store.update(at(&path), |s| {
            s.theme = Some("dark".into());
            Err("no".into())
        });
        assert!(refused.is_err());
        assert_eq!(
            store.get(at(&path)).unwrap().theme.as_deref(),
            Some("light")
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
            let write = store.update(at(&path), |s| {
                s.theme = Some("dark".into());
                Ok(())
            });
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
            // Root may write anyway; otherwise memory keeps what the file has.
            let theme = store.get(at(&path)).unwrap().theme.clone();
            assert_eq!(write.is_err(), theme.as_deref() == Some("light"));
        }
    }

    #[test]
    fn a_change_that_does_not_apply_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let store = SettingsStore::default();
        store_loaded(&store, &path);
        fs::write(&path, b"marker").unwrap();
        let none: Option<()> = store
            .update_if(at(&path), |s| {
                s.theme = Some("dark".into());
                Ok(None)
            })
            .unwrap();
        assert!(none.is_none());
        assert_eq!(fs::read(&path).unwrap(), b"marker");
        assert_eq!(
            store.get(at(&path)).unwrap().theme,
            AppSettings::default().theme
        );
    }

    /// Only the store reads the settings file: the preview jobs,
    /// `generate_preview_for_path`, thumbnails and exports go through
    /// `current`/`try_current`, which read it once per run (the counter
    /// test above).
    #[test]
    fn nothing_but_the_store_reads_the_settings_file() {
        fn walk(dir: &Path, hits: &mut Vec<String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, hits);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = fs::read_to_string(&path).unwrap();
                    let name = path.to_string_lossy().replace('\\', "/");
                    let owner = name.ends_with("src/app_settings.rs")
                        || name.ends_with("src/state/settings.rs");
                    for needle in [
                        "load_settings_file(",
                        "read_settings_file(",
                        "load_settings(",
                    ] {
                        if !owner && text.contains(needle) {
                            hits.push(format!("{name}: {needle}"));
                        }
                    }
                }
            }
        }
        let mut hits = Vec::new();
        walk(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut hits,
        );
        assert!(
            hits.is_empty(),
            "settings file read outside the store: {hits:?}"
        );
    }

    fn store_loaded(store: &SettingsStore, path: &Path) -> PathBuf {
        store.get(at(path)).unwrap();
        path.to_path_buf()
    }
}
