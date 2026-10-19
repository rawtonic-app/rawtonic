//! The camera driver on libgphoto2 (LGPL-2.1-or-later, linked dynamically
//! into the helper only). Which operations a camera offers comes from the
//! result codes of the calls — never from a table of body names, including
//! libgphoto2's own model table, whose flags lag behind bodies its driver
//! controls (e.g. a body listed without capture that captures). Nothing on the camera is changed except the
//! settings the user sets; nothing on it is deleted.
use crate::camera::{Camera, CameraEvent, CameraFile, Driver, Fired, SETTINGS, setting_aliases};
use crate::protocol::{CameraInfo, CameraStatus, ErrorKind, Setting, TetherError};
use gphoto2::error::ErrorKind as Gp;
use gphoto2::widget::{WidgetType, WidgetValue};
use std::collections::HashMap;
use std::time::Duration;

/// A libgphoto2 error as the app shows it.
fn error(context: &str, e: gphoto2::Error) -> TetherError {
    let kind = match e.kind() {
        Gp::UsbClaim | Gp::CameraBusy | Gp::Lock => ErrorKind::Busy,
        Gp::NotSupported => ErrorKind::Unsupported,
        Gp::UsbFind | Gp::ModelNotFound | Gp::UnknownPort => ErrorKind::NotFound,
        Gp::Io => ErrorKind::Disconnected,
        Gp::Timeout => ErrorKind::Timeout,
        Gp::BadParameters => ErrorKind::BadValue,
        Gp::NoSpace | Gp::NoMemory => ErrorKind::Storage,
        Gp::Other(-34 | -35 | -37 | -31) => ErrorKind::Disconnected, // GP_ERROR_IO_READ/WRITE/UPDATE/INIT
        _ => ErrorKind::Other,
    };
    let hint = match kind {
        ErrorKind::Busy => {
            " Another program is using the camera (Image Capture, Photos, Capture One, a camera \
             utility): quit it, then connect again."
        }
        ErrorKind::Disconnected => " The camera was disconnected or switched off.",
        _ => "",
    };
    TetherError::new(kind, format!("{context}: {e}.{hint}"))
}

/// An error from an open camera. "Device not found on the USB port" there
/// means it was unplugged or switched off: the connection is gone, so the
/// helper drops it and says so (the app connects again).
fn session_error(context: &str, e: gphoto2::Error) -> TetherError {
    if e.kind() == Gp::UsbFind {
        return TetherError::new(
            ErrorKind::Disconnected,
            format!("{context}: {e}. The camera was disconnected or switched off."),
        );
    }
    // Busy on a camera this helper holds: the camera itself is busy (still
    // saving, or showing the last photo), not another program.
    if e.kind() == Gp::CameraBusy {
        return TetherError::new(
            ErrorKind::Busy,
            format!(
                "{context}: the camera is busy ({e}). It may still be saving or showing the last \
                 photo (image review set to continuous): half-press its shutter release or wait, \
                 then try again."
            ),
        );
    }
    error(context, e)
}

/// A request the camera refused while it can otherwise shoot (AF, live
/// view): one that keeps showing the last photo after a shot refuses them
/// until the photo is dismissed (seen on a Fujifilm X-H2S with image review
/// set to continuous: the camera answers 0xa001, libgphoto2 reports -1).
fn refused(context: &str, e: gphoto2::Error) -> TetherError {
    if e.kind() != Gp::Generic {
        return session_error(context, e);
    }
    TetherError::new(
        ErrorKind::Other,
        format!(
            "{context}: the camera refused it ({e}). If it keeps showing the last photo after \
             each shot (image review set to continuous), half-press its shutter release, or set \
             image review off or to a short time."
        ),
    )
}

/// Whether libgphoto2's summary shows a Fujifilm camera in USB tether mode
/// (vendor extension 0xe with the CurrentState property 0xd212 — what
/// libgphoto2 itself tests for its Fujifilm tether event handling). Such a
/// camera lists each tethered photo as an object in its transfer queue
/// until the computer deletes that object, and refuses to fire when the
/// queue is full (an X-H2S after 37 photos). Deleting the object frees the
/// queue entry; a card copy stays (X-H2S, checked on the body), but a body
/// may write none (GFX 50R in USB tether mode): then the library copy is the
/// only one, so the app frees a photo only after its verified commit (on
/// unless the user turned it off, as in Capture One).
/// notes: reference/tether.md
fn has_transfer_queue(summary: &str) -> bool {
    summary.contains(": 0xe (") && summary.contains("(d212")
}

