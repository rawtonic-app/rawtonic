//! Test material for checking the monitor transform on a real display
//! (notes: benchmark-results/mcm1-2026-11-27/REPORT.md, WMCM1):
//!
//! `cargo run -p rawtonic-display-profile --example monitor_check -- OUT`
//!
//! writes into the folder OUT
//! - `wide-test-monitor.icc`: a matrix/TRC display profile with Adobe RGB
//!   (1998) primaries and a 2.2 curve (moxcms' own definition), to assign
//!   to a display as if it were a wide-gamut monitor;
//! - `patches.png`: ten sRGB patches, 120 × 120 px each, in one row;
//! - `expected.txt`: each patch's sRGB value and the value the display
//!   should then show for it (the transform the app builds from that
//!   profile), to compare with a screenshot.
//!
//! Existing files are never written over: the run fails instead.
//!
//! On Linux, `--features system-lookup -- --lookup` prints what colord / X11 hold for the display
//! instead.

use std::io::Write;

/// A new file at `path`; an existing one is an error, never truncated.
fn create_new(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

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
const SIDE: u32 = 120;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arg = std::env::args()
        .nth(1)
        .ok_or("usage: monitor_check OUT | --lookup")?;
    if arg == "--lookup" {
        #[cfg(all(target_os = "linux", feature = "system-lookup"))]
        {
            let found = rawtonic_display_profile::linux::profile_of_output(None);
            match &found {
                rawtonic_display_profile::SystemProfile::Assigned { icc, location } => println!(
                    "assigned: {location} ({:?}), transform: {:?}",
                    rawtonic_display_profile::description(icc),
                    rawtonic_display_profile::display_lut(
                        rawtonic_display_profile::Encoding::Srgb,
                        icc
                    )
                    .map(|lut| lut.is_some())
                ),
                other => println!("{other:?}"),
            }
        }
        #[cfg(not(all(target_os = "linux", feature = "system-lookup")))]
        println!(
            "--lookup is for Linux with --features system-lookup; Windows looks up the profile of a window's monitor"
        );
        return Ok(());
    }
    let out = std::path::PathBuf::from(arg);
    std::fs::create_dir_all(&out)?;
    let mut profile = moxcms::ColorProfile::new_adobe_rgb();
    profile.description = Some(moxcms::ProfileText::Localizable(vec![
        moxcms::LocalizableString::new(
            "en".into(),
            "US".into(),
            "Rawtonic wide test monitor".into(),
        ),
    ]));
    let icc = profile.encode()?;
    create_new(&out.join("wide-test-monitor.icc"))?.write_all(&icc)?;

    let width = SIDE * PATCHES.len() as u32;
    let mut rgb = Vec::with_capacity((width * SIDE * 3) as usize);
    for _ in 0..SIDE {
        for patch in PATCHES {
            for _ in 0..SIDE {
                rgb.extend(patch);
            }
        }
    }
    let file = std::io::BufWriter::new(create_new(&out.join("patches.png"))?);
    let mut encoder = png::Encoder::new(file, width, SIDE);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    // Tagged sRGB, so every reader takes the values as sRGB.
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    encoder.write_header()?.write_image_data(&rgb)?;

    let lut =
        rawtonic_display_profile::display_lut(rawtonic_display_profile::Encoding::Srgb, &icc)?
            .ok_or("the test profile converts nothing")?;
    let mut expected = create_new(&out.join("expected.txt"))?;
    writeln!(
        expected,
        "patch\tx_centre\tsRGB\tdisplay value with wide-test-monitor.icc"
    )?;
    for (i, patch) in PATCHES.into_iter().enumerate() {
        let shown = lut
            .apply(patch.map(|v| v as f32 / 255.0))
            .map(|v| (v * 255.0).round() as u8);
        writeln!(
            expected,
            "{i}\t{}\t{patch:?}\t{shown:?}",
            i as u32 * SIDE + SIDE / 2
        )?;
    }
    println!("wrote {}", out.display());
    Ok(())
}
