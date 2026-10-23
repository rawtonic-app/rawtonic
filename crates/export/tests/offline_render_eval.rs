//! Measures how an offline photograph (RAW missing, opened on its preview)
//! renders against the same photograph online: export pipeline, 16-bit
//! sRGB, the same recipe; the offline render takes another path that does
//! not exist, as the app's offline source does. Three ways offline:
//! without the file facts (before source_facts), with them, and with them
//! on the 1:1 preview's pixels (rawtonic_engine::proxy_codec round trip,
//! bands of 256 rows as the app writes them). Prints CIEDE2000 mean/p99
//! against the online render and mean L* of each.
//!
//! RAWTONIC_LENSFUN_DB=src-tauri/lensfun_db RAWTONIC_OFFLINE_EVAL="a.CR3;b.RAF" \
//!   cargo test --release -p rawtonic-export --test offline_render_eval -- --ignored --nocapture

use std::path::Path;
use std::sync::{Arc, Mutex};

use image::{DynamicImage, GenericImageView, Rgb32FImage};
use rayon::prelude::*;

use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_engine::source_facts;
use rawtonic_export::export_processing::{
    ExportSource, RenderEnv, process_image_for_export_pipeline,
};
use rawtonic_gpu::gpu_processing::{GpuContext, RenderOutputPrecision};

