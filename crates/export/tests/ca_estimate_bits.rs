//! Task CM (identity of a faster lateral CA estimate): the bits of the
//! automatic lateral CA estimate of real photographs, version 1, 2 and 3
//! (against the lens data's own lateral CA where the photo has it), for a
//! comparison of two builds. `CA_RAWS` = a file listing RAW paths (one per
//! line), `RAWTONIC_LENSFUN_DB` = the app's lensfun_db folder. Prints one
//! line per photo; without `CA_RAWS` it prints SKIP.

use rawtonic_engine::lateral_ca::{self, CaModel};
use std::sync::Arc;

fn bits(m: Option<CaModel>) -> String {
    match m {
        Some(m) => [m.red.0, m.red.1, m.blue.0, m.blue.1]
            .map(|v| format!("{:08x}", v.to_bits()))
            .join(","),
        None => "none".into(),
    }
}

#[test]
#[ignore = "needs RAW files (CA_RAWS)"]
fn lateral_ca_estimate_bits() {
    let Some(list) = std::env::var_os("CA_RAWS") else {
        eprintln!("SKIP: CA_RAWS not set");
        return;
    };
    if let Some(db) = std::env::var_os("RAWTONIC_LENSFUN_DB") {
        rawtonic_engine::lens_correction::set_global_lens_db(Arc::new(
            rawtonic_engine::lens_correction::load_lensfun_dir(std::path::Path::new(&db)),
        ));
    }
    for path in std::fs::read_to_string(list).unwrap().lines() {
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        let mut recipe = serde_json::json!({});
        let own = match rawtonic_export::exif_processing::prepare_recipe_lens(
            &mut recipe,
            std::path::Path::new(path),
        ) {
            Ok(_) => {
                let params =
                    rawtonic_engine::image_processing::get_geometry_params_from_json(&recipe);
                let own = rawtonic_engine::image_processing::own_tca_model(&params);
                (own != CaModel::NONE).then_some(own)
            }
            Err(_) => None,
        };
        let bytes = std::fs::read(path).unwrap();
        let base = rawtonic_engine::image_loader::BaseImageOptions::default();
        let image = match rawtonic_engine::image_loader::load_base_image_measured(
            &bytes, path, false, &base, None,
        ) {
            Ok((image, _)) => image.to_rgb32f(),
            Err(e) => {
                println!("{path}\tdecode error {e}");
                continue;
            }
        };
        let started = std::time::Instant::now();
        let v1 = lateral_ca::estimate(&image);
        let v2 = lateral_ca::estimate_v2(&image);
        let v3 = own.map(|own| lateral_ca::estimate_versus(&image, own));
        println!(
            "{path}\t{}x{}\tv1 {}\tv2 {}\tv3 {}\town {}\t{:.0} ms",
            image.width(),
            image.height(),
            bits(v1),
            bits(v2),
            v3.map_or("-".into(), bits),
            bits(own),
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
}
