// SPDX-License-Identifier: AGPL-3.0-or-later
//! "Install update" (user decision 2026-11-18, PRIVACY.md): only after
//! the photographer's click, the app fetches the offered release's own
//! `latest.json`, downloads this copy's package from that release, checks
//! its signature against Rawtonic's updater key (tauri-plugin-updater,
//! `plugins.updater` in tauri.conf.json) and keeps it until "Restart now".
//! The webview never names a URL: it names the version the startup check
//! offered, and the check recorded where that version's manifest is.
use super::core::{Offer, Source, package_url_allowed};
use serde::Serialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Error, Update, UpdaterExt};

/// What the last check offered, the download running and the verified
/// package waiting for the restart.
#[derive(Default)]
pub struct UpdateInstall {
    offered: Mutex<Option<(Offer, Source)>>,
    /// The running download: its number, version and task.
    download: Mutex<Option<(u64, String, tauri::async_runtime::JoinHandle<()>)>>,
    ready: Mutex<Option<(Update, Vec<u8>)>>,
}

impl UpdateInstall {
    pub(crate) fn offer(&self, offer: Offer, source: Source) {
        *crate::state::lock::lock_recover(&self.offered) = Some((offer, source));
    }

    fn offered(&self, version: &str) -> Option<(Offer, Source)> {
        crate::state::lock::lock_recover(&self.offered)
            .clone()
            .filter(|(offer, _)| offer.version == version)
    }

    fn ready_for(&self, version: &str) -> bool {
        crate::state::lock::lock_recover(&self.ready)
            .as_ref()
            .is_some_and(|(update, _)| update.version == version)
    }
}

/// Why an update could not be downloaded or installed; the notice shows a
/// message per kind and leaves the running app as it was.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum UpdateFailure {
    /// No connection, a timeout or a server error.
    Network,
    /// The update endpoint did not serve a release manifest.
    ManifestUnavailable,
    /// The package does not carry Rawtonic's signature for this version.
    Signature,
    /// The release has no package for this computer.
    NoPackage,
    /// The disk is full or the app's folder cannot be written.
    Disk,
    /// Rawtonic may not replace itself there: the administrator's approval
    /// (macOS password prompt) was not given, or the folder is not writable.
    Permission,
    /// The new app could not be put in place and the previous one could not
    /// be put back either; it is kept at `path`.
    NotRestored {
        path: String,
    },
    /// Nothing offered for that version (the check ran again meanwhile).
    NotOffered,
    /// The AppImage file was replaced or removed since Rawtonic started;
    /// it is left as it is.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Changed,
    /// The download was cancelled.
    Stopped,
    Other,
}

fn failure(error: &Error) -> UpdateFailure {
    match error {
        Error::Reqwest(_) | Error::Network(_) | Error::Serialization(_) => UpdateFailure::Network,
        // The updater reports non-success HTTP responses (including 404)
        // as ReleaseNotFound and propagates request errors separately.
        Error::ReleaseNotFound => UpdateFailure::ManifestUnavailable,
        Error::Minisign(_)
        | Error::Base64(_)
        | Error::SignatureUtf8(_)
        | Error::SignedVersionMismatch { .. }
        | Error::MissingSignedVersion
        | Error::InvalidUpdaterFormat
        | Error::BinaryNotFoundInArchive => UpdateFailure::Signature,
        Error::TargetNotFound(_) | Error::TargetsNotFound(_) => UpdateFailure::NoPackage,
        Error::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            UpdateFailure::Permission
        }
        Error::AuthenticationFailed => UpdateFailure::Permission,
        Error::Io(_)
        | Error::TempDirNotFound
        | Error::TempDirNotOnSameMountPoint
        | Error::FailedToDetermineExtractPath => UpdateFailure::Disk,
        Error::PreviousAppNotRestored(path) => UpdateFailure::NotRestored {
            path: path.display().to_string(),
        },
        _ => UpdateFailure::Other,
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress<'a> {
    version: &'a str,
    downloaded: u64,
    total: Option<u64>,
}

const PROGRESS_EVENT: &str = "update-download-progress";