fn srgb_to_lab(rgb: [f32; 3]) -> [f32; 3] {
    let lin = rgb.map(|c| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
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

/// CIEDE2000 mean and 99th percentile of `b` against `a`, and the mean L*
/// of each.
fn compare(a: &DynamicImage, b: &DynamicImage) -> (f32, f32, f32, f32) {
    let (a, b) = (a.to_rgb16(), b.to_rgb16());
    assert_eq!(a.dimensions(), b.dimensions());
    let lab = |s: &[u16]| srgb_to_lab([s[0], s[1], s[2]].map(|v| v as f32 / 65535.0));
    let mut rows: Vec<(f32, f32, f32)> = a
        .as_raw()
        .par_chunks(3)
        .zip(b.as_raw().par_chunks(3))
        .map(|(p, q)| {
            let (lp, lq) = (lab(p), lab(q));
            (de2000(lp, lq), lp[0], lq[0])
        })
        .collect();
    let n = rows.len() as f64;
    let mean =
        |f: fn(&(f32, f32, f32)) -> f32| (rows.iter().map(|r| f(r) as f64).sum::<f64>() / n) as f32;
    let (de, la, lb) = (mean(|r| r.0), mean(|r| r.1), mean(|r| r.2));
    rows.par_sort_unstable_by(|x, y| x.0.total_cmp(&y.0));
    let p99 = rows[((rows.len() - 1) as f64 * 0.99) as usize].0;
    (de, p99, la, lb)
}

fn render(
    ctx: &GpuContext,
    processor: &Mutex<Option<rawtonic_gpu::gpu_processing::GpuProcessorState>>,
    cache: &Mutex<Option<rawtonic_gpu::gpu_processing::GpuImageCache>>,
    path: &str,
    image: &Arc<DynamicImage>,
    recipe: &serde_json::Value,
) -> DynamicImage {
    *cache.lock().unwrap() = None;
    // As the app's Develop view and export: the lens defaults of a recipe
    // without a lens mode come from the file.
    let mut recipe = recipe.clone();
    rawtonic_engine::embedded_lens::apply_default(&mut recipe, Path::new(path));
    let tonemapper = |_r: bool| None;
    let lut = |_p: &str| None;
    let env = RenderEnv {
        context: ctx,
        processor,
        image_cache: cache,
        tonemapper_override: &tonemapper,
        load_lut: &lut,
    };
    let source = ExportSource {
        pristine: Arc::clone(image),
        patched: None,
        measures: None,
    };
    process_image_for_export_pipeline(
        path,
        source,
        &recipe,
        &env,
        true,
        "eval",
        RenderOutputPrecision::SixteenBit,
        OutputColorSpace::Srgb,
    )
    .unwrap()
}

/// The 1:1 preview's pixels: the shipped encoding, in bands as written.
fn through_codec(image: &Rgb32FImage) -> Rgb32FImage {
    let width = image.width() as usize;
    let codec = rawtonic_engine::proxy_codec::TRANSFER;
    let pixels: Vec<f32> = image
        .as_raw()
        .par_chunks(width * 3 * 256)
        .map(|band| {
            let bytes = rawtonic_engine::proxy_codec::encode(
                band,
                width,
                rawtonic_engine::proxy_codec::DISTANCE,
                codec,
            )
            .unwrap();
            rawtonic_engine::proxy_codec::decode(&bytes, width, band.len() / 3 / width, codec)
                .unwrap()
        })
        .collect::<Vec<_>>()
        .concat();
    Rgb32FImage::from_raw(image.width(), image.height(), pixels).unwrap()
}

#[test]
#[ignore]
fn offline_render_eval() {
    let files = std::env::var("RAWTONIC_OFFLINE_EVAL").expect("files ; separated");
    let lensfun = std::env::var("RAWTONIC_LENSFUN_DB").unwrap();
    rawtonic_engine::lens_correction::set_global_lens_db(Arc::new(
        rawtonic_engine::lens_correction::load_lensfun_dir(Path::new(&lensfun)),
    ));
    let ctx = GpuContext::headless().unwrap();
    let processor = Mutex::new(None);
    let cache = Mutex::new(None);
    let recipes = [
        ("default", serde_json::json!({})),
        (
            "pushed",
            serde_json::json!({"exposure": 1.5, "shadows": 100, "contrast": 40, "clarity": 50, "saturation": 30, "highlights": -100}),
        ),
        (
            "kelvin",
            serde_json::json!({"whiteBalance": "custom", "wbTemperature": 3800, "wbTint": 8, "exposure": 0.3}),
        ),
    ];
    for file in files.split(';') {
        let bytes = std::fs::read(file).unwrap();
        let base = rawtonic_engine::image_loader::load_base_image(
            &bytes,
            file,
            false,
            &rawtonic_engine::image_loader::BaseImageOptions::default(),
            None,
        )
        .unwrap();
        let facts = source_facts::collect(file);
        let name = Path::new(file).file_name().unwrap().to_string_lossy();
        // Where the offline photograph is listed: its RAW is not there.
        let offline = format!("/nonexistent-offline-volume/{name}");
        let online_image = Arc::new(base.clone());
        let preview = Arc::new(DynamicImage::ImageRgb32F(through_codec(&base.to_rgb32f())));
        println!(
            "\n== {name} {}x{} baseline {:+.2} EV, colour {}, lens {}, noise {:?}",
            base.width(),
            base.height(),
            facts.base_look.as_ref().map_or(0.0, |l| l.baseline_ev),
            facts.colour.is_some(),
            if facts.camera_lens.is_some() {
                "camera"
            } else if facts.lensfun_default.is_some() {
                "lensfun"
            } else {
                "none"
            },
            facts.noise_level
        );
        for (label, recipe) in &recipes {
            let online = render(&ctx, &processor, &cache, file, &online_image, recipe);
            let before = render(&ctx, &processor, &cache, &offline, &online_image, recipe);
            source_facts::register(&offline, facts.clone());
            let after = render(&ctx, &processor, &cache, &offline, &online_image, recipe);
            let on_preview = render(&ctx, &processor, &cache, &offline, &preview, recipe);
            source_facts::forget(&offline);
            let report = |what: &str, out: &DynamicImage| {
                if out.dimensions() != online.dimensions() {
                    return format!(
                        "{what}: size {:?} vs {:?}",
                        out.dimensions(),
                        online.dimensions()
                    );
                }
                let (de, p99, l_on, l_off) = compare(&online, out);
                format!("{what}: dE {de:.3}/{p99:.2} L* {l_on:.2}->{l_off:.2}")
            };
            println!(
                "{label:8} | {} | {} | {}",
                report("no facts", &before),
                report("facts", &after),
                report("facts+1:1 preview", &on_preview)
            );
        }
    }
}
