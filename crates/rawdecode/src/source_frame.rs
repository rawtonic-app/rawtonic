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

use rawler::RawImage;
use rawler::decoders::{FormatHint, RawDecodeParams};
use rawler::imgop::{Point, Rect};
use rawler::rawsource::RawSource;

/// The newest frame version: new edits start with it.
pub const VERSION: u32 = 1;

/// Version 1's fix: a CR3's active area holds its recorded crop.
const CR3_RECORDED_CROP: u32 = 1;

/// Whether some frame version can change the frame of a file with this
/// extension (lower case, no dot): only those files are read to find out.
pub fn may_change(extension: &str) -> bool {
    extension.eq_ignore_ascii_case("cr3")
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
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_frame(source, version)))
        .ok()
        .flatten()
}

fn read_frame(source: &RawSource, version: u32) -> Option<Frame> {
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
    let (_, frame) = crate::raw_processing::developed_areas(
        &mut raw_image,
        source.buf(),
        decoder.as_ref(),
        version,
    );
    (frame.d.w > 0 && frame.d.h > 0).then_some(Frame {
        rect: [frame.p.x, frame.p.y, frame.d.w, frame.d.h],
        orientation,
    })
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
                assert_eq!(effective(&source, VERSION), VERSION);
                changed.push(name);
            } else {
                assert_eq!(effective(&source, VERSION), 0, "{name}");
            }
        }
        println!("frame changed on {changed:?}");
    }
}
