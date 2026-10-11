//! Relative error of DNGs against a reference DNG in scene-linear RGB, by
//! brightness of the reference (EV bins below sensor white).
//!
//! cargo run --release -p rawtonic-merge --example compare_linear -- TRUTH.dng A.dng [B.dng…]
use anyhow::Result;
use rawtonic_merge::frame::load_frame;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let truth = load_frame(args[0].as_ref(), &Default::default())?;
    let t: &[f32] = &truth.image;
    for other in &args[1..] {
        let f = load_frame(other.as_ref(), &Default::default())?;
        let o: &[f32] = &f.image;
        // Exposure scale of `other` against the truth (median luminance ratio).
        let mut ratios: Vec<f32> = t
            .chunks(3)
            .zip(o.chunks(3))
            .step_by(7)
            .filter_map(|(p, q)| {
                let y = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                let yo = 0.2126 * q[0] + 0.7152 * q[1] + 0.0722 * q[2];
                (y > 0.01 && y < 0.05 && yo < 0.7).then(|| yo / y)
            })
            .collect();
        ratios.sort_by(|a, b| a.total_cmp(b));
        let scale = ratios.get(ratios.len() / 2).copied().unwrap_or(1.0);
        let scale = 2f32.powf(scale.log2().round());
        let o: Vec<f32> = o.iter().map(|v| v / scale).collect();
        let o: &[f32] = &o;
        print!("[x{scale}] ");
        // bins: EV from -10 to +4 relative to white
        let mut sum = [0.0f64; 15];
        let mut bias = [0.0f64; 15];
        let mut n = [0u64; 15];
        for (p, q) in t.chunks(3).zip(o.chunks(3)) {
            let y = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
            let yo = 0.2126 * q[0] + 0.7152 * q[1] + 0.0722 * q[2];
            if y <= 0.0 {
                continue;
            }
            let b = ((y.log2() + 10.0).floor() as i64).clamp(0, 14) as usize;
            let rel = ((yo - y) / y) as f64;
            sum[b] += rel * rel;
            bias[b] += rel;
            n[b] += 1;
        }
        // Error map beside the file: |relative luminance error| × 5, 1/8 size.
        {
            let (w, h) = truth.image.dimensions();
            let (mw, mh) = (w / 8, h / 8);
            let mut map = image::RgbImage::new(mw * 2, mh);
            for my in 0..mh {
                for mx in 0..mw {
                    let i = ((my * 8) * w + mx * 8) as usize * 3;
                    let y = 0.2126 * t[i] + 0.7152 * t[i + 1] + 0.0722 * t[i + 2];
                    let yo = 0.2126 * o[i] + 0.7152 * o[i + 1] + 0.0722 * o[i + 2];
                    let e = (((yo - y) / y.max(0.002)).abs() * 5.0).min(1.0);
                    let tone = |v: f32| ((v.max(0.0) / 0.5).min(1.0).powf(1.0 / 2.2) * 255.0) as u8;
                    map.put_pixel(
                        mx,
                        my,
                        image::Rgb([tone(o[i]), tone(o[i + 1]), tone(o[i + 2])]),
                    );
                    map.put_pixel(
                        mw + mx,
                        my,
                        image::Rgb([(e * 255.0) as u8, (e * 255.0) as u8, 0]),
                    );
                }
            }
            map.save(format!("{other}.err.png"))?;
        }
        print!("{:40}", other.rsplit('/').next().unwrap_or(other));
        for b in 0..15 {
            if n[b] > 1000 {
                print!(
                    " {:+}EV:{:.3}/{:+.3}",
                    b as i64 - 10,
                    (sum[b] / n[b] as f64).sqrt(),
                    bias[b] / n[b] as f64
                );
            }
        }
        println!();
    }
    Ok(())
}
