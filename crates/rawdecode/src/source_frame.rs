//! Which frame of a RAW the development gives (ADR 0020): the recipe key
//! `sourceFrameVersion` selects it, so a frame fix never changes the
//! source of an existing edit. Version 0 (recipes without the key) is the
//! frame the development always gave; each later version adds one fix of
//! the decoded area, and a version decodes exactly as the one before on
//! every file its fix leaves alone.
//!
//! 1 (2026-10-26): a CR3's active area grows to hold the crop the camera
//! records. In APS-C crop mode the EOS R5 Mark II records an active area
//! that ends 118 px short of its recorded crop (and of SensorRightBorder),
//! so the frame decoded 4970 × 3392 and ≈ 54 px off-centre instead of the
//! 5088 × 3392 of the camera's JPEG and Camera Raw (notes: parity-gaps/G71).
//!
//! 2 (2026-11-02): a Sony ARW's aspect frame (16:9, 4:3, 1:1 set in the
//! camera) is no longer cut at decode; the whole sensor develops and new
//! edits start with the camera's frame as their crop
//! (`camera_crop`), as for Canon's aspect ratios. Sony's distortion data
//! refer to the sensor, and the camera and Camera Raw correct the whole
//! sensor before they crop: on the frame alone the warp lacked the pixels
//! beyond it (HX95, RX0 II, ZV-E10 16:9 against the camera's JPEG: 11.9,
//! 3.4, 2.0 → 0.35, 0.45, 0.38 JPEG px; notes: parity-gaps/G71, ssz).
//!
//! 3 (2026-11-03): a compressed RAF of the X-T2, X-Pro2, X-E3 and X-T20
//! keeps the active area of the camera's `compressed` mode (rawler 0.8.0's
//! mode selection). Below version 3 the uncompressed files' active area
//! (a 128 px right border of a 6160 px buffer) cut the 6048 px compressed
//! buffer, so the frame developed 5904 × 4000 instead of the 6000 × 4000
//! the camera records and its JPEG shows (notes: parity-gaps/G71, rwf).

use rawler::RawImage;
use rawler::decoders::{FormatHint, RawDecodeParams};
use rawler::imgop::{Point, Rect};
use rawler::rawsource::RawSource;

/// The newest frame version: new edits start with it.
pub const VERSION: u32 = 3;

/// Version 1's fix: a CR3's active area holds its recorded crop.
const CR3_RECORDED_CROP: u32 = 1;

/// Version 2's fix: a Sony ARW keeps the sensor around its aspect frame.
const ARW_WHOLE_SENSOR: u32 = 2;

/// Version 3's fix: a compressed RAF keeps its compressed mode's active area.
const RAF_COMPRESSED_AREA: u32 = 3;

/// Whether some frame version can change the frame of a file with this
/// extension (any case, no dot): only those files are read to find out.
pub fn may_change(extension: &str) -> bool {
    (1..=VERSION).any(|version| changes(version, extension))
}

/// Whether frame version `version`'s fix can change the frame of a file
/// with this extension (any case, no dot): a list of a file's frames made
/// before `version` (an offline photograph's previews) tells its frame at
/// `version` only where it cannot.
pub fn changes(version: u32, extension: &str) -> bool {
    match version {
        CR3_RECORDED_CROP => extension.eq_ignore_ascii_case("cr3"),
        ARW_WHOLE_SENSOR => extension.eq_ignore_ascii_case("arw"),
        RAF_COMPRESSED_AREA => extension.eq_ignore_ascii_case("raf"),
        _ => false,
    }
}

/// Whether frame version `version` develops the whole `base` area of a RAW
/// instead of the camera's aspect `frame` inside it (both in buffer
/// coordinates): from version 2 for a Sony ARW whose frame the camera's
/// crop gives back exactly — centred in `base`, with the size the file
/// records as its image's (`camera_crop`). Elsewhere the frame stays cut,
/// so no photograph loses its camera framing.
pub(crate) fn keeps_sensor(
    format: FormatHint,
    version: u32,
    file_bytes: &[u8],
    base: Rect,
    frame: Rect,
) -> bool {
    if version < ARW_WHOLE_SENSOR || format != FormatHint::ARW {
        return false;
    }
    let Some((width, height, _)) = crate::capture_info::camera_image_size(file_bytes) else {
        return false;
    };
    let (long, short) = (width.max(height), width.min(height));
    let recorded = if frame.d.w >= frame.d.h {
        (long, short)
    } else {
        (short, long)
    };
    // The crop new edits will start with, in buffer axes (`crop_for` centres
    // alike in either orientation, up to a pixel of rounding).
    let Some(crop) = crate::camera_crop::crop_for(base.d.w as u32, base.d.h as u32, recorded)
    else {
        return false;
    };
    (crop.width as usize, crop.height as usize) == (frame.d.w, frame.d.h)
        && (base.p.x + crop.x as usize).abs_diff(frame.p.x) <= 1
        && (base.p.y + crop.y as usize).abs_diff(frame.p.y) <= 1
}

