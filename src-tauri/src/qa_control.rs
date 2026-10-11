//! Loopback control channel for automated QA (feature `qa-control`).
//!
//! Started only when `RAWTONIC_QA_PORT` is set, and bound to 127.0.0.1. One
//! JSON object per line in each direction:
//! `{"id":1,"method":"window.info","params":{}}` →
//! `{"id":1,"result":{…}}` or `{"id":1,"error":"…"}`.
//!
//! Native methods: `ping`, `window.info`, and on macOS `window.capture`,
//! `input.mouse`, `input.click` and `input.drag`. The input methods post real
//! `NSEvent`s into the app's event queue, so WebKit delivers them like
//! hardware input, with their own time stamps. They do not cover the HID and window-server
//! path before the app, so they are not pointer-to-photon. Every other method
//! (`ui.*`, `state.*`) is forwarded to the webview bridge in `src/qa/`.
//!
//! Interface methods change only what the user could change by hand; the
//! channel never writes files itself (captures are returned as base64).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Listener, Manager};

const PORT_ENV: &str = "RAWTONIC_QA_PORT";
const REQUEST_EVENT: &str = "qa-control-request";
const REPLY_EVENT: &str = "qa-control-reply";
const WEBVIEW_TIMEOUT: Duration = Duration::from_secs(30);

type Pending = Mutex<HashMap<u64, SyncSender<Value>>>;

fn pending() -> &'static Pending {
    static PENDING: OnceLock<Pending> = OnceLock::new();
    PENDING.get_or_init(Default::default)
}

/// Starts the server if `RAWTONIC_QA_PORT` names a port.
pub(crate) fn start(app: &AppHandle) {
    let Ok(port) = std::env::var(PORT_ENV) else {
        return;
    };
    let Ok(port) = port.parse::<u16>() else {
        log::error!("[QA] {PORT_ENV}={port} is not a port");
        return;
    };
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            log::error!("[QA] cannot listen on 127.0.0.1:{port}: {error}");
            return;
        }
    };
    app.listen(REPLY_EVENT, |event| {
        let Ok(reply) = serde_json::from_str::<Value>(event.payload()) else {
            return;
        };
        let Some(id) = reply.get("id").and_then(Value::as_u64) else {
            return;
        };
        if let Some(sender) = pending().lock().unwrap().remove(&id) {
            let _ = sender.try_send(reply);
        }
    });
    log::info!("[QA] control channel on 127.0.0.1:{port}");
    let app = app.clone();
    std::thread::Builder::new()
        .name("qa-control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let app = app.clone();
                std::thread::spawn(move || serve(&app, stream));
            }
        })
        .ok();
}

fn serve(app: &AppHandle, stream: TcpStream) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                let method = request.get("method").and_then(Value::as_str).unwrap_or("");
                let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
                match handle(app, method, params) {
                    Ok(result) => json!({ "id": id, "result": result }),
                    Err(error) => json!({ "id": id, "error": error }),
                }
            }
            Err(error) => json!({ "id": null, "error": format!("invalid JSON: {error}") }),
        };
        if writeln!(writer, "{reply}")
            .and_then(|()| writer.flush())
            .is_err()
        {
            return;
        }
    }
}

fn handle(app: &AppHandle, method: &str, params: Value) -> Result<Value, String> {
    match method {
        "ping" => Ok(json!({ "pong": true, "version": env!("CARGO_PKG_VERSION") })),
        "window.info" => on_main(app, window_info),
        "input.mouse" => {
            let event = MouseEvent::from_params(&params)?;
            let activate = activate_requested(&params);
            on_main(app, move |app| {
                if activate {
                    native::activate(app)?;
                }
                native::post_mouse(app, &event)
            })
        }
        "window.capture" => {
            let best = params.get("resolution").and_then(Value::as_str) == Some("best");
            // "p3": pixels in Display P3, so colours beyond sRGB survive.
            let p3 = params.get("color_space").and_then(Value::as_str) == Some("p3");
            on_main(app, move |app| {
                let window = app.get_webview_window("main").ok_or("no main window")?;
                native::capture(&window, best, p3)
            })
        }
        "input.click" => click(app, &params),
        "input.drag" => drag(app, &params),
        // What the app does when the system reports memory pressure (the
        // kernel's notification cannot be raised without root).
        "ai.memory_pressure" => Ok(json!({
            "unloaded": crate::ai_lifecycle::unload_and_notify(
                app,
                crate::ai_lifecycle::UnloadReason::MemoryPressure,
            ),
        })),
        _ => forward(app, method, params),
    }
}

