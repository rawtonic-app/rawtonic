//! Managed state split out of `AppState` (plan step 3.3), one sub-state per
//! concern, and the one lock policy for it. See `README.md` here.

pub mod ai;
pub mod develop;
pub mod lock;
pub mod settings;
pub mod thumbnails;
