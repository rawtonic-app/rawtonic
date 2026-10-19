use anyhow::Result;
use futures::stream::{self, StreamExt};
use image::{DynamicImage, imageops::FilterType};
use ndarray::Array;
use ort::value::Tensor;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::task::JoinHandle;
use walkdir::WalkDir;

use crate::app_state::AppState;
use crate::file_management::{self, parse_virtual_path};
use crate::formats::is_supported_image_file;
use crate::image_processing::ImageMetadata;
use crate::tagging_utils::candidates::TAG_CANDIDATES;
use crate::tagging_utils::hierarchy::TAG_HIERARCHY;

pub const COLOR_TAG_PREFIX: &str = "color:";
pub const USER_TAG_PREFIX: &str = "user:";

/// SigLIP 2 input: the photo squashed to 224 x 224, `(v / 255 - 0.5) / 0.5`.
fn preprocess_siglip_image(image: &DynamicImage) -> Array<f32, ndarray::Dim<[usize; 4]>> {
    let input_size = 224;
    let resized = image.resize_exact(input_size, input_size, FilterType::Triangle);
    let rgb_image = resized.to_rgb8();
    let mut array = Array::zeros((1, 3, input_size as usize, input_size as usize));
    for (x, y, pixel) in rgb_image.enumerate_pixels() {
        for c in 0..3 {
            array[[0, c, y as usize, x as usize]] = pixel[c] as f32 / 127.5 - 1.0;
        }
    }
    array
}

/// SigLIP 2's learned calibration: score = sigmoid(scale * cosine + bias).
const SIGLIP_LOGIT_SCALE: f32 = 112.668_9;
const SIGLIP_LOGIT_BIAS: f32 = -16.771_725;
/// A tag is kept when its score reaches this share of the best tag's.
const RELATIVE_TAG_SCORE: f32 = 0.1;
const MIN_TAG_SCORE: f32 = 0.0005;
/// The prompt the built-in tag embeddings were computed with.
const TAG_PROMPT: &str = "a photo of ";

fn tag_score(cosine: f32) -> f32 {
    1.0 / (1.0 + (-(SIGLIP_LOGIT_SCALE * cosine + SIGLIP_LOGIT_BIAS)).exp())
}

/// The tags of `tags` whose scores are near the best one, best first.
fn select_tags(scores: &[f32], tags: &[String], max_tags: usize) -> Vec<String> {
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
    let best = order.first().map(|&i| scores[i]).unwrap_or(0.0);
    let floor = MIN_TAG_SCORE.max(best * RELATIVE_TAG_SCORE);
    order
        .into_iter()
        .take(max_tags)
        .filter(|&i| scores[i] >= floor)
        .map(|i| tags[i].clone())
        .collect()
}

fn rgb_to_hsv((r, g, b): (u8, u8, u8)) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let h = if delta.abs() < f32::EPSILON {
        0.0
    } else if (max - r).abs() < f32::EPSILON {
        60.0 * (((g - b) / delta) % 6.0)
    } else if (max - g).abs() < f32::EPSILON {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h };

    let s = if max.abs() < f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    let v = max;

    (h, s, v)
}

pub fn extract_color_tags(image: &DynamicImage) -> Vec<String> {
    let resized = image.resize(100, 100, FilterType::Triangle);
    let rgb_image = resized.to_rgb8();
    let mut color_counts: HashMap<String, u32> = HashMap::new();

    for pixel in rgb_image.pixels() {
        let rgb = (pixel[0], pixel[1], pixel[2]);
        let (h, s, v) = rgb_to_hsv(rgb);

        let color_name = if v < 0.2 {
            "black".to_string()
        } else if s < 0.1 {
            if v > 0.8 {
                "white".to_string()
            } else {
                "gray".to_string()
            }
        } else {
            match h {
                _ if !(20.0..340.0).contains(&h) => "red".to_string(),
                _ if (20.0..45.0).contains(&h) => "orange".to_string(),
                _ if (45.0..70.0).contains(&h) => "yellow".to_string(),
                _ if (70.0..160.0).contains(&h) => "green".to_string(),
                _ if (160.0..260.0).contains(&h) => "blue".to_string(),
                _ if (260.0..340.0).contains(&h) => "purple".to_string(),
                _ => "unknown".to_string(),
            }
        };

        if (color_name == "orange" || color_name == "red") && v < 0.6 && s < 0.7 {
            *color_counts.entry("brown".to_string()).or_insert(0) += 1;
        } else {
            *color_counts.entry(color_name).or_insert(0) += 1;
        }
    }

    let mut colorful_tags: Vec<(String, u32)> = color_counts
        .iter()
        .filter(|(name, _)| !matches!(name.as_str(), "black" | "white" | "gray"))
        .map(|(name, &count)| (name.clone(), count))
        .collect();

    colorful_tags.sort_by_key(|b| std::cmp::Reverse(b.1));

    if !colorful_tags.is_empty() {
        colorful_tags
            .into_iter()
            .take(2)
            .map(|(name, _)| name)
            .collect()
    } else {
        color_counts
            .into_iter()
            .max_by_key(|&(_, count)| count)
            .map(|(name, _)| vec![name])
            .unwrap_or_default()
    }
}

