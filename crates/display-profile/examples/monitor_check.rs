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
//! Existing files are never written over: the run fails instead (each
//! file is published whole in one step, rawtonic-fs `persist_atomic`; a
//! volume that cannot do that fails cleanly).
//!
//! On Linux, `--features system-lookup -- --lookup` prints what colord / X11 hold for the display
//! instead; `-- --x11-set FILE N` publishes FILE as monitor N's X11 profile
//! (`_ICC_PROFILE`, `_ICC_PROFILE_N` for N > 0, as colour tools such as
//! `dispwin -I` do) and `-- --x11-clear N` removes it again (a GUI check on
//! a desktop without colord; note what was there first).

use std::io::Write;

/// Writes `bytes` to a temporary file beside `path` and publishes it there
/// without clobbering (rawtonic-fs): an existing file is an error, and a
/// failed write leaves nothing behind.
fn publish(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let folder = path.parent().unwrap_or(std::path::Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(folder)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    rawtonic_fs::no_clobber::persist_atomic(temp, path)
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
    if arg == "--x11-set" || arg == "--x11-clear" {
        #[cfg(all(target_os = "linux", feature = "system-lookup"))]
        {
            let args: Vec<String> = std::env::args().skip(2).collect();
            let (file, monitor) = match (arg.as_str(), args.as_slice()) {
                ("--x11-set", [file, n]) => (Some(std::fs::read(file)?), n.parse::<usize>()?),
                ("--x11-clear", [n]) => (None, n.parse::<usize>()?),
                _ => return Err("usage: --x11-set FILE N | --x11-clear N".into()),
            };
            x11_set(file.as_deref(), monitor)?;
            println!("done");
        }
        #[cfg(not(all(target_os = "linux", feature = "system-lookup")))]
        println!("--x11-set/--x11-clear are for Linux with --features system-lookup");
        return Ok(());
    }
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
    publish(&out.join("wide-test-monitor.icc"), &icc)?;

    let width = SIDE * PATCHES.len() as u32;
    let mut rgb = Vec::with_capacity((width * SIDE * 3) as usize);
    for _ in 0..SIDE {
        for patch in PATCHES {
            for _ in 0..SIDE {
                rgb.extend(patch);
            }
        }
    }
    let mut png_bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut png_bytes, width, SIDE);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    // Tagged sRGB, so every reader takes the values as sRGB.
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgb)?;
    writer.finish()?;
    publish(&out.join("patches.png"), &png_bytes)?;

    let lut =
        rawtonic_display_profile::display_lut(rawtonic_display_profile::Encoding::Srgb, &icc)?
            .ok_or("the test profile converts nothing")?;
    let mut expected = Vec::new();
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
    publish(&out.join("expected.txt"), &expected)?;
    println!("wrote {}", out.display());
    Ok(())
}

/// Sets (or with `None` deletes) monitor `monitor`'s X11 ICC profile atom.
#[cfg(all(target_os = "linux", feature = "system-lookup"))]
fn x11_set(icc: Option<&[u8]>, monitor: usize) -> Result<(), Box<dyn std::error::Error>> {
    let xlib = x11_dl::xlib::Xlib::open()?;
    let name = match monitor {
        0 => "_ICC_PROFILE".to_string(),
        n => format!("_ICC_PROFILE_{n}"),
    };
    let name = std::ffi::CString::new(name)?;
    // SAFETY: the display is opened and closed here; `icc` outlives the call.
    unsafe {
        let display = (xlib.XOpenDisplay)(std::ptr::null());
        if display.is_null() {
            return Err("no X display (DISPLAY)".into());
        }
        let root = (xlib.XDefaultRootWindow)(display);
        let atom = (xlib.XInternAtom)(display, name.as_ptr(), 0);
        let cardinal = (xlib.XInternAtom)(display, c"CARDINAL".as_ptr(), 0);
        match icc {
            Some(icc) => {
                (xlib.XChangeProperty)(
                    display,
                    root,
                    atom,
                    cardinal,
                    8,
                    x11_dl::xlib::PropModeReplace,
                    icc.as_ptr(),
                    icc.len() as i32,
                );
            }
            None => {
                (xlib.XDeleteProperty)(display, root, atom);
            }
        }
        (xlib.XSync)(display, 0);
        (xlib.XCloseDisplay)(display);
    }
    Ok(())
}
