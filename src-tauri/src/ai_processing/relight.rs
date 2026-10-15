// SPDX-License-Identifier: AGPL-3.0-or-later
//! One requested scene analysis; light edits use the persisted geometry only.
use super::*;
use rawtonic_engine::relight::Camera;

const FILE: &str = "relight_geometry_da3_base.onnx";
const SHA256: &str = "070a4ff34ed406d057044daf9b84fd6287e5a7cc66134060acf2ccfe5d5dea23";
type DepthImage = image::ImageBuffer<image::Luma<u16>, Vec<u16>>;

pub async fn get_or_init_relight_model(
    app: &tauri::AppHandle,
    state: &Mutex<Option<AiState>>,
    init: &TokioMutex<()>,
) -> Result<Arc<rawtonic_onnx::Model>> {
    ensure_local_runtime()?;
    let loaded = || {
        state
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.relight_model.clone())
    };
    if let Some(model) = loaded() {
        return Ok(model);
    }
    let _guard = init.lock().await;
    if let Some(model) = loaded() {
        return Ok(model);
    }
    let generation = crate::ai_lifecycle::LIFECYCLE.generation();
    let dir = get_models_dir(app)?;
    download_and_verify_model(app, &dir, FILE, FILE, SHA256, "Relight geometry").await?;
    let path = dir.join(FILE);
    let model = tokio::task::spawn_blocking(move || {
        rawtonic_onnx::Model::load(&path, rawtonic_onnx::ModelKind::Relight)
    })
    .await??;
    crate::register_exit_handler();
    let model = Arc::new(model);
    keep_model(state, generation, |ai| {
        ai.relight_model = Some(model.clone())
    });
    Ok(model)
}

const PEOPLE_FILE: &str = "relight_people_normals_david_base.onnx";
const PEOPLE_SHA256: &str = "3d37ecc812b6b50a09c514f3c8c8ad95da5cd6ae9b68c425bc52f1c9cd78c344";
/// DAViD's square input side.
const PEOPLE_INPUT: u32 = 512;
/// Longest side of the subject bitmap a version-five analysis stores.
const SUBJECT_SIDE: u32 = 1024;

pub async fn get_or_init_people_normals_model(
    app: &tauri::AppHandle,
    state: &Mutex<Option<AiState>>,
    init: &TokioMutex<()>,
) -> Result<Arc<rawtonic_onnx::Model>> {
    ensure_local_runtime()?;
    let loaded = || {
        state
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.people_normals_model.clone())
    };
    if let Some(model) = loaded() {
        return Ok(model);
    }
    let _guard = init.lock().await;
    if let Some(model) = loaded() {
        return Ok(model);
    }
    let generation = crate::ai_lifecycle::LIFECYCLE.generation();
    let dir = get_models_dir(app)?;
    download_and_verify_model(
        app,
        &dir,
        PEOPLE_FILE,
        PEOPLE_FILE,
        PEOPLE_SHA256,
        "Relight people normals",
    )
    .await?;
    let path = dir.join(PEOPLE_FILE);
    let model = tokio::task::spawn_blocking(move || {
        rawtonic_onnx::Model::load(&path, rawtonic_onnx::ModelKind::PeopleNormals)
    })
    .await??;
    crate::register_exit_handler();
    let model = Arc::new(model);
    keep_model(state, generation, |ai| {
        ai.people_normals_model = Some(model.clone())
    });
    Ok(model)
}

