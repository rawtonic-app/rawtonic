//! The monitor profile for what the webview shows on Linux (G23/G62, MCM2;
//! notes: benchmark-results/mcm2-2026-11-27/REPORT.md).
//!
//! WebKitGTK shows images as device values: nothing between the app and
//! the display converts them. So the two streams of photographs it shows
//! are converted here, each in one place, with the main window's display
//! transform (`monitor_profile.rs`, the same 65³ table the Windows present
//! pass samples):
//!
//! - Develop's frames (and multi-view cells, the second window's) as they
//!   are encoded for the webview ([`frame`], called by `preview_jpeg`);
//! - the Library's thumbnails (grid, filmstrip, Loupe), served through the
//!   `rawtonic-thumb` scheme instead of the asset protocol ([`serve`]).
//!
//! Without a transform (an sRGB display, no profile, the compositor
//! colour-managing the window itself) frames are encoded from the
//! untouched pixels and thumbnails are the cached file's bytes. A new
//! transform bumps [`generation`]: thumbnails are converted again (their
//! URL and the converted cache's key carry it) and the frame is requested
//! again (`monitor-colour-changed`).

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};

use rawtonic_display_profile::DisplayLut;
use rayon::prelude::*;

use crate::state::lock::lock_recover;

/// The scheme Linux thumbnails are served by.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const SCHEME: &str = "rawtonic-thumb";
/// Sent (with the new generation) when the transform changed.
pub const CHANGED_EVENT: &str = "monitor-colour-changed";
/// Quality of a converted thumbnail (the cached one is quality 75).
const THUMBNAIL_QUALITY: u8 = 95;
/// Bytes of converted thumbnails kept in memory.
const MAX_CONVERTED_BYTES: usize = 96 * 1024 * 1024;

/// The installed transform and its generation.
#[derive(Default)]
struct Current {
    generation: u64,
    lut: Option<Arc<DisplayLut>>,
}

impl Current {
    /// Installs `lut` (None: no transform); whether it differs from the
    /// one installed (then the generation moves on).
    fn set(&mut self, lut: Option<&DisplayLut>) -> bool {
        if self.lut.as_deref() == lut {
            return false;
        }
        self.lut = lut.cloned().map(Arc::new);
        self.generation += 1;
        true
    }

    fn get(&self) -> (u64, Option<Arc<DisplayLut>>) {
        (self.generation, self.lut.clone())
    }
}

static CURRENT: RwLock<Current> = RwLock::new(Current {
    generation: 0,
    lut: None,
});

fn current() -> (u64, Option<Arc<DisplayLut>>) {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).get()
}

/// Installs `lut` for every webview stream (None: no transform); whether it
/// changed.
pub fn set(lut: Option<&DisplayLut>) -> bool {
    CURRENT.write().unwrap_or_else(|e| e.into_inner()).set(lut)
}

/// The installed transform's generation.
pub fn generation() -> u64 {
    current().0
}

/// `pixels` (8-bit RGBA rows) converted by `lut`, on all threads.
fn convert(lut: &DisplayLut, pixels: &mut [u8]) {
    // 64 Ki pixels a task: a 4K frame in ~130 tasks.
    pixels
        .par_chunks_mut(4 * 65536)
        .for_each(|chunk| lut.convert_rgba8(chunk));
}

/// A frame for the webview: `image` converted into the monitor profile, or
/// `image` itself when there is no transform.
pub fn frame(image: &image::RgbaImage) -> Cow<'_, image::RgbaImage> {
    frame_with(current().1.as_deref(), image)
}

fn frame_with<'a>(lut: Option<&DisplayLut>, image: &'a image::RgbaImage) -> Cow<'a, image::RgbaImage> {
    match lut {
        Some(lut) => {
            let mut converted = image.clone();
            convert(lut, &mut converted);
            Cow::Owned(converted)
        }
        None => Cow::Borrowed(image),
    }
}

/// Converted thumbnails: (file, its length and mtime, generation) → bytes.
type Key = (std::path::PathBuf, u64, Option<std::time::SystemTime>, u64);

#[derive(Default)]
struct Converted {
    entries: VecDeque<(Key, Arc<Vec<u8>>)>,
    bytes: usize,
}