/// Embeddings of the user's own tags ("a photo of <tag>", lower case), one
/// row of `SIGLIP_EMBED_DIM` per tag.
pub fn embed_custom_tags(
    text: &crate::ai_processing::TextEncoder,
    tags: &[String],
) -> Result<Vec<f32>> {
    let prompts: Vec<String> = tags
        .iter()
        .map(|t| format!("{TAG_PROMPT}{}", t.to_lowercase()))
        .collect();
    let encodings = text
        .tokenizer
        .encode_batch(prompts, true)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let len = encodings.first().map(|e| e.get_ids().len()).unwrap_or(0);
    let ids: Vec<i64> = encodings
        .iter()
        .flat_map(|e| e.get_ids().iter().map(|&i| i as i64))
        .collect();
    let ids = Array::from_shape_vec((tags.len(), len), ids)?;
    text.session.run(|session| {
        let outputs = session.run(ort::inputs![Tensor::from_array(ids.clone())?])?;
        let embeds: Vec<f32> = outputs[0]
            .try_extract_array::<f32>()?
            .iter()
            .copied()
            .collect();
        rawtonic_onnx::ensure_finite(&embeds)?;
        Ok(embeds)
    })
}

/// Tags for `image` among `tags`, whose embeddings are `tag_embeddings`
/// (`SIGLIP_EMBED_DIM` per tag). Built-in tags also get colour tags and
/// their parents in the hierarchy.
pub fn generate_tags(
    image: &DynamicImage,
    vision: &rawtonic_onnx::Model,
    tags: &[String],
    tag_embeddings: &[f32],
    is_custom: bool,
    max_tags: usize,
) -> Result<Vec<String>> {
    let dim = crate::ai_processing::SIGLIP_EMBED_DIM;
    let input = preprocess_siglip_image(image);
    let image_embedding: Vec<f32> = vision.run(|session| {
        let outputs = session.run(ort::inputs![Tensor::from_array(input.clone())?])?;
        let embedding: Vec<f32> = outputs[0]
            .try_extract_array::<f32>()?
            .iter()
            .copied()
            .collect();
        rawtonic_onnx::ensure_finite(&embedding)?;
        Ok(embedding)
    })?;
    let scores: Vec<f32> = tag_embeddings
        .chunks_exact(dim)
        .map(|row| tag_score(row.iter().zip(&image_embedding).map(|(a, b)| a * b).sum()))
        .collect();
    let initial_tags = select_tags(&scores, tags, max_tags);

    let mut final_tags_set: HashSet<String> = initial_tags.iter().cloned().collect();

    if !is_custom {
        let color_tags = extract_color_tags(image);
        for color_tag in color_tags {
            final_tags_set.insert(color_tag);
        }

        for tag in &initial_tags {
            if let Some(parents) = TAG_HIERARCHY.get(tag.as_str()) {
                for &parent in parents {
                    final_tags_set.insert(parent.to_string());
                }
            }
        }
    }

    let final_tags = final_tags_set.into_iter().collect();

    Ok(final_tags)
}

