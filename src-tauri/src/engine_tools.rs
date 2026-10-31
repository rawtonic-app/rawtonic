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
            crate::image_processing::auto_adjustments_json(
                &image,
                path,
                true,
                None,
                None,
                crate::image_processing::RENDER_VERSION,
            )
        };
        let mut json = serde_json::json!({});
        crate::auto_adjust::merge_auto_adjustments(&mut json, &auto);
        let look = crate::baseline_exposure::base_look(path);
        println!(
            "PRED {name} {:?}",
            crate::auto_adjust::predicted_display_percentiles(
                &image,
                look.baseline_for(crate::image_processing::RENDER_VERSION),
                crate::auto_adjust::DisplayCurve::for_look(
                    &look,
                    crate::image_processing::RENDER_VERSION
                ),
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
/// `$LENS_FILES` (`;`-separated) as warp parameters, with the read time,
/// then the parameters a recipe without `cameraCaVersion` and
/// `cameraGeometryVersion` gets.
#[test]
#[ignore = "tool: needs LENS_FILES"]
fn print_embedded_lens() {
    use rawtonic_engine::embedded_lens::{read, read_version, to_params};
    let json = |p: Option<rawtonic_engine::lens_correction::LensDistortionParams>| {
        p.map(|p| serde_json::to_string(&p).unwrap_or_default())
            .unwrap_or_default()
    };
    for path in std::env::var("LENS_FILES").unwrap().split(';') {
        let start = std::time::Instant::now();
        let data = read(std::path::Path::new(path));
        let elapsed = start.elapsed();
        let legacy = read_version(std::path::Path::new(path), 0, 0);
        println!(
            "{path}\t{elapsed:?}\t{}\t{}",
            json(data.as_ref().and_then(to_params)),
            json(legacy.as_ref().and_then(to_params))
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
            let small = crate::image_processing::thumbnail(&image, sw as u32, sh as u32);
            analysis_image(
                &crate::image_processing::apply_geometry_warp(&small, &lens, true).unwrap(),
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

/// Tool for the Find Dust measurement (notes: benchmark-results/
/// fd-find-dust-2026-10-31): for each RAW listed in `$DUST_FILES` (one
/// path per line) writes into `$DUST_OUT` STEM.json (base size, flat share
/// and every candidate down to sensitivity 100) and STEM_view.jpg, a local
/// contrast view (square-root luminance minus its wide blur, ×`$DUST_GAIN`,
/// default 6), STEM.work (the detector's work copy) at most 1600 px long for marking dust by eye. With
/// `$DUST_SYNTH` = N, N synthetic dust discs (seeded per file, 2–8 % deep,
/// radius 0.15–0.6 % of the long edge) are painted on the base first and
/// listed as `synthetic` (the second, labelled set).
#[test]
#[ignore = "tool: needs DUST_FILES and DUST_OUT"]
fn print_dust() {
    use image::GenericImageView;
    let out = std::path::PathBuf::from(std::env::var("DUST_OUT").unwrap());
    let gain: f32 = std::env::var("DUST_GAIN")
        .ok()
        .and_then(|g| g.parse().ok())
        .unwrap_or(6.0);
    let synth: usize = std::env::var("DUST_SYNTH")
        .ok()
        .and_then(|g| g.parse().ok())
        .unwrap_or(0);
    let list = std::fs::read_to_string(std::env::var("DUST_FILES").unwrap()).unwrap();
    for path in list.lines().filter(|l| !l.trim().is_empty()) {
        let stem = std::path::Path::new(path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let started = std::time::Instant::now();
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
            println!("{stem}: decode failed");
            continue;
        };
        let decoded = started.elapsed().as_secs_f64();
        let (w, h) = image.dimensions();
        let mut synthetic = Vec::new();
        let image = if synth > 0 {
            let mut rgb = image.to_rgb32f();
            let mut seed = stem.bytes().fold(2_166_136_261u32, |a, b| {
                (a ^ b as u32).wrapping_mul(16_777_619)
            });
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as f64 / u32::MAX as f64
            };
            let long = w.max(h) as f64;
            for _ in 0..synth {
                let (cx, cy) = (next() * w as f64, next() * h as f64);
                let r = long * (0.0015 + 0.0045 * next());
                let depth = 0.02 + 0.06 * next();
                synthetic.push(serde_json::json!({"x": cx, "y": cy, "radius": r, "depth": depth}));
                let reach = (r * 1.4).ceil() as i64;
                for y in (cy as i64 - reach).max(0)..(cy as i64 + reach).min(h as i64) {
                    for x in (cx as i64 - reach).max(0)..(cx as i64 + reach).min(w as i64) {
                        let d = ((x as f64 - cx).hypot(y as f64 - cy)) / r;
                        let t = ((1.3 - d) / 0.6).clamp(0.0, 1.0);
                        let k = 1.0 - depth * t * t * (3.0 - 2.0 * t);
                        let p = rgb.get_pixel_mut(x as u32, y as u32);
                        for c in 0..3 {
                            p[c] *= k as f32;
                        }
                    }
                }
            }
            image::DynamicImage::ImageRgb32F(rgb)
        } else {
            image
        };
        let t = std::time::Instant::now();
        let analysis = rawtonic_engine::dust::analyse(&image, false, &[]);
        let analysed = t.elapsed().as_secs_f64();
        // The work copy, for re-running the detector alone
        // (`rawtonic_engine::dust` test `work_copies`).
        let work = rawtonic_engine::dust::work_copy(&image, false);
        let mut cached = Vec::with_capacity(20 + work.values.len() * 4);
        for v in [
            work.width as u32,
            work.height as u32,
            work.factor as u32,
            w,
            h,
        ] {
            cached.extend_from_slice(&v.to_le_bytes());
        }
        for v in &work.values {
            cached.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(out.join(format!("{stem}.work")), cached).unwrap();
        // The local contrast view, and the share of flat area (|view| small
        // over a 9 × 9 neighbourhood) as a guide to which photos can show dust.
        let factor = w.max(h).div_ceil(1600).max(1);
        let (vw, vh) = (w / factor, h / factor);
        let rgb = image.to_rgb32f();
        let luma: Vec<f32> = (0..vh)
            .flat_map(|y| (0..vw).map(move |x| (x, y)))
            .map(|(x, y)| {
                let mut acc = 0.0f32;
                for yy in y * factor..(y + 1) * factor {
                    for xx in x * factor..(x + 1) * factor {
                        let p = rgb.get_pixel(xx, yy);
                        acc += 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                    }
                }
                (acc / (factor * factor) as f32).max(0.0).sqrt()
            })
            .collect();
        let blur = |v: &[f32], r: i64| -> Vec<f32> {
            let mut out = vec![0.0f32; v.len()];
            let (vw, vh) = (vw as i64, vh as i64);
            let mut tmp = vec![0.0f32; v.len()];
            for y in 0..vh {
                for x in 0..vw {
                    let (a, b) = ((x - r).max(0), (x + r).min(vw - 1));
                    tmp[(y * vw + x) as usize] =
                        (a..=b).map(|xx| v[(y * vw + xx) as usize]).sum::<f32>()
                            / (b - a + 1) as f32;
                }
            }
            for y in 0..vh {
                for x in 0..vw {
                    let (a, b) = ((y - r).max(0), (y + r).min(vh - 1));
                    out[(y * vw + x) as usize] =
                        (a..=b).map(|yy| tmp[(yy * vw + x) as usize]).sum::<f32>()
                            / (b - a + 1) as f32;
                }
            }
            out
        };
        let wide = blur(&luma, 24);
        let detail: Vec<f32> = luma.iter().zip(&wide).map(|(l, b)| l - b).collect();
        let energy = blur(&detail.iter().map(|d| d * d).collect::<Vec<_>>(), 4);
        let flat = energy
            .iter()
            .zip(&wide)
            .filter(|(e, b)| e.sqrt() < 0.004 && **b > 0.12)
            .count() as f64
            / energy.len() as f64;
        let view = image::GrayImage::from_fn(vw, vh, |x, y| {
            let i = (y as usize) * vw as usize + x as usize;
            let rel = detail[i] / wide[i].max(0.05);
            image::Luma([(128.0 + rel * gain * 255.0).clamp(0.0, 255.0) as u8])
        });
        image::DynamicImage::ImageLuma8(view)
            .to_rgb8()
            .save(out.join(format!("{stem}_view.jpg")))
            .unwrap();
        let candidates: Vec<serde_json::Value> = analysis
            .candidates
            .iter()
            .map(|c| serde_json::json!({"x": c.centre.0, "y": c.centre.1, "radius": c.radius, "contrast": c.contrast}))
            .collect();
        std::fs::write(
            out.join(format!("{stem}.json")),
            serde_json::to_string_pretty(&serde_json::json!({
                "file": path, "width": w, "height": h, "viewFactor": factor, "flat": flat,
                "decodeSeconds": decoded, "analyseSeconds": analysed,
                "candidates": candidates, "synthetic": synthetic,
            }))
            .unwrap(),
        )
        .unwrap();
        println!(
            "{stem}: {w}x{h} flat {flat:.2} candidates {} (≥50: {}) analyse {analysed:.2}s",
            analysis.candidates.len(),
            rawtonic_engine::dust::select(&analysis, 50.0).len()
        );
    }
}
