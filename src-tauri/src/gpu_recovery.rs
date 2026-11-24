//! Recovery from a lost GPU device (DVL1, G74/G62).
//!
//! On a Radeon 680M (Windows, Vulkan) a GPU submission of more than about
//! 2 s makes the driver take the device away (Windows' watchdog); every
//! multi-view cell then froze on its last frame and no photo rendered until
//! the app was restarted. Edits were never at risk (they live in the
//! recipe), only the pictures.
//!
//! `install` hears every loss (`device_start::on_device_lost`) and starts
//! `recover` on its own thread: it waits for the renders in flight to fail,
//! drops everything made on the lost device (processors, uploads, the
//! window display with its multi-view cells, the context), starts a new
//! device the way the app's start does (`get_or_init_gpu_context`, then
//! `attach_display`), prepares the processor and tells the window
//! (`gpu-device-recovered`), which renders the open photo and the cells
//! again and says so once. The multi view's layout and zooms go to the new
//! display first (`multi_view::show_again`), so zoomed cells stay zoomed.
//! The window keeps showing its last frames meanwhile: nothing presents
//! until a new frame comes.
//!
//! Repeated losses back off and stop: a GPU reset is not harmless (two
//! seconds apart they once hung Windows on that 680M), so a second loss
//! within `QUICK` waits `BACK_OFF` before the new device, and the
//! `GIVE_UP_AFTER`th within `WINDOW` gives up with `gpu-device-lost`
//! (the window asks for a restart, as before).
//!
//! QA: `gpu.simulate_lost` (qa-control) takes the device as lost without a
//! driver reset (`device_start::simulate_lost`), so the recovery is tested
//! without one.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{Emitter, Manager};

use crate::gpu_processing::GpuContext;
use crate::state::develop::DevelopSession;
use crate::state::gpu::GpuState;
use crate::state::lock::lock_recover;

/// A second loss this soon after the last waits `BACK_OFF` first.
const QUICK: Duration = Duration::from_secs(60);
const BACK_OFF: Duration = Duration::from_secs(10);
/// This many losses within `WINDOW` stop the recovery.
const GIVE_UP_AFTER: usize = 3;
const WINDOW: Duration = Duration::from_secs(5 * 60);
/// Time for the renders in flight on the lost device to fail and let go of
/// its objects before they are dropped.
const SETTLE: Duration = Duration::from_millis(500);

/// What the window hears once a new device works.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recovered {
    /// The lost device's number among this run's devices.
    pub lost: u64,
    /// The driver's reason.
    pub reason: String,
    /// From the loss to the new device, in ms.
    pub took_ms: u64,
}

/// What `recover` does after the loss at `now`, given the earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plan {
    Now,
    After(Duration),
    GiveUp,
}

/// The recent losses (`plan`).
static LOSSES: Mutex<Vec<Instant>> = Mutex::new(Vec::new());
/// One recovery at a time; a loss heard meanwhile is the same one or
/// waits for it.
static RECOVERING: Mutex<()> = Mutex::new(());

pub(crate) fn plan(losses: &mut Vec<Instant>, now: Instant) -> Plan {
    losses.retain(|at| now.duration_since(*at) < WINDOW);
    let previous = losses.last().copied();
    losses.push(now);
    if losses.len() >= GIVE_UP_AFTER {
        Plan::GiveUp
    } else if previous.is_some_and(|at| now.duration_since(at) < QUICK) {
        Plan::After(BACK_OFF)
    } else {
        Plan::Now
    }
}

/// Starts `recover` for every device the driver takes away.
pub fn install(app_handle: &tauri::AppHandle) {
    let app_handle = app_handle.clone();
    rawtonic_gpu::device_start::on_device_lost(move |lost| {
        // wgpu may call this under its own locks: only hand it on.
        let app_handle = app_handle.clone();
        let spawned = std::thread::Builder::new()
            .name("gpu-recover".into())
            .stack_size(rawtonic_runtime::RENDER_STACK_SIZE)
            .spawn(move || recover(&app_handle, lost));
        if let Err(error) = spawned {
            log::error!("GPU recovery could not start: {error}");
        }
    });
}

