//! Linux: the second window's photograph drawn by its webview.
//!
//! WebKitGTK cannot show a native surface under a transparent webview, so
//! on Linux the main window presents its previews as JPEG frames that
//! WebKit draws (editor_preview.rs, browser_preview_transport.rs). The
//! second window does the same: the main window's whole frames (the same
//! JPEG bytes, not encoded again) and the frames this window develops for
//! itself (100 %, a larger fit, lock, the Library's photograph; encoded at
//! the settled main preview's quality) are kept here and served through the
//! `rawtonic-preview` scheme by opaque IDs; the page lays them out with the
//! multi view's geometry (secondViewZoom.ts `fitRect`, `zoomedRect`).
//!
//! [`BrowserView`] has the methods of `WgpuDisplay` that second_window.rs
//! calls, so the window's logic (when to copy, when to develop, which
//! region is current) is one for every platform.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::Emitter;
use uuid::Uuid;

use crate::gpu_processing::{CellExtent, CellLayer, CellZoom, MultiViewCell};
use crate::state::lock::lock_recover;

/// Sent to the second window's page whenever a frame it shows changes.
pub(super) const FRAMES_EVENT: &str = "second-window-frames";
/// The settled main preview's JPEG quality on Linux (editor_preview.rs):
/// the window's own frames look like the main window's.
const QUALITY: u8 = 94;
/// Frames kept for the page: the current whole and region and the ones
/// before them, which a load started just before a replacement still reads.
const KEPT: usize = 6;

/// One frame the page draws: served at `rawtonic-preview://…/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameLayer {
    pub id: String,
    pub width: u32,
    pub height: u32,
    /// The photograph's shown size (cropped, turned), full-resolution px.
    pub shown: [f32; 2],
    /// For a region: its part of the photograph (x, y, w, h in those px).
    pub region: Option<[f32; 4]>,
}

/// What the page draws: the whole photograph and, zoomed in, the sharp
/// region over it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Frames {
    pub path: Option<String>,
    pub whole: Option<FrameLayer>,
    pub region: Option<FrameLayer>,
}

static STORE: Mutex<VecDeque<(Uuid, Arc<Vec<u8>>)>> = Mutex::new(VecDeque::new());
static CURRENT: Mutex<Option<Frames>> = Mutex::new(None);

/// The JPEG of frame `id`, for the `rawtonic-preview` scheme.
pub(crate) fn frame(id: Uuid) -> Option<Vec<u8>> {
    lock_recover(&STORE)
        .iter()
        .find(|(key, _)| *key == id)
        .map(|(_, jpeg)| jpeg.as_ref().clone())
}

/// What the page draws now (it asks once when it loads).
pub(crate) fn current() -> Option<Frames> {
    lock_recover(&CURRENT).clone()
}

fn keep(jpeg: Vec<u8>) -> Uuid {
    let id = Uuid::new_v4();
    let mut store = lock_recover(&STORE);
    store.push_back((id, Arc::new(jpeg)));
    while store.len() > KEPT {
        store.pop_front();
    }
    id
}

/// A developed frame encoded for the page (done before the window's state
/// is locked: the main window's frames wait on that lock).
pub(super) fn encode(rgba: &image::RgbaImage) -> Result<Vec<u8>, String> {
    crate::preview_jpeg::encode_rgbx_444(rgba, QUALITY)
}

pub(crate) struct BrowserView {
    /// Tells the page (the window's webview).
    tell: Box<dyn Fn(&Frames) + Send>,
    frames: Frames,
    region_key: u64,
    changed: bool,
}

impl BrowserView {
    pub(super) fn new(app: &tauri::AppHandle) -> Self {
        let app = app.clone();
        Self::telling(move |frames| {
            let _ = app.emit_to(super::LABEL, FRAMES_EVENT, frames);
        })
    }

    fn telling(tell: impl Fn(&Frames) + Send + 'static) -> Self {
        Self {
            tell: Box::new(tell),
            frames: Frames::default(),
            region_key: 0,
            changed: true,
        }
    }

    /// The photograph shown (the window's single cell); a new one starts
    /// without frames.
    pub(super) fn set_multi_view(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _primary: Option<String>,
        cells: Vec<MultiViewCell>,
    ) {
        self.show(cells.into_iter().next().map(|cell| cell.path));
    }

    fn show(&mut self, path: Option<String>) {
        if path != self.frames.path {
            self.frames = Frames {
                path,
                ..Frames::default()
            };
            self.region_key = 0;
            self.changed = true;
        }
    }

    /// The page positions the frames at its own zoom.
    pub(super) fn set_multi_view_zoom(&mut self, _zoom: Option<CellZoom>) {}