/// The serial number in libgphoto2's summary ("Serial Number: …").
fn serial_number(summary: &str) -> Option<String> {
    summary
        .lines()
        .find_map(|line| line.trim().strip_prefix("Serial Number:"))
        .map(|serial| serial.trim().to_string())
        .filter(|serial| !serial.is_empty())
}

pub struct GphotoDriver {
    context: gphoto2::Context,
}

impl GphotoDriver {
    pub fn new() -> Result<Self, TetherError> {
        let mut context = gphoto2::Context::new().map_err(|e| error("libgphoto2", e))?;
        context.set_error_handler(|text| log::warn!("libgphoto2: {text}"));
        context.set_message_handler(|text| log::info!("libgphoto2: {text}"));
        Ok(Self { context })
    }
}

/// libgphoto2's version (first line of its verbose version).
pub fn library_version() -> String {
    gphoto2::version::library_version()
        .first()
        .cloned()
        .unwrap_or_default()
}

/// The camera models the loaded camera drivers know (0 when libgphoto2
/// found none of its drivers: a packaging fault).
pub fn known_models() -> Result<usize, TetherError> {
    let context = gphoto2::Context::new().map_err(|e| error("libgphoto2", e))?;
    gphoto2::supported_models(&context)
        .map(|m| m.len())
        .map_err(|e| error("Loading the camera drivers", e))
}

impl Driver for GphotoDriver {
    fn library(&self) -> Option<String> {
        Some(library_version())
    }

    fn list(&mut self) -> Result<Vec<CameraInfo>, TetherError> {
        Ok(gphoto2::Camera::autodetect(&self.context)
            .map_err(|e| error("Looking for cameras", e))?
            .into_iter()
            .map(|c| CameraInfo {
                model: c.model,
                port: c.port,
            })
            .collect())
    }

    fn open(&mut self, port: Option<&str>) -> Result<Box<dyn Camera>, TetherError> {
        let cameras = self.list()?;
        let info = cameras
            .into_iter()
            .find(|c| port.is_none_or(|p| p == c.port))
            .ok_or_else(|| {
                TetherError::new(
                    ErrorKind::NotFound,
                    "No camera found on USB. Check the cable, switch the camera on and set its USB \
                     mode to remote/tether shooting (Sony: PC Remote; Fujifilm: USB tether).",
                )
            })?;
        // A fresh context per camera: its handlers log for this session.
        let mut context = gphoto2::Context::new().map_err(|e| error("libgphoto2", e))?;
        context.set_error_handler(|text| log::warn!("libgphoto2: {text}"));
        let camera = gphoto2::Camera::open(&context, &info.model, &info.port)
            .map_err(|e| error(&format!("Opening {}", info.model), e))?;
        let operations = camera
            .abilities()
            .map(|a| a.camera_operations())
            .map_err(|e| error("Reading the camera's abilities", e))?;
        // Logged only: the driver is tried, its "not supported" turns an
        // operation off.
        log::info!(
            "opened {} on {} (model table: capture {}, trigger {}, preview {}, config {})",
            info.model,
            info.port,
            operations.capture_image,
            operations.trigger_capture,
            operations.capture_preview,
            operations.config
        );
        let summary = camera.summary(&context).unwrap_or_default();
        let transfer_queue = has_transfer_queue(&summary);
        let canon = summary.contains(": 0xb (");
        let serial = serial_number(&summary);
        if transfer_queue {
            log::info!(
                "{} keeps a USB transfer queue: committed photos are freed from it",
                info.model
            );
        }
        Ok(Box::new(GphotoCamera {
            context,
            camera,
            info,
            transfer_queue,
            serial,
            canon,
            trigger: true,
            capture: true,
            preview: true,
            names: HashMap::new(),
        }))
    }
}

