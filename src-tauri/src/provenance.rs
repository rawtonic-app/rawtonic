//! The Export panel's Provenance (C2PA) section: the signing identity in the
//! app's data folder (`provenance/`) and the manifest a photo's export would
//! carry. The manifests themselves are written by `rawtonic_export::c2pa`.
//! No network access: nothing is sent anywhere, the identity never leaves
//! this computer.

use std::path::PathBuf;
use std::sync::Arc;

use rawtonic_export::c2pa::{self, IdentityInfo, ManifestPlan, SigningIdentity};
use rawtonic_export::export_processing::ExportSettings;

use crate::file_management::parse_virtual_path;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

fn identity_dir(app_handle: &AppHandle) -> Result<PathBuf, String> {
    app_handle
        .path()
        .app_data_dir()
        .map(|dir| dir.join("provenance"))
        .map_err(|e| e.to_string())
}

/// The identity exports sign with, when one is set up. A stored identity
/// that cannot be read is logged (its error only, never key material) and
/// counts as none: exports asking for a manifest then fail with
/// `PROVENANCE_NEEDS_IDENTITY`.
pub(crate) fn identity_for_export(app_handle: &AppHandle) -> Option<Arc<SigningIdentity>> {
    let dir = identity_dir(app_handle).ok()?;
    match c2pa::identity::load(&dir) {
        Ok(identity) => identity.map(Arc::new),
        Err(e) => {
            log::warn!("Provenance signing identity not loaded: {e}");
            None
        }
    }
}

/// Checks, when a job is queued, that each export asking for a manifest can
/// be signed now (the current identity, a usable certificate, a format that
/// takes it) and pins the identity's fingerprint to it, so the job never
/// signs with a certificate the user did not see when queuing it
/// (`PROVENANCE_SIGNER_CHANGED`). Nothing is queued on an error. Covers
/// every way exports are queued (the Export panel, the File menu's recipes).
pub(crate) fn pin_signers(
    app_handle: &AppHandle,
    requests: &mut [crate::export_queue::ExportRequest],
) -> Result<(), String> {
    if requests
        .iter()
        .all(|request| request.export_settings.provenance.is_none())
    {
        return Ok(());
    }
    let identity = c2pa::identity::load(&identity_dir(app_handle)?)?;
    for request in requests {
        pin_signer(
            &mut request.export_settings,
            &request.output_format,
            identity.as_ref(),
        )?;
    }
    Ok(())
}

/// [`pin_signers`] for one export's settings.
fn pin_signer(
    settings: &mut ExportSettings,
    output_format: &str,
    identity: Option<&SigningIdentity>,
) -> Result<(), String> {
    let Some(provenance) = settings.provenance.as_mut() else {
        return Ok(());
    };
    let identity = identity
        .ok_or_else(|| rawtonic_export::export_processing::PROVENANCE_NEEDS_IDENTITY.to_string())?;
    if let Some(error) = rawtonic_export::export_processing::provenance_format_error(
        &output_format.to_ascii_lowercase(),
    ) {
        return Err(error);
    }
    identity.check_usable()?;
    let fingerprint = identity.fingerprint();
    match &provenance.signer {
        Some(pinned) if *pinned != fingerprint => {
            Err(rawtonic_export::export_processing::PROVENANCE_SIGNER_CHANGED.to_string())
        }
        _ => {
            provenance.signer = Some(fingerprint);
            Ok(())
        }
    }
}

#[tauri::command]
pub async fn provenance_identity(app_handle: AppHandle) -> Result<Option<IdentityInfo>, String> {
    let dir = identity_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || c2pa::identity::info(&dir))
        .await
        .map_err(|e| e.to_string())?
}

/// Makes a key and a local certificate naming `signer` (replacing the
/// identity there was).
#[tauri::command]
pub async fn generate_provenance_identity(
    signer: String,
    app_handle: AppHandle,
) -> Result<IdentityInfo, String> {
    let dir = identity_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || c2pa::identity::generate(&dir, &signer))
        .await
        .map_err(|e| e.to_string())?
}

