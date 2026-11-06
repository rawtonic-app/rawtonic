//! The parsed LUT files (plan step 3.3), shared by the preview, thumbnails,
//! multi view, collages and exports. See `README.md` here.

use std::sync::{Mutex, MutexGuard};

use super::lock::lock_recover;
use crate::lut_processing::LutCache;

/// The LUTs parsed this run (`lut_processing::get_or_load_lut`,
/// `load_and_parse_lut_blocking`).
#[derive(Default)]
pub struct LutState {
    cache: Mutex<LutCache>,
}

impl LutState {
    /// The cache, for one look-up or store.
    pub(crate) fn cache(&self) -> MutexGuard<'_, LutCache> {
        lock_recover(&self.cache)
    }
}
