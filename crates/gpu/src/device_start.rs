//! Starting the processing device with a fallback across graphics backends.
//!
//! The first try is the choice the app always made: one wgpu pick among the
//! preferred backends (`WGPU_BACKEND` when set, else the platform's set).
//! When the device cannot be created on the backend picked, that backend is
//! left out and the pick repeats. Every failure is kept, so a start where
//! nothing works names what failed instead of ending without a word.
//!
//! OpenGL is never used on Windows (`usable_backends`): wgpu's WGL backend
//! panics ("Could not lock adapter context. This is most-likely a deadlock")
//! when a thread waits more than 1 s for the GL context, and one compile of
//! the processing shader holds it for seconds while the start compiles its
//! pipelines on several threads. Seen as a start crash on a Radeon 680M
//! (AMD GL 4.6 driver, notes: parity-gaps/G76-windows-installer-feedback.md).
//! Windows 11 needs a DirectX 12 GPU; a PC with neither DX12 nor Vulkan gets
//! the window's "graphics could not start" message instead of that crash.
//!
//! Microsoft's software renderer (WARP, "Microsoft Basic Render Driver") is
//! refused like a device that does not start: its shader compiler corrupts
//! the heap while compiling the processing pipeline on the first dispatch
//! (`d3d10warp!ShaderJIT`, 0xC0000374), so a WARP start would end in a
//! crash instead of a message. Notes:
//! parity-gaps/G57-dependency-and-distribution-qualification.md (WW).
//!
//! Backends an earlier start crashed on are left out (`avoid`, the app's
//! start guard); `on_step` hears each step before it runs, so the next
//! start knows which backend a crash inside it belongs to.
//!
//! `RAWTONIC_GPU_FAIL_BACKENDS=vulkan` (a comma list as `WGPU_BACKEND`)
//! makes device creation fail on those backends, to test the fallback on a
//! machine where they work.

use crate::gpu_processing::processing_device_descriptor;

/// The backend sets tried in order, each as one wgpu pick: the preferred set
/// first (`preferred` from `WGPU_BACKEND`, else the platform's), then the
/// platform's sets not covered yet; only backends in `available` (those
/// compiled into this build), so a failure names no backend that cannot exist.
pub fn backend_tiers(
    preferred: Option<wgpu::Backends>,
    available: wgpu::Backends,
) -> Vec<wgpu::Backends> {
    // Windows: Vulkan or DX12 (wgpu's order, power preference across both;
    // no OpenGL, see the module). Elsewhere wgpu's default set.
    let platform: &[wgpu::Backends] = if cfg!(target_os = "windows") {
        &[wgpu::Backends::PRIMARY]
    } else {
        &[wgpu::Backends::all()]
    };
    let mut tiers = Vec::with_capacity(platform.len() + 1);
    let mut covered = wgpu::Backends::empty();
    for tier in preferred.into_iter().chain(platform.iter().copied()) {
        let tier = (tier & available) - covered;
        if !tier.is_empty() {
            tiers.push(tier);
            covered |= tier;
        }
    }
    tiers
}

/// Picks an adapter from each set of `tiers` in turn (`adapter_for`) and
/// creates its device (`device_for`); a backend whose device fails is left
/// out and the set picked again. Returns the first adapter and device that
/// start, or every failure in one message.
pub fn start_with_fallback<A, D>(
    tiers: &[wgpu::Backends],
    mut adapter_for: impl FnMut(wgpu::Backends) -> Result<A, String>,
    backend_of: impl Fn(&A) -> wgpu::Backend,
    name_of: impl Fn(&A) -> String,
    mut device_for: impl FnMut(&A) -> Result<D, String>,
) -> Result<(A, D), String> {
    let mut failures: Vec<String> = Vec::new();
    let mut tried = wgpu::Backends::empty();
    for &tier in tiers {
        let mut allowed = tier - tried;
        while !allowed.is_empty() {
            let adapter = match adapter_for(allowed) {
                Ok(adapter) => adapter,
                Err(error) => {
                    failures.push(format!("{}: no adapter ({error})", names(allowed)));
                    break;
                }
            };
            let backend = backend_of(&adapter);
            let backends = wgpu::Backends::from(backend);
            match device_for(&adapter) {
                Ok(device) => {
                    if !failures.is_empty() {
                        log::warn!(
                            "GPU started on {} after: {}",
                            backend.to_str(),
                            failures.join("; ")
                        );
                    }
                    return Ok((adapter, device));
                }
                Err(error) => {
                    let failure = format!("{} ({}): {error}", backend.to_str(), name_of(&adapter));
                    log::warn!("GPU device failed on {failure}; trying the next backend");
                    failures.push(failure);
                }
            }
            if !allowed.contains(backends) {
                // A pick outside the set asked for: do not ask again.
                break;
            }
            allowed -= backends;
            tried |= backends;
        }
        tried |= tier;
    }
    if failures.is_empty() {
        failures.push("no graphics backend available".into());
    }
    Err(format!(
        "No graphics backend could start the GPU: {}",
        failures.join("; ")
    ))
}

