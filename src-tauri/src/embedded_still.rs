//! The camera's own JPEG inside a RAW as the still a step to the photograph
//! shows until its developed frame arrives (G35 culling, notes:
//! parity-gaps/G35-culling.md): the largest embedded preview, decoded at the
//! size the viewer shows it (libjpeg-turbo's DCT scaling), turned by the
//! camera orientation, framed like the development
//! (`raw_processing::developed_size`) so the developed frame replaces it in
//! place, and converted from the camera's colour space to the display's.
//! Real pixels of that photograph, never a fabricated preview. A few
//! decoded stills are kept in memory for the photographs likely shown
//! next (`prepare`, asked for with the neighbours' preview proxies).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use image::{DynamicImage, RgbaImage};
use rawtonic_engine::output_color_space::OutputColorSpace;
use tauri::Manager;

use crate::app_state::AppState;
use crate::file_management::parse_virtual_path;
use crate::formats::is_raw_file;
use crate::gpu_processing;
use crate::image_loader;

/// Stills kept decoded (the open photograph's and the next ones').
const KEPT: usize = 4;
/// At most this many bytes of them (a 3024 × 2016 still is 24 MB).
const KEPT_BYTES: usize = 128 << 20;
/// How far the embedded JPEG's aspect ratio may differ from the developed
/// frame's for it to be trimmed to that frame (cameras' JPEGs leave out a
/// few rows or columns of the sensor's default crop); beyond it the JPEG
/// shows another field and keeps its own shape.
const MAX_ASPECT_DIFF: f32 = 0.03;
/// Photographs of a prefetch request whose stills are prepared ahead.
const PREPARED_AHEAD: usize = 2;

/// A still ready for the display.
pub(crate) struct Still {
    /// Display-encoded RGBA (`gpu_processing::encoded_to_display`).
    pub image: RgbaImage,
    /// The size it stands for as shown (turned, framed like the developed
    /// frame): its placement at fit.
    pub shown: (u32, u32),
}

/// Whether a recipe is a never-edited photograph's: then its cached
/// thumbnail is the same camera JPEG, only smaller, and the embedded one
/// stands in; an edited photograph's thumbnail shows its edits and stays.
pub(crate) fn never_edited(recipe: &serde_json::Value) -> bool {
    recipe.is_null() || recipe.as_object().is_some_and(|recipe| recipe.is_empty())
}

/// The largest embedded JPEG of a RAW's bytes that decodes to 8-bit
/// colour: RAF's, else the largest of a TIFF-based RAW's (NEF, ARW, CR2,
/// DNG, ORF, RW2, PEF…; a DNG's lossless raw tiles are left out).
pub(crate) fn largest_embedded_jpeg(raw: &[u8]) -> Option<&[u8]> {
    let decodable = |jpeg: &&[u8]| {
        turbojpeg::read_header(jpeg).is_ok_and(|header| {
            !header.is_lossless
                && header.width > 0
                && header.height > 0
                && !matches!(
                    header.colorspace,
                    turbojpeg::Colorspace::CMYK | turbojpeg::Colorspace::YCCK
                )
        })
    };
    if let Some(jpeg) = image_loader::raf_embedded_jpeg(raw) {
        return Some(jpeg).filter(decodable);
    }
    image_loader::tiff_jpeg_candidates(raw)?
        .into_iter()
        .find(decodable)
}

/// The libjpeg-turbo scale (k/8) that decodes a JPEG of `size` to the
/// smallest size still covering `want` on both sides; at most 1.
pub(crate) fn decode_scale(size: (u32, u32), want: (u32, u32)) -> turbojpeg::ScalingFactor {
    (1..=8)
        .map(|k| turbojpeg::ScalingFactor::new(k, 8))
        .find(|f| {
            f.scale(size.0 as usize) >= want.0 as usize
                && f.scale(size.1 as usize) >= want.1 as usize
        })
        .unwrap_or(turbojpeg::ScalingFactor::ONE)
}

