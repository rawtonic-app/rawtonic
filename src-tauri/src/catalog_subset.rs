//! Export as Catalog and Import from Catalog (G39): the app glue over
//! `rawtonic_ingest::catalog_export` and `catalog_import`. Photographs chosen in the open
//! library (a session or a catalog) become a new catalog; with "Include
//! previews" their Standard previews go along, so they edit offline where
//! their originals are not.
use crate::preview_proxy;
use rawtonic_ingest::catalog_export::{Exported, Options, Receipt, export_catalog};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Emitter;

static CANCEL_EXPORT: AtomicBool = AtomicBool::new(false);

pub const EXPORT_PROGRESS: &str = "catalog-export-progress";

/// Exports the photographs at `paths` (virtual copies stand for their
/// photograph, which goes with all its copies) of the library at `source`
/// as a new catalog at `destination`. Holds the ingest gate: no import,
/// library copy or session move runs meanwhile.
#[tauri::command]
pub async fn library_export_catalog(
    source: String,
    paths: Vec<String>,
    destination: String,
    include_originals: bool,
    include_previews: bool,
    leave_out_owned_elsewhere: bool,
    app: tauri::AppHandle,
) -> Result<Receipt, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::session_bridge::holding_import_gate(
            "Wait for the current import or library copy to finish",
            || {
                CANCEL_EXPORT.store(false, Ordering::Release);
                let ids = {
                    let lib = rawtonic_catalog::Library::open(Path::new(&source))
                        .map_err(|e| e.to_string())?;
                    let mut ids = crate::session_bridge::asset_ids(&lib, &paths)?;
                    ids.sort();
                    ids.dedup();
                    ids
                };
                let destination = PathBuf::from(&destination);
                let name = destination
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                export_catalog(
                    Path::new(&source),
                    &ids,
                    &destination,
                    &Options {
                        name,
                        include_originals,
                        leave_out_owned_elsewhere,
                    },
                    &CANCEL_EXPORT,
                    |progress| {
                        let _ = app.emit(EXPORT_PROGRESS, progress);
                    },
                    |stage, exported, cancel| {
                        if include_previews {
                            copy_previews(stage, exported, cancel)
                        } else {
                            Ok(0)
                        }
                    },
                )
                .map_err(|e| format!("{e:#}"))
            },
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Of the photographs at `paths` in the library at `source`: how many
/// there are and those another library owns (Export as Catalog leaves them
/// out or refuses: their latest edit is in that library).
#[derive(serde::Serialize)]
pub struct OwnedReport {
    total: usize,
    owned: Vec<rawtonic_ingest::catalog_export::OwnedElsewhere>,
}

#[tauri::command]
pub async fn library_export_catalog_owned(
    source: String,
    paths: Vec<String>,
) -> Result<OwnedReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let lib = rawtonic_catalog::Library::open(Path::new(&source)).map_err(|e| e.to_string())?;
        let mut ids = crate::session_bridge::asset_ids(&lib, &paths)?;
        ids.sort();
        ids.dedup();
        let owned = rawtonic_ingest::catalog_export::owned_elsewhere(&lib, &ids)
            .map_err(|e| format!("{e:#}"))?;
        Ok(OwnedReport {
            total: ids.len(),
            owned,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn library_export_catalog_cancel() {
    CANCEL_EXPORT.store(true, Ordering::Release);
}

static CANCEL_IMPORT: AtomicBool = AtomicBool::new(false);

pub const IMPORT_PROGRESS: &str = "catalog-import-progress";

/// What the catalog at `other` would bring into the open catalog at
/// `target`: new, changed and (counted) unchanged photographs. Reads only.
#[tauri::command]
pub async fn library_import_catalog_review(
    target: String,
    other: String,
) -> Result<rawtonic_ingest::catalog_import::Review, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let lib = rawtonic_catalog::Library::open(Path::new(&target)).map_err(|e| e.to_string())?;
        let source = rawtonic_ingest::catalog_import::open_source(&lib, Path::new(&other))
            .map_err(|e| format!("{e:#}"))?;
        rawtonic_ingest::catalog_import::review(&lib, &source).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Imports the catalog at `other` into the open catalog at `target` as
/// `options` says, in one transaction (copies of originals first). Holds
/// the ingest gate.
#[tauri::command]
pub async fn library_import_catalog(
    target: String,
    other: String,
    options: rawtonic_ingest::catalog_import::Options,
    app: tauri::AppHandle,
) -> Result<rawtonic_ingest::catalog_import::Receipt, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::session_bridge::holding_import_gate(
            "Wait for the current import or library copy to finish",
            || {
                CANCEL_IMPORT.store(false, Ordering::Release);
                let lib = rawtonic_catalog::Library::open(Path::new(&target))
                    .map_err(|e| e.to_string())?;
                let receipt = rawtonic_ingest::catalog_import::import(
                    &lib,
                    Path::new(&other),
                    &options,
                    &CANCEL_IMPORT,
                    |progress| {
                        let _ = app.emit(IMPORT_PROGRESS, progress);
                    },
                )
                .map_err(|e| format!("{e:#}"))?;
                // The editor and listings read an online photograph's
                // edits and virtual copies from the sidecars beside it.
                for id in &receipt.report.touched {
                    crate::session_bridge::rederive(&lib, id);
                }
                Ok(receipt)
            },
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn library_import_catalog_cancel() {
    CANCEL_IMPORT.store(true, Ordering::Release);
}

/// The Standard previews of the exported photographs, into the staged
/// catalog: of a copied original where the catalog keeps its previews, of
/// one left where it is among the catalog's carried previews.
fn copy_previews(
    stage: &Path,
    exported: &[Exported],
    cancel: &AtomicBool,
) -> anyhow::Result<usize> {
    copy_previews_from(
        stage,
        exported,
        cancel,
        preview_proxy::outside_sessions_dir().as_deref(),
    )
}

/// `copy_previews` with the previews of photographs outside sessions in
/// `outside`.
fn copy_previews_from(
    stage: &Path,
    exported: &[Exported],
    cancel: &AtomicBool,
    outside: Option<&Path>,
) -> anyhow::Result<usize> {
    let carried = stage.join(".cache").join(preview_proxy::CARRIED_PREVIEWS);
    let mut copied = 0;
    for photo in exported {
        anyhow::ensure!(!cancel.load(Ordering::Acquire), "Copy cancelled");
        let inside = photo.to.starts_with(stage);
        if preview_proxy::copy_standard(
            &photo.from,
            &photo.to,
            (!inside).then_some(carried.as_path()),
            outside,
        )
        .map_err(anyhow::Error::msg)?
        {
            copied += 1;
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::{Library, Mode};
    use std::fs;

    fn preview_bytes() -> Vec<u8> {
        let image = image::DynamicImage::new_rgb32f(4, 2);
        let key = preview_proxy::ProxyKey {
            file_size: 3,
            modified_ns: 1,
            decoder: "test".into(),
            engine: 1,
        };
        preview_proxy::encode(&image, 8, 4, key)
    }

    /// Previews follow the photographs into the new catalog: a copied
    /// original's (a session's always is) under the catalog's previews,
    /// named for its new place; a referenced one's among the carried
    /// previews, which opening the catalog on another computer makes that
    /// photograph's previews.
    #[test]
    fn standard_previews_go_with_the_exported_photographs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let outside = root.join("app-previews");
        let bytes = preview_bytes();
        let session = Library::create(&root.join("Session"), "S", Mode::Session).unwrap();
        let photo = session.directory.join("Capture/a.arw");
        fs::write(&photo, b"raw").unwrap();
        let id = session.add_asset(&photo).unwrap().id;
        let preview = preview_proxy::proxy_path(&photo).unwrap();
        fs::create_dir_all(preview.parent().unwrap()).unwrap();
        fs::write(&preview, &bytes).unwrap();
        let catalog = Library::create(&root.join("Main"), "Main", Mode::Catalog).unwrap();
        let external = root.join("Shoot/b.arw");
        fs::create_dir_all(external.parent().unwrap()).unwrap();
        fs::write(&external, b"raw").unwrap();
        let external_id = catalog.add_asset(&external).unwrap().id;
        let external_preview =
            preview_proxy::standard_path_with(&external, Some(&outside)).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(&external_preview, &bytes).unwrap();
        let export = |source: &Path, id: &str, name: &str| {
            let to = root.join(name);
            let receipt = export_catalog(
                source,
                &[id.to_string()],
                &to,
                &Options {
                    name: name.into(),
                    include_originals: false,
                    leave_out_owned_elsewhere: false,
                },
                &AtomicBool::new(false),
                |_| {},
                |stage, exported, cancel| {
                    copy_previews_from(stage, exported, cancel, Some(&outside))
                },
            )
            .unwrap();
            assert_eq!(receipt.previews, 1, "{name}");
            to
        };
        let copied = export(&session.directory, &id, "Copied");
        let place = Library::open(&copied).unwrap().asset(&id).unwrap().path;
        let moved = preview_proxy::proxy_path(&place).unwrap();
        assert!(moved.starts_with(&copied), "{}", moved.display());
        assert!(moved.is_file());
        let referenced = export(&catalog.directory, &external_id, "Referenced");
        let carried: Vec<_> = fs::read_dir(
            referenced
                .join(".cache")
                .join(preview_proxy::CARRIED_PREVIEWS),
        )
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
        assert_eq!(carried, [external_preview.file_name().unwrap()]);
        for file in [&preview, &external_preview] {
            assert_eq!(
                fs::read(file).unwrap(),
                bytes,
                "the source's preview is unchanged"
            );
        }
    }

    /// After an import the editor and listings find a photograph's new
    /// edit and virtual copies in the sidecars beside it.
    #[test]
    fn imported_edits_and_copies_reach_the_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let lib = Library::create(&root.join("Main"), "Main", Mode::Catalog).unwrap();
        let photo = root.join("Shoot/a.arw");
        fs::create_dir_all(photo.parent().unwrap()).unwrap();
        fs::write(&photo, b"raw").unwrap();
        let id = lib.add_asset(&photo).unwrap().id;
        lib.save_editor_state(&id, &serde_json::json!({"adjustments": {"exposure": 1}}))
            .unwrap();
        lib.save_copy_state(
            &id,
            "c0ffee",
            &serde_json::json!({"adjustments": {"exposure": 2}}),
        )
        .unwrap();
        // An untouched photograph whose only edit is a virtual copy.
        let untouched = root.join("Shoot/b.arw");
        fs::write(&untouched, b"raw b").unwrap();
        let b = lib.add_asset(&untouched).unwrap().id;
        lib.save_copy_state(
            &b,
            "b0b0b0",
            &serde_json::json!({"adjustments": {"exposure": 3}}),
        )
        .unwrap();
        assert!(lib.editor_state(&b).unwrap().is_none());
        crate::session_bridge::rederive(&lib, &id);
        crate::session_bridge::rederive(&lib, &b);
        let read = |name: &str| -> serde_json::Value {
            serde_json::from_slice(&fs::read(photo.with_file_name(name)).unwrap()).unwrap()
        };
        assert_eq!(read("a.arw.rrdata")["adjustments"]["exposure"], 1);
        assert_eq!(read("a.arw.c0ffee.rrdata")["adjustments"]["exposure"], 2);
        assert_eq!(read("b.arw.b0b0b0.rrdata")["adjustments"]["exposure"], 3);
        assert!(!untouched.with_file_name("b.arw.rrdata").exists());
        assert_eq!(fs::read(&photo).unwrap(), b"raw");
        assert_eq!(fs::read(&untouched).unwrap(), b"raw b");
    }
}