/// `"activate": true` brings the app and window to the front first. In an
/// inactive window AppKit uses the first press only to activate it, as with a
/// real click, so a single click or drag would otherwise be lost. Off by
/// default so that the channel does not take focus from someone at the Mac.
fn activate_requested(params: &Value) -> bool {
    params
        .get("activate")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Runs `f` on the main thread and waits for its result.
fn on_main<F>(app: &AppHandle, f: F) -> Result<Value, String>
where
    F: FnOnce(&AppHandle) -> Result<Value, String> + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = sender.send(f(&handle));
    })
    .map_err(|error| error.to_string())?;
    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "main thread did not answer within 5 s".to_string())?
}

fn window_info(app: &AppHandle) -> Result<Value, String> {
    let window = app.get_webview_window("main").ok_or("no main window")?;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let position = window.inner_position().map_err(|e| e.to_string())?;
    let size = window.inner_size().map_err(|e| e.to_string())?;
    let position = position.to_logical::<f64>(scale);
    let size = size.to_logical::<f64>(scale);
    Ok(json!({
        "x": position.x,
        "y": position.y,
        "width": size.width,
        "height": size.height,
        "scale": scale,
        "focused": window.is_focused().unwrap_or(false),
        "window_number": native::window_number(&window),
        "content_size": native::content_size(&window),
        "native_input": cfg!(target_os = "macos"),
        "color": native::color_info(&window),
    }))
}

/// A mouse event at webview coordinates (CSS pixels from the top left).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MouseEvent {
    pub kind: MouseKind,
    pub x: f64,
    pub y: f64,
    pub clicks: i64,
    pub modifiers: Modifiers,
    /// The secondary button (a right-click opens context menus).
    pub right: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MouseKind {
    Move,
    Down,
    Drag,
    Up,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub option: bool,
    pub command: bool,
}

impl Modifiers {
    fn from_params(params: &Value) -> Result<Self, String> {
        let mut modifiers = Self::default();
        let Some(list) = params.get("modifiers") else {
            return Ok(modifiers);
        };
        for name in list.as_array().ok_or("modifiers must be an array")? {
            match name.as_str() {
                Some("shift") => modifiers.shift = true,
                Some("control") => modifiers.control = true,
                Some("option" | "alt") => modifiers.option = true,
                Some("command" | "meta") => modifiers.command = true,
                other => return Err(format!("unknown modifier {other:?}")),
            }
        }
        Ok(modifiers)
    }
}

/// `"button": "right"` for the secondary button; left by default.
fn right_button(params: &Value) -> Result<bool, String> {
    match params.get("button").and_then(Value::as_str) {
        None | Some("left") => Ok(false),
        Some("right") => Ok(true),
        other => Err(format!("button must be left or right, not {other:?}")),
    }
}

fn coordinate(params: &Value, key: &str) -> Result<f64, String> {
    params
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| format!("missing number {key}"))
}

impl MouseEvent {
    fn from_params(params: &Value) -> Result<Self, String> {
        let kind = match params.get("type").and_then(Value::as_str) {
            Some("move") => MouseKind::Move,
            Some("down") => MouseKind::Down,
            Some("drag") => MouseKind::Drag,
            Some("up") => MouseKind::Up,
            other => {
                return Err(format!(
                    "type must be move, down, drag or up, not {other:?}"
                ));
            }
        };
        Ok(Self {
            kind,
            x: coordinate(params, "x")?,
            y: coordinate(params, "y")?,
            clicks: params.get("clicks").and_then(Value::as_i64).unwrap_or(1),
            modifiers: Modifiers::from_params(params)?,
            right: right_button(params)?,
        })
    }
}