#[tauri::command]
pub async fn start_background_indexing(
    folder_path: String,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if let Some(handle) = state.indexing_task_handle.lock().unwrap().take() {
        println!("Cancelling previous indexing task.");
        handle.abort();
    }

    let settings = crate::app_settings::load_settings(app_handle.clone())?;
    if !settings.enable_ai_tagging.unwrap_or(false) {
        return Ok(());
    }

    let max_concurrent_tasks = settings.tagging_thread_count.unwrap_or(3).max(1) as usize;
    let custom_ai_tags: Vec<String> = settings
        .custom_ai_tags
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let is_custom = !custom_ai_tags.is_empty();
    let ai_tag_count = settings.ai_tag_count.unwrap_or(10) as usize;

    let tag_models = crate::ai_processing::get_or_init_tag_models(
        &app_handle,
        &state.ai_state,
        &state.ai_init_lock,
        is_custom,
    )
    .await
    .map_err(|e| e.to_string())?;
    // The tag list and its embeddings, once for the whole folder.
    let (tag_names, tag_embeddings) = match &tag_models.text {
        Some(text) if is_custom => {
            let embeddings = embed_custom_tags(text, &custom_ai_tags).map_err(|e| e.to_string())?;
            (custom_ai_tags, embeddings)
        }
        _ => (
            TAG_CANDIDATES.iter().map(|&s| s.to_string()).collect(),
            tag_models.tag_embeddings.clone(),
        ),
    };

    let app_handle_clone = app_handle.clone();

    let task: JoinHandle<()> = tokio::spawn(async move {
        let _ = app_handle_clone.emit("indexing-started", ());
        println!("Starting background indexing for: {}", folder_path);
        println!(
            "Using {} concurrent threads for AI tagging.",
            max_concurrent_tasks
        );

        let state_clone = app_handle_clone.state::<AppState>();
        let gpu_context =
            crate::gpu_processing::get_or_init_gpu_context(&state_clone, &app_handle).ok();

        let image_paths: Vec<PathBuf> = match fs::read_dir(&folder_path) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file() && is_supported_image_file(path.to_string_lossy().as_ref())
                })
                .collect(),
            Err(e) => {
                eprintln!("Failed to read directory '{}': {}", folder_path, e);
                let _ = app_handle_clone
                    .emit("indexing-error", format!("Failed to read directory: {}", e));
                *app_handle_clone
                    .state::<AppState>()
                    .indexing_task_handle
                    .lock()
                    .unwrap() = None;
                return;
            }
        };

        println!(
            "Found {} images to process in {}",
            image_paths.len(),
            folder_path
        );
        let total_images = image_paths.len();
        let processed_count = Arc::new(Mutex::new(0));
        let tag_set = Arc::new((tag_names, tag_embeddings));

        stream::iter(image_paths)
            .for_each_concurrent(max_concurrent_tasks, |path| {
                let app_handle_inner = app_handle_clone.clone();
                let tag_models_inner = tag_models.clone();
                let gpu_context_inner = gpu_context.clone();
                let processed_count_inner = Arc::clone(&processed_count);
                let tags_inner = Arc::clone(&tag_set);

                async move {
                    let path_str = path.to_string_lossy().to_string();
                    let (_, sidecar_path) = parse_virtual_path(&path_str);

                    let metadata = crate::exif_processing::load_sidecar(&sidecar_path);

                    let should_generate_tags = match &metadata.tags {
                        None => true,
                        Some(tags) => !tags.iter().any(|tag| {
                            !tag.starts_with(COLOR_TAG_PREFIX) && !tag.starts_with(USER_TAG_PREFIX)
                        }),
                    };

                    if should_generate_tags {
                        match file_management::get_cached_or_generate_thumbnail_image(
                            &path_str,
                            &app_handle_inner,
                            gpu_context_inner.as_ref(),
                        ) {
                            Ok(image) => {
                                if let Ok(ai_tags) = generate_tags(
                                    &image,
                                    &tag_models_inner.vision,
                                    &tags_inner.0,
                                    &tags_inner.1,
                                    is_custom,
                                    ai_tag_count,
                                ) {
                                    println!("Found AI tags for {}: {:?}", path_str, ai_tags);
                                    if let Err(e) = store_ai_tags(&sidecar_path, ai_tags) {
                                        eprintln!("Could not save AI tags for {}: {}", path_str, e);
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "Could not get or generate image for tagging {}: {}",
                                    path_str, e
                                );
                            }
                        }
                    }

                    let mut count = processed_count_inner.lock().unwrap();
                    *count += 1;
                    let _ = app_handle_inner.emit(
                        "indexing-progress",
                        serde_json::json!({
                            "current": *count,
                            "total": total_images
                        }),
                    );
                }
            })
            .await;

        println!("Background indexing finished for: {}", folder_path);
        let _ = app_handle_clone.emit("indexing-finished", ());

        *app_handle_clone
            .state::<AppState>()
            .indexing_task_handle
            .lock()
            .unwrap() = None;
    });

    *state.indexing_task_handle.lock().unwrap() = Some(task);

    Ok(())
}

