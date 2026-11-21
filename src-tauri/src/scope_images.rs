//! The scope images (waveform, RGB waveform, parade, vectorscope PNGs) for
//! the webview by URL: `analytics-update` names an image by its id and the
//! `<img>` loads it from the `rawtonic-scope` URI scheme. Inside the event
//! (Tauri's emit is one evaluated script) the 100–280 KB of base64 cost
//! the webview's main thread ~0.5 ms per RGB update in Blink, beside the
//! drag's preview replies (notes: benchmark-results/scp5-2026-11-20).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use rawtonic_engine::scopes::{HistogramData, VectorscopeTarget, WaveformData};
use uuid::Uuid;

/// The URI scheme the webview loads scope images from.
pub(crate) const SCHEME: &str = "rawtonic-scope";

/// Updates whose images stay loadable: an `<img>` may still be loading one
/// while the next updates (about ten a second while dragging) arrive.
const RECENT: usize = 8;

/// Photos whose newest update's images stay loadable beyond `RECENT`: the
/// editor shows a photo's last scopes again when it returns to it
/// (`globalImageCache`, src/utils/ImageLRUCache.ts, the 20 photos visited
/// last). Kept by the photos' last analytics, which every visit renders
/// (also with the scopes hidden: the histogram), so in the same order, and
/// for more photos than that cache: up to four images (~0.5 MB) each.
const PHOTOS: usize = 32;

type Images = Arc<[(Uuid, Arc<[u8]>)]>;

#[derive(Default)]
struct Kept {
    recent: VecDeque<Images>,
    newest_per_photo: VecDeque<(String, Images)>,
}

impl Kept {
    /// Keeps one update's images; their ids, in order. The ids are random,
    /// so no other content in the webview can guess one.
    fn insert(&mut self, photo: &str, pngs: Vec<Vec<u8>>) -> Vec<Uuid> {
        let images: Images = pngs
            .into_iter()
            .map(|png| (Uuid::new_v4(), Arc::<[u8]>::from(png)))
            .collect();
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(images.clone());
        self.newest_per_photo.retain(|(path, _)| path != photo);
        if self.newest_per_photo.len() == PHOTOS {
            self.newest_per_photo.pop_front();
        }
        self.newest_per_photo
            .push_back((photo.to_owned(), images.clone()));
        images.iter().map(|(id, _)| *id).collect()
    }

    /// A visit to `photo` without scope images (the scopes hidden): its
    /// newest images stay as long as the photo stays recent.
    fn touch(&mut self, photo: &str) {
        if let Some(at) = self
            .newest_per_photo
            .iter()
            .position(|(path, _)| path == photo)
        {
            let entry = self.newest_per_photo.remove(at).expect("position within");
            self.newest_per_photo.push_back(entry);
        }
    }

    fn get(&self, id: Uuid) -> Option<Arc<[u8]>> {
        self.recent
            .iter()
            .rev()
            .chain(self.newest_per_photo.iter().map(|(_, images)| images))
            .flat_map(|images| images.iter())
            .find(|(kept, _)| *kept == id)
            .map(|(_, png)| png.clone())
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
    if waveform.is_none() {
        kept()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .touch(photo);
    }
    let waveform = waveform.map(|w| {
        let pngs = [w.rgb, w.luma, w.parade, w.vectorscope];
        let computed = pngs.each_ref().map(|png| !png.is_empty());
        let mut ids = kept()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                photo,
                pngs.into_iter().filter(|png| !png.is_empty()).collect(),
            )
            .into_iter();
        let [rgb, luma, parade, vectorscope] = computed.map(|computed| {
            computed
                .then(|| ids.next().map(|id| id.to_string()))
                .flatten()
                .unwrap_or_default()
        });
        WaveformUpdate {
            rgb,
            luma,
            parade,
            vectorscope,
            width: w.width,
            height: w.height,
            vectorscope_targets: w.vectorscope_targets,
            skin_line_degrees: w.skin_line_degrees,
        }
    });
    serde_json::json!({ "path": photo, "histogram": histogram, "waveform": waveform })
}

