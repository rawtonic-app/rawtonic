//! Runs `rawtonic_engine::patch_fill::fill` on files, for the object-removal
//! comparison (`scripts/models/eval_inpaint.py`).
//!
//! `cargo run --release -p rawtonic-engine --example patch_fill -- IMAGE MASK OUT [GUIDE] [key=value ...]`
//!
//! IMAGE and GUIDE are 8-bit RGB, MASK is grey (the hole where it is above
//! zero). Keys: radius, em, min_em, pm, coarsest, tex, guide_weight,
//! sharp (0/1), seed. Prints the time of the fill in milliseconds.

use rawtonic_engine::patch_fill::{PatchFillParams, fill};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    assert!(args.len() >= 3, "IMAGE MASK OUT [GUIDE] [key=value ...]");
    let image = image::open(&args[0]).expect("image").to_rgb8();
    let mask = image::open(&args[1]).expect("mask").to_luma8();
    let (w, h) = image.dimensions();
    assert_eq!(mask.dimensions(), (w, h), "mask size");
    let mut guide = None;
    let mut params = PatchFillParams::default();
    for a in &args[3..] {
        match a.split_once('=') {
            Some((k, v)) => match k {
                "radius" => params.patch_radius = v.parse().unwrap(),
                "em" => params.em_iterations = v.parse().unwrap(),
                "min_em" => params.min_em_iterations = v.parse().unwrap(),
                "pm" => params.pm_iterations = v.parse().unwrap(),
                "fine" => params.fine_extent = v.parse().unwrap(),
                "tex" => params.texture_weight = v.parse().unwrap(),
                "coarsest" => params.coarsest_extent = v.parse().unwrap(),
                "guide_weight" => params.guide_weight = v.parse().unwrap(),
                "sharp" => params.sharp_finish = v == "1",
                "seed" => params.seed = v.parse().unwrap(),
                _ => panic!("unknown key {k}"),
            },
            None => guide = Some(image::open(a).expect("guide").to_rgb8()),
        }
    }
    let to_f = |img: &image::RgbImage| -> Vec<[f32; 3]> {
        img.pixels()
            .map(|p| {
                [
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                ]
            })
            .collect()
    };
    let img = to_f(&image);
    let hole: Vec<bool> = mask.pixels().map(|p| p[0] > 0).collect();
    let guide = guide.map(|g| to_f(&g));
    let start = std::time::Instant::now();
    let out = fill(
        w as usize,
        h as usize,
        &img,
        &hole,
        guide.as_deref(),
        &params,
    );
    println!("{}", start.elapsed().as_millis());
    let out = image::RgbImage::from_fn(w, h, |x, y| {
        let v = out[(y * w + x) as usize];
        image::Rgb(v.map(|c| (c * 255.0).round().clamp(0.0, 255.0) as u8))
    });
    out.save(&args[2]).expect("save");
}