/// `image` (turned as shown) trimmed, centred, to the aspect ratio of the
/// `developed` frame and standing for it, when the two differ by at most
/// `MAX_ASPECT_DIFF`; else as it is, standing for its own size.
pub(crate) fn framed(image: RgbaImage, developed: Option<(u32, u32)>) -> Still {
    let (w, h) = image.dimensions();
    let close = |&(dw, dh): &(u32, u32)| {
        let ratio = (dw as f32 / dh as f32) / (w as f32 / h as f32);
        dw > 0 && dh > 0 && (ratio - 1.0).abs() <= MAX_ASPECT_DIFF
    };
    let Some((dw, dh)) = developed.filter(close) else {
        return Still {
            image,
            shown: (w, h),
        };
    };
    // Keep the whole of the relatively shorter side.
    let (cw, ch) = if u64::from(w) * u64::from(dh) > u64::from(h) * u64::from(dw) {
        (
            ((u64::from(h) * u64::from(dw) + u64::from(dh) / 2) / u64::from(dh)) as u32,
            h,
        )
    } else {
        (
            w,
            ((u64::from(w) * u64::from(dh) + u64::from(dw) / 2) / u64::from(dw)) as u32,
        )
    };
    let (cw, ch) = (cw.clamp(1, w), ch.clamp(1, h));
    let image = if (cw, ch) == (w, h) {
        image
    } else {
        image::imageops::crop_imm(&image, (w - cw) / 2, (h - ch) / 2, cw, ch).to_image()
    };
    Still {
        image,
        shown: (dw, dh),
    }
}

/// The TIFF block of a JPEG's EXIF segment (APP1 "Exif\0\0").
fn jpeg_exif(jpeg: &[u8]) -> Option<&[u8]> {
    let mut at = 2;
    while jpeg.get(at) == Some(&0xff) {
        let marker = *jpeg.get(at + 1)?;
        if marker == 0xda || marker == 0xd9 {
            return None;
        }
        let len = u16::from_be_bytes([*jpeg.get(at + 2)?, *jpeg.get(at + 3)?]) as usize;
        let segment = jpeg.get(at + 4..at + 2 + len)?;
        if marker == 0xe1 && segment.starts_with(b"Exif\0\0") {
            return segment.get(6..);
        }
        at += 2 + len;
    }
    None
}

/// The space the camera encoded its embedded JPEG in: Adobe RGB when the
/// RAW (or the JPEG's own EXIF) says so, else sRGB.
fn camera_space(raw: &[u8], jpeg: Option<&[u8]>) -> OutputColorSpace {
    let adobe = rawtonic_rawdecode::capture_info::camera_adobe_rgb(raw)
        .or_else(|| rawtonic_rawdecode::capture_info::camera_adobe_rgb(jpeg_exif(jpeg?)?));
    if adobe == Some(true) {
        OutputColorSpace::AdobeRgb
    } else {
        OutputColorSpace::Srgb
    }
}

/// Fit size of `shown` (width, height) in `viewport` (width, height).
fn fit(viewport: (u32, u32), shown: (u32, u32)) -> (u32, u32) {
    let rect = gpu_processing::fit_placement(
        [0.0, 0.0, viewport.0 as f32, viewport.1 as f32],
        shown.0 as f32,
        shown.1 as f32,
    );
    (rect[2].ceil() as u32, rect[3].ceil() as u32)
}

