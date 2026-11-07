//! File-valued recipe fields. The recipe owns the complete list; metadata
//! visits those fields recursively, without interpreting arbitrary user text.
use crate::Key;

pub const LUT_PATH: Key<Option<String>> = Key::new("lutPath", None, 1);
pub const CAMERA_PROFILE: Key<Option<String>> = Key::new("cameraProfile", None, 1);
pub const RASTER_PATH: Key<Option<String>> = Key::new("raster_path", None, 1);
pub const FILE_KEYS: [&Key<Option<String>>; 3] = [&LUT_PATH, &CAMERA_PROFILE, &RASTER_PATH];

pub fn is_file_key(name: &str) -> bool {
    FILE_KEYS.iter().any(|key| key.name() == name)
}