/// Downloads and verifies the update the last check offered as `version`.
/// Resolves when the package is ready for "Restart now"; a second call
/// starts the download again.
#[tauri::command]
pub async fn update_download(app: AppHandle, version: String) -> Result<(), UpdateFailure> {
    let state = app.state::<UpdateInstall>();
    if state.ready_for(&version) {
        return Ok(());
    }
    let (offer, source) = state.offered(&version).ok_or(UpdateFailure::NotOffered)?;
    static DOWNLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let number = DOWNLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        let mut running = crate::state::lock::lock_recover(&state.download);
        if let Some((_, v, handle)) = running.take() {
            handle.abort();
            log::info!("Update: download of {v} replaced");
        }
        let task_app = app.clone();
        let handle = tauri::async_runtime::spawn(async move {
            let result = fetch(&task_app, &offer, &source).await;
            let state = task_app.state::<UpdateInstall>();
            let result = result.map(|ready| {
                *crate::state::lock::lock_recover(&state.ready) = Some(ready);
            });
            let mut running = crate::state::lock::lock_recover(&state.download);
            if running.as_ref().is_some_and(|(n, ..)| *n == number) {
                running.take();
            }
            drop(running);
            let _ = tx.send(result);
        });
        *running = Some((number, version.clone(), handle));
    }
    // An aborted task drops the sender: the download was cancelled.
    rx.await.unwrap_or(Err(UpdateFailure::Stopped))
}

/// Stops a running download; nothing of it is kept.
#[tauri::command]
pub async fn update_cancel_download(app: AppHandle) {
    let state = app.state::<UpdateInstall>();
    if let Some((_, version, handle)) = crate::state::lock::lock_recover(&state.download).take() {
        handle.abort();
        log::info!("Update: download of {version} cancelled");
    }
}

async fn fetch(
    app: &AppHandle,
    offer: &Offer,
    source: &Source,
) -> Result<(Update, Vec<u8>), UpdateFailure> {
    let log_failure = |stage: &str, error: &Error| {
        log::warn!("Update {}: {stage} failed: {error}", offer.version);
        failure(error)
    };
    let manifest = offer
        .manifest_url
        .parse()
        .map_err(|_| UpdateFailure::Other)?;
    let user_agent = format!("Rawtonic/{}", app.package_info().version);
    let redirect_source = source.clone();
    let updater = app
        .updater_builder()
        .endpoints(vec![manifest])
        .map_err(|e| log_failure("setup", &e))?
        .target(offer.target)
        .configure_client(move |client| {
            let source = redirect_source.clone();
            // Only GitHub's release hosts (or the QA feed), no cookies,
            // no total timeout (a large package on a slow line), but no
            // stalled connection either.
            client
                .user_agent(user_agent.clone())
                .connect_timeout(Duration::from_secs(20))
                .read_timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                    let allowed = attempt.url().scheme() == "https"
                        || source.download_prefix.starts_with("http:");
                    match attempt.url().host_str() {
                        Some(host)
                            if allowed
                                && source.allows_host(host)
                                && attempt.previous().len() < 5 =>
                        {
                            attempt.follow()
                        }
                        _ => attempt.stop(),
                    }
                }))
        })
        .build()
        .map_err(|e| log_failure("setup", &e))?;
    let update = updater
        .check()
        .await
        .map_err(|e| log_failure("manifest", &e))?
        .ok_or_else(|| {
            log::warn!(
                "Update {}: the manifest offers nothing newer",
                offer.version
            );
            UpdateFailure::NotOffered
        })?;
    let same_version = super::core::parse_version(&update.version)
        .ok()
        .zip(super::core::parse_version(&offer.version).ok())
        .is_some_and(|(a, b)| a == b);
    if !same_version || !package_url_allowed(update.download_url.as_str(), offer, source) {
        log::warn!(
            "Update {}: the manifest names version {} at {}, outside the offered release",
            offer.version,
            update.version,
            update.download_url
        );
        return Err(UpdateFailure::Signature);
    }
    let mut downloaded = 0u64;
    let mut last = Instant::now() - Duration::from_secs(1);
    let version = offer.version.clone();
    let bytes = update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                if last.elapsed() >= Duration::from_millis(100) || total == Some(downloaded) {
                    last = Instant::now();
                    let _ = app.emit(
                        PROGRESS_EVENT,
                        Progress {
                            version: &version,
                            downloaded,
                            total,
                        },
                    );
                }
            },
            || {},
        )
        .await
        .map_err(|e| log_failure("download", &e))?;
    log::info!(
        "Update {}: {} bytes downloaded, signature verified",
        offer.version,
        bytes.len()
    );
    Ok((update, bytes))
}