/// Applies the frame fixes of `version` to the areas of an undecoded
/// `raw_image` (before aspect and user crops).
pub(crate) fn apply(raw_image: &mut RawImage, format: FormatHint, version: u32) {
    if version >= CR3_RECORDED_CROP
        && format == FormatHint::CR3
        && let (Some(active), Some(crop)) = (raw_image.active_area, raw_image.crop_area)
    {
        raw_image.active_area = Some(covering_crop(
            active,
            crop,
            raw_image.width,
            raw_image.height,
        ));
    }
    if version < RAF_COMPRESSED_AREA && format == FormatHint::RAF {
        restore_uncompressed_area(raw_image);
    }
}

/// Before version 3 a RAF decoded in the camera's `compressed` mode gets
/// the active area of the camera's base mode back, as the decoder gave it
/// before it selected the mode.
fn restore_uncompressed_area(raw_image: &mut RawImage) {
    if raw_image.camera.mode != "compressed" {
        return;
    }
    let key = (
        raw_image.camera.make.clone(),
        raw_image.camera.model.clone(),
        String::new(),
    );
    let Some(base) = rawler::global_loader().get_cameras().get(&key) else {
        return;
    };
    raw_image.active_area = base.active_area.map(|borders| {
        Rect::new_with_borders(
            rawler::imgop::Dim2::new(raw_image.width, raw_image.height),
            &borders,
        )
    });
}

/// The active area grown to hold the recorded `crop` (within the `width` ×
/// `height` buffer). The crop is what the camera's own JPEG shows; an
/// active area that cuts it is the one that is wrong.
fn covering_crop(active: Rect, crop: Rect, width: usize, height: usize) -> Rect {
    let [l, t, r, b] = active.as_ltrb();
    let [cl, ct, cr, cb] = crop.as_ltrb();
    if cl >= l && ct >= t && cr <= r && cb <= b {
        return active;
    }
    Rect::new_with_points(
        Point::new(l.min(cl), t.min(ct)),
        Point::new(r.max(cr).min(width), b.max(cb).min(height)),
    )
}

/// Where one frame version's development lies in the RAW's buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The developed frame in buffer coordinates (before orientation):
    /// x, y, width, height.
    pub rect: [usize; 4],
    /// The EXIF orientation (1–8) the development applies.
    pub orientation: u16,
}

impl Frame {
    /// Its size as developed (oriented).
    pub fn size(&self) -> (u32, u32) {
        let (w, h) = (self.rect[2] as u32, self.rect[3] as u32);
        if (5..=8).contains(&self.orientation) {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// The frame `version` develops of the RAW in `source`, read from its
/// metadata (no samples decoded). None for files rawler cannot read (also
/// where its decoder panics on them: decode variants of every CR3 render
/// ask for it) and Super CCD layouts (turned by the development).
pub fn frame(source: &RawSource, version: u32) -> Option<Frame> {
    frame_and_sensor(source, version).map(|(frame, _)| frame)
}

/// [`frame`] and the sensor area it lies in, in buffer coordinates
/// (x, y, width, height): the developed area before the camera's aspect
/// crop (Olympus 16:9 or 3:2, Panasonic, Sony) and a DNG's user crop. This
/// is the whole image Lightroom's normalised crop refers to (an Olympus
/// aspect ratio is a crop there; notes:
/// benchmark-results/lc2-lr-render-match-2026-11-01).
pub fn frame_and_sensor(source: &RawSource, version: u32) -> Option<(Frame, [usize; 4])> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_frame(source, version)))
        .ok()
        .flatten()
}

