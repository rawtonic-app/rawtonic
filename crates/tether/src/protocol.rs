//! What the app and the tether helper say to each other.
//!
//! Every message is one frame on the helper's stdin or stdout (`frame`): a
//! JSON frame holds a `Request` (app → helper) or a `Message` (helper →
//! app); a live-view frame holds one JPEG from the camera. Requests carry an
//! id their reply echoes; events have none. `Hello` names the protocol
//! number, so an app never talks to a helper of another build.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bumped whenever a request, reply or event changes shape.
pub const PROTOCOL: u32 = 6;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Request {
    Hello,
    /// The cameras on USB.
    ListCameras,
    /// Opens a camera (the first one found when `port` is `None`).
    /// Downloads go into `staging`, one new folder per file.
    Connect {
        port: Option<String>,
        staging: PathBuf,
        /// Only this body (its serial number): another one is closed again
        /// before anything of it is read or downloaded.
        serial: Option<String>,
        /// The user's opt-in to driver paths no body has been tested on yet
        /// (libgphoto2's Canon EOS path, Preferences > Tethering): without
        /// it such a camera is closed again before anything of it is read.
        experimental: bool,
    },
    Disconnect,
    /// Stops taking photos from the camera but keeps it open: every photo
    /// downloaded so far was delivered before the reply, none follows, and
    /// releases are still served (a library switch takes in the last photos
    /// and frees them on the camera before it is closed). Until `Disconnect`
    /// or `Connect`; firing is refused meanwhile.
    Hold,
    /// The camera's settings as they are now.
    Settings,
    /// Sets a setting and reads it back (`Reply::SettingSet`).
    SetSetting {
        key: String,
        value: String,
    },
    /// Fires the shutter; the photo arrives as a `Captured` event naming
    /// the request number of the `Fired` reply.
    Capture,
    Autofocus,
    /// Starts or stops sending live-view frames.
    LiveView {
        on: bool,
    },
    /// The app committed this photo: frees it from the camera's USB
    /// transfer queue on cameras that keep one and stop shooting when it is
    /// full (never a file on a card); nothing on other cameras.
    Release {
        /// `Captured::connection` of the photo.
        connection: u64,
        /// `Captured::download` of the photo.
        download: u64,
        camera_folder: String,
        camera_name: String,
    },
    /// Ends the helper (it closes the camera first).
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// A camera libgphoto2 found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraInfo {
    pub model: String,
    pub port: String,
}

/// The opened camera.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraStatus {
    pub model: String,
    pub port: String,
    /// Battery as the camera reports it (e.g. "80%"), if it does.
    pub battery: Option<String>,
    /// Where the camera stores a capture (its `capturetarget`), if it says.
    pub capture_target: Option<String>,
    /// Whether the camera can fire its shutter on request.
    pub can_capture: bool,
    /// Whether it sends live view.
    pub can_preview: bool,
    /// The camera keeps every tethered photo until the computer frees it
    /// (`Release`) and stops shooting when that queue is full. Whether it
    /// also writes a card copy is the camera's setting: a Fujifilm GFX 50R
    /// in USB tether mode writes none, so the app frees a photo only after
    /// its verified commit, and the user can turn freeing off.
    pub transfer_queue: bool,
    /// The body's serial number, when it reports one: a reconnect takes
    /// only the same body (two of one model may be attached).
    pub serial: Option<String>,
    /// The body's firmware version, when it reports one.
    pub firmware: Option<String>,
    /// Driven on a path no body has been tested on yet (libgphoto2's Canon
    /// EOS path; the user's opt-in): a photo's report can be lost there, so
    /// every photo is kept on the memory card and nothing is freed.
    pub experimental: bool,
}

/// A camera setting the interface shows (`key` is ours; `camera_key` the
/// camera driver's name for it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Setting {
    pub key: String,
    pub camera_key: String,
    pub label: String,
    pub value: String,
    pub choices: Vec<String>,
    /// Set on the camera body (a dial, a mode) and not changeable here.
    pub read_only: bool,
}

