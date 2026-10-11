//! Ignored evaluation tools for engine modules that need the app's image
//! loader (EXIF cache, Apple RAW, preprocessing settings). Moved here from
//! `rawtonic_engine::{auto_adjust, spot_removal}` (ADR 0009).

/// Private evaluation: RAWTONIC_AUTO_SAMPLES lists "name<TAB>path" lines;
/// writes NAME.json with the auto recipe into RAWTONIC_AUTO_OUT.
#[test]
#[ignore = "requires private RAW samples"]
fn private_raw_auto_recipes() {
    let (Ok(list), Ok(out)) = (
        std::env::var("RAWTONIC_AUTO_SAMPLES"),
        std::env::var("RAWTONIC_AUTO_OUT"),
    ) else {
        eprintln!("SKIP: RAWTONIC_AUTO_SAMPLES / RAWTONIC_AUTO_OUT not set");
        return;
    };
    let settings = crate::app_settings::AppSettings::default();
    for line in std::fs::read_to_string(list).unwrap().lines() {
        let Some((name, path)) = line.split_once('\t') else {
            continue;
        };
        let bytes = std::fs::read(path).unwrap();
        let image =
            crate::image_loader::load_base_image_from_bytes(&bytes, path, false, &settings, None)
                .unwrap();
        let auto = if std::env::var("RAWTONIC_AUTO_LEGACY").is_ok() {
            crate::image_processing::auto_results_to_json(
                &crate::image_processing::perform_auto_analysis(&image),
            )
        } else {
            crate::image_processing::auto_adjustments_json(&image, path, true, None, None)
        };
        let mut json = serde_json::json!({});
        crate::auto_adjust::merge_auto_adjustments(&mut json, &auto);
        let look = crate::baseline_exposure::base_look(path);
        println!(
            "PRED {name} {:?}",
            crate::auto_adjust::predicted_display_percentiles(
                &image,
                look.baseline_ev,
                crate::auto_adjust::DisplayCurve::for_look(&look),
                0.0
            )
        );
        std::fs::write(format!("{out}/{name}.json"), json.to_string()).unwrap();
        println!("{name} {json}");
    }
}

