//! HEIC tests: files written by ImageIO at test time (`encode_heic`), no
//! photographs.

use super::*;

#[test]
fn heif_brands_are_recognised_and_avif_is_left_to_avif() {
    let ftyp = |major: &[u8; 4], compatible: &[&[u8; 4]]| {
        let mut bytes = ((16 + 4 * compatible.len()) as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(major);
        bytes.extend_from_slice(&[0; 4]);
        compatible.iter().for_each(|b| bytes.extend_from_slice(*b));
        bytes.extend_from_slice(&[0; 16]);
        bytes
    };
    assert!(is_heif(&ftyp(b"heic", &[b"mif1", b"heic"])));
    // Canon and Sony .HIF.
    assert!(is_heif(&ftyp(b"heix", &[b"mif1", b"heix"])));
    assert!(is_heif(&ftyp(b"mif1", &[b"heic"])));
    assert!(!is_heif(&ftyp(b"avif", &[b"mif1", b"miaf"])));
    assert!(!is_heif(&ftyp(b"mif1", &[b"avif"])));
    assert!(!is_heif(&ftyp(b"isom", &[b"mp41"])));
    assert!(!is_heif(&[0xFF, 0xD8, 0xFF, 0xE0]));
    assert!(crate::formats::is_supported_image_file("IMG_0001.HEIC"));
    assert!(crate::formats::is_supported_image_file("A7C_0001.HIF"));
    assert!(crate::formats::is_supported_image_file("photo.heif"));
    assert!(!crate::formats::is_raw_file("IMG_0001.HEIC"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn other_systems_say_heic_is_not_supported_yet() {
    let mut bytes = 24u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
    let error = crate::image_loader::load_image_with_orientation(&bytes, None).unwrap_err();
    assert_eq!(error.to_string(), UNSUPPORTED);
    assert!(preview(&bytes, 256).is_none());
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::heif::imageio::{encode_heic, orientation, reference_linear_srgb};

    fn linear(v: f32) -> f32 {
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    /// CIELAB (D65) of linear sRGB.
    fn lab(lin: [f32; 3]) -> [f32; 3] {
        let x = 0.4124 * lin[0] + 0.3576 * lin[1] + 0.1805 * lin[2];
        let y = 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
        let z = 0.0193 * lin[0] + 0.1192 * lin[1] + 0.9505 * lin[2];
        let f = |t: f32| {
            if t > 216.0 / 24389.0 {
                t.cbrt()
            } else {
                (24389.0 / 27.0 * t + 16.0) / 116.0
            }
        };
        let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }

fn de2000(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (l1, a1, b1) = (a[0] as f64, a[1] as f64, a[2] as f64);
    let (l2, a2, b2) = (b[0] as f64, b[1] as f64, b[2] as f64);
    let c1 = (a1 * a1 + b1 * b1).sqrt();
    let c2 = (a2 * a2 + b2 * b2).sqrt();
    let cb = (c1 + c2) / 2.0;
    let g = 0.5 * (1.0 - (cb.powi(7) / (cb.powi(7) + 25f64.powi(7))).sqrt());
    let a1p = a1 * (1.0 + g);
    let a2p = a2 * (1.0 + g);
    let c1p = (a1p * a1p + b1 * b1).sqrt();
    let c2p = (a2p * a2p + b2 * b2).sqrt();
    let h = |b: f64, a: f64| {
        if a == 0.0 && b == 0.0 {
            0.0
        } else {
            let v = b.atan2(a).to_degrees();
            if v < 0.0 { v + 360.0 } else { v }
        }
    };
    let h1p = h(b1, a1p);
    let h2p = h(b2, a2p);
    let dlp = l2 - l1;
    let dcp = c2p - c1p;
    let mut dhp = h2p - h1p;
    if c1p * c2p != 0.0 {
        if dhp > 180.0 {
            dhp -= 360.0
        } else if dhp < -180.0 {
            dhp += 360.0
        }
    } else {
        dhp = 0.0
    }
    let dhh = 2.0 * (c1p * c2p).sqrt() * (dhp.to_radians() / 2.0).sin();
    let lbp = (l1 + l2) / 2.0;
    let cbp = (c1p + c2p) / 2.0;
    let hbp = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (hbp - 30.0).to_radians().cos()
        + 0.24 * (2.0 * hbp).to_radians().cos()
        + 0.32 * (3.0 * hbp + 6.0).to_radians().cos()
        - 0.20 * (4.0 * hbp - 63.0).to_radians().cos();
    let dtheta = 30.0 * (-((hbp - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cbp.powi(7) / (cbp.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (lbp - 50.0).powi(2) / (20.0 + (lbp - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cbp;
    let sh = 1.0 + 0.015 * cbp * t;
    let rt = -(2.0 * dtheta).to_radians().sin() * rc;
    ((dlp / sl).powi(2) + (dcp / sc).powi(2) + (dhh / sh).powi(2) + rt * (dcp / sc) * (dhh / sh))
        .sqrt() as f32
}

    /// Ramps, greys and colours outside sRGB (saturated Display P3).
    fn p3_chart(width: usize, height: usize) -> Vec<[u16; 3]> {
        (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| {
                    let v = (x * 65535 / (width - 1)) as u16;
                    match y * 6 / height {
                        0 => [v, v, v],
                        1 => [65535, v, 0],
                        2 => [0, 65535, v],
                        3 => [v, 0, 65535],
                        4 => [52000, 30000 + v / 4, 12000],
                        _ => [9000, 20000 + v / 3, 50000],
                    }
                })
            })
            .collect()
    }

    #[test]
    fn display_p3_heic_matches_imageio_s_own_colour_within_half_a_delta_e() {
        let (width, height) = (256, 96);
        let bytes = encode_heic(width, height, &p3_chart(width, height), "kCGColorSpaceDisplayP3", 1);
        assert!(is_heif(&bytes));
        let ours = crate::image_loader::load_image_with_orientation(&bytes, None)
            .unwrap()
            .into_rgb32f();
        let (w, h, reference) = reference_linear_srgb(&bytes).unwrap();
        assert_eq!((ours.width() as usize, ours.height() as usize), (w, h));
        let (mut sum, mut max, mut outside) = (0.0f64, 0.0f32, 0usize);
        for (ours, reference) in ours.pixels().zip(&reference) {
            outside += usize::from(reference.iter().any(|&v| v < -0.01));
            let e = de2000(lab(ours.0.map(linear)), lab(*reference));
            sum += f64::from(e);
            max = max.max(e);
        }
        let mean = sum / reference.len() as f64;
        // Colours outside sRGB stay (values below 0), not clipped.
        assert!(outside > reference.len() / 4, "{outside} pixels outside sRGB");
        assert!(mean < 0.5 && max < 0.5, "ΔE00 mean {mean:.3}, max {max:.3}");
    }

    #[test]
    fn ten_bit_heic_keeps_more_than_eight_bits() {
        // A shallow ramp: 1024 columns over a quarter of the range.
        let (width, height) = (1024, 64);
        let pixels: Vec<[u16; 3]> = (0..height)
            .flat_map(|_| {
                (0..width).map(|x| {
                    let v = 20000 + (x * 16) as u16;
                    [v, v, v]
                })
            })
            .collect();
        let bytes = encode_heic(width, height, &pixels, "kCGColorSpaceSRGB", 1);
        let image = crate::image_loader::load_image_with_orientation(&bytes, None)
            .unwrap()
            .into_rgb32f();
        let mut levels: Vec<u32> = image
            .rows()
            .nth(32)
            .unwrap()
            .map(|p| (p.0[1] * 65535.0).round() as u32)
            .collect();
        levels.dedup();
        // 8 bits give at most 65 levels over this range; 10 bits about 256.
        assert!(levels.len() > 150, "{} levels", levels.len());
    }

    #[test]
    fn heic_orientations_follow_the_container_as_imageio_reads_them() {
        let (width, height) = (64, 32);
        // A different colour in each quadrant.
        let pixels: Vec<[u16; 3]> = (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| match (x < width / 2, y < height / 2) {
                    (true, true) => [60000, 0, 0],
                    (false, true) => [0, 60000, 0],
                    (true, false) => [0, 0, 60000],
                    (false, false) => [60000, 60000, 60000],
                })
            })
            .collect();
        let upright = encode_heic(width, height, &pixels, "kCGColorSpaceSRGB", 1);
        let upright = crate::image_loader::load_image_with_orientation(&upright, None).unwrap();
        for code in 1..=8u16 {
            let bytes = encode_heic(width, height, &pixels, "kCGColorSpaceSRGB", i64::from(code));
            assert_eq!(orientation(&bytes), Some(i64::from(code)), "ImageIO");
            assert_eq!(crate::avif::orientation(&bytes), code, "container");
            assert_eq!(crate::image_loader::source_orientation(&bytes, "x.heic"), code);
            let loaded = crate::image_loader::load_image_with_orientation(&bytes, None)
                .unwrap()
                .into_rgb32f();
            let expected = crate::image_processing::apply_orientation(
                upright.clone(),
                rawler::Orientation::from_u16(code),
            )
            .into_rgb32f();
            assert_eq!(loaded.dimensions(), expected.dimensions(), "orientation {code}");
            // Each quadrant's centre has its colour.
            for (x, y) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let at = |image: &image::Rgb32FImage| {
                    let (w, h) = image.dimensions();
                    image.get_pixel((w as f32 * x) as u32, (h as f32 * y) as u32).0
                };
                let (a, b) = (at(&loaded), at(&expected));
                assert!(
                    a.iter().zip(&b).all(|(a, b)| (a - b).abs() < 0.02),
                    "orientation {code}: {a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn heic_preview_is_oriented_and_sized() {
        let (width, height) = (512, 256);
        let bytes = encode_heic(width, height, &p3_chart(width, height), "kCGColorSpaceDisplayP3", 6);
        let image = preview(&bytes, 128).unwrap();
        // Rotated a quarter turn, at most 128 on its long side, at least 64
        // on its short side.
        assert!(image.height() > image.width(), "{}×{}", image.width(), image.height());
        assert!(image.height() <= 256 && image.width() >= 64);
    }
}