/// Adds `ai_tags` to the photo's saved tags. Inference takes seconds, and
/// the user may edit, rate or label the photo meanwhile: the sidecar is read
/// again here, under the same turn as those edits, and only its tags change.
fn store_ai_tags(sidecar_path: &Path, ai_tags: Vec<String>) -> Result<(), String> {
    let _turn = file_management::sidecar_field_edit();
    let mut metadata = crate::exif_processing::load_sidecar(sidecar_path);
    let mut tags: HashSet<String> = metadata.tags.unwrap_or_default().into_iter().collect();
    tags.extend(ai_tags);
    let mut tags: Vec<String> = tags.into_iter().collect();
    tags.sort_unstable();
    metadata.tags = Some(tags);
    let json_string = serde_json::to_string_pretty(&metadata).map_err(|e| e.to_string())?;
    crate::session_bridge::write_editor_sidecar(sidecar_path, json_string)
        .map_err(|e| e.to_string())
}

fn modify_tags_for_path(
    path_str: &str,
    app_handle: &AppHandle,
    modify_fn: impl Fn(&mut Vec<String>),
) -> Result<(), String> {
    let (source_path, sidecar_path) = parse_virtual_path(path_str);

    let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

    let mut tags = metadata.tags.unwrap_or_default();
    modify_fn(&mut tags);

    tags.sort_unstable();
    tags.dedup();

    if tags.is_empty() {
        metadata.tags = None;
    } else {
        metadata.tags = Some(tags);
    }

    let json_string = serde_json::to_string_pretty(&metadata).map_err(|e| e.to_string())?;
    crate::session_bridge::write_editor_sidecar(&sidecar_path, json_string)
        .map_err(|e| e.to_string())?;

    if let Ok(settings) = crate::app_settings::load_settings(app_handle.clone())
        && settings.enable_xmp_sync.unwrap_or(false)
    {
        let create_if_missing = settings.create_xmp_if_missing.unwrap_or(false);
        file_management::sync_metadata_to_xmp(&source_path, &metadata, create_if_missing);
    }

    Ok(())
}

#[tauri::command]
pub async fn add_tag_for_paths(
    paths: Vec<String>,
    tag: String,
    app_handle: AppHandle,
) -> Result<(), String> {
    crate::file_management::blocking(move || add_tag_for_paths_blocking(paths, tag, app_handle))
        .await?
}

fn add_tag_for_paths_blocking(
    paths: Vec<String>,
    tag: String,
    app_handle: AppHandle,
) -> Result<(), String> {
    let _turn = file_management::sidecar_field_edit();
    let failures: Vec<(String, String)> = paths
        .par_iter()
        .filter_map(|path| {
            modify_tags_for_path(path, &app_handle, |tags| {
                if !tags.contains(&tag) {
                    tags.push(tag.clone());
                }
            })
            .err()
            .map(|e| (path.clone(), e))
        })
        .collect();
    file_management::SaveFailure::check(paths.len(), failures).map_err(|f| f.message)
}

#[tauri::command]
pub async fn remove_tag_for_paths(
    paths: Vec<String>,
    tag: String,
    app_handle: AppHandle,
) -> Result<(), String> {
    crate::file_management::blocking(move || remove_tag_for_paths_blocking(paths, tag, app_handle))
        .await?
}

fn remove_tag_for_paths_blocking(
    paths: Vec<String>,
    tag: String,
    app_handle: AppHandle,
) -> Result<(), String> {
    let _turn = file_management::sidecar_field_edit();
    let failures: Vec<(String, String)> = paths
        .par_iter()
        .filter_map(|path| {
            modify_tags_for_path(path, &app_handle, |tags| {
                tags.retain(|t| t != &tag);
            })
            .err()
            .map(|e| (path.clone(), e))
        })
        .collect();
    file_management::SaveFailure::check(paths.len(), failures).map_err(|f| f.message)
}

fn rrdata_source_path(rrdata: &Path) -> Option<PathBuf> {
    let name = rrdata.file_name()?.to_str()?;
    let base = name.strip_suffix(".rrdata")?;

    let source_filename = if base.len() >= 7 && base.as_bytes()[base.len() - 7] == b'.' {
        let id = &base[base.len() - 6..];
        if id.chars().all(|c| c.is_ascii_hexdigit()) {
            &base[..base.len() - 7]
        } else {
            base
        }
    } else {
        base
    };

    Some(rrdata.with_file_name(source_filename))
}

fn sync_xmp_for_rrdata(
    rrdata_path: &Path,
    metadata: &ImageMetadata,
    enable_xmp_sync: bool,
    create_xmp_if_missing: bool,
) {
    if !enable_xmp_sync {
        return;
    }
    if let Some(source_path) = rrdata_source_path(rrdata_path) {
        file_management::sync_metadata_to_xmp(&source_path, metadata, create_xmp_if_missing);
    }
}