impl Converted {
    fn get(&self, key: &Key) -> Option<Arc<Vec<u8>>> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    fn insert(&mut self, key: Key, bytes: Arc<Vec<u8>>) {
        // Another generation's conversions are never asked for again.
        let generation = key.3;
        self.entries.retain(|(k, _)| k.3 == generation);
        self.bytes = self.entries.iter().map(|(_, v)| v.len()).sum::<usize>() + bytes.len();
        self.entries.push_back((key, bytes));
        while self.bytes > MAX_CONVERTED_BYTES
            && let Some((_, old)) = self.entries.pop_front()
        {
            self.bytes -= old.len();
        }
    }
}

static CONVERTED: Mutex<Option<Converted>> = Mutex::new(None);

/// A cached thumbnail converted by `lut`: decoded, converted, encoded again.
pub fn convert_thumbnail(lut: &DisplayLut, jpeg: &[u8]) -> Result<Vec<u8>, String> {
    let mut image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    convert(lut, &mut image);
    crate::preview_jpeg::encode_rgbx_444_as_is(&image, THUMBNAIL_QUALITY)
}

/// The bytes for the thumbnail at `path` in the cache folder `root`: the
/// file itself without a transform, else its conversion. `None` for a file
/// outside `root` or one that cannot be read.
fn thumbnail(
    root: &std::path::Path,
    path: &std::path::Path,
    (generation, lut): (u64, Option<Arc<DisplayLut>>),
) -> Option<Arc<Vec<u8>>> {
    use rawtonic_fs::Canonical;
    let root = root.canonical().ok()?;
    let path = path.canonical().ok()?;
    if !path.starts_with(&root) || path.extension().is_none_or(|e| e != "jpg") {
        return None;
    }
    let Some(lut) = lut else {
        return std::fs::read(&path).ok().map(Arc::new);
    };
    let meta = std::fs::metadata(&path).ok()?;
    let key = (path.clone(), meta.len(), meta.modified().ok(), generation);
    if let Some(bytes) = lock_recover(&CONVERTED).get_or_insert_default().get(&key) {
        return Some(bytes);
    }
    let file = std::fs::read(&path).ok()?;
    let bytes = match convert_thumbnail(&lut, &file) {
        Ok(bytes) => Arc::new(bytes),
        Err(error) => {
            log::warn!("Thumbnail {} not converted: {error}", path.display());
            return Some(Arc::new(file));
        }
    };
    lock_recover(&CONVERTED)
        .get_or_insert_default()
        .insert(key, bytes.clone());
    Some(bytes)
}

/// The file a `rawtonic-thumb://localhost/<path>` request names.
fn requested_path(uri: &tauri::http::Uri) -> Option<std::path::PathBuf> {
    let decoded = percent_encoding::percent_decode_str(uri.path())
        .decode_utf8()
        .ok()?;
    // `convertFileSrc` encodes the whole path after the root's slash.
    let path = decoded.strip_prefix('/').unwrap_or(&decoded);
    Some(std::path::PathBuf::from(path))
}