    /// Shows the JPEG `jpeg` (`size` px) of `path` as `layer`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn set_jpeg(
        &mut self,
        path: &str,
        key: u64,
        extent: CellExtent,
        layer: CellLayer,
        size: (u32, u32),
        jpeg: Vec<u8>,
    ) -> bool {
        if self.frames.path.as_deref() != Some(path) {
            return false;
        }
        let frame = FrameLayer {
            id: keep(jpeg).to_string(),
            width: size.0,
            height: size.1,
            shown: extent.shown,
            region: extent.region,
        };
        match layer {
            CellLayer::Whole => self.frames.whole = Some(frame),
            CellLayer::Region => {
                self.frames.region = Some(frame);
                self.region_key = key;
            }
        }
        self.changed = true;
        true
    }

    pub(super) fn remove_cell_region(&mut self, path: &str) {
        if self.frames.path.as_deref() == Some(path) && self.frames.region.is_some() {
            self.frames.region = None;
            self.region_key = 0;
            self.changed = true;
        }
    }

    /// The recipe tag and extent of the region shown for `path`.
    pub(super) fn region_of(&self, path: &str) -> Option<(u64, CellExtent)> {
        let region = self
            .frames
            .region
            .as_ref()
            .filter(|_| self.frames.path.as_deref() == Some(path))?;
        Some((
            self.region_key,
            CellExtent {
                shown: region.shown,
                region: region.region,
            },
        ))
    }

    /// Tells the page what changed since the last call.
    pub(super) fn render(&mut self) {
        if !std::mem::take(&mut self.changed) {
            return;
        }
        *lock_recover(&CURRENT) = Some(self.frames.clone());
        (self.tell)(&self.frames);
    }
}

impl Drop for BrowserView {
    fn drop(&mut self) {
        lock_recover(&STORE).clear();
        *lock_recover(&CURRENT) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extent(region: Option<[f32; 4]>) -> CellExtent {
        CellExtent {
            shown: [6000.0, 4000.0],
            region,
        }
    }

    // One test: the frame store is shared by the process.
    #[test]
    fn the_page_gets_each_change_once_and_only_the_newest_frames_are_kept() {
        let told = Arc::new(Mutex::new(Vec::<Frames>::new()));
        let log = Arc::clone(&told);
        let mut view = BrowserView::telling(move |frames| log.lock().unwrap().push(frames.clone()));
        view.show(Some("a".into()));
        // A frame of another photograph is not taken.
        assert!(!view.set_jpeg("b", 1, extent(None), CellLayer::Whole, (300, 200), vec![1]));
        assert!(view.set_jpeg(
            "a",
            1,
            extent(None),
            CellLayer::Whole,
            (300, 200),
            vec![1, 2]
        ));
        let region = Some([10.0, 20.0, 800.0, 600.0]);
        assert!(view.set_jpeg(
            "a",
            7,
            extent(region),
            CellLayer::Region,
            (800, 600),
            vec![3]
        ));
        view.render();
        view.render();
        assert_eq!(view.region_of("a"), Some((7, extent(region))));
        assert_eq!(view.region_of("b"), None);
        let frames = told.lock().unwrap().clone();
        assert_eq!(frames.len(), 1, "told once per change");
        assert_eq!(current().as_ref(), Some(&frames[0]));
        let whole = frames[0].whole.clone().unwrap();
        assert_eq!((whole.width, whole.height, whole.region), (300, 200, None));
        let id = Uuid::parse_str(&whole.id).unwrap();
        assert_eq!(frame(id), Some(vec![1, 2]));
        let json = serde_json::to_value(&frames[0]).unwrap();
        assert_eq!(
            json["region"]["region"],
            serde_json::json!([10.0, 20.0, 800.0, 600.0])
        );
        assert_eq!(json["whole"]["shown"], serde_json::json!([6000.0, 4000.0]));
        assert_eq!(json["path"], "a");

        view.remove_cell_region("a");
        assert_eq!(view.region_of("a"), None);
        view.show(Some("c".into()));
        view.render();
        let last = told.lock().unwrap().last().cloned().unwrap();
        assert_eq!(
            last,
            Frames {
                path: Some("c".into()),
                ..Frames::default()
            }
        );

        let ids: Vec<Uuid> = (0..KEPT).map(|i| keep(vec![i as u8])).collect();
        assert_eq!(frame(id), None, "older frames are let go");
        assert_eq!(frame(ids[KEPT - 1]), Some(vec![(KEPT - 1) as u8]));
        drop(view);
        assert_eq!(frame(ids[KEPT - 1]), None, "a closed window keeps none");
        assert_eq!(current(), None);
    }
}