pub struct GphotoCamera {
    context: gphoto2::Context,
    camera: gphoto2::Camera,
    info: CameraInfo,
    /// What the camera's driver offers: each turns off when the driver
    /// answers "not supported" for it.
    trigger: bool,
    capture: bool,
    preview: bool,
    /// The camera keeps every tethered photo in a USB transfer queue until
    /// the computer frees it, and stops shooting when the queue is full.
    transfer_queue: bool,
    serial: Option<String>,
    /// libgphoto2's Canon path: its "Save to" shows the preference, and it
    /// captures to the camera's memory when no card is in (config.c,
    /// camera_canon_eos_update_capture_target).
    canon: bool,
    /// Our setting key → the camera's name for it, once found.
    names: HashMap<&'static str, String>,
}

fn text(value: WidgetValue) -> Option<String> {
    match value {
        WidgetValue::Text(s) | WidgetValue::Choice(s) => Some(s),
        WidgetValue::Range(f) => Some(format!("{f}")),
        WidgetValue::Toggle(on) => Some(if on { "on" } else { "off" }.into()),
        _ => None,
    }
}

impl GphotoCamera {
    /// Gives the computer priority over the body's controls, when the camera
    /// has a priority mode that is on the body ("Camera"). Whether it did.
    fn take_priority(&self) -> bool {
        let Some(widget) = self.widget("prioritymode") else {
            return false;
        };
        let (Ok(WidgetValue::Choice(current)), Ok(choices)) = (widget.value(), widget.choices())
        else {
            return false;
        };
        if current != "Camera" {
            return false;
        }
        let Some(computer) = choices.into_iter().find(|c| c != "Camera") else {
            return false;
        };
        let set = widget.set_choice(&computer).and_then(|()| {
            self.camera
                .set_single_config(&self.context, "prioritymode", &widget)
        });
        match set {
            Ok(()) => {
                log::info!("priority mode set to {computer}");
                true
            }
            Err(e) => {
                log::warn!("priority mode not set to {computer}: {e}");
                false
            }
        }
    }

    fn widget(&self, name: &str) -> Option<gphoto2::widget::Widget> {
        self.camera.get_single_config(&self.context, name).ok()
    }

    /// The camera's widget for one of our settings (its first alias the
    /// camera has).
    fn find(
        &mut self,
        key: &'static str,
        aliases: &[&str],
    ) -> Option<(String, gphoto2::widget::Widget)> {
        if let Some(name) = self.names.get(key).cloned() {
            return self.widget(&name).map(|w| (name, w));
        }
        for alias in aliases {
            if let Some(widget) = self.widget(alias) {
                self.names.insert(key, alias.to_string());
                return Some((alias.to_string(), widget));
            }
        }
        None
    }

    fn single_text(&self, names: &[&str]) -> Option<String> {
        names
            .iter()
            .find_map(|n| self.widget(n).and_then(|w| w.value().ok()).and_then(text))
    }
}

impl Camera for GphotoCamera {
    fn status(&mut self) -> CameraStatus {
        CameraStatus {
            model: self.info.model.clone(),
            port: self.info.port.clone(),
            battery: self.single_text(&["batterylevel"]),
            capture_target: self.single_text(&["capturetarget"]),
            can_capture: self.capture || self.trigger,
            can_preview: self.preview,
            transfer_queue: self.transfer_queue,
            serial: self.serial.clone(),
        }
    }

    fn settings(&mut self) -> Result<Vec<Setting>, TetherError> {
        let mut settings = Vec::new();
        for (key, label, aliases) in SETTINGS {
            let Some((camera_key, widget)) = self.find(key, aliases) else {
                continue;
            };
            let Some(value) = widget.value().ok().and_then(text) else {
                continue;
            };
            let choices = match widget.widget_type() {
                Ok(WidgetType::Radio | WidgetType::Menu) => widget.choices().unwrap_or_default(),
                _ => vec![],
            };
            settings.push(Setting {
                key: key.to_string(),
                camera_key,
                label: label.to_string(),
                value,
                choices,
                read_only: widget.readonly().unwrap_or(false),
            });
        }
        // Where captures go is the camera's choice; shown, settable.
        if let Some(widget) = self.widget("capturetarget")
            && let Some(value) = widget.value().ok().and_then(text)
        {
            settings.push(Setting {
                key: "capturetarget".into(),
                camera_key: "capturetarget".into(),
                label: "Save to".into(),
                value,
                choices: widget.choices().unwrap_or_default(),
                read_only: widget.readonly().unwrap_or(false),
            });
        }
        if settings.is_empty() {
            // A camera without readable settings still answers this.
            self.camera
                .summary(&self.context)
                .map_err(|e| session_error("Reading the camera's settings", e))?;
        }
        Ok(settings)
    }

