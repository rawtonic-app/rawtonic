//! Library, files and sidecars of the desktop app, split by concern into
//! the modules below (ADR 0009); their items are re-exported here so
//! `crate::file_management::…` paths and the command registrations hold.

// Not compute-heavy — these threads mostly block waiting on iCloud to
// materialize a file, not burning CPU — so a small fixed pool is enough and
// doesn't need a user-facing setting the way thumbnail_worker_threads does.

pub use rawtonic_engine::file_io::read_file_mapped;

// Camera JPEGs are sufficient for the browser even when smaller than the
// configured medium cache size. Develop/export still decode the original RAW.

pub use rawtonic_export::export_processing::generate_filename_from_template;

mod albums;
mod edits;
mod folders;
mod listing;
mod operations;
mod presets;
mod thumbnails;
mod xmp_sync;

pub use albums::*;
pub use edits::*;
pub use folders::*;
pub use listing::*;
pub use operations::*;
pub use presets::*;
pub use thumbnails::*;
pub use xmp_sync::*;

/// Runs a command's blocking work (file IO, a catalog write that may wait
/// for a locked database) on a blocking worker: a synchronous command runs
/// on the main thread, which then freezes the window and the UI while it
/// waits. The work opens its own catalog connections there.
pub(crate) async fn blocking<R: Send + 'static>(
    work: impl FnOnce() -> R + Send + 'static,
) -> Result<R, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())
}
