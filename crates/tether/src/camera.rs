//! What the helper needs from a camera driver: libgphoto2 (`gphoto`) or
//! the scripted mock (`mock`).
use crate::protocol::{CameraInfo, CameraStatus, Setting, TetherError};
use std::time::Duration;

/// A file on the camera.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CameraFile {
    pub folder: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CameraEvent {
    /// Nothing happened before the timeout.
    Timeout,
    /// A new photo is ready to download.
    FileAdded(CameraFile),
    CaptureComplete,
    /// The camera reported changed settings (a dial turned).
    SettingsChanged,
    /// Anything else the driver reported.
    Other,
}

/// What firing the shutter gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fired {
    /// The photo will come as a `FileAdded` event.
    Pending,
    /// The driver waited for the photo itself (cameras without a separate
    /// trigger): this file.
    File(CameraFile),
}

/// Where the helper may stop dead on purpose in tests (`Camera::abort_at`;
/// an exit without any clean-up, like a crash but without a crash report).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortPoint {
    /// Half of the download written to its `.part` file.
    MidDownload,
    /// The download published, before the app was told.
    BeforeEvent,
}

pub trait Driver {
    /// The camera library's version, `None` for the mock.
    fn library(&self) -> Option<String>;
    fn list(&mut self) -> Result<Vec<CameraInfo>, TetherError>;
    /// Opens the camera on `port`, or the first one found.
    fn open(&mut self, port: Option<&str>) -> Result<Box<dyn Camera>, TetherError>;
}

pub trait Camera {
    fn status(&mut self) -> CameraStatus;
    fn settings(&mut self) -> Result<Vec<Setting>, TetherError>;
    fn set_setting(&mut self, key: &str, value: &str) -> Result<(), TetherError>;
    fn fire(&mut self) -> Result<Fired, TetherError>;
    fn autofocus(&mut self) -> Result<(), TetherError>;
    /// One live-view JPEG.
    fn preview(&mut self) -> Result<Vec<u8>, TetherError>;
    fn wait_event(&mut self, timeout: Duration) -> Result<CameraEvent, TetherError>;
    /// The file's bytes and the size the camera reports for it, if it does.
    fn download(&mut self, file: &CameraFile) -> Result<(Vec<u8>, Option<u64>), TetherError>;
    /// Frees a downloaded photo the app committed from the camera's USB
    /// transfer queue, on cameras that keep one; never a file on a card.
    /// Nothing on others.
    fn release(&mut self, _file: &CameraFile) -> Result<(), TetherError> {
        Ok(())
    }
    /// Test hook: whether the helper should stop dead at `point` while
    /// handling the camera's `shot`-th file (1-based). Only the mock says yes.
    fn abort_at(&self, _point: AbortPoint, _shot: u64) -> bool {
        false
    }
}

/// The settings the interface shows, each with the names camera drivers
/// use for it (libgphoto2's names differ between makers and models; the
/// first one a camera has is used).
pub const SETTINGS: &[(&str, &str, &[&str])] = &[
    ("iso", "ISO", &["iso", "isospeed", "iso-speed"]),
    (
        "shutterspeed",
        "Shutter speed",
        &["shutterspeed", "shutterspeed2", "exposuretime"],
    ),
    ("aperture", "Aperture", &["f-number", "aperture", "fnumber"]),
    (
        "exposurecompensation",
        "Exposure compensation",
        &["exposurecompensation", "exposurebiascompensation"],
    ),
    ("whitebalance", "White balance", &["whitebalance"]),
    (
        "colortemperature",
        "Colour temperature",
        &["colortemperature", "wbcolortemperature"],
    ),
    (
        "exposuremode",
        "Exposure mode",
        &["expprogram", "autoexposuremode", "exposureprogram"],
    ),
    (
        "meteringmode",
        "Metering",
        &["exposuremetermode", "meteringmode"],
    ),
    ("focusmode", "Focus mode", &["focusmode"]),
    ("drivemode", "Drive mode", &["capturemode", "drivemode"]),
    (
        "imagequality",
        "Image quality",
        &["imagequality", "imageformat"],
    ),
];

/// Our key for a camera's setting name, if the interface shows it.
pub fn setting_aliases(key: &str) -> Option<(&'static str, &'static [&'static str])> {
    SETTINGS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, label, aliases)| (*label, *aliases))
}