/// Press and release at one point (`count` 2 for a double click).
fn click(app: &AppHandle, params: &Value) -> Result<Value, String> {
    let (x, y) = (coordinate(params, "x")?, coordinate(params, "y")?);
    let modifiers = Modifiers::from_params(params)?;
    let right = right_button(params)?;
    let count = params
        .get("count")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .clamp(1, 3);
    let event = |kind, clicks| MouseEvent {
        kind,
        x,
        y,
        clicks,
        modifiers,
        right,
    };
    let mut events = vec![event(MouseKind::Move, 0)];
    for clicks in 1..=count {
        events.push(event(MouseKind::Down, clicks));
        events.push(event(MouseKind::Up, clicks));
    }
    if activate_requested(params) {
        on_main(app, native::activate)?;
    }
    post_paced(app, events, Duration::from_millis(30))?;
    Ok(json!({ "x": x, "y": y, "count": count }))
}

/// The events of a drag along `points`: hover, press, one drag event per
/// further point, release at the last point.
pub(crate) fn drag_events(points: &[(f64, f64)], modifiers: Modifiers) -> Vec<MouseEvent> {
    let event = |kind, (x, y): (f64, f64), clicks| MouseEvent {
        kind,
        x,
        y,
        clicks,
        modifiers,
        right: false,
    };
    let (Some(&first), Some(&last)) = (points.first(), points.last()) else {
        return Vec::new();
    };
    let mut events = vec![
        event(MouseKind::Move, first, 0),
        event(MouseKind::Down, first, 1),
    ];
    events.extend(
        points[1..]
            .iter()
            .map(|&point| event(MouseKind::Drag, point, 1)),
    );
    events.push(event(MouseKind::Up, last, 1));
    events
}

