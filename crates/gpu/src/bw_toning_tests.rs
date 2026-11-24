//! B&W Toning in the shader (`apply_bw_toning`) against the same step
//! computed here from the plain black-and-white render and the CPU's table:
//! the grey's Oklab colour shifted by the table's entry at its lightness.
//! The model was measured and fitted on these steps (notes:
//! benchmark-results/spt1-2026-11-24), so the shader has to do the same.
//!
//! Run: `cargo test -p rawtonic-gpu -- --ignored bw_toning`.

use super::*;
use super::color_grading::{decode, encode, from_oklab, to_oklab};
use rawtonic_engine::bw_toning::BW_TONING_LUT_SIZE;
use rawtonic_engine::image_processing::get_all_adjustments_from_json;

fn entry(table: &[[f32; 4]; BW_TONING_LUT_SIZE], lightness: f64) -> [f64; 3] {
    let x = lightness.clamp(0.0, 1.0) * (BW_TONING_LUT_SIZE - 1) as f64;
    let i = (x as usize).min(BW_TONING_LUT_SIZE - 2);
    let f = x - i as f64;
    std::array::from_fn(|k| table[i][k] as f64 * (1.0 - f) + table[i + 1][k] as f64 * f)
}

#[test]
#[ignore = "requires a native GPU; B&W Toning's shader path against the same step on the CPU"]
fn bw_toning_adds_the_tables_shift_to_the_grey() {
    let context = context();
    let slot = GpuProcessorSlot::new(None);
    let charts = Charts::new();
    let tonings = [
        // Lightness kept, white and black neutral (the defaults).
        json!({"highlights": {"hue": 40, "saturation": 60}, "shadows": {"hue": 220, "saturation": 60}}),
        // A toned paper white and a dense highlights tone.
        json!({"highlights": {"hue": 41, "saturation": 56, "density": 80}, "balance": -12, "paper": 88}),
        // Strong shadows that leave sRGB, moved by Balance.
        json!({"shadows": {"hue": 250, "saturation": 100, "density": 30}, "balance": -40}),
    ];
    let chart = &charts.images[0];
    let mut plain_recipe = base_recipe();
    plain_recipe["blackAndWhite"] = json!(true);
    let plain = render_srgb(&context, &slot, chart, &plain_recipe, true, &[]);
    for (number, toning) in tonings.iter().enumerate() {
        let mut recipe = plain_recipe.clone();
        recipe["blackAndWhiteToning"] = toning.clone();
        // The table the renderer receives for this recipe.
        let table = get_all_adjustments_from_json(&recipe, false, None).global.bw_toning_lut;
        let rendered = render_srgb(&context, &slot, chart, &recipe, true, &[]);
        // Colours inside the cube, and those on or near its surface, which
        // the output's knee eases a little (ease_into_gamut): the table puts
        // some tints on the surface, and its interpolation between entries
        // may cross it.
        let mut errors = [Vec::new(), Vec::new()];
        let mut toned = 0;
        for (before, after) in plain.iter().zip(&rendered) {
            let lab = to_oklab(before.map(|e| decode(e as f64)));
            let shift = entry(&table, lab[0]);
            let wanted = from_oklab([lab[0] + shift[0], lab[1] + shift[1], lab[2] + shift[2]])
                .map(|c| encode(c).clamp(0.0, 1.0));
            if shift[1].hypot(shift[2]) > 0.01 {
                toned += 1;
            }
            let near_edge = wanted.iter().any(|&c| !(0.03..=0.97).contains(&c));
            errors[usize::from(near_edge)].push(
                (0..3)
                    .map(|k| (wanted[k] - after[k] as f64).abs())
                    .fold(0.0, f64::max),
            );
        }
        let pixels = errors[0].len() + errors[1].len();
        assert!(toned > pixels / 10, "toning {number} tints the ramp");
        for (kind, errors) in errors.iter_mut().enumerate() {
            if errors.is_empty() {
                continue;
            }
            errors.sort_by(f64::total_cmp);
            let (median, p90, max) = (
                errors[errors.len() / 2],
                errors[errors.len() * 9 / 10],
                errors[errors.len() - 1],
            );
            eprintln!(
                "toning {number}: {toned} toned of {pixels} pixels, {} {}: |shader − CPU| median {median:.5} p90 {p90:.5} max {max:.5}",
                errors.len(),
                ["inside", "near the edge"][kind],
            );
            assert!(max < 0.03, "toning {number}");
            if kind == 0 {
                assert!(median < 0.0005 && p90 < 0.002, "toning {number}");
            }
        }
    }
}
