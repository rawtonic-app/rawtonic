use std::convert::AsRef;
use std::path::Path;

use rawtonic_rawdecode::format_extensions::is_image_extension;
pub use rawtonic_rawdecode::format_extensions::{NON_RAW_EXTENSIONS, RAW_EXTENSIONS};

/// Raster content wins over a misleading suffix. TIFFs must first pass the
/// camera-container classifier, since their first image can be a preview.
pub fn raster_content_type(path: &Path) -> Option<&'static str> {
    if rawtonic_fs::is_cloud_placeholder(path) {
        return None;
    }
    use std::io::Read;
    let mut header = [0; 32];
    let len = std::fs::File::open(path).ok()?.read(&mut header).ok()?;
    let bytes = &header[..len];
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("JPEG")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("PNG")
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        (!rawtonic_rawdecode::raw_tiff::classify_cached(path).is_raw()).then_some("TIFF")
    } else if crate::heif::is_heif(bytes) {
        Some("HEIF")
    } else {
        None
    }
}

pub fn is_raw_file<P: AsRef<Path>>(path: P) -> bool {
    let ext = match path.as_ref().extension().and_then(|s| s.to_str()) {
        Some(e) => e,
        None => return false,
    };

    if RAW_EXTENSIONS
        .iter()
        .any(|(raw_ext, _)| raw_ext.eq_ignore_ascii_case(ext))
    {
        return raster_content_type(path.as_ref()).is_none();
    }
    // Some cameras wrote their raw files as `.tif` (Phase One backs, Canon
    // EOS-1D/1Ds, Kodak DCS); their first TIFF image is a thumbnail, so they
    // are told from RGB TIFFs by content.
    is_tiff_extension(ext) && rawtonic_rawdecode::raw_tiff::classify_cached(path.as_ref()).is_raw()
}

pub fn is_tiff_extension(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("tif") || ext.eq_ignore_ascii_case("tiff")
}

pub fn is_supported_image_file<P: AsRef<Path>>(path: P) -> bool {
    let path = path.as_ref();

    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
    {
        return false;
    }

    let ext = match path.extension().and_then(|s| s.to_str()) {
        Some(e) => e,
        None => return false,
    };

    is_image_extension(ext)
}

/// Whether a file handed to the app (Open With, a launch path) is a photo it
/// can show: a known photo extension, or a raster image under another name
/// whose bytes say what it is (`.jpe`, `.jfif`, …; the raster decoder reads
/// the content). TIFF content under an unknown name is refused: camera RAWs
/// the app does not decode yet are TIFF containers too (macOS offers the app
/// for every camera RAW type it knows).
pub fn is_openable_photo<P: AsRef<Path>>(path: P) -> bool {
    let path = path.as_ref();
    if is_supported_image_file(path) {
        return true;
    }
    let Ok(reader) = image::ImageReader::open(path).and_then(|r| r.with_guessed_format()) else {
        return false;
    };
    reader
        .format()
        .is_some_and(|format| format != image::ImageFormat::Tiff)
}

#[cfg(test)]
mod tests {
    use super::is_raw_file;

    #[test]
    fn a_file_handed_over_opens_by_extension_or_raster_content() {
        use super::is_openable_photo;
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let mut jpeg = Vec::new();
        image::RgbImage::new(2, 2)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        assert!(is_openable_photo(write("shot.NEF", b"not checked")));
        assert!(is_openable_photo(write("photo.jpe", &jpeg)));
        // A RAW format the app does not decode yet: a TIFF container.
        assert!(!is_openable_photo(write(
            "merged.nefx",
            b"II*\0\x08\0\0\0\0\0"
        )));
        assert!(!is_openable_photo(write("unknown.axr", b"\0\x01\x02\x03")));
        assert!(!is_openable_photo(dir.path().join("missing.pwl")));
    }

    /// A TIFF header followed by Phase One's IIQ magic and an empty IFD:
    /// a raw container, whatever the decoder makes of it.
    fn iiq_tiff() -> Vec<u8> {
        let mut bytes = b"II*\0\x10\0\0\0IIIICwaR".to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        bytes
    }

    fn rgb_tiff(path: &std::path::Path) {
        image::RgbImage::from_pixel(64, 48, image::Rgb([90, 120, 150]))
            .save_with_format(path, image::ImageFormat::Tiff)
            .unwrap();
    }

    #[test]
    fn raster_content_overrides_raw_suffix() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["photo.NEF", "photo.CR2"] {
            let path = dir.path().join(name);
            for format in [
                image::ImageFormat::Jpeg,
                image::ImageFormat::Png,
                image::ImageFormat::Tiff,
            ] {
                image::RgbImage::new(2, 2)
                    .save_with_format(&path, format)
                    .unwrap();
                assert!(!is_raw_file(&path), "{name}: {format:?}");
                let bytes = std::fs::read(&path).unwrap();
                let decoded = crate::image_loader::load_base_image(
                    &bytes,
                    path.to_str().unwrap(),
                    false,
                    &crate::image_loader::BaseImageOptions::default(),
                    None,
                )
                .unwrap();
                assert_eq!((decoded.width(), decoded.height()), (2, 2));
            }
            std::fs::write(&path, iiq_tiff()).unwrap();
            assert!(is_raw_file(&path));
            // TIFF/EP CFA photometric tag, with no decodable pixel data.
            // A RAW must fail as RAW rather than opening a TIFF preview.
            let mut cfa = b"II*\0\x08\0\0\0".to_vec();
            cfa.extend_from_slice(&1_u16.to_le_bytes());
            cfa.extend_from_slice(&262_u16.to_le_bytes());
            cfa.extend_from_slice(&3_u16.to_le_bytes());
            cfa.extend_from_slice(&1_u32.to_le_bytes());
            cfa.extend_from_slice(&32803_u32.to_le_bytes());
            cfa.extend_from_slice(&0_u32.to_le_bytes());
            std::fs::write(&path, cfa).unwrap();
            assert!(is_raw_file(&path));
        }
    }

    #[test]
    fn tiffs_route_by_content_and_follow_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Capture.TIF");
        std::fs::write(&path, iiq_tiff()).unwrap();
        assert!(is_raw_file(&path));
        // Replaced by an RGB TIFF at the same path (size and time change).
        rgb_tiff(&path);
        assert!(!is_raw_file(&path));
        let tiff = dir.path().join("scan.tiff");
        rgb_tiff(&tiff);
        assert!(!is_raw_file(&tiff));
        // Missing files and other extensions keep their extension's answer.
        assert!(!is_raw_file(dir.path().join("missing.tif")));
        assert!(is_raw_file("missing.NEF"));
        assert!(!is_raw_file(dir.path().join("photo.jpg")));
    }
}
