use std::convert::AsRef;
use std::path::Path;

use rawtonic_rawdecode::format_extensions::is_image_extension;
pub use rawtonic_rawdecode::format_extensions::{NON_RAW_EXTENSIONS, RAW_EXTENSIONS};

pub fn is_raw_file<P: AsRef<Path>>(path: P) -> bool {
    let ext = match path.as_ref().extension().and_then(|s| s.to_str()) {
        Some(e) => e,
        None => return false,
    };

    if RAW_EXTENSIONS
        .iter()
        .any(|(raw_ext, _)| raw_ext.eq_ignore_ascii_case(ext))
    {
        return true;
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

#[cfg(test)]
mod tests {
    use super::is_raw_file;

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