    fn set_setting(&mut self, key: &str, value: &str) -> Result<(), TetherError> {
        let (name, widget) = if key == "capturetarget" {
            let widget = self.widget(key).ok_or_else(|| {
                TetherError::new(ErrorKind::Unsupported, "The camera has no capture target.")
            })?;
            (key.to_string(), widget)
        } else {
            let (key, aliases) = SETTINGS
                .iter()
                .find(|(k, _, _)| *k == key)
                .map(|(k, _, _)| (*k, setting_aliases(k).unwrap().1))
                .ok_or_else(|| {
                    TetherError::new(ErrorKind::Unsupported, format!("No setting {key}."))
                })?;
            self.find(key, aliases).ok_or_else(|| {
                TetherError::new(
                    ErrorKind::Unsupported,
                    format!("This camera does not offer {key} over USB."),
                )
            })?
        };
        if widget.readonly().unwrap_or(false) {
            return Err(TetherError::new(
                ErrorKind::ReadOnly,
                format!("{name} is set on the camera body (a dial or the camera's mode)."),
            ));
        }
        let set = match widget.widget_type() {
            Ok(WidgetType::Radio | WidgetType::Menu) => widget.set_choice(value),
            Ok(WidgetType::Text) => widget.set_text(value),
            Ok(WidgetType::Range) => match value.trim_end_matches('K').trim().parse::<f32>() {
                Ok(v) => widget.set_range(v),
                Err(_) => {
                    return Err(TetherError::new(
                        ErrorKind::BadValue,
                        format!("{value} is not a number."),
                    ));
                }
            },
            Ok(WidgetType::Toggle) => widget.set_toggle(matches!(value, "on" | "1" | "true")),
            _ => {
                return Err(TetherError::new(
                    ErrorKind::Unsupported,
                    format!("{name} cannot be set here."),
                ));
            }
        };
        set.map_err(|e| session_error(&format!("{name} = {value}"), e))?;
        self.camera
            .set_single_config(&self.context, &name, &widget)
            .map_err(|e| session_error(&format!("Setting {name} to {value}"), e))
    }

    fn fire(&mut self) -> Result<Fired, TetherError> {
        if self.trigger {
            // Refused as busy while the body has priority: a camera whose
            // controls go to the body between shots (libgphoto2's Fujifilm
            // event wait hands priority back each time) fires once the
            // computer has it. Nothing was fired, so trying again is no
            // second exposure (the helper also retries after reading the
            // camera's events).
            let mut fired = self.camera.trigger_capture(&self.context);
            if fired.as_ref().is_err_and(|e| e.kind() == Gp::CameraBusy) && self.take_priority() {
                fired = self.camera.trigger_capture(&self.context);
            }
            match fired {
                Ok(()) => return Ok(Fired::Pending),
                // Chosen by the result code: this driver needs the blocking
                // capture. Nothing was fired, so firing that way is no second
                // exposure.
                Err(e) if e.kind() == Gp::NotSupported => {
                    log::info!("trigger capture not supported; using capture");
                    self.trigger = false;
                }
                Err(e) if self.transfer_queue && e.kind() == Gp::Generic => {
                    return Err(TetherError::new(
                        ErrorKind::Other,
                        format!(
                            "Firing the shutter: the camera refused it ({e}). This camera keeps \
                             every tethered photo until the computer frees it and stops \
                             shooting when it holds too many: turn on \"Free photos on the \
                             camera once they are in the library\" or reconnect the camera."
                        ),
                    ));
                }
                Err(e) => return Err(session_error("Firing the shutter", e)),
            }
        }
        if !self.capture {
            return Err(TetherError::new(
                ErrorKind::Unsupported,
                "This camera cannot be fired from the computer over USB.",
            ));
        }
        let path = match self.camera.capture_image(&self.context) {
            Ok(path) => path,
            Err(e) if e.kind() == Gp::NotSupported => {
                self.capture = false;
                return Err(TetherError::new(
                    ErrorKind::Unsupported,
                    "This camera cannot be fired from the computer over USB.",
                ));
            }
            Err(e) => return Err(session_error("Capturing", e)),
        };
        Ok(Fired::File(CameraFile {
            folder: path.folder,
            name: path.name,
        }))
    }

