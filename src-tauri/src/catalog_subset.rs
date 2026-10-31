//! Export as Catalog (G39): the app glue over
//! `rawtonic_ingest::catalog_export`. Photographs chosen in the open
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

#[tauri::command]
pub fn library_export_catalog_cancel() {
    CANCEL_EXPORT.store(true, Ordering::Release);
}

/// The Standard previews of the exported photographs, into the staged
/// catalog: of a copied original where the catalog keeps its previews, of
/// one left where it is among the catalog's carried previews.
fn copy_previews(
    stage: &Path,
    exported: &[Exported],
    cancel: &AtomicBool,
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

    /// Previews follow the photographs into the new catalog: a copied
    /// original's under the catalog's previews, named for its new place; a
    /// referenced one's among the carried previews, which opening the
    /// catalog on another computer makes that photograph's previews.
    #[test]
    fn standard_previews_go_with_the_exported_photographs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let session = Library::create(&root.join("Session"), "S", Mode::Session).unwrap();
        let photo = session.directory.join("Capture/a.arw");
        fs::write(&photo, b"raw").unwrap();
        let id = session.add_asset(&photo).unwrap().id;
        let preview = preview_proxy::proxy_path(&photo).unwrap();
        fs::create_dir_all(preview.parent().unwrap()).unwrap();
        let image = image::DynamicImage::new_rgb32f(4, 2);
        let key = preview_proxy::ProxyKey {
            file_size: 3,
            modified_ns: 1,
            decoder: "test".into(),
            engine: 1,
        };
        let bytes = preview_proxy::encode(&image, 8, 4, key);
        fs::write(&preview, &bytes).unwrap();
        for (copy, name) in [(true, "Copied"), (false, "Referenced")] {
            let to = root.join(name);
            let receipt = export_catalog(
                &session.directory,
                std::slice::from_ref(&id),
                &to,
                &Options {
                    name: name.into(),
                    include_originals: copy,
                },
                &AtomicBool::new(false),
                |_| {},
                copy_previews,
            )
            .unwrap();
            assert_eq!(receipt.previews, 1, "{name}");
            let catalog = Library::open(&to).unwrap();
            let place = catalog.asset(&id).unwrap().path;
            if copy {
                let moved = preview_proxy::proxy_path(&place).unwrap();
                assert!(moved.starts_with(&to), "{}", moved.display());
                assert!(moved.is_file());
            } else {
                let carried = fs::read_dir(to.join(".cache").join(preview_proxy::CARRIED_PREVIEWS))
                    .unwrap()
                    .count();
                assert_eq!(carried, 1);
            }
        }
        assert_eq!(
            fs::read(&preview).unwrap(),
            bytes,
            "the source's preview is unchanged"
        );
    }
}
