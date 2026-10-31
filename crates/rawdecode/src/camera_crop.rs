//! The aspect ratio set in the camera, where the RAW keeps the whole sensor
//! (ADR 0020, option C).
//!
//! Some cameras record an aspect ratio set in the camera (16:9 on a 4:3
//! sensor, 1:1 on a 3:2 one) but keep the whole sensor in the RAW: the
//! Samsung EX2F only as the size of its own JPEG (Exif image size 4000 ×
//! 2248 over a 4000 × 3000 sensor), Canon in AspectInfo (CR2, CR3: the
//! cropped image's size and place, centred in the developed frame on every
//! sample). Lightroom and Camera Raw start such photos with that frame as
//! a crop the photographer can widen. Rawtonic keeps the developed frame
//! (the lens warp corrects the whole sensor) and gives new edits the
//! camera's frame as an ordinary crop (`camera_crop`). Formats whose aspect
//! tags the decode already crops to (Panasonic, Olympus / OM System, Sony:
//! `raw_processing::apply_aspect_crop`) develop that frame, so the sizes
//! agree and no crop follows; a DNG's own default crop is its frame (its
//! Exif size is the original camera's).

use rawler::rawsource::RawSource;

/// A crop in the developed, oriented frame's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CameraCrop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Aspect ratios closer than this (|ln(a / b)|) are the same frame: the
/// cameras' JPEG sizes round their sensor's (6000 × 4000 of 6024 × 4020).
/// 3:2 against 16:9 is 0.17, 4:3 against 3:2 0.12.
const SAME_ASPECT: f64 = 0.02;

/// The crop of the frame the camera recorded for the RAW in `source` when
/// it develops frame version `frame_version` (`crop_for` of the camera
/// image's size). None where that frame is the developed one, or the file
/// records no usable size.
pub fn camera_crop(source: &RawSource, frame_version: u32) -> Option<CameraCrop> {
    // rawler's decoders panic on some files' metadata (`source_frame::frame`).
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(source, frame_version)))
        .ok()
        .flatten()
}

fn read(source: &RawSource, frame_version: u32) -> Option<CameraCrop> {
    if rawler::get_decoder(source).ok()?.format_hint() == rawler::decoders::FormatHint::DNG {
        return None;
    }
    let (width, height) = crate::source_frame::frame(source, frame_version)?.size();
    let camera = crate::capture_info::camera_image_size(source.buf())?;
    crop_for(width, height, camera)
}

/// The camera image `camera` (width, height in any orientation) as a crop
/// of a `width` × `height` developed frame, centred, turned to the frame's
/// orientation: its own size where it fits, else the largest rectangle of
/// its aspect ratio. None where the aspect ratio is the frame's.
pub fn crop_for(width: u32, height: u32, camera: (u32, u32)) -> Option<CameraCrop> {
    let (long, short) = (width.max(height) as f64, width.min(height) as f64);
    let (camera_long, camera_short) =
        (camera.0.max(camera.1) as f64, camera.0.min(camera.1) as f64);
    // A thumbnail's or a reduced JPEG's size tells nothing reliable, nor do
    // degenerate sizes.
    if short < 1.0 || camera_short < 1.0 || camera_long * 2.0 < long {
        return None;
    }
    let (frame_ratio, camera_ratio) = (long / short, camera_long / camera_short);
    if (camera_ratio / frame_ratio).ln().abs() < SAME_ASPECT || camera_ratio > 4.0 {
        return None;
    }
    // Along the frame's long side.
    let (crop_long, crop_short) = if camera_long <= long && camera_short <= short {
        (camera_long, camera_short)
    } else if camera_ratio > frame_ratio {
        (long, (long / camera_ratio).round())
    } else {
        ((short * camera_ratio).round(), short)
    };
    let (crop_width, crop_height) = if width >= height {
        (crop_long, crop_short)
    } else {
        (crop_short, crop_long)
    };
    let (crop_width, crop_height) = (crop_width as u32, crop_height as u32);
    Some(CameraCrop {
        x: (width - crop_width) / 2,
        y: (height - crop_height) / 2,
        width: crop_width,
        height: crop_height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_camera_frame_as_a_crop() {
        // Samsung EX2F 16:9: 4000 × 2248 over the 4000 × 3000 sensor.
        assert_eq!(
            crop_for(4000, 3000, (4000, 2248)),
            Some(CameraCrop {
                x: 0,
                y: 376,
                width: 4000,
                height: 2248
            })
        );
        // Turned upright: the frame's long side is its height.
        assert_eq!(
            crop_for(3000, 4000, (4000, 2248)),
            Some(CameraCrop {
                x: 376,
                y: 0,
                width: 2248,
                height: 4000
            })
        );
        // 1:1 on a 3:2 frame: the short side.
        assert_eq!(
            crop_for(6000, 4000, (4000, 4000)),
            Some(CameraCrop {
                x: 1000,
                y: 0,
                width: 4000,
                height: 4000
            })
        );
        // The same aspect, rounded differently: no crop.
        assert_eq!(crop_for(6024, 4020, (6000, 4000)), None);
        assert_eq!(crop_for(5472, 3080, (5472, 3080)), None);
        // A thumbnail's size: no crop.
        assert_eq!(crop_for(6000, 4000, (160, 120)), None);
        assert_eq!(crop_for(0, 4000, (4000, 2248)), None);
    }
}

/// Local camera fixtures only; never check photographs into the repo.
#[cfg(test)]
mod fixture_tests {
    use super::*;

    /// Tool and check: every file of the fixture folder with its developed
    /// frame, the camera image's size and the crop new edits get.
    #[test]
    #[ignore = "requires RAWTONIC_RAW_FIXTURES pointing to a local sample directory"]
    fn camera_crops_of_camera_files() {
        let dir = std::env::var("RAWTONIC_RAW_FIXTURES").expect("local fixture directory");
        let mut paths: Vec<_> = walk(std::path::Path::new(&dir));
        paths.sort();
        for path in paths {
            let Ok(source) = RawSource::new(&path) else {
                continue;
            };
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let frame = crate::source_frame::frame(&source, crate::source_frame::VERSION);
            let camera = crate::capture_info::camera_image_size(source.buf());
            let crop = camera_crop(&source, crate::source_frame::VERSION);
            println!(
                "{name}\t{:?}\t{camera:?}\t{crop:?}\t{:?}",
                frame.map(|f| f.size()),
                frame.map(|f| f.rect)
            );
        }
    }

    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }
}
