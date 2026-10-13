// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dense CPU visibility reference. It deliberately does not use the GPU's
//! sparse marching schedule, so narrow missed occluders become measurable.
use super::*;
use base64::Engine as _;

#[test]
#[ignore = "native GPU: projected stripes must preserve full-precision camera surfaces"]
fn relight_camera_plane_precision() {
    let context = context();
    const SIZE: u32 = 512;
    let processor = GpuProcessor::new(context.clone(), SIZE, SIZE).unwrap();
    let image = DynamicImage::ImageRgba32F(ImageBuffer::from_pixel(
        SIZE,
        SIZE,
        image::Rgba([0.5, 0.5, 0.5, 1.]),
    ));
    let texture = create_input_texture(&context.device, &context.queue, &image, SIZE).unwrap();
    let view = texture.create_view(&Default::default());
    let mut failures = Vec::new();
    for resolution in [64, 192] {
        let mut png = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageLuma16(ImageBuffer::from_fn(resolution, resolution, |x, y| {
            let q = 1.03 + 0.02 * (x as f32 + 0.5) / resolution as f32
                - 0.03 * (y as f32 + 0.5) / resolution as f32;
            image::Luma([((q - 0.8) / 0.4 * 65535.).round() as u16])
        }))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        for scale in [0.1f32, 0.4] {
            for elevation in [5f32, 25.] {
                for softness in [0f32, 0.2] {
                    let mut recipe = json!({"relightLights":[{"pattern":"blinds","intensity":25,"color":"#ffffff","rotation":0,"patternRotation":31,"scale":scale*100.,"softness":softness*100.,"elevation":elevation,"diffusion":0,"shadows":100}],"relightOriginalLight":0,"relightDepthAmount":100,"relightSkinReflections":0});
                    recipe["relightDepth"] = json!({"data":data,"version":4,"geometry":rawtonic_engine::relight::geometry(&recipe),"camera":{"fx":1.2,"fy":1.2,"cx":0.5,"cy":0.5,"inverseMin":0.8,"inverseMax":1.2,"reference":1.0}});
                    let adjustments = get_all_adjustments_from_json(&recipe, false, Some(0));
                    let depth =
                        rawtonic_engine::relight::depth(adjustments.global.relight_depth).unwrap();
                    // Deliberately retain the original CPU surface; quantizing
                    // this reference would hide the storage error under test.
                    let surface = rawtonic_engine::relight::surface_texture(&depth);
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
                    let camera = depth.camera.unwrap();
                    let (e, r) = elevation.to_radians().sin_cos();
                    let (s, c) = 31f32.to_radians().sin_cos();
                    let edge = (0.002 + softness * 0.22).max(1. / (SIZE as f32 * scale));
                    let mut errors = Vec::new();
                    for y in 0..SIZE {
                        for x in 0..SIZE {
                            let uv = [x as f32 / SIZE as f32, y as f32 / SIZE as f32];
                            let p = sample(&surface, uv);
                            let world = camera.position(uv, p[2].recip(), 1.);
                            // Intersect the directional ray with reference z=0.
                            let py = (-s * world[0] + c * (world[1] - r / e * world[2])) / scale;
                            let cell = (py - py.floor() - 0.5).abs();
                            let mask = 1. - smooth(0.23 - edge, 0.23 + edge, cell);
                            let n = [
                                -camera.fx * p[0],
                                -camera.fy * p[1],
                                p[2] - p[0] * (uv[0] - 0.5) - p[1] * (uv[1] - 0.5),
                            ];
                            let response = ((n[1] * r + n[2] * e)
                                / n.iter().map(|a| a * a).sum::<f32>().sqrt())
                            .max(0.);
                            let linear =
                                ((0.5f32 + 0.055) / 1.055).powf(2.4) * 0.75 * mask * response;
                            let expected = if linear <= 0.0031308 {
                                linear * 12.92
                            } else {
                                1.055 * linear.powf(1. / 2.4) - 0.055
                            };
                            errors.push(
                                (pixels[((y * SIZE + x) * 4) as usize] as f32
                                    - (expected * 255.).round())
                                .abs(),
                            );
                        }
                    }
                    errors.sort_by(f32::total_cmp);
                    let mean = errors.iter().sum::<f32>() / errors.len() as f32;
                    let p99 = errors[errors.len() * 99 / 100];
                    let max = errors.last().copied().unwrap();
                    eprintln!(
                        "camera precision resolution={resolution} scale={scale} elevation={elevation} softness={softness}: mean={mean:.4} p99={p99} max={max}"
                    );
                    // The renderer adds sub-LSB output dither; the independent
                    // undithered reference may differ by one U8 level.
                    if mean > 0.25 || max > 1. {
                        failures.push((resolution, scale, elevation, softness, mean, p99, max));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "camera surface precision loss: {failures:?}"
    );
}

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
    camera: Option<rawtonic_engine::relight::Camera>,
) -> f32 {
    let receiver = sample(surface, uv);
    if receiver[3] < 0.99 {
        return 1.;
    }
    let resolution = surface.width().min(surface.height()) as f32;
    let origin = camera.map(|c| c.position(uv, receiver[2].recip(), 1.));
    let ratio = camera.map_or(1., |c| receiver[2].recip() / c.reference);
    let mut travel = 1.25 / resolution * ratio;
    let mut result: f32 = 1.;
    while travel < 2. {
        let ray = origin.map(|o| std::array::from_fn::<_, 3, _>(|i| o[i] + direction[i] * travel));
        let mut ratio = 1.;
        let q = if let (Some(c), Some(ray)) = (camera, ray) {
            let distance = c.reference - ray[2] * c.world_unit();
            if distance <= c.reference * 0.0001 {
                break;
            }
            ratio = distance / c.reference;
            [
                ray[0] * c.world_unit() * c.fx / distance + c.cx,
                ray[1] * c.world_unit() * c.fy / distance + c.cy,
            ]
        } else {
            [uv[0] + direction[0] * travel, uv[1] + direction[1] * travel]
        };
        if q.iter().any(|v| *v <= 0. || *v >= 1.) {
            break;
        }
        let blocker = sample(surface, q);
        if blocker[3] < 0.995 {
            break;
        }
        let clearance = if let (Some(c), Some(ray)) = (camera, ray) {
            ray[2] - c.position(q, blocker[2].recip(), 1.)[2]
        } else {
            receiver[2] * 0.25 + direction[2] * travel - blocker[2] * 0.25
        };
        let penumbra = 0.0008 + softness * travel * 0.18;
        result = result.min(smooth(-penumbra, penumbra, clearance + 0.001));
        travel += 0.25 / resolution * ratio;
    }
    1. + (result - 1.) * smooth(0.99, 1., receiver[3])
}

#[test]
#[ignore = "native GPU: dense shadow reference across face scales and directions"]
fn relight_shadow_dense_reference() {
    dense_reference_case(2);
}

#[test]
#[ignore = "native GPU: camera-space dense shadow reference"]
fn relight_camera_shadow_dense_reference() {
    dense_reference_case(4);
}

fn dense_reference_case(version: u64) {
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
                recipe["relightDepth"] = json!({"data":data,"geometry":rawtonic_engine::relight::geometry(&recipe),"version":version,"faces":[face]});
                if version == 4 {
                    recipe["relightDepth"]["camera"] = json!({"fx":1.2,"fy":1.2,"cx":0.5,"cy":0.5,"inverseMin":0.6,"inverseMax":1.4,"reference":1.0});
                    recipe["relightSkinReflections"] = json!(0);
                }
                let adjustments = get_all_adjustments_from_json(&recipe, false, Some(0));
                let depth =
                    rawtonic_engine::relight::depth(adjustments.global.relight_depth).unwrap();
                let mut surface = rawtonic_engine::relight::surface_texture(&depth);
                if version != 4 {
                    for v in surface.as_mut() {
                        *v = f16::from_f32(*v).to_f32();
                    }
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
                        let normal = depth.camera.map_or([-p[0] * 0.25, -p[1] * 0.25, 1.], |c| {
                            [
                                -c.fx * p[0],
                                -c.fy * p[1],
                                p[2] - p[0] * (uv[0] - c.cx) - p[1] * (uv[1] - c.cy),
                            ]
                        });
                        let length = normal.iter().map(|v| v * v).sum::<f32>().sqrt();
                        let response = normal
                            .iter()
                            .zip(direction)
                            .map(|(n, l)| n * l)
                            .sum::<f32>()
                            .max(0.)
                            / length;
                        let visible = visibility(&surface, uv, direction, softness, depth.camera);
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
