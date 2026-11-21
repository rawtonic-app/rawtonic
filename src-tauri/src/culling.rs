// SPDX-License-Identifier: AGPL-3.0-or-later
//! Assisted culling (G35; notes: benchmark-results/cul1-2026-11-21/
//! REPORT.md "CUL2 build plan"): the Library's Culling panel reads its
//! suggestions — misfires, documents, closed eyes — from the catalog
//! (`rawtonic_catalog::cull`) and the photographer dismisses the ones that
//! are no problem. Nothing here flags, rejects, hides or removes a
//! photograph: the panel's "Reject" is the ordinary reject flag, one undo
//! step.
//!
//! Misfire and document scores are made by content search's background
//! pass (`content_search`) from each embedding as the model gives it (never
//! from the stored bytes, whose rounding the calibration's ×112 slope turns
//! into score changes of up to ~46 %), with the frozen prompts' text
//! embeddings (`prompt_set`): one image pass for both and no new model. The prompt embeddings are made once per text model and prompt
//! set and kept in the models folder, so the text encoder is not loaded
//! again for them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rawtonic_catalog::Library;
use rawtonic_catalog::cull::{CRITERIA, CullPhoto, DOCUMENT_PROMPTS, MISFIRE_PROMPTS, PromptSet};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Emitter;

use crate::ipc::CommandError;
use crate::session_bridge::with_library;
use crate::state::lock::lock_recover;

/// Suggestions changed in the library at the payload's directory.
pub const CULLING_CHANGED: &str = "library-culling-changed";

