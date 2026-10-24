// SPDX-License-Identifier: AGPL-3.0-or-later
//! Commands of the contribution channels (ADR 0017): prepare a body for the
//! "Review what will be sent" dialog, send it after the user clicked Send,
//! delete a submission by its id. No channel calls them on its own, and a
//! build without a relay URL sends nothing.

use rawtonic_contribute::client::{Receipt, RelayClient};
use rawtonic_contribute::payload::{self, Channel, Prepared};
use rawtonic_contribute::scrub::Scrubber;
use std::sync::LazyLock;
use tauri::AppHandle;

static SCRUBBER: LazyLock<Scrubber> = LazyLock::new(Scrubber::for_this_machine);

fn client(app: &AppHandle) -> Result<RelayClient, String> {
    RelayClient::from_build(&app.package_info().version.to_string())
        .ok_or_else(|| "not-configured".to_string())
}

/// Whether this build can send contributions at all.
#[tauri::command]
pub fn contribution_available(app_handle: AppHandle) -> bool {
    client(&app_handle).is_ok()
}

/// Scrubs a channel's payload and returns the exact body to review. A
/// report's screenshot (`screenshot.data`, base64) and photo file
/// (`photo.path`) become attachments the body announces by size and
/// SHA-256; hashing a photo's file runs off the main thread.
#[tauri::command]
pub async fn contribution_prepare(
    channel: Channel,
    payload: serde_json::Value,
) -> Result<Prepared, String> {
    tauri::async_runtime::spawn_blocking(move || payload::prepare(channel, payload, &SCRUBBER))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Sends a reviewed body, once. Errors are the client's codes.
#[tauri::command]
pub async fn contribution_send(
    app_handle: AppHandle,
    prepared: Prepared,
) -> Result<Receipt, String> {
    let client = client(&app_handle)?;
    let receipt = client.send(&prepared, &SCRUBBER).await.map_err(|e| {
        log::warn!("Contribution not sent: {e}");
        e.code().to_string()
    })?;
    if !receipt.not_uploaded.is_empty() {
        log::warn!("Contribution sent without {:?}", receipt.not_uploaded);
    }
    Ok(receipt)
}

/// Deletes a submission by the id its receipt gave.
#[tauri::command]
pub async fn contribution_delete(app_handle: AppHandle, id: String) -> Result<(), String> {
    let client = client(&app_handle)?;
    client.delete(&id).await.map_err(|e| {
        log::warn!("Contribution not deleted: {e}");
        e.code().to_string()
    })
}