/// Tool: Visualize Spots views (sensitivity 50, 1600 px) of `$VIS_FILES`
/// (`;`-separated) written next to `$VIS_OUT`.
#[test]
#[ignore = "tool: needs VIS_FILES and VIS_OUT"]
fn print_visualize_spots() {
    let out = std::path::PathBuf::from(std::env::var("VIS_OUT").unwrap());
    for path in std::env::var("VIS_FILES").unwrap().split(';') {
        let bytes = std::fs::read(path).unwrap();
        let image = crate::image_loader::load_base_image_from_bytes(
            &bytes,
            path,
            false,
            &crate::app_settings::AppSettings::default(),
            None,
        )
        .unwrap();
        let (band, median) = crate::spot_removal::spot_band_pass(&image, 1600);
        let stem = std::path::Path::new(path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        crate::spot_removal::visualize(&band, median, 50.0)
            .save(out.join(format!("{stem}_vis.png")))
            .unwrap();
    }
}

/// Tool: the automatic source for `$SPOT` = "file;x;y;radius" (base
/// frame of an unedited photo).
#[test]
#[ignore = "tool: needs SPOT"]
fn print_spot_source() {
    let spec = std::env::var("SPOT").unwrap();
    let parts: Vec<&str> = spec.split(';').collect();
    let bytes = std::fs::read(parts[0]).unwrap();
    let image = crate::image_loader::load_base_image_from_bytes(
        &bytes,
        parts[0],
        false,
        &crate::app_settings::AppSettings::default(),
        None,
    )
    .unwrap();
    let n = |i: usize| parts[i].parse::<f64>().unwrap();
    println!(
        "{:?}",
        crate::spot_removal::find_source(&image, &serde_json::json!({}), n(1), n(2), n(3))
    );
}

/// Tool: the automatic source for a brush stroke, `$STROKE` =
/// "file;radius;x1,y1;x2,y2;…" (base frame of an unedited photo).
#[test]
#[ignore = "tool: needs STROKE"]
fn print_stroke_source() {
    let spec = std::env::var("STROKE").unwrap();
    let parts: Vec<&str> = spec.split(';').collect();
    let bytes = std::fs::read(parts[0]).unwrap();
    let image = crate::image_loader::load_base_image_from_bytes(
        &bytes,
        parts[0],
        false,
        &crate::app_settings::AppSettings::default(),
        None,
    )
    .unwrap();
    let points: Vec<crate::spot_removal::StrokePoint> = parts[2..]
        .iter()
        .map(|p| {
            let (x, y) = p.split_once(',').unwrap();
            crate::spot_removal::StrokePoint {
                x: x.parse().unwrap(),
                y: y.parse().unwrap(),
            }
        })
        .collect();
    println!(
        "{:?}",
        crate::spot_removal::find_stroke_source(
            &image,
            &serde_json::json!({}),
            &points,
            parts[1].parse().unwrap()
        )
    );
}

/// Tool: the automatic lateral CA estimate (red and blue scale) of the RAWs
/// in `$CA_FILES` (`;`-separated), on the unedited base image.
#[test]
#[ignore = "tool: needs CA_FILES"]
fn print_lateral_ca() {
    for path in std::env::var("CA_FILES").unwrap().split(';') {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(image) = crate::image_loader::load_base_image_from_bytes(
            &bytes,
            path,
            false,
            &crate::app_settings::AppSettings::default(),
            None,
        ) else {
            println!("{path}\tdecode failed");
            continue;
        };
        let start = std::time::Instant::now();
        let estimate = rawtonic_engine::lateral_ca::estimate(&image.to_rgb32f());
        println!("{path}\t{estimate:?}\t{:?}", start.elapsed());
    }
}

/// Tool: the camera's own lens data (`embedded_lens::read`) of the RAWs in
/// `$LENS_FILES` (`;`-separated) as warp parameters, with the read time.
#[test]
#[ignore = "tool: needs LENS_FILES"]
fn print_embedded_lens() {
    for path in std::env::var("LENS_FILES").unwrap().split(';') {
        let start = std::time::Instant::now();
        let data = rawtonic_engine::embedded_lens::read(std::path::Path::new(path));
        let elapsed = start.elapsed();
        let params = data
            .as_ref()
            .and_then(rawtonic_engine::embedded_lens::to_params);
        println!(
            "{path}\t{elapsed:?}\t{}",
            params
                .map(|p| serde_json::to_string(&p).unwrap_or_default())
                .unwrap_or_default()
        );
    }
}

/// Tool: the Upright recipes of the RAWs in `$UP_FILES` (`;`-separated) for
/// Auto, Level, Vertical and Full, computed as the app's `calculate_upright`
/// does (on the lens-corrected base image the default lens mode gives),
/// written as `$UP_OUT/<n>_<mode>.json` (n = index in the list) with Constrain
/// crop off.
#[test]
#[ignore = "tool: needs UP_FILES, UP_OUT"]
fn write_upright_recipes() {
    use crate::upright::{UprightMode, analysis_image, detect_segments, upright_lines};
    let out = std::path::PathBuf::from(std::env::var("UP_OUT").unwrap());
    for (n, path) in std::env::var("UP_FILES").unwrap().split(';').enumerate() {
        let bytes = std::fs::read(path).unwrap();
        let image = crate::image_loader::load_base_image_from_bytes(
            &bytes,
            path,
            false,
            &crate::app_settings::AppSettings::default(),
            None,
        )
        .unwrap();
        let mut lens = serde_json::json!({});
        rawtonic_engine::embedded_lens::apply_default(&mut lens, std::path::Path::new(path));
        let (gray, sw, sh) = if lens
            .get("lensDistortionParams")
            .is_some_and(|p| !p.is_null())
        {
            let (_, sw, sh) = analysis_image(&image, 1400);
            let small = image.thumbnail(sw as u32, sh as u32);
            analysis_image(
                &crate::image_processing::apply_geometry_warp(&small, &lens),
                1400,
            )
        } else {
            analysis_image(&image, 1400)
        };
        let segments = detect_segments(&gray, sw, sh, (sw as f64).hypot(sh as f64) * 0.02);
        for (name, mode) in [
            ("auto", UprightMode::Auto),
            ("level", UprightMode::Level),
            ("vertical", UprightMode::Vertical),
            ("full", UprightMode::Full),
        ] {
            let Some(mut result) = upright_lines(&segments, sw as f64, sh as f64, mode, false)
            else {
                println!("{path}\t{name}\tno result");
                continue;
            };
            for line in &mut result.lines {
                for p in [&mut line.p1, &mut line.p2] {
                    p.x /= sw as f64;
                    p.y /= sh as f64;
                }
            }
            println!("{path}\t{name}\tapplied {}", result.applied);
            let recipe = serde_json::json!({"guidedPerspective": {"enabled": true,
                "lines": result.lines, "autoCrop": false, "upright": name}});
            std::fs::write(out.join(format!("{n}_{name}.json")), recipe.to_string()).unwrap();
        }
    }
}

/// Tool: the segments `detect_segments` finds in `$SEG_FILE` (lengths and
/// tilt from vertical), for tuning Upright.
#[test]
#[ignore = "tool: needs SEG_FILE"]
fn print_upright_segments() {
    let path = std::env::var("SEG_FILE").unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let image = crate::image_loader::load_base_image_from_bytes(
        &bytes,
        &path,
        false,
        &crate::app_settings::AppSettings::default(),
        None,
    )
    .unwrap();
    let (gray, sw, sh) = crate::upright::analysis_image(&image, 1400);
    let diagonal = (sw as f64).hypot(sh as f64);
    for min in [0.02, 0.01, 0.005] {
        let segments = crate::upright::detect_segments(&gray, sw, sh, diagonal * min);
        let near: Vec<_> = segments
            .iter()
            .filter(|s| s.tilt_from_vertical().abs() < 25f64.to_radians())
            .map(|s| {
                format!(
                    "{:.3}@{:.1}°",
                    s.length() / diagonal,
                    s.tilt_from_vertical().to_degrees()
                )
            })
            .collect();
        println!(
            "min {min}: {} segments, near-vertical {:?}",
            segments.len(),
            near
        );
    }
    let mean = gray.iter().sum::<f32>() / gray.len() as f32;
    println!("analysis image {sw}x{sh}, mean grey {mean:.1}");
    if let Ok(out) = std::env::var("SEG_OUT") {
        image::GrayImage::from_raw(
            sw as u32,
            sh as u32,
            gray.iter().map(|v| v.round() as u8).collect(),
        )
        .unwrap()
        .save(out)
        .unwrap();
        let rgb = image.to_rgb32f();
        let max = rgb.as_raw().iter().fold(0f32, |m, &v| m.max(v));
        let mean_rgb = rgb.as_raw().iter().sum::<f32>() / rgb.as_raw().len() as f32;
        println!(
            "base image {}x{} max {max:.3} mean {mean_rgb:.3}",
            rgb.width(),
            rgb.height()
        );
    }
}