/// The still of the RAW `raw` (the bytes of `path`) for a viewer of
/// `viewport` device pixels (width, height) on a display encoding
/// `display`. Without an embedded JPEG (CR3, CRW) the decoder's embedded
/// preview; none when the file has neither.
pub(crate) fn decode(
    raw: &[u8],
    path: &str,
    viewport: (u32, u32),
    display: OutputColorSpace,
) -> Option<Still> {
    let orientation = image_loader::source_orientation(raw, path);
    let turned = (5..=8).contains(&orientation);
    let jpeg = largest_embedded_jpeg(raw);
    let image = if let Some(jpeg) = jpeg {
        let header = turbojpeg::read_header(jpeg).ok()?;
        let size = (header.width as u32, header.height as u32);
        let shown = if turned { (size.1, size.0) } else { size };
        let want = fit(viewport, shown);
        let want = if turned { (want.1, want.0) } else { want };
        let factor = decode_scale(size, want);
        let scaled = header.scaled(factor);
        let mut decompressor = turbojpeg::Decompressor::new().ok()?;
        decompressor.set_scaling_factor(factor).ok()?;
        let mut pixels = vec![0u8; 4 * scaled.width * scaled.height];
        decompressor
            .decompress(
                jpeg,
                turbojpeg::Image {
                    pixels: &mut pixels[..],
                    width: scaled.width,
                    pitch: 4 * scaled.width,
                    height: scaled.height,
                    format: turbojpeg::PixelFormat::RGBA,
                },
            )
            .ok()?;
        let image = RgbaImage::from_raw(scaled.width as u32, scaled.height as u32, pixels)?;
        rawtonic_rawdecode::orientation::apply_orientation(
            DynamicImage::ImageRgba8(image),
            rawler::Orientation::from_u16(orientation),
        )
        .into_rgba8()
    } else if image_loader::tiff_jpeg_candidates(raw).is_none() {
        // Not a TIFF container: the decoder's preview, already turned.
        let preview = image_loader::safe_embedded_preview_fallback(raw, path)?;
        let want = fit(viewport, (preview.width(), preview.height()));
        if preview.width() > want.0.saturating_mul(2) {
            preview
                .resize(want.0, want.1, image::imageops::FilterType::Triangle)
                .into_rgba8()
        } else {
            preview.into_rgba8()
        }
    } else {
        return None;
    };
    let mut still = framed(
        image,
        rawtonic_rawdecode::raw_processing::developed_size(raw),
    );
    gpu_processing::encoded_to_display(camera_space(raw, jpeg), display, &mut still.image);
    Some(still)
}

/// What a kept still was made from and for.
#[derive(Clone, Debug, PartialEq)]
struct Key {
    source: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    viewport: (u32, u32),
    display: OutputColorSpace,
}

impl Key {
    fn of(source: &Path, viewport: (u32, u32), display: OutputColorSpace) -> Option<Self> {
        let meta = std::fs::metadata(source).ok()?;
        Some(Key {
            source: source.to_path_buf(),
            modified: meta.modified().ok(),
            len: meta.len(),
            viewport,
            display,
        })
    }
}

/// Decoded stills, newest first, within `KEPT` and `KEPT_BYTES` (only the
/// newest under memory pressure).
#[derive(Default)]
struct Kept(VecDeque<(Key, Arc<Still>)>);

impl Kept {
    fn get(&mut self, key: &Key) -> Option<Arc<Still>> {
        let at = self.0.iter().position(|(k, _)| k == key)?;
        let entry = self.0.remove(at)?;
        let still = Arc::clone(&entry.1);
        self.0.push_front(entry);
        Some(still)
    }

    fn keep(&mut self, key: Key, still: Arc<Still>, short_of_memory: bool) {
        self.0.retain(|(k, _)| *k != key);
        self.0.push_front((key, still));
        let mut bytes = 0;
        let mut count = 0;
        self.0.retain(|(_, still)| {
            bytes += still.image.as_raw().len();
            count += 1;
            count == 1 || (!short_of_memory && count <= KEPT && bytes <= KEPT_BYTES)
        });
    }
}

static KEPT_STILLS: Mutex<Kept> = Mutex::new(Kept(VecDeque::new()));

/// The kept still of `source` for `viewport` and `display`, if decoded.
pub(crate) fn kept(
    source: &Path,
    viewport: (u32, u32),
    display: OutputColorSpace,
) -> Option<Arc<Still>> {
    let key = Key::of(source, viewport, display)?;
    KEPT_STILLS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
}

