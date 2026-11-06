//! The open photograph's source (plan step 3.3): its decode, its preview
//! proxy while the decode runs, whether it is offline, which photograph the
//! editor asked for last, and the signals that tell waiting work when the
//! source changed or was rendered. Written by `image_loader` (and the proxy
//! worker), read by the preview, the AI tools and exports of the open
//! photograph. Locks: `README.md` here.

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Condvar, Mutex};

use image::DynamicImage;
use tokio::sync::Notify;

/// The open photograph's source slots. Part of `AppState` (`AppState.source`).
pub struct SourceState {
    pub original_image: Mutex<Option<LoadedImage>>,
    pub proxy_image: Mutex<Option<LoadedProxy>>,
    pub proxy_memory: Mutex<crate::preview_proxy::ProxyMemory>,
    /// The open photograph when its RAW is offline and it opened on a preview
    /// proxy: its 1:1 proxy is `original_image` (preview data, never reused
    /// by exports), or its Standard proxy stays `proxy_image` and no full
    /// decode will come.
    pub offline_source: Mutex<Option<String>>,
    /// Signalled when `original_image`, `proxy_image` or `offline_source`
    /// changes.
    pub source_signal: SourceSignal,
    pub rendered_source: RenderedSource,
    /// The photograph the editor asked for last (load_image / load_image_proxy).
    pub current_source: Mutex<String>,
    /// Bumped by every load of a photograph; a load, proxy or preview job
    /// of an older generation stops or drops its result.
    pub load_image_generation: Arc<AtomicUsize>,
}

