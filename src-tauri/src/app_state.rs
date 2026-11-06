use serde::{Deserialize, Serialize};

pub use crate::gpu_processing::{AnalyticsConfig, AnalyticsJob, GpuProcessorState};
pub use crate::state::ai::{AiTaskGuard, AiTaskToken};

#[derive(Serialize, Deserialize)]
pub struct WindowState {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub maximized: bool,
    /// Scale when these physical pixel dimensions were saved. Older files
    /// omitted it; Linux uses its configured size once to migrate those.
    #[serde(default)]
    pub scale_factor: Option<f64>,
    /// Always written false and not restored (full screen is the photo-only
    /// view); kept so older files still read.
    #[serde(default)]
    pub fullscreen: bool,
}

/// The mask layer the native display draws over the preview while it is
/// edited (the webview's Show mask state), sent with each preview request.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskOverlayRequest {
    pub mask_id: String,
    pub color: [u8; 3],
    /// Percent.
    pub opacity: u8,
    pub grayscale: bool,
}

/// The editor's state: the open photograph's source and its preview. The
/// editor's commands take it as `State<AppState>` (those pinned by
/// `src/contracts/commandErrors.test.ts` keep that signature); the other
/// managed states are in `state/` (see its `README.md`).
#[derive(Default)]
pub struct AppState {
    pub source: crate::state::source::SourceState,
    pub preview: crate::state::preview::PreviewState,
}