fn read_frame(source: &RawSource, version: u32) -> Option<(Frame, [usize; 4])> {
    let decoder = rawler::get_decoder(source).ok()?;
    let params = RawDecodeParams::default();
    let mut raw_image = decoder.raw_image(source, &params, true).ok()?;
    let orientation = crate::capture_info::phase_one_orientation(source.buf())
        .or_else(|| decoder.raw_metadata(source, &params).ok()?.exif.orientation)
        .or_else(|| crate::capture_info::crw_orientation(source.buf()))
        .filter(|o| (1..=8).contains(o))
        .unwrap_or(1);
    if crate::raw_processing::superccd_layout(&raw_image, source.buf()).is_some() {
        return None;
    }
    let sensor = {
        let mut sensor = raw_image.clone();
        apply(&mut sensor, decoder.format_hint(), version);
        let full = Rect::new(Point::new(0, 0), sensor.dim());
        let active = sensor
            .active_area
            .map(|area| area.intersection(&full))
            .filter(|area| !area.is_empty())
            .unwrap_or(full);
        sensor
            .crop_area
            .map(|crop| crop.intersection(&active))
            .filter(|crop| !crop.is_empty())
            .unwrap_or(active)
    };
    let (_, frame) = crate::raw_processing::developed_areas(
        &mut raw_image,
        source.buf(),
        decoder.as_ref(),
        version,
    );
    (frame.d.w > 0 && frame.d.h > 0).then_some((
        Frame {
            rect: [frame.p.x, frame.p.y, frame.d.w, frame.d.h],
            orientation,
        },
        [sensor.p.x, sensor.p.y, sensor.d.w, sensor.d.h],
    ))
}