/// DAViD's normals of the region `crop` (x, y, w, h in `rgb`), in the
/// renderer's frame (x right, y down, z towards the camera), as a function
/// of a position in `rgb`. The crop is padded to a square by repeating its
/// border, as DAViD's runtime does.
fn people_normals(
    rgb: &image::RgbImage,
    crop: [u32; 4],
    model: &rawtonic_onnx::Model,
) -> Result<impl Fn(f32, f32) -> [f32; 3] + use<>> {
    let [cx, cy, cw, ch] = crop;
    let side = cw.max(ch);
    let (pad_x, pad_y) = ((side - cw) / 2, (side - ch) / 2);
    let n = PEOPLE_INPUT as usize;
    let scale = side as f32 / PEOPLE_INPUT as f32;
    // BGR in [0, 1], area-sampled from the padded crop.
    let mut input = ndarray::Array4::<f32>::zeros((1, 3, n, n));
    for y in 0..n {
        for x in 0..n {
            let mut sum = [0f32; 3];
            let mut count = 0.;
            let (x0, y0) = (x as f32 * scale, y as f32 * scale);
            let step = (scale / 2.).max(1.);
            let mut sy = y0;
            while sy < y0 + scale {
                let mut sx = x0;
                while sx < x0 + scale {
                    let px = (sx as i64 - pad_x as i64).clamp(0, cw as i64 - 1) as u32 + cx;
                    let py = (sy as i64 - pad_y as i64).clamp(0, ch as i64 - 1) as u32 + cy;
                    let p = rgb.get_pixel(px, py);
                    for c in 0..3 {
                        sum[c] += p[c] as f32 / 255.;
                    }
                    count += 1.;
                    sx += step;
                }
                sy += step;
            }
            for c in 0..3 {
                input[[0, c, y, x]] = sum[2 - c] / count;
            }
        }
    }
    let values = model.run(|session| {
        let output = session.run(ort::inputs!["input" => Tensor::from_array(input.clone())?])?;
        let (shape, values) = output[0].try_extract_tensor::<f32>()?;
        anyhow::ensure!(
            shape.as_ref() == [1, 3, n as i64, n as i64],
            "Invalid people normal output shape"
        );
        rawtonic_onnx::ensure_finite(values)?;
        Ok(values.to_vec())
    })?;
    Ok(move |x: f32, y: f32| {
        // Position in the model grid, bilinear.
        let gx = ((x - cx as f32 + pad_x as f32) / scale - 0.5).clamp(0., n as f32 - 1.);
        let gy = ((y - cy as f32 + pad_y as f32) / scale - 0.5).clamp(0., n as f32 - 1.);
        let (x0, y0) = (gx as usize, gy as usize);
        let (x1, y1) = ((x0 + 1).min(n - 1), (y0 + 1).min(n - 1));
        let (fx, fy) = (gx.fract(), gy.fract());
        let at = |c: usize, x: usize, y: usize| values[c * n * n + y * n + x];
        let v: [f32; 3] = std::array::from_fn(|c| {
            let top = at(c, x0, y0) + (at(c, x1, y0) - at(c, x0, y0)) * fx;
            let bottom = at(c, x0, y1) + (at(c, x1, y1) - at(c, x0, y1)) * fx;
            top + (bottom - top) * fy
        });
        let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-6);
        // DAViD: y up; the renderer: y down.
        [v[0] / length, -v[1] / length, (v[2] / length).max(0.)]
    })
}

/// The version-five subject of the displayed frame: the subject matte, the
/// people normals inside it and the photo's own lighting on the people's
/// skin (`rawtonic_engine::relight::source`). Returns the subject PNG and
/// the lighting coefficients, if they could be fitted.
/// What a version-five subject analysis reads besides the photo and its
/// subject matte; each part is optional.
pub struct SubjectContext<'a> {
    /// Landmarked faces (`rawtonic_faces::relight::analyze`).
    pub faces: &'a [Vec<[f32; 4]>],
    /// The whole photo's people parse, for people without landmarks.
    pub parse: Option<&'a rawtonic_faces::ParseCrop>,
    pub people_model: Option<&'a rawtonic_onnx::Model>,
    /// The scene depth and camera (objects' normals, background reach).
    pub scene: Option<(&'a DepthImage, Camera)>,
    /// The sky mask: a new light does not reach the sky.
    pub sky: Option<&'a image::GrayImage>,
}