/// The still of `source` (kept, else decoded from the file and kept) for
/// `viewport` and `display`. Reads the file only.
pub(crate) fn still_for(
    source: &Path,
    viewport: (u32, u32),
    display: OutputColorSpace,
) -> Option<Arc<Still>> {
    let key = Key::of(source, viewport, display)?;
    if let Some(still) = KEPT_STILLS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
    {
        return Some(still);
    }
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("embedded_still_decode");
    let still = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let raw = rawtonic_engine::file_io::read_file_mapped(source).ok()?;
        decode(&raw, &source.to_string_lossy(), viewport, display)
    }))
    .ok()
    .flatten()?;
    let still = Arc::new(still);
    KEPT_STILLS.lock().unwrap_or_else(|e| e.into_inner()).keep(
        key,
        Arc::clone(&still),
        crate::memory_pressure::level_now().is_some(),
    );
    Some(still)
}

/// Raised by each `prepare` request (the newest replaces the others).
static PREPARING: AtomicUsize = AtomicUsize::new(0);

/// Prepares the stills of the first photographs of `paths` (the likeliest
/// next ones, asked for at each step) in the background: RAWs never edited
/// without a stored preview proxy (with one, a step shows its developed
/// frame instead), for the single view's viewport. A new request replaces
/// the last one.
#[tauri::command]
pub(crate) fn prepare_embedded_stills(paths: Vec<String>, app_handle: tauri::AppHandle) {
    let generation = PREPARING.fetch_add(1, Ordering::SeqCst) + 1;
    let paths: Vec<String> = paths.into_iter().take(PREPARED_AHEAD).collect();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let target = state
            .gpu_context
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|context| {
                let display = context.display.lock().unwrap_or_else(|e| e.into_inner());
                let display = display.as_ref()?;
                let [_, _, w, h] = display.viewport?;
                (!display.multi_view().is_active() && w >= 1.0 && h >= 1.0)
                    .then_some(((w as u32, h as u32), context.display_space))
            });
        let Some((viewport, space)) = target else {
            return;
        };
        for path in paths {
            if PREPARING.load(Ordering::SeqCst) != generation {
                return;
            }
            let (source, sidecar) = parse_virtual_path(&path);
            if !is_raw_file(&source)
                || crate::preview_proxy::proxy_path(&source).is_some_and(|p| p.is_file())
                || !never_edited(&crate::exif_processing::load_sidecar(&sidecar).adjustments)
            {
                continue;
            }
            let _ = still_for(&source, viewport, space);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A JPEG of `w` × `h` whose left half is red and right half blue.
    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(w, h, |x, _| {
            if x < w / 2 {
                image::Rgb([220, 20, 20])
            } else {
                image::Rgb([20, 20, 220])
            }
        });
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    /// A little-endian TIFF RAW stand-in: IFD0 with `orientation` and a
    /// small JPEG preview, chained to IFD1 with a large one (old-style
    /// JPEGInterchangeFormat tags), as NEF/ARW files carry them.
    fn tiff_with_previews(orientation: u16, small: &[u8], large: &[u8]) -> Vec<u8> {
        let entry = |tag: u16, typ: u16, value: u32| {
            [
                &tag.to_le_bytes()[..],
                &typ.to_le_bytes(),
                &1u32.to_le_bytes(),
                &value.to_le_bytes(),
            ]
            .concat()
        };
        // IFD0 at 8: 3 entries (42 bytes) → IFD1 at 50: 2 entries (30) → data at 80.
        let (small_at, large_at) = (80u32, 80 + small.len() as u32);
        let mut data = b"II*\0".to_vec();
        data.extend(8u32.to_le_bytes());
        data.extend(3u16.to_le_bytes());
        data.extend(entry(0x0112, 3, u32::from(orientation)));
        data.extend(entry(0x0201, 4, small_at));
        data.extend(entry(0x0202, 4, small.len() as u32));
        data.extend(50u32.to_le_bytes());
        data.extend(2u16.to_le_bytes());
        data.extend(entry(0x0201, 4, large_at));
        data.extend(entry(0x0202, 4, large.len() as u32));
        data.extend(0u32.to_le_bytes());
        assert_eq!(data.len(), 80);
        data.extend(small);
        data.extend(large);
        data
    }

    #[test]
    fn the_largest_embedded_jpeg_is_chosen() {
        let (small, large) = (jpeg(16, 8), jpeg(160, 80));
        let raw = tiff_with_previews(1, &small, &large);
        assert_eq!(largest_embedded_jpeg(&raw), Some(&large[..]));
        // A broken large one gives way to the next that decodes.
        let raw = tiff_with_previews(1, &small, &[0xff, 0xd8, 0xff, 0xd9, 0, 0, 0, 0]);
        assert_eq!(largest_embedded_jpeg(&raw), Some(&small[..]));
        assert_eq!(largest_embedded_jpeg(b"no raw at all"), None);
    }

    #[test]
    fn the_decode_scale_is_the_smallest_that_covers_the_view() {
        let scale = |size, want| {
            let f = decode_scale(size, want);
            (f.num(), f.denom())
        };
        assert_eq!(scale((6048, 4024), (3000, 1996)), (1, 2));
        assert_eq!(scale((6048, 4024), (3024, 2012)), (1, 2));
        assert_eq!(scale((6048, 4024), (3025, 2012)), (5, 8));
        assert_eq!(scale((6048, 4024), (700, 466)), (1, 8));
        // Never enlarged.
        assert_eq!(scale((1616, 1080), (2700, 1800)), (1, 1));
    }

    #[test]
    fn a_still_is_trimmed_to_the_developed_frame_and_stands_for_it() {
        // A 3:2 JPEG for a 6048 × 4024 development: two rows trimmed off.
        let still = framed(RgbaImage::new(3000, 2000), Some((6048, 4024)));
        assert_eq!(still.shown, (6048, 4024));
        assert_eq!(still.image.dimensions(), (3000, 1996));
        // Taller than the development: columns stay, rows go.
        let still = framed(RgbaImage::new(1500, 1010), Some((6000, 4000)));
        assert_eq!(still.image.dimensions(), (1500, 1000));
        // Another field (16:9 JPEG of a 3:2 frame) or no frame known: its own.
        let still = framed(RgbaImage::new(1920, 1080), Some((6000, 4000)));
        assert_eq!(
            (still.shown, still.image.dimensions()),
            ((1920, 1080), (1920, 1080))
        );
        let still = framed(RgbaImage::new(1920, 1080), None);
        assert_eq!(still.shown, (1920, 1080));
    }

    /// The still of a turned photograph is turned like its development, so
    /// it takes the developed frame's placement: no jump at the handover.
    #[test]
    fn a_turned_still_takes_the_developed_frames_placement() {
        let raw = tiff_with_previews(6, &jpeg(16, 8), &jpeg(320, 160));
        let viewport = (300, 300);
        // No RAW decoder reads this stand-in: its orientation is read as a
        // TIFF's, and its own (turned) shape stands.
        let still = decode(&raw, "turned.tif", viewport, OutputColorSpace::Srgb).unwrap();
        assert_eq!(still.shown, (160, 320));
        assert_eq!(still.image.dimensions(), (160, 320));
        // Orientation 6 turns clockwise: the JPEG's left (red) half is on top.
        let top = still.image.get_pixel(80, 20).0;
        let bottom = still.image.get_pixel(80, 300).0;
        assert!(top[0] > 150 && top[2] < 90, "{top:?}");
        assert!(bottom[2] > 150 && bottom[0] < 90, "{bottom:?}");
    }

    /// A DNG the decoder develops (64 × 48 mosaic, orientation 6) with a
    /// 68 × 50 camera JPEG beside it: its still is turned and trimmed to the
    /// development and placed where the developed frame will be.
    #[test]
    fn the_developed_frame_replaces_the_still_in_place() {
        let mut dng = rawtonic_rawdecode::fingerprint::synthetic::synthetic_dng();
        let preview = jpeg(68, 50);
        let count = u16::from_le_bytes([dng[8], dng[9]]) as usize;
        let next = 8 + 2 + 12 * count;
        let ifd1 = dng.len() as u32;
        dng[next..next + 4].copy_from_slice(&ifd1.to_le_bytes());
        dng.extend(2u16.to_le_bytes());
        for (tag, value) in [(0x0201u16, ifd1 + 30), (0x0202, preview.len() as u32)] {
            dng.extend(tag.to_le_bytes());
            dng.extend(4u16.to_le_bytes());
            dng.extend(1u32.to_le_bytes());
            dng.extend(value.to_le_bytes());
        }
        dng.extend(0u32.to_le_bytes());
        dng.extend(&preview);
        let developed = rawtonic_rawdecode::raw_processing::develop_raw_image(
            &dng,
            false,
            2.5,
            "auto".into(),
            None,
        )
        .unwrap();
        assert_eq!((developed.width(), developed.height()), (48, 64));
        let still = decode(&dng, "synthetic.dng", (300, 200), OutputColorSpace::Srgb).unwrap();
        assert_eq!(still.shown, (48, 64));
        // 68 × 50 turned is 50 × 68: one row trimmed to the frame's 3:4.
        assert_eq!(still.image.dimensions(), (50, 67));
        let viewport = [10.0, 20.0, 300.0, 200.0];
        assert_eq!(
            gpu_processing::fit_placement(viewport, still.shown.0 as f32, still.shown.1 as f32),
            gpu_processing::fit_placement(viewport, 48.0, 64.0)
        );
    }

    #[test]
    fn a_large_jpeg_is_decoded_at_the_size_shown() {
        let raw = tiff_with_previews(1, &jpeg(16, 8), &jpeg(1600, 800));
        let still = decode(&raw, "large.nef", (400, 400), OutputColorSpace::Srgb).unwrap();
        // 1600 × 800 in a 400 × 400 view is shown 400 × 200: decoded at 1/4.
        assert_eq!(still.image.dimensions(), (400, 200));
    }

    #[test]
    fn without_an_embedded_jpeg_there_is_no_still() {
        let raw = tiff_with_previews(1, &[0; 8], &[0; 8]);
        assert!(decode(&raw, "none.nef", (400, 400), OutputColorSpace::Srgb).is_none());
    }

    #[test]
    fn exif_colour_of_a_jpeg_is_found() {
        let mut jpeg_bytes = vec![0xff, 0xd8, 0xff, 0xe1];
        let tiff = b"II*\0\x08\0\0\0\0\0\0\0\0\0";
        let segment = [&b"Exif\0\0"[..], tiff].concat();
        jpeg_bytes.extend(((segment.len() + 2) as u16).to_be_bytes());
        jpeg_bytes.extend(&segment);
        jpeg_bytes.extend([0xff, 0xda, 0, 2]);
        assert_eq!(jpeg_exif(&jpeg_bytes), Some(&tiff[..]));
        assert_eq!(jpeg_exif(&[0xff, 0xd8, 0xff, 0xda, 0, 2]), None);
        assert_eq!(camera_space(b"none", None), OutputColorSpace::Srgb);
    }

    #[test]
    fn kept_stills_stay_within_their_bounds() {
        let key = |n: u32| Key {
            source: PathBuf::from(format!("/p/{n}.nef")),
            modified: None,
            len: 1,
            viewport: (100, 100),
            display: OutputColorSpace::Srgb,
        };
        let still = |side: u32| {
            Arc::new(Still {
                image: RgbaImage::new(side, side),
                shown: (side, side),
            })
        };
        let mut kept = Kept::default();
        for n in 0..6 {
            kept.keep(key(n), still(10), false);
        }
        assert_eq!(kept.0.len(), KEPT);
        assert!(kept.get(&key(0)).is_none());
        assert!(kept.get(&key(3)).is_some());
        // Bytes: a 4096 × 4096 still (64 MiB) leaves room for one more.
        kept.keep(key(10), still(4096), false);
        kept.keep(key(11), still(4096), false);
        assert_eq!(
            kept.0.len(),
            1 + usize::from(2 * 64 * 1024 * 1024 <= KEPT_BYTES)
        );
        // Short of memory only the newest stays.
        kept.keep(key(12), still(10), true);
        assert_eq!(kept.0.len(), 1);
        assert!(kept.get(&key(12)).is_some());
        // The same photograph for another viewport or display is another still.
        let mut other = key(12);
        other.viewport = (200, 200);
        assert!(kept.get(&other).is_none());
    }
}
