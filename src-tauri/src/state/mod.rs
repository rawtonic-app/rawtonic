//! Managed state split out of `AppState` (plan step 3.3), one sub-state per
//! concern, and the one lock policy for it. See `README.md` here.

pub mod ai;
pub mod develop;
pub mod export;
pub mod gpu;
pub mod library;
pub mod library_files;
pub mod lock;
pub mod merge;
pub mod settings;
pub mod startup;
pub mod thumbnails;
