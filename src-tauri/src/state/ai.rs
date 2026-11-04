//! The AI tools' managed state (plan step 3.3). See `README.md` here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::lock::lock_recover;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

pub struct AiTaskToken {
    cancelled: AtomicBool,
    notify: Notify,
}

impl AiTaskToken {
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub async fn wait_for_cancel(&self) {
        let notified = self.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

impl Default for AiTaskToken {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AiTaskGuard<'a> {
    active_ai_tasks: &'a Mutex<HashMap<String, Arc<AiTaskToken>>>,
    task_id: String,
    pub token: Arc<AiTaskToken>,
}

impl<'a> AiTaskGuard<'a> {
    pub fn new(
        active_ai_tasks: &'a Mutex<HashMap<String, Arc<AiTaskToken>>>,
        task_id: String,
    ) -> Self {
        let token = Arc::new(AiTaskToken::new());
        lock_recover(active_ai_tasks).insert(task_id.clone(), Arc::clone(&token));
        Self {
            active_ai_tasks,
            task_id,
            token,
        }
    }
}

impl<'a> Drop for AiTaskGuard<'a> {
    fn drop(&mut self) {
        let mut tasks = lock_recover(self.active_ai_tasks);
        if let Some(existing) = tasks.get(&self.task_id)
            && Arc::ptr_eq(existing, &self.token)
        {
            tasks.remove(&self.task_id);
        }
    }
}

/// The AI tools' managed state: the loaded models and their caches, the
/// locks that serialise loading and running them, their use and unloads
/// (`ai_lifecycle`), and the running tasks. Locks: `README.md` here.
pub struct AiState {
    /// The masking, depth, relight, tagging and Remove models and their
    /// cached results (`ai_processing::AiModels`).
    pub models: Mutex<Option<crate::ai_processing::AiModels>>,
    /// Serialises loading the models that share it (Subject, SAM, depth,
    /// relight, tagging, MI-GAN, AI Denoise, faces).
    pub init_lock: TokioMutex<()>,
    /// Serialises loading the sky model, apart from `init_lock`: a Sky click
    /// does not wait for BiRefNet's Core ML load (1-3 s) or SAM's.
    pub sky_init_lock: TokioMutex<()>,
    /// Serialises loading Moebius, apart from `init_lock`: its first use
    /// downloads about 620 MB, which must not hold up the masking models.
    pub moebius_init_lock: TokioMutex<()>,
    /// Serialises SAM encodes: a click that arrives while the photo is being
    /// encoded in the background waits for that encode instead of starting a
    /// second one.
    pub sam_encode_lock: TokioMutex<()>,
    /// Serialises Subject runs: a click on Subject or Background that arrives
    /// while the photo is computed in the background waits for that run
    /// instead of starting a second one.
    pub subject_lock: TokioMutex<()>,
    /// The same for Sky.
    pub sky_lock: TokioMutex<()>,
    /// A warm-up loaded ONNX Runtime instead of Core ML in this launch.
    pub warm_up_fell_back: AtomicBool,
    /// Use, running computations and unloads of the masking models.
    pub lifecycle: Arc<crate::ai_lifecycle::Lifecycle>,
    /// AI Denoise (`ai_denoise`) by `aiDenoiseVersion` (index 0: version 1),
    /// each loaded on first use.
    pub denoisers: Mutex<[Option<Arc<dyn rawtonic_rawdecode::mosaic_denoise::MosaicDenoiser>>; 2]>,
    /// Face models and the open photo's faces (`ai_faces`), loaded on first use.
    pub faces: crate::ai_faces::SharedFaceState,
    /// Running AI tasks by the webview's task id, for `cancel_ai_task`.
    pub tasks: Mutex<HashMap<String, Arc<AiTaskToken>>>,
    /// The running library tagging pass (`tagging`).
    pub indexing_task: Mutex<Option<JoinHandle<()>>>,
}

impl Default for AiState {
    fn default() -> Self {
        Self {
            models: Mutex::new(None),
            init_lock: TokioMutex::new(()),
            sky_init_lock: TokioMutex::new(()),
            moebius_init_lock: TokioMutex::new(()),
            sam_encode_lock: TokioMutex::new(()),
            subject_lock: TokioMutex::new(()),
            sky_lock: TokioMutex::new(()),
            warm_up_fell_back: AtomicBool::new(false),
            lifecycle: Arc::new(crate::ai_lifecycle::Lifecycle::new()),
            denoisers: Mutex::new([None, None]),
            faces: Mutex::new(Default::default()),
            tasks: Mutex::new(HashMap::new()),
            indexing_task: Mutex::new(None),
        }
    }
}