#[tauri::command]
pub async fn clear_ai_tags(root_path: String, app_handle: AppHandle) -> Result<usize, String> {
    crate::file_management::blocking(move || clear_ai_tags_blocking(root_path, app_handle)).await?
}

fn clear_ai_tags_blocking(root_path: String, app_handle: AppHandle) -> Result<usize, String> {
    if !Path::new(&root_path).exists() {
        return Err(format!("Root path does not exist: {}", root_path));
    }
    let _turn = file_management::sidecar_field_edit();

    let settings = crate::app_settings::load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
    let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

    let mut updated_count = 0;
    let walker = WalkDir::new(root_path).into_iter();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file()
            && path.extension().and_then(|s| s.to_str()) == Some("rrdata")
            && let Ok(content) = fs::read_to_string(path)
            && let Ok(mut metadata) = serde_json::from_str::<ImageMetadata>(&content)
            && let Some(tags) = &mut metadata.tags
        {
            let original_len = tags.len();
            // Keep color tags and user tags, remove others (AI tags)
            tags.retain(|tag| {
                tag.starts_with(COLOR_TAG_PREFIX) || tag.starts_with(USER_TAG_PREFIX)
            });

            if tags.len() < original_len {
                if tags.is_empty() {
                    metadata.tags = None;
                }
                if let Ok(json_string) = serde_json::to_string_pretty(&metadata)
                    && rawtonic_metadata::atomic_write(path, json_string).is_ok()
                {
                    updated_count += 1;
                    sync_xmp_for_rrdata(path, &metadata, enable_xmp_sync, create_xmp_if_missing);
                }
            }
        }
    }
    Ok(updated_count)
}

#[tauri::command]
pub async fn clear_all_tags(root_path: String, app_handle: AppHandle) -> Result<usize, String> {
    crate::file_management::blocking(move || clear_all_tags_blocking(root_path, app_handle)).await?
}

