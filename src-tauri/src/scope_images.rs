//! The scope images (waveform, RGB waveform, parade, vectorscope PNGs) for
//! the webview by URL: `analytics-update` names an image by its id and the
//! `<img>` loads it from the `rawtonic-scope` URI scheme. Inside the event
//! (Tauri's emit is one evaluated script) the 100–280 KB of base64 cost
//! the webview's main thread ~0.5 ms per RGB update in Blink, beside the
//! drag's preview replies (notes: benchmark-results/scp5-2026-11-20).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use rawtonic_engine::scopes::{HistogramData, VectorscopeTarget, WaveformData};

/// The URI scheme the webview loads scope images from.
pub(crate) const SCHEME: &str = "rawtonic-scope";

/// Images kept from the newest updates: an `<img>` may still be loading
/// one while the next updates (about ten a second while dragging) arrive.
const RECENT: usize = 8;

/// Photos whose newest image is kept beyond `RECENT`: the editor shows a
/// photo's last scopes again when it returns to it (`globalImageCache`
/// holds 20 photos in src/utils/ImageLRUCache.ts).
const PHOTOS: usize = 24;

type Png = Arc<[u8]>;

#[derive(Default)]
struct Kept {
    next: u64,
    recent: VecDeque<(u64, Png)>,
    newest_per_photo: VecDeque<(String, u64, Png)>,
}

impl Kept {
    fn insert(&mut self, photo: &str, png: Vec<u8>) -> u64 {
        self.next += 1;
        let id = self.next;
        let png: Png = png.into();
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back((id, png.clone()));
        self.newest_per_photo.retain(|(path, ..)| path != photo);
        if self.newest_per_photo.len() == PHOTOS {
            self.newest_per_photo.pop_front();
        }
        self.newest_per_photo.push_back((photo.to_owned(), id, png));
        id
    }

    fn get(&self, id: u64) -> Option<Png> {
        self.recent
            .iter()
            .rev()
            .find(|(kept, _)| *kept == id)
            .map(|(_, png)| png.clone())
            .or_else(|| {
                self.newest_per_photo
                    .iter()
                    .find(|(_, kept, _)| *kept == id)
                    .map(|(.., png)| png.clone())
            })
    }
}

fn kept() -> &'static Mutex<Kept> {
    static KEPT: OnceLock<Mutex<Kept>> = OnceLock::new();
    KEPT.get_or_init(Default::default)
}

/// `WaveformData` as the event carries it: each image as the id the
/// webview loads it by, empty for a mode that was not computed.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct WaveformUpdate {
    rgb: String,
    luma: String,
    parade: String,
    vectorscope: String,
    width: u32,
    height: u32,
    vectorscope_targets: Vec<VectorscopeTarget>,
    skin_line_degrees: f32,
}

/// The `analytics-update` payload of `photo`'s update: the scope images
/// are kept here and named by id.
pub(crate) fn update(
    photo: &str,
    histogram: Option<HistogramData>,
    waveform: Option<WaveformData>,
) -> serde_json::Value {
    let waveform = waveform.map(|w| {
        let mut kept = kept().lock().unwrap_or_else(|e| e.into_inner());
        let mut id = |png: Vec<u8>| {
            if png.is_empty() {
                String::new()
            } else {
                kept.insert(photo, png).to_string()
            }
        };
        WaveformUpdate {
            rgb: id(w.rgb),
            luma: id(w.luma),
            parade: id(w.parade),
            vectorscope: id(w.vectorscope),
            width: w.width,
            height: w.height,
            vectorscope_targets: w.vectorscope_targets,
            skin_line_degrees: w.skin_line_degrees,
        }
    });
    serde_json::json!({ "path": photo, "histogram": histogram, "waveform": waveform })
}