/// Puts the downloaded update in place. Called only after the photographer
/// chose "Restart now" and the edits were saved (the quit's checks run in
/// the webview first). macOS and Linux: resolves, and the webview restarts
/// the app. Windows: the per-user setup takes over and the app ends here; the
/// setup starts the new version. A failure leaves the running app as it was
/// and keeps the package for another try.
#[tauri::command]
pub async fn update_install(app: AppHandle, version: String) -> Result<(), UpdateFailure> {
    let state = app.state::<UpdateInstall>();
    let (update, bytes) = {
        let mut ready = crate::state::lock::lock_recover(&state.ready);
        match ready.take() {
            Some((update, bytes)) if update.version == version => (update, bytes),
            other => {
                *ready = other;
                return Err(UpdateFailure::NotOffered);
            }
        }
    };
    log::info!("Update {version}: installing");
    let place_app = app.clone();
    let (update, bytes, result) = tauri::async_runtime::spawn_blocking(move || {
        let result = put_in_place(&place_app, &update, &bytes);
        (update, bytes, result)
    })
    .await
    .map_err(|_| UpdateFailure::Other)?;
    match result {
        Ok(Placed::Restart) => {
            log::info!("Update {version}: installed, restart pending");
            Ok(())
        }
        Ok(Placed::SetupStarted) => {
            // Everything was written before the setup started; the clean
            // exit is marked now, then the quit's own path (lib.rs
            // ExitRequested, whose writes are then no-ops) ends the process.
            // The passive setup closes it if it still runs and starts the
            // new version.
            log::info!("Update {version}: setup started, Rawtonic ends");
            rawtonic_onnx::stop_checks();
            crate::contribute::clean_exit();
            crate::startup::flush_logs();
            app.exit(0);
            Ok(())
        }
        Err(kind) => {
            log::warn!("Update {version}: install failed: {kind:?}");
            if !matches!(kind, UpdateFailure::NotRestored { .. }) {
                *crate::state::lock::lock_recover(&state.ready) = Some((update, bytes));
            }
            Err(kind)
        }
    }
}

enum Placed {
    /// The new app is in place; the webview restarts it.
    #[cfg_attr(windows, allow(dead_code))]
    Restart,
    /// Windows: the per-user setup runs; this process must end.
    #[cfg_attr(not(windows), allow(dead_code))]
    SetupStarted,
}

/// Puts the verified package in place without ever leaving a partial app,
/// even if the process ends meanwhile: macOS swaps the bundles in one step
/// (the plugin; with the administrator's approval where the folder needs
/// it), Linux writes the new AppImage beside the old one and renames it over
/// it (rawtonic-fs `atomic_write`, which keeps its permissions), Windows
/// writes the setup to a temporary folder and starts it — the app ends only
/// once the setup runs, so a setup that cannot start leaves it open.
#[cfg_attr(not(windows), allow(unused_variables))]
fn put_in_place(app: &AppHandle, update: &Update, bytes: &[u8]) -> Result<Placed, UpdateFailure> {
    update.verify(bytes).map_err(|e| failure(&e))?;
    #[cfg(target_os = "macos")]
    {
        update.install(bytes).map_err(|e| failure(&e))?;
        Ok(Placed::Restart)
    }
    #[cfg(target_os = "linux")]
    {
        let Some(appimage) = super::running_appimage() else {
            return Err(UpdateFailure::NoPackage);
        };
        // Replaced or removed since Rawtonic started (another updater, a
        // manual copy): never overwrite what is there now.
        if !super::appimage_unchanged() {
            return Err(UpdateFailure::Changed);
        }
        if !bytes.starts_with(b"\x7fELF") {
            return Err(UpdateFailure::Signature);
        }
        rawtonic_fs::atomic_write(&appimage, bytes).map_err(|e| io_failure(&e))?;
        Ok(Placed::Restart)
    }
    #[cfg(windows)]
    {
        if !bytes.starts_with(b"MZ") {
            return Err(UpdateFailure::Signature);
        }
        // A new folder of its own inside the app's per-user data folder
        // (%LOCALAPPDATA%, only this user may write there), never a
        // predictable name in a shared TEMP: nobody else can swap the
        // verified setup before it starts.
        use super::staging;
        let root = staging::updates_dir(app).map_err(|_| UpdateFailure::Disk)?;
        std::fs::create_dir_all(&root).map_err(|e| io_failure(&anyhow::Error::from(e)))?;
        let dir = tempfile::Builder::new()
            .prefix(&format!("{}-", update.version))
            .rand_bytes(staging::SUFFIX_LEN)
            .tempdir_in(&root)
            .map_err(|e| io_failure(&anyhow::Error::from(e)))?;
        let setup = dir.path().join(staging::setup_name(&update.version));
        rawtonic_fs::atomic_write(&setup, bytes).map_err(|e| io_failure(&e))?;
        // Passive (a progress bar, no questions, no administrator prompt for
        // the per-user setup), as an update, and start Rawtonic when done.
        // What the process still writes on its way out (the photo, settings
        // and library writes were drained before this command) is written
        // now, while the app keeps running: the window states (atomic
        // writes) and the log. If the setup cannot start, nothing is lost;
        // once it runs, its check for a running Rawtonic finds nothing left
        // to cut off.
        crate::window_state::flush();
        crate::second_window::flush();
        crate::startup::flush_logs();
        std::process::Command::new(&setup)
            .args(["/P", "/UPDATE", "/R"])
            .spawn()
            .map_err(|e| io_failure(&anyhow::Error::from(e)))?;
        // The running setup keeps its folder; on any error above the
        // folder was dropped, which removes it with the staged setup.
        let _ = dir.keep();
        Ok(Placed::SetupStarted)
    }
}