/// The backends a device may start on: those compiled into this build,
/// without wgpu's no-op backend (it never yields an adapter unless asked
/// for) and, on Windows, without OpenGL (see the module).
pub fn usable_backends() -> wgpu::Backends {
    usable_of(
        wgpu::Instance::enabled_backend_features(),
        cfg!(target_os = "windows"),
    )
}

fn usable_of(compiled: wgpu::Backends, windows: bool) -> wgpu::Backends {
    let unusable = if windows {
        wgpu::Backends::NOOP | wgpu::Backends::GL
    } else {
        wgpu::Backends::NOOP
    };
    compiled - unusable
}

/// The shader compiles the device may run at once
/// (`gpu_processing::set_compile_limit`): one on AMD's Windows Vulkan driver,
/// which committed 25–30 GB of memory when several ran at once (Radeon 680M,
/// driver 25.10; notes: parity-gaps/G76-windows-installer-feedback.md), with
/// no slower first display variant; no limit elsewhere.
pub fn compile_limit(info: &wgpu::AdapterInfo, windows: bool) -> Option<usize> {
    const AMD: u32 = 0x1002;
    (windows && info.backend == wgpu::Backend::Vulkan && info.vendor == AMD).then_some(1)
}

/// Whether renders on the adapter go in short submissions
/// (`gpu_processing::set_short_submissions`): integrated GPUs, where the
/// 2 s watchdog took the device away mid-session (DVL1: Radeon 680M, a
/// single submission of 2.15 s lost it, 1.67 s did not).
pub fn short_submissions(info: &wgpu::AdapterInfo) -> bool {
    info.device_type == wgpu::DeviceType::IntegratedGpu
}

/// Why the adapter cannot run the processing pipeline, if it cannot: WARP,
/// found by its PCI ids (Microsoft vendor 0x1414, device 0x8C) on DX12.
pub fn unsupported_adapter(info: &wgpu::AdapterInfo) -> Option<&'static str> {
    (info.backend == wgpu::Backend::Dx12 && info.vendor == 0x1414 && info.device == 0x8c).then_some(
        "Windows' software renderer (WARP) is not supported: it crashes compiling the \
         processing shader; a GPU with a Vulkan or DirectX 12 driver is needed",
    )
}

fn names(backends: wgpu::Backends) -> String {
    wgpu::Backend::ALL
        .into_iter()
        .filter(|backend| backends.contains((*backend).into()))
        .map(wgpu::Backend::to_str)
        .collect::<Vec<_>>()
        .join("/")
}

/// The processing device on the high-performance adapter, with the backend
/// fallback of this module. `surface_for` makes the window's surface for an
/// instance when the adapter must present to it.
pub struct StartedDevice {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub surface: Option<wgpu::Surface<'static>>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// A step of a device start, heard before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartStep {
    /// Picking an adapter among these backends.
    Adapter(wgpu::Backends),
    /// Creating the device on this backend.
    Device(wgpu::Backend),
}