/// Answers `rawtonic-scope://localhost/<id>` with the kept PNG, 404 when
/// it is no longer kept (the webview keeps showing the image it has).
pub(crate) fn serve(request: tauri::http::Request<Vec<u8>>, responder: tauri::UriSchemeResponder) {
    let png = (request.method() == tauri::http::Method::GET)
        .then(|| {
            request
                .uri()
                .path()
                .trim_start_matches('/')
                .parse::<u64>()
                .ok()
        })
        .flatten()
        .and_then(|id| kept().lock().unwrap_or_else(|e| e.into_inner()).get(id));
    let (status, body) = match png {
        Some(png) => (200, png.to_vec()),
        None => (404, Vec::new()),
    };
    responder.respond(
        tauri::http::Response::builder()
            .status(status)
            .header("Content-Type", "image/png")
            // Ids start again at each launch.
            .header("Cache-Control", "no-store")
            .header("Access-Control-Allow-Origin", "*")
            .header("X-Content-Type-Options", "nosniff")
            .body(body)
            .expect("static scope image response headers"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_images_and_each_photo_s_newest_stay_loadable() {
        let mut kept = Kept::default();
        let first = kept.insert("a.arw", vec![1]);
        let older = kept.insert("b.arw", vec![2]);
        let newest_b = kept.insert("b.arw", vec![3]);
        for n in 0..RECENT as u8 {
            kept.insert("c.arw", vec![10 + n]);
        }
        // Out of the recent ones, but still a's and b's newest.
        assert_eq!(kept.get(first).as_deref(), Some(&[1][..]));
        assert_eq!(kept.get(newest_b).as_deref(), Some(&[3][..]));
        assert_eq!(kept.get(older), None);
        assert_eq!(kept.get(0), None);
        assert_eq!(kept.get(u64::MAX), None);
    }

    #[test]
    fn the_photos_kept_are_bounded() {
        let mut kept = Kept::default();
        let first = kept.insert("0", vec![0]);
        for n in 1..=PHOTOS {
            kept.insert(&n.to_string(), vec![1]);
        }
        assert_eq!(kept.get(first), None);
        assert_eq!(kept.newest_per_photo.len(), PHOTOS);
        assert_eq!(kept.recent.len(), RECENT);
    }

    /// The event names the image; the URL serves exactly the scope's PNG
    /// bytes (the pixels the webview showed from the base64 before).
    #[test]
    fn the_event_names_the_scope_png_it_serves() {
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 48, |x, y| {
            image::Rgb([(x * 4) as u8, (y * 5) as u8, ((x + y) * 2) as u8])
        }));
        let space = rawtonic_engine::output_color_space::OutputColorSpace::Srgb;
        for mode in ["rgb", "luma", "parade", "vectorscope"] {
            let waveform =
                rawtonic_engine::scopes::calculate_waveform_from_image(&image, space, Some(mode))
                    .unwrap();
            let want = match mode {
                "rgb" => waveform.rgb.clone(),
                "luma" => waveform.luma.clone(),
                "parade" => waveform.parade.clone(),
                _ => waveform.vectorscope.clone(),
            };
            assert!(want.starts_with(b"\x89PNG"), "{mode}");
            let payload = update("p.arw", None, Some(waveform));
            let scope = &payload["waveform"];
            for other in ["rgb", "luma", "parade", "vectorscope"] {
                let id = scope[other].as_str().unwrap();
                if other != mode {
                    assert_eq!(id, "", "{mode}: {other} was not computed");
                    continue;
                }
                let id: u64 = id.parse().unwrap();
                let served = kept().lock().unwrap().get(id).unwrap();
                assert_eq!(&served[..], &want[..], "{mode}");
            }
            assert_eq!(scope["width"], 256);
            assert!(
                scope["vectorscopeTargets"]
                    .as_array()
                    .is_some_and(|t| !t.is_empty())
            );
            // No image bytes in the event itself.
            assert!(payload.to_string().len() < 2_000, "{mode}");
        }
    }
}