fn recover(app_handle: &tauri::AppHandle, lost: rawtonic_gpu::device_start::LostDevice) {
    let lost_at = Instant::now();
    let _one = lock_recover(&RECOVERING);
    let gpu = app_handle.state::<GpuState>();
    // A loss heard after its recovery already replaced the device.
    if !rawtonic_gpu::device_start::device_lost() {
        log::info!(
            "GPU recovery: device {} was already replaced",
            lost.generation
        );
        return;
    }
    match plan(&mut lock_recover(&LOSSES), lost_at) {
        Plan::GiveUp => {
            log::error!(
                "GPU recovery: {GIVE_UP_AFTER} device losses within {} min, not starting another device",
                WINDOW.as_secs() / 60
            );
            let _ = app_handle.emit("gpu-device-lost", &lost.message);
            return;
        }
        Plan::After(wait) => {
            log::warn!(
                "GPU recovery: a second loss within {} s, waiting {} s",
                QUICK.as_secs(),
                wait.as_secs()
            );
            std::thread::sleep(wait);
        }
        Plan::Now => {}
    }
    log::warn!(
        "GPU recovery: device {} lost ({}), starting a new one",
        lost.generation,
        lost.message
    );
    std::thread::sleep(SETTLE);
    // The multi view's layout and zooms, shown again on the new display
    // (none on Linux, which has no native multi view).
    let shown = gpu.context().and_then(|context| {
        lock_recover(&context.display)
            .as_ref()
            .and_then(crate::multi_view::shown_on)
    });
    let develop = app_handle.state::<DevelopSession>();
    drop_device_state(&gpu, Some(&develop));
    let started = crate::gpu_processing::get_or_init_gpu_context(&gpu, app_handle);
    let context = match started {
        Ok(context) => context,
        Err(error) => {
            log::error!("GPU recovery: no new device: {error}");
            let _ = app_handle.emit("gpu-device-lost", &error);
            return;
        }
    };
    // The window surfaces are made on the main thread (AppKit's view and
    // layer; wgpu panics making one elsewhere), as at start.
    #[cfg(not(target_os = "linux"))]
    {
        let (done, attached) = std::sync::mpsc::channel();
        let app = app_handle.clone();
        let sent = app_handle.run_on_main_thread(move || {
            crate::gpu_processing::attach_display(&app.state::<GpuState>(), &app);
            let _ = done.send(());
        });
        if sent.is_err() || attached.recv_timeout(Duration::from_secs(30)).is_err() {
            log::error!("GPU recovery: the window display was not attached to the new device");
        }
    }
    // `attach_display` replaces the context when the adapter cannot present.
    let context = gpu.context().unwrap_or(context);
    if let Some(shown) = shown {
        crate::multi_view::show_again(app_handle, shown);
    }
    tauri::async_runtime::block_on(crate::second_window::after_device_recovered(
        app_handle, &context,
    ));
    crate::gpu_processing::prewarm_gpu_processor(&gpu.processor, &context);
    let recovered = Recovered {
        lost: lost.generation,
        reason: lost.message,
        took_ms: lost_at.elapsed().as_millis() as u64,
    };
    log::warn!(
        "GPU recovery: a new device works {} ms after the loss of device {}",
        recovered.took_ms,
        recovered.lost
    );
    let _ = app_handle.emit("gpu-device-recovered", &recovered);
}