pub fn start_device(
    surface_for: impl Fn(&wgpu::Instance) -> Option<wgpu::Surface<'static>>,
    avoid: wgpu::Backends,
    on_step: impl Fn(StartStep),
) -> Result<StartedDevice, String> {
    let failing = std::env::var("RAWTONIC_GPU_FAIL_BACKENDS")
        .map(|list| wgpu::Backends::from_comma_list(&list))
        .unwrap_or(wgpu::Backends::empty());
    let usable = usable_backends();
    let available = usable - avoid;
    if available.is_empty() {
        return Err(format!(
            "No graphics backend is left to try: {} crashed while starting at an earlier start \
             (Reset graphics settings and retry tries them again)",
            names(usable & avoid)
        ));
    }
    let tiers = backend_tiers(wgpu::Backends::from_env(), available);
    let ((instance, adapter, surface), (device, queue)) = start_with_fallback(
        &tiers,
        |backends| {
            on_step(StartStep::Adapter(backends));
            let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
            descriptor.backends = backends;
            let instance = wgpu::Instance::new(descriptor);
            let surface = surface_for(&instance);
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: surface.as_ref(),
                    ..Default::default()
                }))
                .map_err(|e| e.to_string())?;
            Ok((instance, adapter, surface))
        },
        |(_, adapter, _)| adapter.get_info().backend,
        |(_, adapter, _)| adapter.get_info().name,
        |(_, adapter, _)| {
            if let Some(reason) = unsupported_adapter(&adapter.get_info()) {
                return Err(reason.into());
            }
            on_step(StartStep::Device(adapter.get_info().backend));
            if failing.contains(adapter.get_info().backend.into()) {
                return Err("failed on purpose (RAWTONIC_GPU_FAIL_BACKENDS)".into());
            }
            pollster::block_on(adapter.request_device(&processing_device_descriptor(adapter)))
                .map_err(|e| e.to_string())
        },
    )?;
    crate::gpu_processing::set_compile_limit(compile_limit(
        &adapter.get_info(),
        cfg!(target_os = "windows"),
    ));
    crate::gpu_processing::set_short_submissions(short_submissions(&adapter.get_info()));
    // There each submission's own GPU time is logged when it passes the
    // watchdog budget (`gpu_submit`); elsewhere the time from queuing it.
    // QA builds can ask for the timing everywhere (`qa_log_requested`).
    if crate::gpu_processing::short_submissions()
        || rawtonic_rawdecode::gpu_submit::qa_log_requested()
    {
        rawtonic_rawdecode::gpu_submit::time_on_gpu(&device);
    } else {
        rawtonic_rawdecode::gpu_submit::stop_timing();
    }
    // A lost device otherwise shows only as the next resource's validation
    // error (WGT5: "Buffer … is invalid").
    let generation = mark_started();
    device.set_device_lost_callback(move |reason, message| {
        log::error!("GPU device lost ({reason:?}): {message} (device {generation})");
        if marks_device_lost(reason) {
            mark_lost(generation);
            if let Some(hook) = LOST_HOOK.get() {
                hook(LostDevice {
                    generation,
                    message,
                });
            }
        }
    });
    Ok(StartedDevice {
        instance,
        adapter,
        surface,
        device,
        queue,
    })
}

/// A device the driver took away (`on_device_lost`).
#[derive(Debug, Clone)]
pub struct LostDevice {
    /// The device's number among this process's starts (from 1).
    pub generation: u64,
    /// The driver's or wgpu's reason.
    pub message: String,
}