/// No photograph open.
impl Default for SourceState {
    fn default() -> Self {
        Self {
            original_image: Mutex::new(None),
            proxy_image: Mutex::new(None),
            proxy_memory: Mutex::new(Default::default()),
            offline_source: Mutex::new(None),
            source_signal: Default::default(),
            rendered_source: Default::default(),
            current_source: Mutex::new(String::new()),
            load_image_generation: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[derive(Clone)]
pub struct LoadedImage {
    pub path: String,
    pub image: Arc<DynamicImage>,
    pub is_raw: bool,
    /// The decode variant it was developed for (colour noise filter and
    /// dead-photosite test, `image_loader::load_base_image_from_bytes_for`);
    /// None for preview data.
    pub decode: Option<crate::image_loader::DecodeVariant>,
    /// The measures of the decode (`image_loader::DecodeMeasures`); None for
    /// preview data, drafts and other formats.
    pub measures: Option<crate::image_loader::DecodeMeasures>,
}

impl LoadedImage {
    /// Which development of the photo `image` is: the same while it is
    /// these pixels (also back from the decoded-image cache), another after
    /// the photo is developed again (AI Denoise, decode options, a 1:1
    /// preview). Caches of results computed from the pixels (AI masks,
    /// depth, faces) include it in their keys.
    pub fn source_generation(&self) -> u64 {
        source_generation(&self.image)
    }
}

/// Generations of the developments alive. An entry's `Weak` keeps the
/// allocation, so its address is not reused while it is listed; entries
/// whose image is gone are dropped, and generations never repeat.
static SOURCE_GENERATIONS: Mutex<(u64, Vec<LiveSource>)> = Mutex::new((0, Vec::new()));

/// A development alive and its generation.
type LiveSource = (std::sync::Weak<DynamicImage>, u64);

fn source_generation(image: &Arc<DynamicImage>) -> u64 {
    let mut generations = SOURCE_GENERATIONS.lock().unwrap_or_else(|e| e.into_inner());
    let (last, live) = &mut *generations;
    live.retain(|(source, _)| source.strong_count() > 0);
    if let Some((_, generation)) = live
        .iter()
        .find(|(source, _)| std::ptr::eq(source.as_ptr(), Arc::as_ptr(image)))
    {
        return *generation;
    }
    *last += 1;
    live.push((Arc::downgrade(image), *last));
    *last
}

/// The open photograph's preview proxy (preview_proxy.rs) while its full
/// decode runs: `image` at `scale` proxy pixels per full-resolution pixel.
#[derive(Clone)]
pub struct LoadedProxy {
    pub path: String,
    pub image: Arc<DynamicImage>,
    pub full_width: u32,
    pub full_height: u32,
    pub is_raw: bool,
    /// The decode variant a stored preview proxy was made for (from its
    /// key); None for a draft of the decode under way.
    pub made_for: Option<crate::image_loader::DecodeVariant>,
    /// The measures of the decode a stored proxy was made from (its facts);
    /// None for drafts and proxies made before they were kept.
    pub measures: Option<crate::image_loader::DecodeMeasures>,
}

/// The source the preview worker last rendered a frame from (a decode, its
/// draft or a stored proxy), so that work can wait until the photograph is
/// on screen: the work a decode leaves for later (its preview proxy and
/// thumbnail) and the full development after its draft. Started beside that
/// frame, it took the cores the frame needed.
#[derive(Default)]
pub struct RenderedSource {
    source: Mutex<std::sync::Weak<DynamicImage>>,
    rendered: Condvar,
}

impl RenderedSource {
    /// Call when a frame from `source` is rendered.
    pub fn mark(&self, source: &Arc<DynamicImage>) {
        *self.source.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(source);
        self.rendered.notify_all();
    }

    /// Waits until a frame from `source` is rendered or `timeout` passed;
    /// whether it was rendered. Holds no strong reference to the source.
    pub fn wait(
        &self,
        source: &std::sync::Weak<DynamicImage>,
        timeout: std::time::Duration,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut last = self.source.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if last.ptr_eq(source) {
                return true;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            last = self
                .rendered
                .wait_timeout(last, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// Wakes the commands waiting for the open photograph's full decode
/// (`image_loader::wait_for_original_image`) when its source slots change:
/// a blocking worker waits on the condition variable, an async command on
/// the notification, so neither polls.
#[derive(Default)]
pub struct SourceSignal {
    changes: Mutex<u64>,
    changed: Condvar,
    notify: Notify,
}

impl SourceSignal {
    /// Call after a write that can settle a waiting command (a decode in
    /// `original_image`, `offline_source` set, `proxy_image` cleared), with
    /// their locks released.
    pub fn changed(&self) {
        *self.changes.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.changed.notify_all();
        self.notify.notify_waiters();
    }

    /// `check`'s first `Some`, checked now and after each change until
    /// `timeout`; then `None`. Blocks the calling thread.
    pub fn wait_for<T>(
        &self,
        timeout: std::time::Duration,
        mut check: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        let deadline = std::time::Instant::now() + timeout;
        // Held while checking: a change cannot slip between the check and
        // the wait.
        let mut changes = self.changes.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(value) = check() {
                return Some(value);
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            changes = self
                .changed
                .wait_timeout(changes, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// `wait_for` without blocking the async runtime's thread.
    pub async fn wait_for_async<T>(
        &self,
        timeout: std::time::Duration,
        mut check: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // Registered before the check, so a change after it wakes us.
            notified.as_mut().enable();
            if let Some(value) = check() {
                return Some(value);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return check();
            }
        }
    }
}

#[cfg(test)]
mod rendered_source_tests {
    use super::RenderedSource;
    use image::DynamicImage;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn waits_for_a_frame_of_that_decode() {
        let rendered = Arc::new(RenderedSource::default());
        let decode = Arc::new(DynamicImage::new_rgb32f(4, 4));
        let other = Arc::new(DynamicImage::new_rgb32f(4, 4));
        // No frame of it: the wait ends at its timeout.
        rendered.mark(&other);
        let start = Instant::now();
        assert!(!rendered.wait(&Arc::downgrade(&decode), Duration::from_millis(50)));
        assert!(start.elapsed() >= Duration::from_millis(50));
        // A frame rendered meanwhile ends the wait at once.
        let marker = {
            let (rendered, decode) = (Arc::clone(&rendered), Arc::clone(&decode));
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                rendered.mark(&decode);
            })
        };
        let start = Instant::now();
        assert!(rendered.wait(&Arc::downgrade(&decode), Duration::from_secs(10)));
        assert!(start.elapsed() < Duration::from_secs(5));
        marker.join().unwrap();
        // And one rendered before the wait.
        assert!(rendered.wait(&Arc::downgrade(&decode), Duration::ZERO));
    }
}

#[cfg(test)]
mod source_signal_tests {
    use super::SourceSignal;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn a_blocking_waiter_wakes_on_the_change_and_gives_up_at_the_deadline() {
        let signal = Arc::new(SourceSignal::default());
        let ready = Arc::new(AtomicBool::new(false));
        let writer = {
            let (signal, ready) = (signal.clone(), ready.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                ready.store(true, Ordering::SeqCst);
                signal.changed();
            })
        };
        let started = Instant::now();
        let woke = signal.wait_for(Duration::from_secs(20), || {
            ready.load(Ordering::SeqCst).then_some(())
        });
        writer.join().unwrap();
        assert!(woke.is_some());
        assert!(started.elapsed() < Duration::from_secs(10));

        let started = Instant::now();
        assert!(
            signal
                .wait_for(Duration::from_millis(40), || None::<()>)
                .is_none()
        );
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    /// The async wait yields its thread: on a single-threaded runtime the
    /// task that publishes the source runs while the command waits (a sleep
    /// loop there would hold the thread until its deadline).
    #[test]
    fn an_async_waiter_lets_the_runtime_publish_the_source() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let signal = Arc::new(SourceSignal::default());
        let ready = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let woke = runtime.block_on(async {
            let publisher = {
                let (signal, ready) = (signal.clone(), ready.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    ready.store(true, Ordering::SeqCst);
                    signal.changed();
                })
            };
            let woke = signal
                .wait_for_async(Duration::from_secs(20), || {
                    ready.load(Ordering::SeqCst).then_some(())
                })
                .await;
            publisher.await.unwrap();
            woke
        });
        assert!(woke.is_some());
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(
            runtime
                .block_on(signal.wait_for_async(Duration::from_millis(40), || None::<()>))
                .is_none()
        );
    }
}