/// Drag along `points` ([[x, y], …]) with `interval_ms` between events
/// (default 16.7, a 60 Hz mouse).
fn drag(app: &AppHandle, params: &Value) -> Result<Value, String> {
    let points = params
        .get("points")
        .and_then(Value::as_array)
        .ok_or("points must be an array of [x, y]")?
        .iter()
        .map(|point| match point.as_array().map(Vec::as_slice) {
            Some([x, y]) => x.as_f64().zip(y.as_f64()).ok_or("points must be numbers"),
            _ => Err("each point must be [x, y]"),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if points.len() < 2 {
        return Err("a drag needs at least two points".into());
    }
    let interval = params
        .get("interval_ms")
        .and_then(Value::as_f64)
        .unwrap_or(1000.0 / 60.0);
    if !(1.0..=1000.0).contains(&interval) {
        return Err("interval_ms must be between 1 and 1000".into());
    }
    let events = drag_events(&points, Modifiers::from_params(params)?);
    if activate_requested(params) {
        on_main(app, native::activate)?;
    }
    let started = Instant::now();
    post_paced(app, events, Duration::from_secs_f64(interval / 1000.0))?;
    Ok(json!({
        "points": points.len(),
        "interval_ms": interval,
        "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
    }))
}

/// Posts events in order, one per `interval`, against an absolute schedule so
/// that a late wake-up does not stretch the rest of the drag.
fn post_paced(app: &AppHandle, events: Vec<MouseEvent>, interval: Duration) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("native input is macOS-only; use ui.click/ui.drag".into());
    }
    let start = Instant::now();
    for (index, event) in events.into_iter().enumerate() {
        let due = start + interval * index as u32;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let handle = app.clone();
        app.run_on_main_thread(move || {
            if let Err(error) = native::post_mouse(&handle, &event) {
                log::warn!("[QA] mouse event not posted: {error}");
            }
        })
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Sends a request to the webview bridge and waits for its reply.
fn forward(app: &AppHandle, method: &str, params: Value) -> Result<Value, String> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = mpsc::sync_channel(1);
    pending().lock().unwrap().insert(id, sender);
    let request = json!({ "id": id, "method": method, "params": params });
    if let Err(error) = app.emit_to("main", REQUEST_EVENT, request) {
        pending().lock().unwrap().remove(&id);
        return Err(error.to_string());
    }
    let reply = receiver.recv_timeout(WEBVIEW_TIMEOUT);
    pending().lock().unwrap().remove(&id);
    let reply = reply.map_err(|_| {
        format!("no webview reply to {method} (is this a VITE_RAWTONIC_QA=1 frontend?)")
    })?;
    match reply.get("error") {
        Some(error) if !error.is_null() => {
            Err(error.as_str().unwrap_or("webview error").to_string())
        }
        _ => Ok(reply.get("result").cloned().unwrap_or(Value::Null)),
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::{MouseEvent, MouseKind};
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use serde_json::Value;
    use std::ffi::c_void;
    use tauri::{AppHandle, Manager, WebviewWindow};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSPoint {
        x: f64,
        y: f64,
    }

    // NSEventType and NSEventModifierFlags values from AppKit.
    const LEFT_MOUSE_DOWN: u64 = 1;
    const LEFT_MOUSE_UP: u64 = 2;
    const MOUSE_MOVED: u64 = 5;
    const LEFT_MOUSE_DRAGGED: u64 = 6;
    const RIGHT_MOUSE_DOWN: u64 = 3;
    const RIGHT_MOUSE_UP: u64 = 4;
    const RIGHT_MOUSE_DRAGGED: u64 = 7;
    const SHIFT: u64 = 1 << 17;
    const CONTROL: u64 = 1 << 18;
    const OPTION: u64 = 1 << 19;
    const COMMAND: u64 = 1 << 20;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NSRect {
        origin: NSPoint,
        size: NSPoint,
    }

    /// Width and height of the window's content view in points. Main thread
    /// only. Tauri's inner size can disagree with it after the window moves
    /// between screens of different scale, and event locations need this one.
    pub(super) fn content_size(window: &WebviewWindow) -> Option<(f64, f64)> {
        let ns_window = window.ns_window().ok()?.cast::<Object>();
        if ns_window.is_null() {
            return None;
        }
        // SAFETY: main thread; contentView and its frame are plain reads.
        let frame: NSRect = unsafe {
            let view: *mut Object = msg_send![ns_window, contentView];
            if view.is_null() {
                return None;
            }
            msg_send![view, frame]
        };
        Some((frame.size.x, frame.size.y))
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCreateImage(
            bounds: NSRect,
            list: u32,
            window: u32,
            options: u32,
        ) -> *mut c_void;
        fn CGImageGetWidth(image: *mut c_void) -> usize;
        fn CGImageGetHeight(image: *mut c_void) -> usize;
        fn CGImageRelease(image: *mut c_void);
        fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
        fn CGColorSpaceRelease(space: *mut c_void);
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: *mut c_void,
            info: u32,
        ) -> *mut c_void;
        fn CGContextDrawImage(context: *mut c_void, rect: NSRect, image: *mut c_void);
        fn CGContextRelease(context: *mut c_void);
        static kCGColorSpaceSRGB: *const c_void;
        static kCGColorSpaceDisplayP3: *const c_void;
    }

    // CGWindowListOption / CGWindowImageOption / CGBitmapInfo values.
    const INCLUDING_WINDOW: u32 = 1 << 3;
    const IGNORE_FRAMING: u32 = 1 << 0;
    const BEST_RESOLUTION: u32 = 1 << 3;
    const NOMINAL_RESOLUTION: u32 = 1 << 4;
    const PREMULTIPLIED_LAST: u32 = 1;

    /// PNG (base64) of the main window as the window server composites it,
    /// so the native GPU layer under the transparent webview is included.
    /// A process may capture its own windows without Screen Recording
    /// permission. Main thread only.
    pub(super) fn capture(window: &WebviewWindow, best: bool, p3: bool) -> Result<Value, String> {
        use base64::Engine as _;
        let number = window_number(window).as_i64().ok_or("no window number")?;
        let null_rect = NSRect {
            origin: NSPoint {
                x: f64::INFINITY,
                y: f64::INFINITY,
            },
            size: NSPoint { x: 0.0, y: 0.0 },
        };
        let options = IGNORE_FRAMING
            | if best {
                BEST_RESOLUTION
            } else {
                NOMINAL_RESOLUTION
            };
        // SAFETY: CoreGraphics calls with owned references released below;
        // the bitmap context draws into `pixels`, which outlives it.
        let (width, height, mut pixels) = unsafe {
            let image =
                CGWindowListCreateImage(null_rect, INCLUDING_WINDOW, number as u32, options);
            if image.is_null() {
                return Err("the window server returned no image".into());
            }
            let (width, height) = (CGImageGetWidth(image), CGImageGetHeight(image));
            let mut pixels = vec![0u8; width * height * 4];
            let space = CGColorSpaceCreateWithName(if p3 {
                kCGColorSpaceDisplayP3
            } else {
                kCGColorSpaceSRGB
            });
            let context = CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * 4,
                space,
                PREMULTIPLIED_LAST,
            );
            if !context.is_null() {
                let rect = NSRect {
                    origin: NSPoint { x: 0.0, y: 0.0 },
                    size: NSPoint {
                        x: width as f64,
                        y: height as f64,
                    },
                };
                CGContextDrawImage(context, rect, image);
                CGContextRelease(context);
            }
            CGColorSpaceRelease(space);
            CGImageRelease(image);
            if context.is_null() {
                return Err("could not create a bitmap context".into());
            }
            (width, height, pixels)
        };
        // Undo premultiplication so the PNG shows what the window shows.
        for pixel in pixels.as_chunks_mut::<4>().0 {
            let alpha = u32::from(pixel[3]);
            if alpha != 0 && alpha != 255 {
                for channel in &mut pixel[..3] {
                    *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
        let image = image::RgbaImage::from_raw(width as u32, height as u32, pixels)
            .ok_or("bitmap size mismatch")?;
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok(serde_json::json!({
            "width": width,
            "height": height,
            "png_base64": base64::engine::general_purpose::STANDARD.encode(png),
        }))
    }

    /// NSString or CFString (toll-free bridged) as UTF-8.
    unsafe fn ns_string(string: *mut Object) -> Option<String> {
        if string.is_null() {
            return None;
        }
        // SAFETY: the caller passes a live NSString; UTF8String lives as long as it.
        let utf8: *const std::ffi::c_char = unsafe { msg_send![string, UTF8String] };
        if utf8.is_null() {
            return None;
        }
        Some(
            unsafe { std::ffi::CStr::from_ptr(utf8) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    /// Colour spaces that decide how the window server colour-matches the
    /// window: the NSWindow's, and each CAMetalLayer's (`null` = none set, so
    /// its pixels are taken to be in the window's space). Main thread only.
    pub(super) fn color_info(window: &WebviewWindow) -> Value {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGColorSpaceCopyName(space: *mut c_void) -> *mut Object;
            fn CFRelease(object: *mut c_void);
        }
        unsafe fn metal_layers(layer: *mut Object, out: &mut Vec<Value>) {
            if layer.is_null() {
                return;
            }
            // SAFETY: main thread, live layer tree; plain property reads.
            unsafe {
                let is_metal: bool = msg_send![layer, isKindOfClass: class!(CAMetalLayer)];
                if is_metal {
                    let space: *mut c_void = msg_send![layer, colorspace];
                    let name = if space.is_null() {
                        Value::Null
                    } else {
                        let name = CGColorSpaceCopyName(space);
                        let text = ns_string(name).unwrap_or_else(|| "unnamed".into());
                        if !name.is_null() {
                            CFRelease(name.cast());
                        }
                        Value::from(text)
                    };
                    let format: u64 = msg_send![layer, pixelFormat];
                    let edr: bool = msg_send![layer, wantsExtendedDynamicRangeContent];
                    out.push(serde_json::json!({
                        "colorspace": name,
                        "pixel_format": format,
                        "edr": edr,
                    }));
                }
                let sublayers: *mut Object = msg_send![layer, sublayers];
                if !sublayers.is_null() {
                    let count: usize = msg_send![sublayers, count];
                    for i in 0..count {
                        let child: *mut Object = msg_send![sublayers, objectAtIndex: i];
                        metal_layers(child, out);
                    }
                }
            }
        }
        let Ok(ns_window) = window.ns_window() else {
            return Value::Null;
        };
        let ns_window = ns_window.cast::<Object>();
        if ns_window.is_null() {
            return Value::Null;
        }
        // SAFETY: main thread with the live NSWindow and its content view.
        unsafe {
            let space: *mut Object = msg_send![ns_window, colorSpace];
            let space_name = if space.is_null() {
                None
            } else {
                ns_string(msg_send![space, localizedName])
            };
            let view: *mut Object = msg_send![ns_window, contentView];
            let mut layers = Vec::new();
            if !view.is_null() {
                metal_layers(msg_send![view, layer], &mut layers);
            }
            serde_json::json!({ "window_color_space": space_name, "metal_layers": layers })
        }
    }

    pub(super) fn window_number(window: &WebviewWindow) -> Value {
        let Ok(ns_window) = window.ns_window() else {
            return Value::Null;
        };
        if ns_window.is_null() {
            return Value::Null;
        }
        // SAFETY: called on the main thread with the live NSWindow.
        let number: isize = unsafe { msg_send![ns_window.cast::<Object>(), windowNumber] };
        Value::from(number as i64)
    }

    /// Activates the app and makes the main window key. Main thread only.
    pub(super) fn activate(app: &AppHandle) -> Result<Value, String> {
        let window = app.get_webview_window("main").ok_or("no main window")?;
        let ns_window = window
            .ns_window()
            .map_err(|e| e.to_string())?
            .cast::<Object>();
        if ns_window.is_null() {
            return Err("no NSWindow".into());
        }
        // SAFETY: main thread; plain AppKit activation calls on live objects.
        unsafe {
            let ns_app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let () = msg_send![ns_app, activateIgnoringOtherApps: true];
            let nil: *mut Object = std::ptr::null_mut();
            let () = msg_send![ns_window, makeKeyAndOrderFront: nil];
        }
        Ok(Value::Null)
    }

    /// Posts one mouse event to the main window. Main thread only.
    pub(super) fn post_mouse(
        app: &AppHandle,
        event: &MouseEvent,
    ) -> Result<serde_json::Value, String> {
        let window = app.get_webview_window("main").ok_or("no main window")?;
        let (_, height) = content_size(&window).ok_or("no content view")?;
        let ns_window = window
            .ns_window()
            .map_err(|e| e.to_string())?
            .cast::<Object>();
        if ns_window.is_null() {
            return Err("no NSWindow".into());
        }
        let kind = match (event.kind, event.right) {
            (MouseKind::Move, _) => MOUSE_MOVED,
            (MouseKind::Down, false) => LEFT_MOUSE_DOWN,
            (MouseKind::Drag, false) => LEFT_MOUSE_DRAGGED,
            (MouseKind::Up, false) => LEFT_MOUSE_UP,
            (MouseKind::Down, true) => RIGHT_MOUSE_DOWN,
            (MouseKind::Drag, true) => RIGHT_MOUSE_DRAGGED,
            (MouseKind::Up, true) => RIGHT_MOUSE_UP,
        };
        let flags = [
            (event.modifiers.shift, SHIFT),
            (event.modifiers.control, CONTROL),
            (event.modifiers.option, OPTION),
            (event.modifiers.command, COMMAND),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .fold(0, |flags, (_, bit)| flags | bit);
        // Window coordinates have their origin at the bottom left; the
        // borderless window's content fills it.
        let location = NSPoint {
            x: event.x,
            y: height - event.y,
        };
        let pressure: f32 = if matches!(event.kind, MouseKind::Down | MouseKind::Drag) {
            1.0
        } else {
            0.0
        };
        // SAFETY: main thread; NSEvent's factory method takes plain values and
        // a nil graphics context, and NSApp retains the posted event.
        unsafe {
            let number: isize = msg_send![ns_window, windowNumber];
            let process: *mut Object = msg_send![class!(NSProcessInfo), processInfo];
            let timestamp: f64 = msg_send![process, systemUptime];
            let nil: *mut Object = std::ptr::null_mut();
            let ns_event: *mut Object = msg_send![class!(NSEvent),
                mouseEventWithType: kind
                location: location
                modifierFlags: flags
                timestamp: timestamp
                windowNumber: number
                context: nil
                eventNumber: 0isize
                clickCount: event.clicks as isize
                pressure: pressure];
            if ns_event.is_null() {
                return Err("NSEvent was not created".into());
            }
            let ns_app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let () = msg_send![ns_app, postEvent: ns_event atStart: false];
        }
        Ok(Value::Null)
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use super::MouseEvent;
    use serde_json::Value;
    use tauri::{AppHandle, WebviewWindow};

    pub(super) fn window_number(_window: &WebviewWindow) -> Value {
        Value::Null
    }

    pub(super) fn color_info(_window: &WebviewWindow) -> Value {
        Value::Null
    }

    pub(super) fn content_size(_window: &WebviewWindow) -> Option<(f64, f64)> {
        None
    }

    pub(super) fn activate(_app: &AppHandle) -> Result<Value, String> {
        Err("native input is macOS-only; use ui.click/ui.drag".into())
    }

    pub(super) fn capture(_window: &WebviewWindow, _best: bool) -> Result<Value, String> {
        Err("window capture is macOS-only".into())
    }

    pub(super) fn post_mouse(_app: &AppHandle, _event: &MouseEvent) -> Result<Value, String> {
        Err("native input is macOS-only; use ui.click/ui.drag".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_event_params() {
        let event = MouseEvent::from_params(&json!({
            "type": "drag", "x": 10.5, "y": 20, "modifiers": ["shift", "alt"]
        }))
        .unwrap();
        assert_eq!(event.kind, MouseKind::Drag);
        assert_eq!((event.x, event.y, event.clicks), (10.5, 20.0, 1));
        assert!(event.modifiers.shift && event.modifiers.option);
        assert!(!event.modifiers.command && !event.modifiers.control);

        assert!(!event.right);
        let right =
            MouseEvent::from_params(&json!({ "type": "down", "x": 1, "y": 2, "button": "right" }))
                .unwrap();
        assert!(right.right);
        assert!(
            MouseEvent::from_params(&json!({ "type": "down", "x": 1, "y": 2, "button": "middle" }))
                .is_err()
        );
        assert!(MouseEvent::from_params(&json!({ "type": "hover", "x": 1, "y": 1 })).is_err());
        assert!(MouseEvent::from_params(&json!({ "type": "down", "x": 1 })).is_err());
        assert!(
            MouseEvent::from_params(
                &json!({ "type": "up", "x": 1, "y": 1, "modifiers": ["hyper"] })
            )
            .is_err()
        );
    }

    #[test]
    fn drag_hovers_presses_moves_and_releases() {
        let events = drag_events(&[(1.0, 2.0), (3.0, 4.0), (5.0, 6.0)], Modifiers::default());
        let kinds: Vec<_> = events
            .iter()
            .map(|event| (event.kind, event.x, event.y))
            .collect();
        assert_eq!(
            kinds,
            [
                (MouseKind::Move, 1.0, 2.0),
                (MouseKind::Down, 1.0, 2.0),
                (MouseKind::Drag, 3.0, 4.0),
                (MouseKind::Drag, 5.0, 6.0),
                (MouseKind::Up, 5.0, 6.0),
            ]
        );
        assert!(drag_events(&[], Modifiers::default()).is_empty());
    }
}
