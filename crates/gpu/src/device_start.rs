//! Starting the processing device with a fallback across graphics backends.
//!
//! The first try is the choice the app always made: one wgpu pick among the
//! preferred backends (`WGPU_BACKEND` when set, else the platform's set).
//! When the device cannot be created on the backend picked, that backend is
//! left out and the pick repeats; then the platform's last resorts follow
//! (Windows: OpenGL, which wgpu offers only where a GL driver exists). Every
//! failure is kept, so a start where nothing works names what failed instead
//! of ending without a word.
//!
//! Microsoft's software renderer (WARP, "Microsoft Basic Render Driver") is
//! refused like a device that does not start: its shader compiler corrupts
//! the heap while compiling the processing pipeline on the first dispatch
//! (`d3d10warp!ShaderJIT`, 0xC0000374), so a WARP start would end in a
//! crash instead of a message. Notes:
//! parity-gaps/G57-dependency-and-distribution-qualification.md (WW).
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
    // Windows: Vulkan or DX12 (wgpu's order, power preference across both),
    // OpenGL only when neither starts. Elsewhere wgpu's default set.
    let platform: &[wgpu::Backends] = if cfg!(target_os = "windows") {
        &[wgpu::Backends::PRIMARY, wgpu::Backends::GL]
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

pub fn start_device(
    surface_for: impl Fn(&wgpu::Instance) -> Option<wgpu::Surface<'static>>,
) -> Result<StartedDevice, String> {
    let failing = std::env::var("RAWTONIC_GPU_FAIL_BACKENDS")
        .map(|list| wgpu::Backends::from_comma_list(&list))
        .unwrap_or(wgpu::Backends::empty());
    // wgpu's no-op backend never yields an adapter unless asked for.
    let available = wgpu::Instance::enabled_backend_features() - wgpu::Backends::NOOP;
    let tiers = backend_tiers(wgpu::Backends::from_env(), available);
    let ((instance, adapter, surface), (device, queue)) = start_with_fallback(
        &tiers,
        |backends| {
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
            if failing.contains(adapter.get_info().backend.into()) {
                return Err("failed on purpose (RAWTONIC_GPU_FAIL_BACKENDS)".into());
            }
            pollster::block_on(adapter.request_device(&processing_device_descriptor(adapter)))
                .map_err(|e| e.to_string())
        },
    )?;
    Ok(StartedDevice {
        instance,
        adapter,
        surface,
        device,
        queue,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
            transient_saves_memory: false,
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
            assert_eq!(platform_last, [Backends::PRIMARY, Backends::GL]);
        } else {
            // Linux and macOS: wgpu's own pick, as before the fallback.
            assert_eq!(platform_last, [Backends::all()]);
        }
        // A preferred DX12 that fails goes on with the platform's sets.
        let (result, _) = start(&tiers, &[Backend::Vulkan, Backend::Dx12], Backends::DX12);
        assert_eq!(result, Ok(Backend::Vulkan));
    }
}
