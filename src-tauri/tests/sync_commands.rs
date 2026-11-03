//! Tauri 2 runs a command declared with plain `fn` on the main thread, so a
//! command that reads files, parses them or waits on a lock stalls the window.
//! Such commands are `async` and do their blocking work in
//! `tauri::async_runtime::spawn_blocking`. This test lists the commands that
//! are still synchronous and why that is fine; a new synchronous command fails
//! it until it is made async or listed here with its reason.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// (command, why it may run on the main thread)
const SYNC_COMMANDS: &[(&str, &str)] = &[
    (
        "handle_import_presets_from_files",
        "read-modify-write of the whole preset library: stays ordered with the frontend's preset saves on the main thread; moving it off needs a library-level lock (follow-up)",
    ),
    (
        "handle_import_legacy_presets_from_file",
        "read-modify-write of the whole preset library: stays ordered with the frontend's preset saves on the main thread; moving it off needs a library-level lock (follow-up)",
    ),
    (
        "handle_import_presets_from_file",
        "read-modify-write of the whole preset library: stays ordered with the frontend's preset saves on the main thread; moving it off needs a library-level lock (follow-up)",
    ),
    (
        "app_menu_text_action",
        "main-thread-only API: menu, window, quit or webview",
    ),
    (
        "build_previews",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "calculate_guided_perspective",
        "pure computation on the arguments",
    ),
    (
        "calculate_manual_transform",
        "pure computation on the arguments",
    ),
    (
        "cancel_ai_task",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "cancel_export",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "cancel_image_listing",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "cancel_profile_thumbnails",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "cancel_quit",
        "main-thread-only API: menu, window, quit or webview",
    ),
    (
        "cancel_series_match",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "cancel_thumbnail_generation",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    ("capture_time_plan", "pure computation on the arguments"),
    (
        "clear_image_caches",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "clear_series_match_cache",
        "in-memory state behind a short lock; no I/O",
    ),
    ("contribution_app_info", "pure computation on the arguments"),
    (
        "contribution_available",
        "pure computation on the arguments",
    ),
    (
        "discard_merge",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "duplicates_cancel",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "duplicates_start",
        "spawns the file manager or a background job and returns at once",
    ),
    (
        "edit_with_interrupted",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "export_queue_cancel",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "export_queue_hold",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "export_queue_pause",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "export_queue_remove",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "export_queue_resume",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "finish_quit",
        "main-thread-only API: menu, window, quit or webview",
    ),
    (
        "folder_sync_cancel",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    ("frontend_log", "writes one log line"),
    (
        "frontend_ready",
        "main-thread-only API: menu, window, quit or webview",
    ),
    ("get_log_file_path", "pure computation on the arguments"),
    (
        "get_supported_file_types",
        "pure computation on the arguments",
    ),
    (
        "gpu_start_error",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "library_export_catalog_cancel",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "library_import_catalog_cancel",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "next_image_listing_page",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "note_ai_mask_use",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "prepare_embedded_stills",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "prepare_preview_proxies",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "preview_build_progress",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "quit_saving",
        "main-thread-only API: menu, window, quit or webview",
    ),
    (
        "session_cancel_ingest",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "set_app_menu",
        "main-thread-only API: menu, window, quit or webview",
    ),
    (
        "set_app_menu_state",
        "main-thread-only API: menu, window, quit or webview",
    ),
    (
        "set_multi_view_live",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "set_window_full_screen",
        "main-thread-only API: menu, window, quit or webview",
    ),
    (
        "show_in_finder",
        "spawns the file manager or a background job and returns at once",
    ),
    (
        "stop_eye_check",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "stop_file_operation",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "stop_preview_build",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "storage_cancel_scan",
        "sets a cancel flag or wakes a worker; no I/O",
    ),
    (
        "tether_live_view_frame",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "tether_next_capture",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "tether_status",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "unload_ai_models",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "update_thumbnail_queue",
        "in-memory state behind a short lock; no I/O",
    ),
    (
        "usage_stats_note_tools",
        "in-memory state behind a short lock; no I/O",
    ),
];

fn source_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Names of `#[tauri::command]` functions declared without `async`.
fn synchronous_commands() -> BTreeSet<String> {
    let mut files = Vec::new();
    source_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut found = BTreeSet::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        let mut in_command = false;
        for line in text.lines() {
            let line = line.trim_start();
            if line.starts_with("#[tauri::command") {
                in_command = true;
            } else if in_command && line.starts_with("#[") {
                // another attribute between the command attribute and the fn
            } else if in_command {
                in_command = false;
                let signature = line
                    .strip_prefix("pub(crate) ")
                    .or_else(|| line.strip_prefix("pub "))
                    .unwrap_or(line);
                if let Some(rest) = signature.strip_prefix("fn ") {
                    let name = rest.split(['(', '<']).next().unwrap().trim();
                    found.insert(name.to_string());
                }
            }
        }
    }
    found
}

#[test]
fn synchronous_commands_are_the_listed_ones() {
    let found = synchronous_commands();
    let listed: BTreeSet<String> = SYNC_COMMANDS.iter().map(|(n, _)| n.to_string()).collect();
    let new: Vec<_> = found.difference(&listed).collect();
    assert!(
        new.is_empty(),
        "synchronous #[tauri::command] fns run on the main thread: make them async with \
         spawn_blocking for file I/O, parsing or lock waits, or list them in SYNC_COMMANDS \
         with a reason: {new:?}"
    );
    let stale: Vec<_> = listed.difference(&found).collect();
    assert!(
        stale.is_empty(),
        "no longer synchronous commands, remove from SYNC_COMMANDS: {stale:?}"
    );
}

#[test]
fn every_listed_command_is_registered() {
    let handler =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs")).unwrap();
    for (name, reason) in SYNC_COMMANDS {
        assert!(!reason.is_empty(), "{name} needs a reason");
        assert!(
            handler
                .lines()
                .any(|l| l.trim().ends_with(&format!("::{name},"))),
            "{name} is not in the invoke handler list"
        );
    }
}