/// The oldest frame version that develops the RAW in `source` exactly as
/// `version` does (0 when no fix up to `version` changes its frame): bases
/// of the two decode the same pixels, so caches and previews can share
/// them. `version` itself when the file cannot be read.
pub fn effective(source: &RawSource, version: u32) -> u32 {
    let version = version.min(VERSION);
    let Some(wanted) = frame(source, version) else {
        return version;
    };
    (0..version)
        .rev()
        .take_while(|older| frame(source, *older) == Some(wanted))
        .last()
        .unwrap_or(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ltrb(l: usize, t: usize, r: usize, b: usize) -> Rect {
        Rect::new_with_points(Point::new(l, t), Point::new(r, b))
    }

    /// A little-endian TIFF header whose Exif image size is `width` ×
    /// `height` (PixelXDimension, PixelYDimension).
    fn exif_size(width: u32, height: u32) -> Vec<u8> {
        let mut data = b"II*\0".to_vec();
        data.extend(8u32.to_le_bytes());
        // IFD0 at 8: one entry, the Exif IFD pointer to 26.
        data.extend(1u16.to_le_bytes());
        data.extend(0x8769u16.to_le_bytes());
        data.extend(4u16.to_le_bytes());
        data.extend(1u32.to_le_bytes());
        data.extend(26u32.to_le_bytes());
        data.extend(0u32.to_le_bytes());
        // Exif IFD at 26.
        data.extend(2u16.to_le_bytes());
        for (tag, value) in [(0xa002u16, width), (0xa003, height)] {
            data.extend(tag.to_le_bytes());
            data.extend(4u16.to_le_bytes());
            data.extend(1u32.to_le_bytes());
            data.extend(value.to_le_bytes());
        }
        data.extend(0u32.to_le_bytes());
        data
    }

    /// Frame version 2 leaves a Sony aspect frame uncut only where the
    /// camera crop new edits start with gives it back.
    #[test]
    fn a_sony_aspect_frame_stays_uncut_where_the_camera_crop_gives_it_back() {
        // HX95 16:9: default crop 12,18 4896 × 3672; frame 12,478 4896 × 2752.
        let base = Rect::new(Point::new(12, 18), rawler::imgop::Dim2::new(4896, 3672));
        let frame = Rect::new(Point::new(12, 478), rawler::imgop::Dim2::new(4896, 2752));
        let exif = exif_size(4896, 2752);
        assert!(keeps_sensor(FormatHint::ARW, 2, &exif, base, frame));
        // Up to version 1, and in other formats, the frame is cut as before.
        assert!(!keeps_sensor(FormatHint::ARW, 1, &exif, base, frame));
        assert!(!keeps_sensor(FormatHint::RW2, 2, &exif, base, frame));
        // An Exif size turned with the photo reads along the frame.
        assert!(keeps_sensor(
            FormatHint::ARW,
            2,
            &exif_size(2752, 4896),
            base,
            frame
        ));
        // The recorded size is not the frame's, or the frame is off-centre:
        // the camera crop would not give it back, so it stays cut.
        assert!(!keeps_sensor(
            FormatHint::ARW,
            2,
            &exif_size(4800, 2700),
            base,
            frame
        ));
        let off = Rect::new(Point::new(12, 400), rawler::imgop::Dim2::new(4896, 2752));
        assert!(!keeps_sensor(FormatHint::ARW, 2, &exif, base, off));
        // No Exif size at all.
        assert!(!keeps_sensor(FormatHint::ARW, 2, b"II*\0", base, frame));
    }

    #[test]
    fn the_active_area_holds_the_recorded_crop() {
        // EOS R5 Mark II, APS-C crop: IAD1 crop 272..5359 x 172..3563, active
        // area 132..5243 (exclusive end 5242 as read) x 160..3567, buffer
        // 5376 x 3574.
        let active = ltrb(132, 160, 5242, 3566);
        let crop = ltrb(272, 172, 5360, 3564);
        let grown = covering_crop(active, crop, 5376, 3574);
        assert_eq!(grown.as_ltrb(), [132, 160, 5360, 3566]);
        assert_eq!(crop.intersection(&grown), crop);
        // Full frame (crop inside the active area): unchanged.
        let active = ltrb(260, 164, 8474, 5642);
        assert_eq!(
            covering_crop(active, ltrb(272, 176, 8464, 5640), 8480, 5650),
            active
        );
        // Never beyond the buffer.
        assert_eq!(
            covering_crop(ltrb(10, 10, 90, 90), ltrb(20, 20, 120, 95), 100, 100).as_ltrb(),
            [10, 10, 100, 95]
        );
    }
}

/// Local camera fixtures only; never check photographs into the repo.
#[cfg(test)]
mod fixture_tests {
    use super::*;

    /// Every file of the fixture folder: which frame version changes its
    /// frame, and that version 0 is the frame the development always gave.
    #[test]
    #[ignore = "requires RAWTONIC_RAW_FIXTURES pointing to a local sample directory"]
    fn frame_versions_of_camera_files() {
        let dir = std::env::var("RAWTONIC_RAW_FIXTURES").expect("local fixture directory");
        let mut changed = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let Ok(source) = RawSource::new(&path) else {
                continue;
            };
            let (Some(old), Some(new)) = (frame(&source, 0), frame(&source, VERSION)) else {
                continue;
            };
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let frame_of = crate::raw_processing::developed_frame(&source, 0);
            assert_eq!(frame_of.and_then(|f| f.size), Some(old.size()), "{name}");
            if old != new {
                assert!(may_change(&path.extension().unwrap().to_string_lossy()));
                println!("{name}: {:?} -> {:?}", old.rect, new.rect);
                // The first version that frames it as the newest does.
                let first = (1..=VERSION)
                    .find(|&v| frame(&source, v) == Some(new))
                    .unwrap();
                assert_eq!(effective(&source, VERSION), first, "{name}");
                changed.push(name);
            } else {
                assert_eq!(effective(&source, VERSION), 0, "{name}");
            }
        }
        println!("frame changed on {changed:?}");
    }

    /// Compressed RAFs (X-T2, X-Pro2, X-E3, X-T20) develop the crop the
    /// camera records from version 3 on and the old, narrower frame below
    /// it; uncompressed RAFs develop alike in every version.
    #[test]
    #[ignore = "requires RAWTONIC_RAW_FIXTURES pointing to a local sample directory"]
    fn compressed_rafs_develop_the_recorded_crop_from_version_3() {
        let dir = std::env::var("RAWTONIC_RAW_FIXTURES").expect("local fixture directory");
        let mut compressed = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("raf"))
            {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let source = RawSource::new(&path).unwrap();
            let raw_image = rawler::get_decoder(&source)
                .unwrap()
                .raw_image(&source, &RawDecodeParams::default(), true)
                .unwrap();
            let (old, new) = (
                frame(&source, RAF_COMPRESSED_AREA - 1),
                frame(&source, VERSION),
            );
            if raw_image.camera.mode == "compressed" {
                let crop = raw_image.crop_area.unwrap();
                let new = new.unwrap();
                assert_eq!(new.rect, [crop.p.x, crop.p.y, crop.d.w, crop.d.h], "{name}");
                assert!(old.unwrap().rect[2] < new.rect[2], "{name}");
                assert_eq!(old, frame(&source, 0), "{name}");
                assert_eq!(effective(&source, VERSION), RAF_COMPRESSED_AREA, "{name}");
                println!("{name}: {:?} -> {:?}", old.unwrap().rect, new.rect);
                compressed += 1;
            } else {
                assert_eq!(old, new, "{name}");
            }
        }
        assert!(compressed > 0, "no compressed RAF in the fixture folder");
    }
}
