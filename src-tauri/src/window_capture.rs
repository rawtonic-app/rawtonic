// SPDX-License-Identifier: AGPL-3.0-or-later
//! The main window as the window server composites it, so the native GPU
//! layer under the transparent webview is included: the QA channel's
//! `window.capture` and the problem report's screenshot. A process may
//! capture its own windows without Screen Recording permission (macOS;
//! other systems have no capture yet).

use tauri::WebviewWindow;

/// The window's pixels (straight alpha), in sRGB or Display P3, at the
/// screen's backing resolution (`best`) or in points. Main thread only.
#[cfg(target_os = "macos")]
pub(crate) fn capture_rgba(
    window: &WebviewWindow,
    best: bool,
    p3: bool,
) -> Result<image::RgbaImage, String> {
    macos::capture_rgba(window, best, p3)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn capture_rgba(
    _window: &WebviewWindow,
    _best: bool,
    _p3: bool,
) -> Result<image::RgbaImage, String> {
    Err("window capture is macOS-only".into())
}

/// Whether [`capture_rgba`] works on this system.
pub(crate) const AVAILABLE: bool = cfg!(target_os = "macos");

/// The window server's number for the window (macOS). Main thread only.
#[cfg(all(target_os = "macos", feature = "qa-control"))]
pub(crate) fn window_number(window: &WebviewWindow) -> Option<i64> {
    macos::window_number(window)
}

#[cfg(target_os = "macos")]
mod macos {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    use std::ffi::c_void;
    use tauri::WebviewWindow;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGPoint,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCreateImage(
            bounds: CGRect,
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
        fn CGContextDrawImage(context: *mut c_void, rect: CGRect, image: *mut c_void);
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

    pub(super) fn window_number(window: &WebviewWindow) -> Option<i64> {
        let ns_window = window.ns_window().ok()?;
        if ns_window.is_null() {
            return None;
        }
        // SAFETY: called on the main thread with the live NSWindow.
        let number: isize = unsafe { msg_send![ns_window.cast::<Object>(), windowNumber] };
        Some(number as i64)
    }

    pub(super) fn capture_rgba(
        window: &WebviewWindow,
        best: bool,
        p3: bool,
    ) -> Result<image::RgbaImage, String> {
        let number = window_number(window).ok_or("no window number")?;
        let null_rect = CGRect {
            origin: CGPoint {
                x: f64::INFINITY,
                y: f64::INFINITY,
            },
            size: CGPoint { x: 0.0, y: 0.0 },
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
                let rect = CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: CGPoint {
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
        // Undo premultiplication so the image shows what the window shows.
        for pixel in pixels.as_chunks_mut::<4>().0 {
            let alpha = u32::from(pixel[3]);
            if alpha != 0 && alpha != 255 {
                for channel in &mut pixel[..3] {
                    *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
        image::RgbaImage::from_raw(width as u32, height as u32, pixels)
            .ok_or_else(|| "bitmap size mismatch".to_string())
    }
}
