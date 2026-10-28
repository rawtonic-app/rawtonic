//! What the helper needs from a camera driver: libgphoto2 (`gphoto`) or
//! the scripted mock (`mock`).
use crate::protocol::{CameraInfo, CameraStatus, Setting, TetherError};
use serde::{Deserialize, Serialize};
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
    /// An error when it cannot be told (then nothing is read or fired).
    fn memory_only(&mut self) -> Result<bool, TetherError> {
        Ok(memory_only(self.status().capture_target.as_deref()))
    }
    /// Sets Save to to an option that keeps a card copy, where the camera
    /// offers one; whether it did.
    fn prefer_card(&mut self) -> bool {
        false
    }
    /// Frees a downloaded photo the app committed from the camera's USB
    /// transfer queue, on cameras that keep one; never a file on a card.
    /// Nothing on others. Whether it freed one.
    fn release(&mut self, _file: &CameraFile) -> Result<bool, TetherError> {
        Ok(false)
    }
    /// The photos in the camera's USB transfer queue now, oldest first, with
    /// the size the camera reports for each (read only: nothing is
    /// downloaded or freed). They were taken while no computer took them
    /// (before the connect, or while the helper was away), and on a camera
    /// that writes no card copy the queue is their only copy. Empty on
    /// cameras without a queue.
    fn queued(&mut self) -> Result<Vec<(CameraFile, Option<u64>)>, TetherError> {
        Ok(Vec::new())
    }
    /// Whether a shot may only be fired once the last one's photo came: the
    /// camera takes no shot it is fired while it is still busy with the
    /// last, and says nothing about it.
    fn one_shot_at_a_time(&self) -> bool {
        false
    }
    /// Whether the camera is driven on a path no body was tested on yet
    /// (libgphoto2's Canon EOS path), as the driver knows it from opening
    /// the camera — without reading its settings: on that path a settings
    /// read already prepares the camera for capture.
    fn experimental(&self) -> bool {
        false
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

/// A storage the camera lists (libgphoto2's storage information), as far
/// as it tells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Storage {
    /// The PTP storage ID (libgphoto2's folder "/store_<id>").
    pub id: Option<u32>,
    /// A removable medium (PTP storage type removable RAM or ROM).
    pub removable: Option<bool>,
    pub capacity_kb: Option<u64>,
}

impl Storage {
    /// A memory card with room on it: what a card in the camera looks like.
    pub fn card() -> Self {
        Self {
            id: Some(0x0002_0001),
            removable: Some(true),
            capacity_kb: Some(64 << 20),
        }
    }
}

/// Canon's virtual storage (live view and in-memory captures), listed
/// also without a card; libgphoto2 itself takes a camera that lists only it
/// as one without a card.
pub const VIRTUAL_STORAGE: u32 = 0x8000_0001;

/// Whether the storages show a memory card: only on positive evidence — a
/// listed storage other than the virtual one that says it is removable and
/// has a capacity above 0. Anything the camera does not tell counts as no
/// card (the card copy is what keeps a photo safe there).
pub fn card_in(storages: &[Storage]) -> bool {
    storages.iter().any(|storage| {
        storage.id.is_some_and(|id| id != VIRTUAL_STORAGE)
            && storage.removable == Some(true)
            && storage.capacity_kb.is_some_and(|kb| kb > 0)
    })
}

/// Canon EOS's CaptureDestination property (0xd11c), which libgphoto2
/// exposes as the generic setting "d11c" (its value in decimal): where the
/// camera really puts a capture. libgphoto2's Save to is only its saved
/// preference: with no other destination offered it keeps the computer
/// (4) while saying "card". notes: reference/tether.md
pub const CANON_DESTINATION: &str = "d11c";

/// The CaptureDestination value of the computer's side (the camera's
/// memory, handed over and acknowledged on transfer): no card copy.
const CANON_DESTINATION_HOST: u32 = 4;

/// Whether a Canon's CaptureDestination keeps a card copy: only a value the
/// camera reported that is not the computer's (nor 0). Unknown counts as
/// no card copy.
pub fn canon_destination_keeps_card(value: Option<&str>) -> bool {
    value
        .and_then(|value| value.trim().parse::<u32>().ok())
        .is_some_and(|value| value != CANON_DESTINATION_HOST && value != 0)
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

    #[test]
    fn a_canon_destination_keeps_a_card_copy_only_when_the_camera_says_so() {
        assert!(canon_destination_keeps_card(Some("1")));
        assert!(canon_destination_keeps_card(Some("2")));
        assert!(!canon_destination_keeps_card(Some("4")));
        assert!(!canon_destination_keeps_card(Some("0")));
        assert!(!canon_destination_keeps_card(Some("Memory card")));
        assert!(!canon_destination_keeps_card(None));
    }

    #[test]
    fn a_card_is_only_a_removable_storage_with_room_that_is_not_the_virtual_one() {
        assert!(card_in(&[Storage::card()]));
        assert!(!card_in(&[]));
        // Canon's virtual storage alone: no card (libgphoto2's own test).
        let virtual_only = Storage {
            id: Some(VIRTUAL_STORAGE),
            ..Storage::card()
        };
        assert!(!card_in(std::slice::from_ref(&virtual_only)));
        assert!(card_in(&[virtual_only, Storage::card()]));
        // What the camera does not tell is no evidence of a card.
        let unknown = |change: fn(&mut Storage)| {
            let mut storage = Storage::card();
            change(&mut storage);
            card_in(&[storage])
        };
        assert!(!unknown(|s| s.id = None));
        assert!(!unknown(|s| s.removable = None));
        assert!(!unknown(|s| s.removable = Some(false)));
        assert!(!unknown(|s| s.capacity_kb = None));
        assert!(!unknown(|s| s.capacity_kb = Some(0)));
    }
}