fn clear_all_tags_blocking(root_path: String, app_handle: AppHandle) -> Result<usize, String> {
    if !Path::new(&root_path).exists() {
        return Err(format!("Root path does not exist: {}", root_path));
    }
    let _turn = file_management::sidecar_field_edit();

    let settings = crate::app_settings::load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
    let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

    let mut updated_count = 0;
    let walker = WalkDir::new(root_path).into_iter();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file()
            && path.extension().and_then(|s| s.to_str()) == Some("rrdata")
            && let Ok(content) = fs::read_to_string(path)
            && let Ok(mut metadata) = serde_json::from_str::<ImageMetadata>(&content)
            && let Some(tags) = &mut metadata.tags
        {
            let original_len = tags.len();
            // Keep only color tags, remove AI and user tags
            tags.retain(|tag| tag.starts_with(COLOR_TAG_PREFIX));

            if tags.len() < original_len {
                if tags.is_empty() {
                    metadata.tags = None;
                }
                if let Ok(json_string) = serde_json::to_string_pretty(&metadata)
                    && rawtonic_metadata::atomic_write(path, json_string).is_ok()
                {
                    updated_count += 1;
                    sync_xmp_for_rrdata(path, &metadata, enable_xmp_sync, create_xmp_if_missing);
                }
            }
        }
    }
    Ok(updated_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Background tagging must never lose an edit saved while its model ran:
    /// it re-reads the sidecar when it stores the tags, and waits while an
    /// edit holds the sidecar turn.
    #[test]
    fn ai_tags_keep_edits_saved_during_inference() {
        let tmp = tempfile::tempdir().unwrap();
        let sidecar = tmp.path().join("A.jpg.rrdata");
        let write = |json: &str| {
            crate::session_bridge::write_editor_sidecar(&sidecar, json).unwrap();
        };
        write(r#"{"version":1,"rating":0,"adjustments":{}}"#);

        // An edit saved after tagging read the photo for inference.
        write(r#"{"version":1,"rating":4,"tags":["user:trip"],"adjustments":{"exposure":1.5}}"#);
        store_ai_tags(&sidecar, vec!["dog".into()]).unwrap();
        let saved = crate::exif_processing::load_sidecar(&sidecar);
        assert_eq!(saved.rating, 4);
        assert_eq!(saved.adjustments["exposure"], 1.5);
        assert_eq!(saved.tags.unwrap(), ["dog", "user:trip"]);

        // An edit in progress (read, change, write) finishes first.
        let turn = file_management::sidecar_field_edit();
        let tagger = std::thread::spawn({
            let sidecar = sidecar.clone();
            move || store_ai_tags(&sidecar, vec!["beach".into()]).unwrap()
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        write(
            r#"{"version":1,"rating":5,"tags":["dog","user:trip"],"adjustments":{"exposure":2.0}}"#,
        );
        drop(turn);
        tagger.join().unwrap();
        let saved = crate::exif_processing::load_sidecar(&sidecar);
        assert_eq!(saved.rating, 5);
        assert_eq!(saved.adjustments["exposure"], 2.0);
        assert_eq!(saved.tags.unwrap(), ["beach", "dog", "user:trip"]);
    }

    #[test]
    fn tags_near_the_best_score_are_kept_best_first() {
        let tags: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        // c is best; a reaches a tenth of it, d does not, b is under the floor.
        let scores = [0.012, 0.0001, 0.1, 0.009];
        assert_eq!(select_tags(&scores, &tags, 10), ["c", "a"]);
        assert_eq!(select_tags(&scores, &tags, 1), ["c"]);
        // Nothing above the absolute floor: no tags.
        assert!(select_tags(&[0.0002, 0.0001], &tags[..2], 10).is_empty());
    }

    #[test]
    fn scores_follow_siglips_calibration() {
        // sigmoid(scale * cos + bias) is one half where the cosine cancels the bias.
        let half = -SIGLIP_LOGIT_BIAS / SIGLIP_LOGIT_SCALE;
        assert!((tag_score(half) - 0.5).abs() < 1e-4);
        assert!(tag_score(0.05) < 0.001 && tag_score(0.25) > 0.99);
    }

    /// The text encoder gives the user's own tags the same embeddings the
    /// built-in list was exported with (same tokenizer, prompt and model).
    #[test]
    #[ignore = "needs AI_MODELS (pack incl. siglip2_text.onnx) and ORT_DYLIB_PATH"]
    fn own_tags_embed_like_the_built_in_list() {
        let models = std::path::PathBuf::from(std::env::var("AI_MODELS").unwrap());
        let text = crate::ai_processing::TextEncoder {
            session: rawtonic_onnx::Model::load_file(&models.join("siglip2_text.onnx")).unwrap(),
            tokenizer: tokenizers::Tokenizer::from_file(models.join("siglip2_tokenizer.json"))
                .unwrap(),
        };
        let built_in = crate::ai_processing::read_tag_embeddings(
            &std::fs::read(models.join("siglip2_tags.bin")).unwrap(),
            TAG_CANDIDATES.len(),
        )
        .unwrap();
        let dim = crate::ai_processing::SIGLIP_EMBED_DIM;
        let row = |i: usize| built_in[i * dim..(i + 1) * dim].to_vec();
        let picks = ["mountain", "golden hour", "dog"];
        let own: Vec<String> = picks.iter().map(|s| s.to_uppercase()).collect();
        let embedded = embed_custom_tags(&text, &own).unwrap();
        for (k, name) in picks.iter().enumerate() {
            let i = TAG_CANDIDATES.iter().position(|t| t == name).unwrap();
            let cosine: f32 = row(i)
                .iter()
                .zip(&embedded[k * dim..(k + 1) * dim])
                .map(|(a, b)| a * b)
                .sum();
            assert!(cosine > 0.9999, "{name}: cosine {cosine}");
        }
    }

    /// Tool: built-in tags for the images in `$AI_FILES` (`;`-separated).
    #[test]
    #[ignore = "tool: needs AI_FILES, AI_MODELS and ORT_DYLIB_PATH"]
    fn print_tags() {
        let models = std::path::PathBuf::from(std::env::var("AI_MODELS").unwrap());
        let vision = rawtonic_onnx::Model::load_file(&models.join("siglip2_vision.onnx")).unwrap();
        let embeddings = crate::ai_processing::read_tag_embeddings(
            &std::fs::read(models.join("siglip2_tags.bin")).unwrap(),
            TAG_CANDIDATES.len(),
        )
        .unwrap();
        let tags: Vec<String> = TAG_CANDIDATES.iter().map(|s| s.to_string()).collect();
        for path in std::env::var("AI_FILES").unwrap().split(';') {
            let image = image::open(path).unwrap();
            let found = generate_tags(&image, &vision, &tags, &embeddings, true, 10).unwrap();
            println!("{path}\t{}", found.join(", "));
        }
    }
}