/// A photo the helper downloaded into staging.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Captured {
    /// The complete download (the camera's own file name).
    pub path: PathBuf,
    /// The helper's number for the connection the photo came from (named
    /// again in `Release`).
    pub connection: u64,
    /// Its number among that connection's downloads (named again in
    /// `Release`: a camera file name may stand for a newer photo by then).
    pub download: u64,
    /// Its exposure's number in that connection: the RAW and JPEG of one
    /// exposure share it.
    pub exposure: u64,
    /// Which exposure it belongs to cannot be told (a reused file name;
    /// `server` explains): the exposures it may belong to, besides being a
    /// new one; empty when certain.
    pub ambiguous: Vec<u64>,
    /// Where it was on the camera.
    pub camera_folder: String,
    pub camera_name: String,
    pub size: u64,
    /// Whether the shot was requested by the app (else fired on the body).
    pub requested: bool,
    /// The app request it answers (`Reply::Fired`), if any.
    pub request: Option<u64>,
    /// Milliseconds on the helper's clock since the capture request (app
    /// shots) or the camera's event (body shots): the camera reported the
    /// file, and the download was complete.
    pub ms_to_event: Option<u64>,
    pub ms_to_downloaded: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// No camera on USB (or not the one asked for).
    NotFound,
    /// Another program holds the camera (macOS Image Capture, Capture One…).
    Busy,
    /// The camera does not offer this (e.g. remote capture, live view).
    Unsupported,
    /// Set on the body and not changeable remotely.
    ReadOnly,
    /// The value is not one the camera takes.
    BadValue,
    /// The camera went away (unplugged, switched off, slept).
    Disconnected,
    /// No camera is open.
    NotConnected,
    /// The camera did not answer in time.
    Timeout,
    /// The helper did not answer in time (it hangs in the camera library);
    /// the app ends it.
    Unresponsive,
    /// Writing the download failed (disk full, permissions).
    Storage,
    /// The helper and the app do not match, or a message was malformed.
    Protocol,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TetherError {
    pub kind: ErrorKind,
    pub message: String,
}

impl TetherError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for TetherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TetherError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Reply {
    Ok,
    Hello {
        protocol: u32,
        /// libgphoto2's version, or `None` for the mock camera.
        library: Option<String>,
    },
    Cameras {
        cameras: Vec<CameraInfo>,
    },
    Connected {
        status: CameraStatus,
    },
    Settings {
        settings: Vec<Setting>,
    },
    /// The answer to `Capture`: the shutter fired; this request's number
    /// (`Captured::request`, `Event::CaptureFailed::request`).
    Fired {
        request: u64,
    },
    /// The answer to `SetSetting`: the setting's value as the camera reports
    /// it afterwards (`None`: it could not be read). It is the value asked
    /// for unless the camera kept or chose another one.
    SettingSet {
        value: Option<String>,
    },
    /// The answer to `Release`: whether the photo was freed from the
    /// camera's transfer queue (false: the camera keeps none, or the photo
    /// is not the one to free any more).
    Released {
        freed: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    Captured {
        captured: Captured,
    },
    /// A shot was asked for or reported but did not arrive.
    CaptureFailed {
        error: TetherError,
        /// The app request it was for, when known.
        request: Option<u64>,
        /// The connection and exposure (`Captured`) of a file that could
        /// not be downloaded or saved; none for a shot that never came.
        exposure: Option<(u64, u64)>,
        /// The exposures it may belong to (`Captured::ambiguous`).
        ambiguous: Vec<u64>,
    },
    /// The camera reported changed settings (a dial turned): read them again.
    SettingsChanged,
    /// The camera is gone; it has to be connected again.
    Disconnected {
        error: TetherError,
    },
    /// Live view stopped on its own (the camera refused a frame).
    LiveViewStopped {
        error: TetherError,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "message",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Message {
    Reply {
        id: u64,
        result: Result<Reply, TetherError>,
    },
    Event {
        event: Event,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_status_requires_the_current_protocol_fields() {
        let status = CameraStatus::default();
        let mut value = serde_json::to_value(&status).unwrap();
        assert_eq!(
            serde_json::from_value::<CameraStatus>(value.clone()).unwrap(),
            status
        );
        value.as_object_mut().unwrap().remove("transferQueue");
        assert!(serde_json::from_value::<CameraStatus>(value).is_err());
    }

    #[test]
    fn requests_and_messages_survive_json() {
        let request = Envelope {
            id: 7,
            request: Request::Connect {
                port: Some("usb:020,005".into()),
                staging: "/s/.tether".into(),
                serial: Some("CA7F603B7405".into()),
                experimental: false,
            },
        };
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Envelope>(&text).unwrap(), request);
        let reply = Message::Reply {
            id: 7,
            result: Err(TetherError::new(ErrorKind::Busy, "in use")),
        };
        let text = serde_json::to_string(&reply).unwrap();
        assert_eq!(serde_json::from_str::<Message>(&text).unwrap(), reply);
    }
}