/// Serves a thumbnail request off the main thread.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn serve(
    app: tauri::AppHandle,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    tauri::async_runtime::spawn_blocking(move || {
        let root = crate::file_management::resolve_thumbnail_cache_dir(&app).ok();
        let body = root
            .zip(requested_path(request.uri()))
            .and_then(|(root, path)| thumbnail(&root, &path, current()));
        let response = match body {
            Some(body) => tauri::http::Response::builder()
                .status(200)
                .header("Content-Type", "image/jpeg")
                .header("Access-Control-Allow-Origin", "*")
                .header("X-Content-Type-Options", "nosniff")
                .body(body.as_ref().clone()),
            None => tauri::http::Response::builder()
                .status(404)
                .body(Vec::new()),
        };
        responder.respond(response.expect("static thumbnail response headers"));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adobe_lut() -> DisplayLut {
        let icc = moxcms::ColorProfile::new_adobe_rgb().encode().unwrap();
        rawtonic_display_profile::display_lut(rawtonic_display_profile::Encoding::Srgb, &icc)
            .unwrap()
            .unwrap()
    }

    /// Ten sRGB patches 32 px wide (whole JPEG blocks), as `monitor_check`.
    fn patches() -> image::RgbaImage {
        const PATCHES: [[u8; 3]; 10] = [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [200, 100, 50],
            [50, 120, 200],
            [128, 128, 128],
            [255, 255, 255],
            [0, 0, 0],
            [240, 200, 170],
            [20, 60, 30],
        ];
        image::RgbaImage::from_fn(320, 32, |x, _| {
            let p = PATCHES[(x / 32) as usize];
            image::Rgba([p[0], p[1], p[2], 255])
        })
    }

    fn centre(image: &image::RgbImage, patch: u32) -> [u8; 3] {
        image.get_pixel(patch * 32 + 16, 16).0
    }

    #[test]
    fn frames_and_thumbnails_get_the_tables_values_srgb_stays_byte_identical() {
        let mut installed = Current::default();
        let source = patches();
        // No transform: the frame is the very image, the thumbnail the file.
        assert!(!installed.set(None));
        assert!(matches!(frame_with(None, &source), Cow::Borrowed(_)));
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("abc_small.jpg");
        let jpeg = crate::preview_jpeg::encode_rgbx_444_as_is(&source, 90).unwrap();
        std::fs::write(&file, &jpeg).unwrap();
        assert_eq!(*thumbnail(root.path(), &file, installed.get()).unwrap(), jpeg);
        assert!(thumbnail(root.path(), &root.path().join("missing.jpg"), installed.get()).is_none());
        let outside = tempfile::tempdir().unwrap();
        let elsewhere = outside.path().join("x.jpg");
        std::fs::write(&elsewhere, &jpeg).unwrap();
        assert!(thumbnail(root.path(), &elsewhere, installed.get()).is_none());

        // A wide-gamut monitor: each patch as the table gives it (±1 in
        // the frame; the thumbnail, through a second JPEG encode, within
        // ±3 of the conversion of the cached file's own pixels).
        let lut = adobe_lut();
        assert!(installed.set(Some(&lut)));
        assert!(!installed.set(Some(&lut)));
        assert_eq!(installed.get().0, 1);
        let converted = frame_with(Some(&lut), &source).into_owned();
        let decoded = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        let thumb = image::load_from_memory(&thumbnail(root.path(), &file, installed.get()).unwrap())
            .unwrap()
            .to_rgb8();
        for patch in 0..10 {
            let input = source.get_pixel(patch * 32 + 16, 16).0;
            let want = lut.apply([0, 1, 2].map(|c| input[c] as f32 / 255.0));
            let got = converted.get_pixel(patch * 32 + 16, 16).0;
            let from_file = lut.apply([0, 1, 2].map(|c| centre(&decoded, patch)[c] as f32 / 255.0));
            for c in 0..3 {
                let want = (want[c] * 255.0).round() as i16;
                assert!((got[c] as i16 - want).abs() <= 1, "patch {patch}: {got:?}");
                let from_file = (from_file[c] * 255.0).round() as i16;
                let shown = centre(&thumb, patch)[c] as i16;
                assert!((shown - from_file).abs() <= 3, "thumbnail patch {patch}: {shown} vs {from_file}");
            }
        }
        // Red really moved (Adobe RGB's red is wider than sRGB's).
        assert!(converted.get_pixel(16, 16).0[0] < 250);
        // Back to sRGB: the file's bytes again, a new generation.
        assert!(installed.set(None));
        assert_eq!(installed.get().0, 2);
        assert_eq!(*thumbnail(root.path(), &file, installed.get()).unwrap(), jpeg);
    }

    /// The cost of converting a 4K frame (3840 × 2160) on all threads;
    /// `cargo test -p rawtonic-app --release --lib browser_colour -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing probe"]
    fn frame_conversion_timing_4k() {
        let lut = adobe_lut();
        let image = image::RgbaImage::from_fn(3840, 2160, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8, 255])
        });
        for run in 0..5 {
            let mut times = Vec::new();
            for _ in 0..20 {
                let start = std::time::Instant::now();
                let converted = frame_with(Some(&lut), &image);
                std::hint::black_box(&converted);
                times.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            let mut copy = Vec::new();
            for _ in 0..20 {
                let start = std::time::Instant::now();
                std::hint::black_box(image.clone());
                copy.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(f64::total_cmp);
            copy.sort_by(f64::total_cmp);
            println!(
                "run {run}: 4K frame conversion {:.2} ms median of 20 (of which the copy {:.2} ms), {} threads",
                times[10],
                copy[10],
                rayon::current_num_threads()
            );
        }
    }
}