/// The number of the newest device started (0: none yet) and of the newest
/// one lost other than by being dropped (its driver reset it; G76/WCC6:
/// 'Out of memory', DVL1: 'Unexpected error variant' on a Radeon 680M).
/// A device started after a loss (the app's recovery, `on_device_lost`)
/// is not lost: only the lost device's own work fails with the lost error.
static STARTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LOST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Called once per lost device, on the thread wgpu reports it on (it may
/// hold wgpu's locks: the hook only hands the work on).
type LostHook = Box<dyn Fn(LostDevice) + Send + Sync>;
static LOST_HOOK: std::sync::OnceLock<LostHook> = std::sync::OnceLock::new();

/// Hears every device the driver takes away, so the app can start a new
/// one (DVL1: the multi view froze until a restart). Set once; later calls
/// are ignored.
pub fn on_device_lost(hook: impl Fn(LostDevice) + Send + Sync + 'static) {
    let _ = LOST_HOOK.set(Box::new(hook));
}

/// Takes the newest device as lost without a driver reset, for QA and
/// tests of the recovery: its renders fail with the lost error and the
/// hook hears it, as after a real loss. `None` before any device started.
pub fn simulate_lost(message: &str) -> Option<LostDevice> {
    let generation = STARTED.load(std::sync::atomic::Ordering::Acquire);
    if generation == 0 {
        return None;
    }
    log::error!("GPU device lost (simulated): {message} (device {generation})");
    mark_lost(generation);
    let lost = LostDevice {
        generation,
        message: message.to_string(),
    };
    if let Some(hook) = LOST_HOOK.get() {
        hook(lost.clone());
    }
    Some(lost)
}

fn mark_started() -> u64 {
    STARTED.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1
}

fn mark_lost(generation: u64) {
    LOST.fetch_max(generation, std::sync::atomic::Ordering::AcqRel);
}

/// Whether a device lost for `reason` counts as lost (`LOST`): not one the
/// app dropped itself (`Destroyed`, e.g. a context replaced for the
/// window's display).
fn marks_device_lost(reason: wgpu::DeviceLostReason) -> bool {
    reason != wgpu::DeviceLostReason::Destroyed
}

/// Whether the newest GPU device was lost: its renders fail with
/// `rawtonic_runtime::gpu_memory::GPU_DEVICE_LOST` until a new device
/// starts.
pub fn device_lost() -> bool {
    lost_of(
        STARTED.load(std::sync::atomic::Ordering::Acquire),
        LOST.load(std::sync::atomic::Ordering::Acquire),
    )
}

fn lost_of(started: u64, lost: u64) -> bool {
    lost != 0 && lost >= started
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_driver_reset_marks_the_device_lost_a_dropped_device_does_not() {
        assert!(marks_device_lost(wgpu::DeviceLostReason::Unknown));
        assert!(!marks_device_lost(wgpu::DeviceLostReason::Destroyed));
    }

    #[test]
    fn a_device_started_after_a_loss_is_not_lost() {
        assert!(!lost_of(0, 0), "nothing started");
        assert!(!lost_of(1, 0), "running");
        assert!(lost_of(1, 1), "the only device lost");
        assert!(!lost_of(2, 1), "a new device after the loss");
        assert!(lost_of(2, 2), "the new device lost too");
    }
    use wgpu::{Backend, Backends};

    /// A fake adapter: its backend.
    fn start(
        tiers: &[Backends],
        adapters: &[Backend],
        broken: Backends,
    ) -> (Result<Backend, String>, Vec<Backends>) {
        let mut asked = Vec::new();
        let result = start_with_fallback(
            tiers,
            |allowed| {
                asked.push(allowed);
                adapters
                    .iter()
                    .copied()
                    .find(|backend| allowed.contains((*backend).into()))
                    .ok_or_else(|| "none".to_string())
            },
            |backend| *backend,
            |backend| format!("GPU on {}", backend.to_str()),
            |backend| {
                if broken.contains((*backend).into()) {
                    Err("device lost".to_string())
                } else {
                    Ok(())
                }
            },
        )
        .map(|(backend, ())| backend);
        (result, asked)
    }

    #[test]
    fn the_first_pick_wins_when_its_device_starts() {
        let tiers = [Backends::PRIMARY, Backends::GL];
        let (result, asked) = start(
            &tiers,
            &[Backend::Vulkan, Backend::Dx12, Backend::Gl],
            Backends::empty(),
        );
        assert_eq!(result, Ok(Backend::Vulkan));
        assert_eq!(asked, [Backends::PRIMARY]);
    }

    #[test]
    fn a_failing_device_falls_back_vulkan_dx12_gl() {
        let tiers = [Backends::PRIMARY, Backends::GL];
        let adapters = [Backend::Vulkan, Backend::Dx12, Backend::Gl];
        let (result, asked) = start(&tiers, &adapters, Backends::VULKAN);
        assert_eq!(result, Ok(Backend::Dx12));
        assert_eq!(
            asked,
            [Backends::PRIMARY, Backends::PRIMARY - Backends::VULKAN]
        );

        let (result, asked) = start(&tiers, &adapters, Backends::VULKAN | Backends::DX12);
        assert_eq!(result, Ok(Backend::Gl));
        assert_eq!(
            asked,
            [
                Backends::PRIMARY,
                Backends::PRIMARY - Backends::VULKAN,
                Backends::METAL | Backends::BROWSER_WEBGPU,
                Backends::GL,
            ]
        );
    }

    #[test]
    fn when_nothing_starts_the_error_names_every_failure() {
        let tiers = [Backends::PRIMARY, Backends::GL];
        let (result, _) = start(
            &tiers,
            &[Backend::Vulkan, Backend::Dx12],
            Backends::VULKAN | Backends::DX12,
        );
        let error = result.unwrap_err();
        assert!(
            error.contains("vulkan (GPU on vulkan): device lost"),
            "{error}"
        );
        assert!(error.contains("dx12 (GPU on dx12): device lost"), "{error}");
        assert!(error.contains("gl: no adapter (none)"), "{error}");
    }

    fn adapter(backend: Backend, vendor: u32, device: u32) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo {
            name: "adapter".into(),
            vendor,
            device,
            device_type: wgpu::DeviceType::Cpu,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend,
            subgroup_min_size: 0,
            subgroup_max_size: 0,
            transient_saves_memory: Some(false),
            limit_bucket: None,
        }
    }

    #[test]
    fn warp_is_refused_and_other_adapters_are_not() {
        assert!(unsupported_adapter(&adapter(Backend::Dx12, 0x1414, 0x8c)).is_some());
        // Other software adapters (llvmpipe, SwiftShader) and real GPUs start.
        assert!(unsupported_adapter(&adapter(Backend::Vulkan, 0x10005, 0)).is_none());
        assert!(unsupported_adapter(&adapter(Backend::Dx12, 0x10de, 0x2206)).is_none());
    }

    #[test]
    fn amd_vulkan_on_windows_compiles_one_at_a_time() {
        let amd = |backend| wgpu::AdapterInfo {
            backend,
            ..adapter(backend, 0x1002, 0x1681)
        };
        assert_eq!(compile_limit(&amd(Backend::Vulkan), true), Some(1));
        assert_eq!(compile_limit(&amd(Backend::Dx12), true), None);
        assert_eq!(compile_limit(&amd(Backend::Vulkan), false), None);
        assert_eq!(
            compile_limit(&adapter(Backend::Vulkan, 0x10de, 0x2206), true),
            None
        );
    }

    #[test]
    fn open_gl_is_never_used_on_windows() {
        let compiled = Backends::VULKAN | Backends::DX12 | Backends::GL | Backends::NOOP;
        assert_eq!(usable_of(compiled, true), Backends::PRIMARY & compiled);
        assert_eq!(
            usable_of(compiled, false),
            Backends::VULKAN | Backends::DX12 | Backends::GL
        );
        assert!(!usable_backends().contains(Backends::NOOP));
        // Nothing reaches OpenGL there, not even a preference for it
        // (`WGPU_BACKEND=gl`): it is not in the backends tried.
        let usable = usable_of(compiled, true);
        let tiers = backend_tiers(Some(Backends::GL), usable);
        assert!(tiers.iter().all(|tier| !tier.contains(Backends::GL)));
    }

    #[test]
    fn backends_missing_from_the_build_are_not_tried() {
        let available = Backends::VULKAN | Backends::DX12 | Backends::GL;
        let tiers = backend_tiers(Some(Backends::METAL), available);
        assert!(tiers.iter().all(|tier| available.contains(*tier)));
        assert!(!tiers.is_empty());
    }

    #[test]
    fn the_preferred_set_comes_first_and_is_not_tried_twice() {
        let platform_last = backend_tiers(None, Backends::all());
        let tiers = backend_tiers(Some(Backends::DX12), Backends::all());
        assert_eq!(tiers[0], Backends::DX12);
        assert!(tiers[1..].iter().all(|tier| !tier.contains(Backends::DX12)));
        assert_eq!(
            tiers
                .iter()
                .fold(Backends::empty(), |all, tier| all | *tier),
            platform_last
                .iter()
                .fold(Backends::DX12, |all, tier| all | *tier)
        );
        if cfg!(target_os = "windows") {
            assert_eq!(platform_last, [Backends::PRIMARY]);
        } else {
            // Linux and macOS: wgpu's own pick, as before the fallback.
            assert_eq!(platform_last, [Backends::all()]);
        }
        // A preferred DX12 that fails goes on with the platform's sets.
        let (result, _) = start(&tiers, &[Backend::Vulkan, Backend::Dx12], Backends::DX12);
        assert_eq!(result, Ok(Backend::Vulkan));
    }
}
