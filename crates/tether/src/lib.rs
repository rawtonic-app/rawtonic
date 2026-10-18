//! Camera tethering (G46): the camera library runs in a helper process
//! (`rawtonic-tether`), never in the app, so a crash or a hung USB transfer
//! in it can only end the helper. libgphoto2 (LGPL-2.1-or-later) is linked
//! dynamically into the helper alone, behind the `gphoto2` feature; without
//! it the helper offers only the scripted mock camera.
//!
//! - `protocol`, `frame`: what app and helper say over the helper's pipes.
//! - `camera`: what the helper needs from a camera driver; `mock` and
//!   `gphoto` implement it.
//! - `server`: the helper's loop; `staging`: its crash-safe transfer folder.
//! - `client`: the app's side.
//!
//! Nothing here deletes or changes files on the camera.
pub mod camera;
pub mod client;
pub mod frame;
#[cfg(all(feature = "gphoto2", any(target_os = "macos", target_os = "linux")))]
pub mod gphoto;
pub mod mock;
pub mod protocol;
pub mod server;
pub mod staging;

/// The helper's file name.
pub const HELPER: &str = if cfg!(windows) {
    "rawtonic-tether.exe"
} else {
    "rawtonic-tether"
};
