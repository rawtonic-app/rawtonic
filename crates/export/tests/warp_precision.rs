//! Task BX7 (PR 2 gate): how far a lens warp computed in f32 on the GPU
//! would sample from the CPU warp's positions, on real photographs' lens
//! data. `WARP_RAWS` = a file listing RAW paths (one per line),
//! `RAWTONIC_LENSFUN_DB` = the app's lensfun_db folder. Prints one line per
//! photo and a summary; without `WARP_RAWS` it prints SKIP.

use std::sync::Arc;

#[test]
#[ignore = "needs RAW files (WARP_RAWS)"]
fn lens_warp_positions_in_f32() {
    let Some(list) = std::env::var_os("WARP_RAWS") else {
        eprintln!("SKIP: WARP_RAWS not set");
        return;
    };
    if let Some(db) = std::env::var_os("RAWTONIC_LENSFUN_DB") {
        rawtonic_engine::lens_correction::set_global_lens_db(Arc::new(
            rawtonic_engine::lens_correction::load_lensfun_dir(std::path::Path::new(&db)),
        ));
    }
    let paths: Vec<String> = std::fs::read_to_string(list)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    let mut worst = (0.0f64, String::new());
    let mut means = Vec::new();
    for path in &paths {
        let mut recipe = serde_json::json!({});
        if let Err(e) = rawtonic_export::exif_processing::prepare_recipe_lens(
            &mut recipe,
            std::path::Path::new(path),
        ) {
            println!("{path}\tlens error {e}");
            continue;
        }
        let params = rawtonic_engine::image_processing::get_geometry_params_from_json(&recipe);
        if rawtonic_engine::image_processing::is_geometry_identity(&params) {
            println!("{path}\tidentity");
            continue;
        }
        let bytes = std::fs::read(path).unwrap();
        let base = rawtonic_engine::image_loader::BaseImageOptions::default();
        let image = match rawtonic_engine::image_loader::load_base_image_measured(
            &bytes, path, false, &base, None,
        ) {
            Ok((image, _)) => image,
            Err(e) => {
                println!("{path}\tdecode error {e}");
                continue;
            }
        };
        let (w, h) = (image.width(), image.height());
        let p = rawtonic_engine::image_processing::warp_f32_displacement(&params, w, h, 7);
        println!(
            "{path}\t{w}x{h}\tmodel {}\ttable {}\tf64-steps max {:.5} mean {:.6}\tf32 max {:.5} mean {:.6}\tapart max {:.5}\tn {}",
            params.lens_model,
            params.lens_table.len(),
            p.old.0,
            p.old.1,
            p.new.0,
            p.new.1,
            p.apart,
            p.pixels
        );
        means.push(p.new.1);
        if p.new.0 > worst.0 {
            worst = (p.new.0, path.clone());
        }
    }
    println!(
        "SUMMARY files {} f32 warp vs exact: worst max {:.5} px ({}) mean of means {:.6} px",
        means.len(),
        worst.0,
        worst.1,
        means.iter().sum::<f64>() / means.len().max(1) as f64
    );
}