    fn autofocus(&mut self) -> Result<(), TetherError> {
        for name in ["autofocusdrive", "autofocus"] {
            let Some(widget) = self.widget(name) else {
                continue;
            };
            if !matches!(widget.widget_type(), Ok(WidgetType::Toggle)) {
                continue;
            }
            widget
                .set_toggle(true)
                .and_then(|()| self.camera.set_single_config(&self.context, name, &widget))
                .map_err(|e| refused("Focusing", e))?;
            if name == "autofocus" {
                // A half-press: held, then released.
                std::thread::sleep(Duration::from_millis(300));
                widget
                    .set_toggle(false)
                    .and_then(|()| self.camera.set_single_config(&self.context, name, &widget))
                    .map_err(|e| session_error("Focusing", e))?;
            }
            return Ok(());
        }
        Err(TetherError::new(
            ErrorKind::Unsupported,
            "This camera cannot be focused from the computer over USB.",
        ))
    }

    fn preview(&mut self) -> Result<Vec<u8>, TetherError> {
        let file = match self.camera.capture_preview(&self.context) {
            Ok(file) => file,
            Err(e) if e.kind() == Gp::NotSupported => {
                self.preview = false;
                return Err(TetherError::new(
                    ErrorKind::Unsupported,
                    "This camera has no live view over USB.",
                ));
            }
            Err(e) => return Err(refused("Live view", e)),
        };
        file.data()
            .map(<[u8]>::to_vec)
            .map_err(|e| session_error("Live view", e))
    }

    fn memory_only(&mut self) -> bool {
        if crate::camera::memory_only(self.single_text(&["capturetarget"]).as_deref()) {
            return true;
        }
        // A card copy needs a card: on the Canon path "Memory card" alone
        // does not prove one is in.
        self.canon
            && !self
                .camera
                .storage_info(&self.context)
                .is_ok_and(|storages| {
                    storages
                        .iter()
                        .any(|s| s.capacity_kb.is_some_and(|kb| kb > 0))
                })
    }

    fn release(&mut self, file: &CameraFile) -> Result<(), TetherError> {
        if !self.transfer_queue {
            return Ok(());
        }
        self.camera
            .delete_file(&self.context, &file.folder, &file.name)
            .map_err(|e| session_error(&format!("Freeing {} on the camera", file.name), e))
    }

    fn wait_event(&mut self, timeout: Duration) -> Result<CameraEvent, TetherError> {
        let event = self
            .camera
            .wait_for_event(&self.context, timeout)
            .map_err(|e| session_error("Waiting for the camera", e))?;
        Ok(match event {
            gphoto2::CameraEvent::Timeout => CameraEvent::Timeout,
            gphoto2::CameraEvent::FileAdded(path) => CameraEvent::FileAdded(CameraFile {
                folder: path.folder,
                name: path.name,
            }),
            gphoto2::CameraEvent::CaptureComplete => CameraEvent::CaptureComplete,
            gphoto2::CameraEvent::Unknown(text) => {
                // PTP property changes (a dial turned) come as text.
                let lower = text.to_lowercase();
                if lower.contains("property") && lower.contains("changed") {
                    CameraEvent::SettingsChanged
                } else {
                    if !text.is_empty() {
                        log::debug!("camera event: {text}");
                    }
                    CameraEvent::Other
                }
            }
            _ => CameraEvent::Other,
        })
    }

    fn download(&mut self, file: &CameraFile) -> Result<(Vec<u8>, Option<u64>), TetherError> {
        let path = gphoto2::camera::CameraFilePath {
            folder: file.folder.clone(),
            name: file.name.clone(),
        };
        let size = self
            .camera
            .file_info(&self.context, &file.folder, &file.name)
            .ok()
            .and_then(|info| info.size);
        let data = self
            .camera
            .download(&self.context, &path)
            .map_err(|e| session_error(&format!("Downloading {}", file.name), e))?;
        let bytes = data
            .data()
            .map(<[u8]>::to_vec)
            .map_err(|e| session_error(&format!("Downloading {}", file.name), e))?;
        Ok((bytes, size))
    }
}