/// Drops everything made on the current device, each lock held for its own
/// step: the processors (the display's, then the exports'), the uploads
/// (scopes, the open photo's), then the window display (its surface and
/// the multi-view cells) and the context itself. The next use starts a new
/// device (`get_or_init_gpu_context`). Returns the dropped context.
pub(crate) fn drop_device_state(
    gpu: &GpuState,
    develop: Option<&DevelopSession>,
) -> Option<GpuContext> {
    for slot in [&gpu.processor, &gpu.export_processor] {
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    *lock_recover(&gpu.scope_cache) = None;
    for cache in &gpu.scope_extremes_cache {
        *lock_recover(cache) = None;
    }
    if let Some(develop) = develop {
        *lock_recover(&develop.gpu_image_cache) = None;
    }
    let mut context = gpu.context_lock();
    let old = context.take();
    if let Some(old) = &old {
        // Nothing presents from the lost device: the window keeps its last
        // frame until the new display presents.
        *lock_recover(&old.display) = None;
    }
    drop(context);
    *lock_recover(&gpu.adapter) = None;
    old
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_losses_back_off_then_stop() {
        let mut losses = Vec::new();
        let t0 = Instant::now();
        assert_eq!(plan(&mut losses, t0), Plan::Now);
        // Ten minutes later the first is forgotten.
        let t1 = t0 + Duration::from_secs(600);
        assert_eq!(plan(&mut losses, t1), Plan::Now);
        assert_eq!(
            plan(&mut losses, t1 + Duration::from_secs(20)),
            Plan::After(BACK_OFF)
        );
        assert_eq!(
            plan(&mut losses, t1 + Duration::from_secs(200)),
            Plan::GiveUp
        );
        // Spread losses never give up.
        let mut losses = Vec::new();
        for i in 0..10u64 {
            assert_eq!(
                plan(&mut losses, t0 + Duration::from_secs(i * 200)),
                Plan::Now
            );
        }
    }

    /// A simulated loss (no driver reset): the lost device's renders fail
    /// with the lost error; after `drop_device_state` a new device and
    /// processor render the same photograph again.
    #[test]
    #[ignore = "requires a native GPU"]
    fn after_a_simulated_loss_a_new_device_renders_again() {
        use crate::gpu_processing::{
            GpuImageCache, ReadbackShader, RenderOutputPrecision, RenderRequest, RenderSource,
            process_and_get_dynamic_image_inner,
        };
        use crate::image_processing::get_all_adjustments_from_json;
        let gpu = GpuState::default();
        let develop = DevelopSession::default();
        let start = |gpu: &GpuState| {
            let context = GpuContext::headless().expect("native QA requires a GPU");
            *gpu.context_slot() = Some(context.clone());
            context
        };
        let photo = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(640, 480, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
        }));
        let render = |context: &GpuContext, cache: &std::sync::Mutex<Option<GpuImageCache>>| {
            process_and_get_dynamic_image_inner(
                context,
                &gpu.processor,
                cache,
                RenderSource::Image {
                    image: &photo,
                    transform_hash: 1,
                },
                RenderRequest {
                    adjustments: get_all_adjustments_from_json(
                        &serde_json::json!({"exposure":0.3}),
                        false,
                        None,
                    ),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                "dvl1_recovery",
                RenderOutputPrecision::EightBit,
                false,
                None,
                None,
                ReadbackShader::Export,
            )
        };
        let first = start(&gpu);
        let before = render(&first, &develop.gpu_image_cache).expect("renders");

        rawtonic_gpu::device_start::simulate_lost("DVL1 test");
        assert!(rawtonic_gpu::device_start::device_lost());
        let failed = render(&first, &develop.gpu_image_cache).unwrap_err();
        assert_eq!(failed, rawtonic_runtime::gpu_memory::GPU_DEVICE_LOST);

        let old = drop_device_state(&gpu, Some(&develop)).expect("the old context");
        assert!(gpu.context().is_none());
        assert!(
            gpu.processor
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
        );
        assert!(develop.gpu_image_cache.lock().unwrap().is_none());
        drop(old);

        let second = start(&gpu);
        assert!(!std::sync::Arc::ptr_eq(&first.device, &second.device));
        assert!(!rawtonic_gpu::device_start::device_lost());
        // A render that took the old context before the recovery and runs
        // after it (the simulated device still works) leaves a processor
        // and an upload of the old device behind: the next render on the
        // new device makes its own (DVL2).
        render(&first, &develop.gpu_image_cache).expect("the old device still renders");
        let after = render(&second, &develop.gpu_image_cache).expect("renders again");
        assert_eq!(before.as_bytes(), after.as_bytes());
        let processor = gpu.processor.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            processor
                .as_ref()
                .is_some_and(|state| state.processor.uses(&second))
        );
        drop(processor);
    }

    /// DVL3: a single photo's render (a collage's cell) that meets a loss
    /// goes once more on the new device and gives the same pixels, never
    /// the lost error.
    #[test]
    #[ignore = "requires a native GPU"]
    fn a_single_render_hit_by_a_simulated_loss_goes_again_on_the_new_device() {
        use crate::gpu_processing::{
            ReadbackShader, RenderOutputPrecision, RenderRequest, RenderSource,
            process_and_get_dynamic_image_inner,
        };
        use crate::image_processing::get_all_adjustments_from_json;
        use std::sync::atomic::AtomicBool;
        let gpu = std::sync::Arc::new(GpuState::default());
        let develop = DevelopSession::default();
        let start = |gpu: &GpuState| {
            let context = GpuContext::headless().expect("native QA requires a GPU");
            *gpu.context_slot() = Some(context.clone());
            context
        };
        let photo = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(640, 480, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
        }));
        let render = |context: &GpuContext| {
            process_and_get_dynamic_image_inner(
                context,
                &gpu.processor,
                &develop.gpu_image_cache,
                RenderSource::Image {
                    image: &photo,
                    transform_hash: 1,
                },
                RenderRequest {
                    adjustments: get_all_adjustments_from_json(
                        &serde_json::json!({"exposure":0.3}),
                        false,
                        None,
                    ),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                "dvl3_single",
                RenderOutputPrecision::EightBit,
                false,
                None,
                None,
                ReadbackShader::Export,
            )
        };
        let first = std::sync::Arc::new(start(&gpu));
        let before = render(&first).expect("renders");

        // The app's recovery, as `recover` does it: once the loss is
        // known, drop the old device's state and start a new device.
        let recovery = {
            let gpu = std::sync::Arc::clone(&gpu);
            std::thread::spawn(move || {
                while !rawtonic_gpu::device_start::device_lost() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                std::thread::sleep(Duration::from_millis(200));
                drop(drop_device_state(&gpu, None));
                let context = GpuContext::headless().expect("native QA requires a GPU");
                *gpu.context_slot() = Some(context);
            })
        };
        let mut attempts = 0;
        let after = crate::export_processing::render_after_device_loss(
            &gpu,
            &first,
            &AtomicBool::new(false),
            Duration::from_secs(10),
            |context| {
                attempts += 1;
                if attempts == 1 {
                    // The loss comes while the render runs; the driver's
                    // reset can make wgpu panic in the readback's poll.
                    rawtonic_gpu::device_start::simulate_lost("DVL3 test");
                    panic!("wgpu: device lost while polling");
                }
                render(context)
            },
        )
        .expect("the render goes again on the new device");
        recovery.join().unwrap();
        assert_eq!(attempts, 2);
        assert_eq!(before.as_bytes(), after.as_bytes());
    }
}