/// Answers `rawtonic-scope://localhost/<id>` with the kept PNG, 404 when
/// it is no longer kept (the webview keeps showing the image it has). No
/// CORS header: the editor's `<img>` needs none, and content of another
/// origin (an embedded map) must not read the photo's scopes.
pub(crate) fn serve(request: tauri::http::Request<Vec<u8>>, responder: tauri::UriSchemeResponder) {
    let png = (request.method() == tauri::http::Method::GET)
        .then(|| Uuid::parse_str(request.uri().path().trim_start_matches('/')).ok())
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
            // Each id names one image for good.
            .header("Cache-Control", "no-store")
            .header("X-Content-Type-Options", "nosniff")
            .body(body)
            .expect("static scope image response headers"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_updates_and_each_photo_s_newest_stay_loadable() {
        let mut kept = Kept::default();
        // A settled update: every mode's image.
        let a = kept.insert("a.arw", vec![vec![1], vec![2], vec![3], vec![4]]);
        let older = kept.insert("b.arw", vec![vec![5]]);
        let newest_b = kept.insert("b.arw", vec![vec![6]]);
        for n in 0..RECENT as u8 {
            kept.insert("c.arw", vec![vec![10 + n]]);
        }
        // Out of the recent ones, but still a's and b's newest update: all of it.
        for (id, want) in a.iter().zip(1u8..) {
            assert_eq!(kept.get(*id).as_deref(), Some(&[want][..]));
        }
        assert_eq!(kept.get(newest_b[0]).as_deref(), Some(&[6][..]));
        assert_eq!(kept.get(older[0]), None);
        assert_eq!(kept.get(Uuid::nil()), None);
        assert_ne!(a[0], a[1]);
    }

    #[test]
    fn the_photos_kept_are_bounded() {
        let mut kept = Kept::default();
        let first = kept.insert("0", vec![vec![0]]);
        for n in 1..=PHOTOS {
            kept.insert(&n.to_string(), vec![vec![1]]);
        }
        assert_eq!(kept.get(first[0]), None);
        assert_eq!(kept.newest_per_photo.len(), PHOTOS);
        assert_eq!(kept.recent.len(), RECENT);
    }

    /// The review's culling pass: a photo whose scopes were hidden on its
    /// last visit keeps its images as long as the frontend's cache (which
    /// that visit refreshed) can show them again.
    #[test]
    fn a_visit_with_the_scopes_hidden_keeps_the_photo_s_images() {
        let mut kept = Kept::default();
        let a = kept.insert("a", vec![vec![1], vec![2], vec![3], vec![4]]);
        for n in 1..PHOTOS {
            kept.insert(&n.to_string(), vec![vec![5]]);
        }
        // Back to a with the scopes hidden: a histogram-only update.
        kept.touch("a");
        kept.insert("new", vec![vec![6]]);
        for id in &a {
            assert!(kept.get(*id).is_some());
        }
        // Without that visit, a would have been the oldest and gone.
        assert_eq!(
            kept.get(kept.newest_per_photo[0].1[0].0).as_deref(),
            Some(&[5][..])
        );
        kept.touch("not kept");
        assert_eq!(kept.newest_per_photo.len(), PHOTOS);
    }

    fn frame() -> image::DynamicImage {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 48, |x, y| {
            image::Rgb([(x * 4) as u8, (y * 5) as u8, ((x + y) * 2) as u8])
        }))
    }

    fn served(id: &serde_json::Value) -> Vec<u8> {
        let id = Uuid::parse_str(id.as_str().unwrap()).unwrap();
        kept().lock().unwrap().get(id).unwrap().to_vec()
    }

    /// The event names the image; the URL serves exactly the scope's PNG
    /// bytes (the pixels the webview showed from the base64 before).
    #[test]
    fn the_event_names_the_scope_png_it_serves() {
        let space = rawtonic_engine::output_color_space::OutputColorSpace::Srgb;
        let modes = ["rgb", "luma", "parade", "vectorscope"];
        let png = |w: &WaveformData, mode: &str| match mode {
            "rgb" => w.rgb.clone(),
            "luma" => w.luma.clone(),
            "parade" => w.parade.clone(),
            _ => w.vectorscope.clone(),
        };
        // A drag's update (one mode) and a settled one (every mode).
        for active in modes.map(Some).into_iter().chain([None]) {
            let waveform =
                rawtonic_engine::scopes::calculate_waveform_from_image(&frame(), space, active)
                    .unwrap();
            let want = modes.map(|mode| png(&waveform, mode));
            let payload = update("p.arw", None, Some(waveform));
            let scope = &payload["waveform"];
            for (mode, want) in modes.iter().zip(&want) {
                if active.is_some_and(|active| active != *mode) {
                    assert_eq!(scope[mode], "", "{active:?}: {mode} was not computed");
                    continue;
                }
                assert!(want.starts_with(b"\x89PNG"), "{mode}");
                assert_eq!(&served(&scope[mode]), want, "{active:?}: {mode}");
            }
            assert_eq!(scope["width"], 256);
            assert!(
                scope["vectorscopeTargets"]
                    .as_array()
                    .is_some_and(|t| !t.is_empty())
            );
            // No image bytes in the event itself.
            assert!(payload.to_string().len() < 2_000, "{active:?}");
        }
    }
}
