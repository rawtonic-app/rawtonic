// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dense CPU visibility reference. It deliberately does not use the GPU's
//! sparse marching schedule, so narrow missed occluders become measurable.
use super::*;
use base64::Engine as _;

fn sample(surface: &ImageBuffer<image::Rgba<f32>, Vec<f32>>, uv: [f32; 2]) -> [f32; 4] {
    let x = (uv[0] * surface.width() as f32 - 0.5).clamp(0., (surface.width() - 1) as f32);
    let y = (uv[1] * surface.height() as f32 - 0.5).clamp(0., (surface.height() - 1) as f32);
    let x0 = x as u32;
    let y0 = y as u32;
    let x1 = (x0 + 1).min(surface.width() - 1);
    let y1 = (y0 + 1).min(surface.height() - 1);
    std::array::from_fn(|i| {
        let a = surface.get_pixel(x0, y0)[i];
        let b = surface.get_pixel(x1, y0)[i];
        let c = surface.get_pixel(x0, y1)[i];
        let d = surface.get_pixel(x1, y1)[i];
        (a + (b - a) * x.fract()) * (1. - y.fract()) + (c + (d - c) * x.fract()) * y.fract()
    })
}

fn smooth(a: f32, b: f32, v: f32) -> f32 {
    let t = ((v - a) / (b - a)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

fn visibility(
    surface: &ImageBuffer<image::Rgba<f32>, Vec<f32>>,
    uv: [f32; 2],
    direction: [f32; 3],
    softness: f32,
) -> f32 {
    let receiver = sample(surface, uv);
    if receiver[3] < 0.99 {
        return 1.;
    }
    let resolution = surface.width().min(surface.height()) as f32;
    let mut travel = 1.25 / resolution;
    let mut result: f32 = 1.;
    while travel < 2. {
        let q = [uv[0] + direction[0] * travel, uv[1] + direction[1] * travel];
        if q.iter().any(|v| *v <= 0. || *v >= 1.) {
            break;
        }
        let blocker = sample(surface, q);
        if blocker[3] < 0.995 {
            break;
        }
        let clearance = receiver[2] * 0.25 + direction[2] * travel - blocker[2] * 0.25;
        let penumbra = 0.0008 + softness * travel * 0.18;
        result = result.min(smooth(-penumbra, penumbra, clearance + 0.001));
        travel += 0.25 / resolution;
    }
    1. + (result - 1.) * smooth(0.99, 1., receiver[3])
}

#[test]
#[ignore = "native GPU: dense shadow reference across face scales and directions"]
fn relight_shadow_dense_reference() {
    let context = context();
    const SIZE: u32 = 192;
    let processor = GpuProcessor::new(context.clone(), SIZE, SIZE).unwrap();
    let image = DynamicImage::ImageRgba32F(ImageBuffer::from_pixel(
        SIZE,
        SIZE,
        image::Rgba([0.5, 0.5, 0.5, 1.]),
    ));
    let view = context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("relight dense reference"),
                size: wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&to_rgba_f16(&image)),
        )
        .create_view(&Default::default());
    let mut failures = Vec::new();
    for resolution in [128, 512] {
        let mut png = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageLuma16(ImageBuffer::from_pixel(
            resolution,
            resolution,
            image::Luma([32768]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        let encode = |v: f32| {
            let v = if v <= 0.0031308 {
                v * 12.92
            } else {
                1.055 * v.powf(1. / 2.4) - 0.055
            };
            (v.clamp(0., 1.) * 255.).round()
        };
        let input_linear = ((0.5f32 + 0.055) / 1.055).powf(2.4);
        for scale in [0.35, 0.65, 1.] {
            let face: Vec<_> = rawtonic_engine::relight::test_face_geometry()
                .into_iter()
                .map(|p| {
                    [
                        0.5 + (p[0] - 0.5) * scale,
                        0.5 + (p[1] - 0.5) * scale,
                        p[2] * scale,
                        p[3],
                    ]
                })
                .collect();
            for (angle, softness, elevation) in [
                (90f32, 0., 20f32),
                (135., 0.3, 20.),
                (180., 0.6, 20.),
                (90., 0.3, 5.),
                (180., 0.3, 5.),
                (0., 0.3, 75.),
            ] {
                let mut recipe = json!({"relightLights":[{"intensity":25,"color":"#ffffff","rotation":angle,"elevation":elevation,"diffusion":0,"softness":softness*100.,"shadows":100}],"relightOriginalLight":0,"relightDepthAmount":100});
                recipe["relightDepth"] = json!({"data":data,"geometry":rawtonic_engine::relight::geometry(&recipe),"version":2,"faces":[face]});
                let adjustments = get_all_adjustments_from_json(&recipe, false, Some(0));
                let depth =
                    rawtonic_engine::relight::depth(adjustments.global.relight_depth).unwrap();
                let mut surface = rawtonic_engine::relight::surface_texture(&depth);
                for v in surface.as_mut() {
                    *v = f16::from_f32(*v).to_f32();
                }
                let (pixels, ..) = processor
                    .run(
                        &view,
                        SIZE,
                        SIZE,
                        RenderRequest {
                            adjustments,
                            mask_bitmaps: &[],
                            lut: None,
                            roi: None,
                        },
                        false,
                        RenderOutputPrecision::EightBit,
                    )
                    .unwrap();
                let RenderedPixels::U8(pixels) = pixels else {
                    unreachable!()
                };
                let (s, c) = angle.to_radians().sin_cos();
                let (e, r) = elevation.to_radians().sin_cos();
                let direction = [-s * r, c * r, e];
                let mut errors = Vec::new();
                for y in 0..SIZE {
                    for x in 0..SIZE {
                        let uv = [x as f32 / SIZE as f32, y as f32 / SIZE as f32];
                        let p = sample(&surface, uv);
                        if p[3] < 0.99 {
                            continue;
                        }
                        let normal = [-p[0] * 0.25, -p[1] * 0.25, 1.];
                        let length = normal.iter().map(|v| v * v).sum::<f32>().sqrt();
                        let response = normal
                            .iter()
                            .zip(direction)
                            .map(|(n, l)| n * l)
                            .sum::<f32>()
                            .max(0.)
                            / length;
                        let visible = visibility(&surface, uv, direction, softness);
                        let expected = encode(input_linear * 0.75 * response * visible);
                        let actual = pixels[((y * SIZE + x) * 4) as usize] as f32;
                        errors.push((actual - expected).abs());
                    }
                }
                errors.sort_by(f32::total_cmp);
                let mean = errors.iter().sum::<f32>() / errors.len() as f32;
                let p99 = errors[errors.len() * 99 / 100];
                let max = *errors.last().unwrap();
                eprintln!(
                    "dense visibility resolution={resolution} scale={scale} angle={angle} elevation={elevation} softness={softness}: mean={mean:.3} p99={p99} max={max}"
                );
                if mean >= 0.5 || p99 > 2. || max > 20. {
                    failures.push((resolution, scale, angle, elevation, mean, p99, max));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "sparse shadows diverge from dense visibility: {failures:?}"
    );
}
