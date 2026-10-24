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

/// Scrubs a channel's payload and returns the exact body to review.
#[tauri::command]
pub fn contribution_prepare(
    channel: Channel,
    payload: serde_json::Value,
) -> Result<Prepared, String> {
    payload::prepare(channel, payload, &SCRUBBER).map_err(|e| e.to_string())
}

/// Sends a reviewed body, once. Errors are the client's codes.
#[tauri::command]
pub async fn contribution_send(
    app_handle: AppHandle,
    prepared: Prepared,
) -> Result<Receipt, String> {
    let client = client(&app_handle)?;
    client.send(&prepared, &SCRUBBER).await.map_err(|e| {
        log::warn!("Contribution not sent: {e}");
        e.code().to_string()
    })
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