/// The prompt set's id: the text model and every prompt, so a change to
/// either scores every photograph again.
pub fn prompt_set_id() -> String {
    let mut hash = Sha256::new();
    hash.update(crate::content_search::TEXT_SHA256);
    for prompt in MISFIRE_PROMPTS {
        hash.update(b"\0m");
        hash.update(prompt);
    }
    for prompt in DOCUMENT_PROMPTS {
        hash.update(b"\0d");
        hash.update(prompt);
    }
    hash.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The prompt embeddings as kept on disk.
#[derive(Serialize, Deserialize)]
struct StoredPrompts {
    id: String,
    misfire: Vec<Vec<f32>>,
    document: Vec<Vec<f32>>,
}

fn stored_path(models: &Path, id: &str) -> PathBuf {
    models.join(format!("culling-prompts-{id}.json"))
}

fn as_set(stored: StoredPrompts) -> PromptSet {
    PromptSet {
        id: stored.id,
        misfire: stored.misfire,
        document: stored.document,
        logit_scale: crate::tagging::SIGLIP_LOGIT_SCALE,
        logit_bias: crate::tagging::SIGLIP_LOGIT_BIAS,
    }
}

/// The stored prompt embeddings of `id` in `models`, if there and whole.
fn read_stored(models: &Path, id: &str) -> Option<PromptSet> {
    let bytes = std::fs::read(stored_path(models, id)).ok()?;
    let stored: StoredPrompts = serde_json::from_slice(&bytes).ok()?;
    let whole =
        stored.id == id
            && stored.misfire.len() == MISFIRE_PROMPTS.len()
            && stored.document.len() == DOCUMENT_PROMPTS.len()
            && stored.misfire.iter().chain(&stored.document).all(|p| {
                p.len() == rawtonic_catalog::content::DIM && p.iter().all(|v| v.is_finite())
            });
    whole.then(|| as_set(stored))
}

/// `rows` (one embedding per prompt, flat) cut into unit vectors.
fn rows(flat: &[f32], count: usize) -> anyhow::Result<Vec<Vec<f32>>> {
    let dim = rawtonic_catalog::content::DIM;
    anyhow::ensure!(
        flat.len() == dim * count,
        "The text model gave {} values for {count} prompts",
        flat.len()
    );
    Ok(flat
        .chunks(dim)
        .map(|row| {
            let norm = row
                .iter()
                .map(|v| v * v)
                .sum::<f32>()
                .sqrt()
                .max(f32::MIN_POSITIVE);
            row.iter().map(|v| v / norm).collect()
        })
        .collect())
}

static PROMPTS: Mutex<Option<Arc<PromptSet>>> = Mutex::new(None);

/// The prompt set the pass scores with: kept in memory, else read from the
/// models folder, else made with content search's text encoder (loaded
/// from the installed model, never downloaded) and written there. `None`
/// while the text model is not installed.
pub fn prompt_set(app: &tauri::AppHandle) -> anyhow::Result<Option<Arc<PromptSet>>> {
    let id = prompt_set_id();
    if let Some(set) = lock_recover(&PROMPTS).as_ref().filter(|s| s.id == id) {
        return Ok(Some(set.clone()));
    }
    let models = crate::ai_processing::models_dir(app)?;
    let set = match read_stored(&models, &id) {
        Some(set) => set,
        None => {
            let encoder =
                match tauri::async_runtime::block_on(crate::content_search::text_encoder(app)) {
                    Ok(encoder) => encoder,
                    Err(e) if e.code == crate::ipc::ErrorCode::ModelsMissing => return Ok(None),
                    Err(e) => anyhow::bail!("{}", e.message),
                };
            let prompts = |list: &[&str]| list.iter().map(|p| p.to_string()).collect();
            let misfire = rows(
                &crate::tagging::embed_texts(&encoder, prompts(&MISFIRE_PROMPTS))?,
                MISFIRE_PROMPTS.len(),
            )?;
            let document = rows(
                &crate::tagging::embed_texts(&encoder, prompts(&DOCUMENT_PROMPTS))?,
                DOCUMENT_PROMPTS.len(),
            )?;
            let stored = StoredPrompts {
                id: id.clone(),
                misfire,
                document,
            };
            if let Err(e) =
                rawtonic_fs::atomic_write(&stored_path(&models, &id), serde_json::to_vec(&stored)?)
            {
                log::warn!("Culling prompts not kept: {e:#}");
            }
            as_set(stored)
        }
    };
    let set = Arc::new(set);
    *lock_recover(&PROMPTS) = Some(set.clone());
    Ok(Some(set))
}

/// The prompt set when it is in memory already (never loads anything).
pub fn cached_prompt_set() -> Option<Arc<PromptSet>> {
    let id = prompt_set_id();
    lock_recover(&PROMPTS)
        .as_ref()
        .filter(|s| s.id == id)
        .cloned()
}

/// Announces new scores or dismissals in the library at `directory`.
pub fn announce(app: &tauri::AppHandle, directory: &Path) {
    let _ = app.emit(CULLING_CHANGED, directory);
}

fn asset_id(lib: &Library, path: &str) -> Option<String> {
    let file = rawtonic_metadata::photo_ref::source_of(path);
    lib.asset_at(Path::new(file)).ok().flatten().map(|a| a.id)
}

/// What the panel shows of the listed photographs: each one's suggestions
/// (by path as listed; photographs not in the library left out).
#[tauri::command]
pub async fn cull_suggestions(
    directory: String,
    paths: Vec<String>,
) -> Result<HashMap<String, CullPhoto>, CommandError> {
    with_library(directory, false, move |lib| {
        let ids: Vec<(String, String)> = paths
            .into_iter()
            .filter_map(|path| asset_id(lib, &path).map(|id| (path, id)))
            .collect();
        let read = lib
            .cull_photos(&ids.iter().map(|(_, id)| id.clone()).collect::<Vec<_>>())
            .map_err(|e| e.to_string())?;
        Ok(ids
            .into_iter()
            .filter_map(|(path, id)| read.get(&id).map(|photo| (path, photo.clone())))
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(CommandError::from)
}

/// Dismisses criterion `criterion`'s suggestion for `paths` ("not a
/// problem"), or with `dismissed` false shows it again. Returns the paths
/// whose state changed and announces the change.
#[tauri::command]
pub async fn cull_dismiss(
    directory: String,
    paths: Vec<String>,
    criterion: String,
    dismissed: bool,
    app_handle: tauri::AppHandle,
) -> Result<Vec<String>, CommandError> {
    if !CRITERIA.contains(&criterion.as_str()) {
        return Err(CommandError::from(format!(
            "Unknown culling suggestion {criterion}"
        )));
    }
    let announced = directory.clone();
    let changed = with_library(directory, false, move |lib| {
        let ids: Vec<(String, String)> = paths
            .into_iter()
            .filter_map(|path| asset_id(lib, &path).map(|id| (path, id)))
            .collect();
        let changed = lib
            .set_cull_dismissed(
                &ids.iter().map(|(_, id)| id.clone()).collect::<Vec<_>>(),
                &criterion,
                dismissed,
            )
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(
            ids.into_iter()
                .filter(|(_, id)| changed.contains(id))
                .map(|(path, _)| path)
                .collect::<Vec<_>>(),
        )
    })
    .await
    .map_err(|e| e.to_string())??;
    if !changed.is_empty() {
        let _ = app_handle.emit(CULLING_CHANGED, announced);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_set_id_names_the_text_model_and_every_prompt() {
        let id = prompt_set_id();
        assert_eq!(id.len(), 16);
        assert_eq!(id, prompt_set_id());
    }

    #[test]
    fn stored_prompts_are_read_back_only_when_whole() {
        let dir = tempfile::tempdir().unwrap();
        let unit = |n: usize| {
            let mut v = vec![0.0; rawtonic_catalog::content::DIM];
            v[n] = 1.0;
            v
        };
        let stored = StoredPrompts {
            id: "abc".into(),
            misfire: (0..MISFIRE_PROMPTS.len()).map(unit).collect(),
            document: (0..DOCUMENT_PROMPTS.len()).map(unit).collect(),
        };
        rawtonic_fs::atomic_write(
            &stored_path(dir.path(), "abc"),
            serde_json::to_vec(&stored).unwrap(),
        )
        .unwrap();
        let set = read_stored(dir.path(), "abc").unwrap();
        assert_eq!(set.misfire.len(), MISFIRE_PROMPTS.len());
        assert_eq!(set.logit_scale, crate::tagging::SIGLIP_LOGIT_SCALE);
        // Another id, or a file cut short, is made again.
        assert!(read_stored(dir.path(), "abd").is_none());
        std::fs::write(stored_path(dir.path(), "abc"), b"{\"id\":\"abc\"").unwrap();
        assert!(read_stored(dir.path(), "abc").is_none());
    }

    #[test]
    fn rows_are_unit_vectors_and_a_wrong_count_is_refused() {
        let dim = rawtonic_catalog::content::DIM;
        let flat = vec![2.0; dim * 2];
        let out = rows(&flat, 2).unwrap();
        assert_eq!(out.len(), 2);
        let norm: f32 = out[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
        assert!(rows(&flat, 3).is_err());
    }
}