/// Imports the user's certificate from a PKCS#12 file. The password opens
/// the file once and is not kept.
#[tauri::command]
pub async fn import_provenance_certificate(
    path: String,
    password: String,
    app_handle: AppHandle,
) -> Result<IdentityInfo, String> {
    let dir = identity_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || {
        c2pa::identity::import_pkcs12(&dir, std::path::Path::new(&path), &password)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Deletes the signing identity (the user's request).
#[tauri::command]
pub async fn remove_provenance_identity(app_handle: AppHandle) -> Result<bool, String> {
    let dir = identity_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || c2pa::identity::remove(&dir))
        .await
        .map_err(|e| e.to_string())?
}

/// What the Export panel shows before an export: the manifest one photo
/// gets, who signs it, and whether the format can carry it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenancePreview {
    pub manifest: ManifestPlan,
    pub identity: Option<IdentityInfo>,
    /// Why this format cannot carry a manifest, if it cannot.
    pub format_error: Option<String>,
}

/// The manifest `path`'s export with `export_settings` gets, read as its
/// export reads the photo (`pin_photos`): the recipe (the editor's for the
/// open photo), its creator for the author. A resize is taken as changing
/// the size (the export knows for sure).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn export_provenance_preview(
    path: String,
    export_settings: ExportSettings,
    output_format: String,
    current_edit_path: Option<String>,
    current_edit_adjustments: Option<Value>,
    library: crate::export_queue::SubmittedFrom,
    app_handle: AppHandle,
) -> Result<ProvenancePreview, String> {
    let dir = identity_dir(&app_handle)?;
    tauri::async_runtime::spawn_blocking(move || {
        let pinned = crate::export_processing::pin_photos(
            std::slice::from_ref(&path),
            current_edit_path.as_deref(),
            current_edit_adjustments.as_ref(),
            library.directory(),
        )?
        .remove(0);
        let (source, _) = parse_virtual_path(&path);
        let changes = c2pa::OutputChanges {
            resized: export_settings.resize.is_some() || export_settings.super_resolution,
            framed_or_watermarked: export_settings.border.is_some()
                || export_settings.pad.is_some()
                || export_settings.watermark.is_some(),
        };
        let metadata = pinned.metadata.clone();
        let source_str = source.to_string_lossy();
        let exif = || pinned.exif(&source);
        let manifest = rawtonic_export::export_processing::provenance_plan(
            &source_str,
            &pinned.adjustments,
            &export_settings,
            &exif,
            &|| metadata.clone(),
            changes,
        );
        Ok(ProvenancePreview {
            manifest,
            identity: c2pa::identity::info(&dir)?,
            format_error: rawtonic_export::export_processing::provenance_format_error(
                &output_format.to_ascii_lowercase(),
            ),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(format_ok: bool) -> (ExportSettings, &'static str) {
        let settings = serde_json::from_value(serde_json::json!({
            "jpegQuality": 90, "resize": null, "keepMetadata": false, "stripGps": true,
            "filenameTemplate": null, "watermark": null, "provenance": {"includeAuthor": false}
        }))
        .unwrap();
        (settings, if format_ok { "jpg" } else { "webp" })
    }

    #[test]
    fn queued_exports_pin_the_signer_they_were_checked_with() {
        let dir = tempfile::tempdir().unwrap();
        c2pa::identity::generate(dir.path(), "Studio").unwrap();
        let identity = c2pa::identity::load(dir.path()).unwrap().unwrap();

        let (mut ok, format) = settings(true);
        pin_signer(&mut ok, format, Some(&identity)).unwrap();
        assert_eq!(
            ok.provenance.as_ref().unwrap().signer.as_deref(),
            Some(identity.fingerprint().as_str())
        );
        // Resumed later with the same identity: still fine.
        pin_signer(&mut ok, format, Some(&identity)).unwrap();

        // Another identity by the time it runs: refused.
        let other_dir = tempfile::tempdir().unwrap();
        c2pa::identity::generate(other_dir.path(), "Other").unwrap();
        let other = c2pa::identity::load(other_dir.path()).unwrap().unwrap();
        assert!(pin_signer(&mut ok.clone(), format, Some(&other)).is_err());

        let (mut none, format) = settings(true);
        assert_eq!(
            pin_signer(&mut none, format, None).unwrap_err(),
            rawtonic_export::export_processing::PROVENANCE_NEEDS_IDENTITY
        );
        let (mut webp, format) = settings(false);
        assert!(pin_signer(&mut webp, format, Some(&identity)).is_err());

        // Exports without the option are left as they are.
        let mut plain = settings(true).0;
        plain.provenance = None;
        pin_signer(&mut plain, "webp", None).unwrap();
        assert!(plain.provenance.is_none());
    }
}