#[cfg(any(target_os = "linux", windows))]
fn io_failure(error: &anyhow::Error) -> UpdateFailure {
    match error
        .downcast_ref::<std::io::Error>()
        .map(std::io::Error::kind)
    {
        Some(std::io::ErrorKind::PermissionDenied) => UpdateFailure::Permission,
        _ => UpdateFailure::Disk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Restart now" starts the setup while the app is still ending; the
    /// template's kill of a process that is just exiting fails ("Failed to
    /// kill ... close it first", UPDX2 on Windows 11). The setup must wait
    /// for the old process in an update, before the template's check.
    #[test]
    fn update_setup_waits_for_the_old_process_before_the_template_check() {
        let hooks = include_str!("../../windows/installer-hooks.nsh");
        let setup = include_str!("../../windows/installer.nsi");
        let hook = hooks
            .split("!macro NSIS_HOOK_PREINSTALL")
            .nth(1)
            .expect("a pre-install hook")
            .split("!macroend")
            .next()
            .unwrap();
        assert!(hook.contains("$UpdateMode = 1"));
        assert!(hook.contains("nsis_tauri_utils::FindProcess"));
        assert!(hook.contains("Sleep"));
        // The template runs the hook right before its running-app check.
        let section = setup.split("Section Install").nth(1).unwrap();
        let hook_at = section.find("NSIS_HOOK_PREINSTALL").unwrap();
        let check_at = section.find("CheckIfAppIsRunning").unwrap();
        assert!(hook_at < check_at);
    }

    #[test]
    fn failures_name_what_the_photographer_can_act_on() {
        assert_eq!(
            failure(&Error::Network("503".into())),
            UpdateFailure::Network
        );
        assert_eq!(
            failure(&Error::MissingSignedVersion),
            UpdateFailure::Signature
        );
        assert_eq!(
            failure(&Error::SignedVersionMismatch {
                signed: "0.1.0".into(),
                announced: "0.2.0".into()
            }),
            UpdateFailure::Signature
        );
        assert_eq!(
            failure(&Error::InvalidUpdaterFormat),
            UpdateFailure::Signature
        );
        assert_eq!(
            failure(&Error::TargetNotFound("darwin-aarch64-app".into())),
            UpdateFailure::NoPackage
        );
        assert_eq!(
            failure(&Error::Io(std::io::Error::from(
                std::io::ErrorKind::StorageFull
            ))),
            UpdateFailure::Disk
        );
        assert_eq!(
            failure(&Error::AuthenticationFailed),
            UpdateFailure::Permission
        );
        // macOS reports a refused or cancelled administrator prompt this way.
        assert_eq!(
            failure(&Error::Io(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied
            ))),
            UpdateFailure::Permission
        );
        assert_eq!(
            failure(&Error::PreviousAppNotRestored("/tmp/x".into())),
            UpdateFailure::NotRestored {
                path: "/tmp/x".into()
            }
        );
        assert_eq!(
            serde_json::to_value(UpdateFailure::NotRestored { path: "/a".into() }).unwrap(),
            serde_json::json!({"kind": "notRestored", "path": "/a"})
        );
    }

    #[test]
    fn manifest_and_network_failures_have_distinct_ipc_kinds() {
        for (error, kind) in [
            (Error::ReleaseNotFound, "manifestUnavailable"),
            (Error::Network("503".into()), "network"),
        ] {
            assert_eq!(
                serde_json::to_value(failure(&error)).unwrap(),
                serde_json::json!({ "kind": kind })
            );
        }
    }

    /// The release build's configuration: the public key is Rawtonic's, the
    /// signature must name the version, the Windows setup runs without
    /// questions, and no endpoint is fixed (each release names its own).
    #[test]
    fn the_shipped_configuration_requires_signed_versions() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        let updater = &conf["plugins"]["updater"];
        assert_eq!(updater["requireSignedVersion"], true);
        assert_eq!(updater["windows"]["installMode"], "passive");
        assert!(updater.get("endpoints").is_none());
        assert!(updater.get("dangerousInsecureTransportProtocol").is_none());
        let key = updater["pubkey"].as_str().unwrap();
        let decoded = String::from_utf8(
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, key).unwrap(),
        )
        .unwrap();
        assert!(decoded.starts_with("untrusted comment: minisign public key"));
        assert!(
            !conf["bundle"]
                .get("createUpdaterArtifacts")
                .is_some_and(|v| v == true)
        );
    }
}
