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
    /// Whether the camera keeps its photos only in its memory now (no card
    /// copy): such a photo exists only until it is transferred, so the
    /// helper neither polls nor fires it then.
    fn memory_only(&mut self) -> bool {
        memory_only(self.status().capture_target.as_deref())
    }
    /// Sets Save to to an option that keeps a card copy, where the camera
    /// offers one; whether it did.
    fn prefer_card(&mut self) -> bool {
        false
    }
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

/// Whether a "Save to" (capture target) keeps photos only in the camera's
/// memory: such a photo exists only until it has been transferred, and
/// libgphoto2 acknowledges some transfers (Canon EOS) before the helper has
/// written the file — so the helper neither polls nor fires then.
pub fn memory_only(capture_target: Option<&str>) -> bool {
    capture_target.is_some_and(|target| {
        let target = target.to_lowercase();
        target.contains("ram") && !target.contains("card")
    })
}

/// The Save to choice that keeps a card copy and still hands each photo to
/// the computer ("card+sdram" before "card": card alone may not transfer).
pub fn card_choice(choices: &[String]) -> Option<&String> {
    let lower = |c: &String| c.to_lowercase();
    choices
        .iter()
        .find(|c| lower(c).contains("card") && lower(c).contains("ram"))
        .or_else(|| {
            choices
                .iter()
                .find(|c| lower(c).contains("card") && !memory_only(Some(c)))
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_to_keeps_a_card_copy_and_still_hands_photos_to_the_computer() {
        let choices = |c: &[&str]| c.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Sony: card alone would not transfer.
        let sony = choices(&["sdram", "card", "card+sdram"]);
        assert_eq!(card_choice(&sony).unwrap(), "card+sdram");
        // Canon: the card is the only option with one.
        let canon = choices(&["Internal RAM", "Memory card"]);
        assert_eq!(card_choice(&canon).unwrap(), "Memory card");
        assert!(card_choice(&choices(&["sdram"])).is_none());
        assert!(memory_only(Some("Internal RAM")) && memory_only(Some("sdram")));
        assert!(!memory_only(Some("card+sdram")) && !memory_only(None));
    }
}