pub fn analyze_subject(
    displayed: &DynamicImage,
    linear: bool,
    matte: &image::GrayImage,
    context: SubjectContext,
) -> Result<(Vec<u8>, Option<[f32; 9]>)> {
    let SubjectContext {
        faces,
        parse,
        people_model,
        scene,
        sky,
    } = context;
    use rawtonic_engine::relight::source;
    let (w, h) = displayed.dimensions();
    let scale = SUBJECT_SIDE as f32 / w.max(h) as f32;
    let (tw, th) = if scale < 1. {
        (
            ((w as f32 * scale).round() as u32).max(1),
            ((h as f32 * scale).round() as u32).max(1),
        )
    } else {
        (w, h)
    };
    let mut matte = image::imageops::resize(matte, tw, th, FilterType::Triangle);
    // People: the subject's extent, with a margin, in the displayed frame.
    let (mut lo, mut hi) = ([u32::MAX; 2], [0u32; 2]);
    for (x, y, p) in matte.enumerate_pixels() {
        if p[0] > 128 {
            lo = [lo[0].min(x), lo[1].min(y)];
            hi = [hi[0].max(x), hi[1].max(y)];
        }
    }
    // A subject of at least 5 % of the frame: else the photo is a scene.
    let covered = matte.pixels().filter(|p| p[0] > 128).count();
    anyhow::ensure!(
        hi[0] >= lo[0] && hi[1] >= lo[1] && covered * 20 >= (tw * th) as usize,
        "No subject to relight"
    );
    let to_frame = w as f32 / tw as f32;
    let margin = ((hi[0] - lo[0]).max(hi[1] - lo[1]) as f32 * 0.08) as u32;
    let x0 = ((lo[0].saturating_sub(margin)) as f32 * to_frame) as u32;
    let y0 = ((lo[1].saturating_sub(margin)) as f32 * to_frame) as u32;
    let x1 = (((hi[0] + margin + 1) as f32 * to_frame) as u32).min(w);
    let y1 = (((hi[1] + margin + 1) as f32 * to_frame) as u32).min(h);
    let crop = [x0, y0, (x1 - x0).max(1), (y1 - y0).max(1)];
    // Skin: the landmarked faces', else the parse's face and body skin
    // (people the face detector misses, a profile). People: some skin.
    let skin = if faces.is_empty() {
        parse.map_or_else(
            || image::GrayImage::new(tw, th),
            |parse| {
                image::GrayImage::from_fn(tw, th, |x, y| {
                    let (fx, fy) = ((x as f32 + 0.5) * to_frame, (y as f32 + 0.5) * to_frame);
                    let face = parse
                        .sample(rawtonic_faces::parse::Class::FaceSkin, fx, fy)
                        .0;
                    let body = parse
                        .sample(rawtonic_faces::parse::Class::BodySkin, fx, fy)
                        .0;
                    image::Luma([((face + body).min(1.) * 255.).round() as u8])
                })
            },
        )
    } else {
        source::face_skin_mask(tw, th, faces)
    };
    let people = skin.pixels().filter(|p| p[0] > 128).count() * 300 >= (tw * th) as usize;
    // People normals only with people and the model.
    let normal_at = match people_model {
        Some(model) if people => {
            let rgb = display_rgb8(displayed.clone(), linear);
            Some(people_normals(&rgb, crop, model)?)
        }
        _ => None,
    };
    let inside = |x: f32, y: f32| {
        x >= crop[0] as f32
            && y >= crop[1] as f32
            && x < (crop[0] + crop[2]) as f32
            && y < (crop[1] + crop[3]) as f32
    };
    // Objects (no people): the smoothed scene depth's normals.
    let object_normals = (!people)
        .then(|| {
            scene.map(|(depth, camera)| {
                rawtonic_engine::relight::scene_normals(depth, camera, tw, th)
            })
        })
        .flatten();
    let mut normals = source::Normals::new(tw, th);
    let mut people = image::GrayImage::new(tw, th);
    for (x, y, p) in normals.enumerate_pixels_mut() {
        let (fx, fy) = ((x as f32 + 0.5) * to_frame, (y as f32 + 0.5) * to_frame);
        // Mostly covered texels only: the soft edge counts as background
        // for the subject normal and own light.
        let covered = matte.get_pixel(x, y)[0] > 127;
        match (&normal_at, &object_normals) {
            (Some(normal_at), _) if covered && inside(fx, fy) => {
                p.0 = normal_at(fx, fy);
                people.put_pixel(x, y, *matte.get_pixel(x, y));
            }
            (None, Some(object)) if covered => {
                p.0 = object.get_pixel(x, y).0;
                people.put_pixel(x, y, *matte.get_pixel(x, y));
            }
            _ => p.0 = [0., 0., 1.],
        }
    }
    // The photo in linear light for the lighting fit.
    let small = displayed
        .resize_exact(tw, th, FilterType::Triangle)
        .into_rgb32f();
    let decode = |v: f32| {
        if linear {
            v
        } else if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let linear_rgb = image::ImageBuffer::from_fn(tw, th, |x, y| {
        image::Rgb(small.get_pixel(x, y).0.map(decode))
    });
    let subject: Vec<[f32; 3]> = normals
        .enumerate_pixels()
        .filter(|(x, y, _)| people.get_pixel(*x, *y)[0] > 128)
        .map(|(_, _, n)| n.0)
        .collect();
    // People are fitted on their skin, objects on their dominant material.
    let material = if normal_at.is_none() && object_normals.is_some() {
        source::dominant_material(&linear_rgb, &matte)
    } else {
        skin
    };
    let light = (normal_at.is_some() || object_normals.is_some())
        .then(|| source::fit(&linear_rgb, &normals, &subject, &material))
        .flatten();
    // People only: an object's other materials (a white label) are its own.
    if let Some(light) = light.as_ref().filter(|_| normal_at.is_some()) {
        source::limit_to_consistent(
            &linear_rgb,
            &normals,
            &mut people,
            &mut matte,
            &material,
            light,
        );
    }
    let fitted = light;
    let light = fitted.map(|l| l.coefficients);
    let reach = scene.and_then(|(depth, camera)| {
        let inverse = rawtonic_engine::relight::subject_inverse(depth, camera, &matte)?;
        let map = image::ImageBuffer::from_fn(depth.width(), depth.height(), |x, y| {
            image::Luma([camera.inverse(depth.get_pixel(x, y)[0])])
        });
        let mut reach = source::background_reach(&map, &matte, inverse);
        // The sky cannot be lit (monocular depth puts it only a few times
        // farther than the subject).
        if let Some(sky) = sky {
            let sky = image::imageops::resize(sky, tw, th, FilterType::Triangle);
            for (r, s) in reach.pixels_mut().zip(sky.pixels()) {
                r[0] = (r[0] as u32 * (255 - s[0] as u32) / 255) as u8;
            }
        }
        Some(reach)
    });
    let specular = fitted
        .as_ref()
        .map(|light| source::skin_specular(&linear_rgb, &normals, &material, light));
    let encoded =
        source::encode_subject(&normals, &matte, &people, reach.as_ref(), specular.as_ref());
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba16(encoded).write_to(&mut png, image::ImageFormat::Png)?;
    Ok((png.into_inner(), light))
}

fn size(width: u32, height: u32) -> (u32, u32) {
    let scale = 518. / width.max(height).max(1) as f64;
    // A single patch along an axis makes the upstream camera head singular.
    let side = |n| ((n as f64 * scale / 14.).round() as u32).clamp(2, 37) * 14;
    (side(width), side(height))
}

fn encode(w: u32, h: u32, values: &[f32], intrinsics: &[f32]) -> Result<(DepthImage, Camera)> {
    anyhow::ensure!(
        values.len() == (w * h) as usize && intrinsics.len() == 9,
        "Invalid Relight geometry dimensions"
    );
    rawtonic_onnx::ensure_finite(values)?;
    rawtonic_onnx::ensure_finite(intrinsics)?;
    anyhow::ensure!(values.iter().all(|v| *v > 0.), "Invalid Relight distance");
    let mut ordered = values.to_vec();
    let middle = ordered.len() / 2;
    ordered.select_nth_unstable_by(middle, f32::total_cmp);
    let reference = ordered[middle];
    // Monocular distance is relative. Normalize once, retaining ratios, and
    // bound extreme sky/outlier distances before reciprocal GPU projection.
    let inverse: Vec<_> = values
        .iter()
        .map(|d| (reference / d).clamp(0.0001, 100.))
        .collect();
    let lo = inverse.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = inverse.iter().copied().fold(0., f32::max);
    let camera = Camera {
        fx: intrinsics[0] / w as f32,
        fy: intrinsics[4] / h as f32,
        cx: intrinsics[2] / w as f32,
        cy: intrinsics[5] / h as f32,
        inverse_min: lo,
        inverse_max: hi,
        reference: 1.,
    };
    anyhow::ensure!(
        Camera::from_recipe(&serde_json::json!({"relightDepth":{"version":4,"camera":camera}}))
            .is_some(),
        "Invalid Relight camera"
    );
    let range = hi - lo;
    let codes = inverse
        .iter()
        .map(|q| {
            if range > 0. {
                ((q - lo) / range * 65535.).round() as u16
            } else {
                0
            }
        })
        .collect();
    Ok((
        image::ImageBuffer::from_raw(w, h, codes)
            .ok_or_else(|| anyhow::anyhow!("Invalid Relight dimensions"))?,
        camera,
    ))
}

pub fn run_relight_model(
    image: &DynamicImage,
    model: &rawtonic_onnx::Model,
    linear: bool,
) -> Result<(DepthImage, Camera)> {
    let (w, h) = size(image.width(), image.height());
    let rgb = display_rgb8(image.resize_exact(w, h, FilterType::CatmullRom), linear);
    let mean = [0.485, 0.456, 0.406];
    let std = [0.229, 0.224, 0.225];
    let input =
        ndarray::Array5::from_shape_fn((1, 1, 3, h as usize, w as usize), |(_, _, c, y, x)| {
            (rgb.get_pixel(x as u32, y as u32)[c] as f32 / 255. - mean[c]) / std[c]
        });
    model.run(|session| {
        let output = session.run(ort::inputs!["image" => Tensor::from_array(input.clone())?])?;
        let (shape, values) = output["depth"].try_extract_tensor::<f32>()?;
        anyhow::ensure!(
            shape.as_ref() == [1, 1, h as i64, w as i64],
            "Invalid Relight output shape"
        );
        let (_, k) = output["intrinsics"].try_extract_tensor::<f32>()?;
        encode(w, h, values, k)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distance_ratios_and_camera_survive_the_recipe() {
        let (image, c) = encode(
            4,
            2,
            &[1., 2., 3., 4., 2., 3., 4., 5.],
            &[6., 0., 2., 0., 6., 1., 0., 0., 1.],
        )
        .unwrap();
        for (i, &d) in [1., 2., 3., 4., 2., 3., 4., 5.].iter().enumerate() {
            let decoded = c.inverse(image.as_raw()[i]).recip();
            assert!((decoded - d / 3.).abs() < 0.0001);
        }
        assert_eq!(c.fx, 1.5);
        assert_eq!(c.fy, 3.);
        assert_eq!(c.cx, 0.5);
        assert_eq!(c.cy, 0.5);
        let flat = encode(2, 2, &[4.; 4], &[6., 0., 1., 0., 6., 1., 0., 0., 1.]).unwrap();
        assert!(flat.0.as_raw().iter().all(|v| flat.1.inverse(*v) == 1.));
    }
    #[test]
    fn invalid_model_geometry_is_rejected() {
        for values in [vec![], vec![0.; 4], vec![f32::NAN; 4], vec![-1.; 4]] {
            assert!(encode(2, 2, &values, &[3., 0., 1., 0., 3., 1., 0., 0., 1.]).is_err());
        }
        assert!(encode(2, 2, &[1.; 4], &[0.; 9]).is_err());
        assert_eq!(size(6000, 4000), (518, 350));
        assert_eq!(size(4000, 6000), (350, 518));
        assert_eq!(size(20000, 1), (518, 28));
    }
    /// The analysis `generate_relight_depth` stores, of a JPEG/PNG without
    /// the app: writes `analysis.json` (the recipe's `relightDepth` for an
    /// untransformed photo) and `subject.png` to RELIGHT_OUT. Render it with
    /// `rawtonic export PHOTO --adjustments recipe.json` and Relight lights.
    #[test]
    #[ignore = "requires RELIGHT_PHOTO, RELIGHT_MODELS (the app's model folder), RELIGHT_OUT and ORT_DYLIB_PATH"]
    fn writes_the_analysis_of_a_photo() {
        use base64::Engine as _;
        let env = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name}"));
        // RELIGHT_APP_LOAD: decode as the app's editor does (engine loader).
        let photo = if std::env::var_os("RELIGHT_APP_LOAD").is_some() {
            let path = env("RELIGHT_PHOTO");
            rawtonic_engine::image_loader::load_base_image(
                &std::fs::read(&path).unwrap(),
                &path,
                false,
                &rawtonic_engine::image_loader::BaseImageOptions::from_preferences(
                    None,
                    String::new(),
                    None,
                    None,
                    None,
                ),
                None,
            )
            .unwrap()
        } else {
            image::open(env("RELIGHT_PHOTO")).unwrap()
        };
        let dir = std::path::PathBuf::from(env("RELIGHT_MODELS"));
        let out = std::path::PathBuf::from(env("RELIGHT_OUT"));
        std::fs::create_dir_all(&out).unwrap();
        let start = std::time::Instant::now();
        let geometry =
            rawtonic_onnx::Model::load(&dir.join(FILE), rawtonic_onnx::ModelKind::Relight).unwrap();
        let (depth, camera) = run_relight_model(&photo, &geometry, false).unwrap();
        eprintln!("geometry {} ms", start.elapsed().as_millis());
        let small = if photo.width().max(photo.height()) > 1600 {
            crate::image_processing::thumbnail(&photo, 1600, 1600)
        } else {
            photo.clone()
        };
        let faces_models = rawtonic_faces::FaceModels::load(&dir).unwrap();
        let faces = match std::env::var("RELIGHT_FACES") {
            // Landmarks of another analysis (e.g. the app's), for comparisons.
            Ok(path) => serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap(),
            Err(_) => rawtonic_faces::relight::analyze(
                &rawtonic_faces::recipe::display_rgb8(&small, false),
                &faces_models,
            )
            .unwrap(),
        };
        eprintln!("{} faces, {} ms", faces.len(), start.elapsed().as_millis());
        let mut result = serde_json::json!({
            "geometry": rawtonic_engine::relight::geometry(&serde_json::json!({})),
            "faces": faces,
            "version": 5,
            "camera": camera,
        });
        let subject = super::super::SubjectModel::onnx(&dir.join("birefnet_subject.onnx")).unwrap();
        let matte = super::super::run_subject_model(&small, &subject, false, None).unwrap();
        let people = rawtonic_onnx::Model::load(
            &dir.join(PEOPLE_FILE),
            rawtonic_onnx::ModelKind::PeopleNormals,
        )
        .unwrap();
        let t = std::time::Instant::now();
        let parse = faces.is_empty().then(|| {
            rawtonic_faces::relight::parse_people(
                &rawtonic_faces::recipe::display_rgb8(&small, false),
                &faces_models,
            )
            .unwrap()
        });
        let sky = super::super::SkyModel::onnx(&dir.join("skyseg_upernet_convnext_t.onnx"))
            .and_then(|model| super::super::run_sky_seg_model(&small, &model, false, None))
            .ok();
        if let Ok((png, light)) = analyze_subject(
            &small,
            false,
            &matte,
            SubjectContext {
                faces: &faces,
                parse: parse.as_ref(),
                people_model: Some(&people),
                scene: Some((&depth, camera)),
                sky: sky.as_ref(),
            },
        ) {
            eprintln!("subject {} ms, lighting {light:?}", t.elapsed().as_millis());
            std::fs::write(out.join("subject.png"), &png).unwrap();
            matte.save(out.join("matte.png")).unwrap();
            result["subject"] = format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            )
            .into();
            if let Some(light) = light {
                result["sourceLight"] = serde_json::json!(light);
            }
            if let Some(inverse) = rawtonic_engine::relight::subject_inverse(&depth, camera, &matte)
            {
                result["subjectDepth"] = serde_json::json!(inverse);
            }
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma16(depth)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        result["data"] = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png.into_inner())
        )
        .into();
        std::fs::write(
            out.join("analysis.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[ignore = "requires RELIGHT_GEOMETRY_MODEL and ORT_DYLIB_PATH"]
    fn native_geometry_handles_portrait_landscape_and_narrow_frames() {
        let path = std::env::var("RELIGHT_GEOMETRY_MODEL").expect("RELIGHT_GEOMETRY_MODEL");
        let model = rawtonic_onnx::Model::load_on(
            Path::new(&path),
            rawtonic_onnx::ModelKind::Relight,
            &[rawtonic_onnx::Provider::Cpu],
            &[],
        )
        .unwrap();
        for (w, h) in [
            (518, 350),
            (350, 518),
            (518, 518),
            (518, 140),
            (518, 14),
            (14, 518),
        ] {
            let image = DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
                image::Rgb([(255 * x / w) as u8, (255 * y / h) as u8, 100])
            }));
            let start = std::time::Instant::now();
            let (depth, camera) = run_relight_model(&image, &model, false).unwrap();
            assert_eq!(depth.dimensions(), size(w, h));
            assert!(
                depth
                    .as_raw()
                    .iter()
                    .all(|v| camera.inverse(*v).is_finite())
            );
            eprintln!("Native Relight {w}x{h}: {} ms", start.elapsed().as_millis());
        }
    }
}
