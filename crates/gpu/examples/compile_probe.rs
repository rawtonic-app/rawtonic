//! Measures what one compute-pipeline compile of the image-processing shader
//! costs the GPU driver: wall time and, on Windows, the process's peak
//! private memory (commit charge; `null` on other systems). A tool for
//! finding where a driver's compiler spends its time (G76/WCC: the Radeon
//! 680M's Windows Vulkan driver took ~70 s and ~7 GB for one display
//! variant), not part of the app.
//!
//! cargo run --release -p rawtonic-gpu --example compile_probe -- SPECS_FILE [OUT.jsonl]
//!
//! Each line of SPECS_FILE is one candidate, compiled in its own child
//! process (clean peak memory, no in-process reuse):
//!
//! `NAME features=N [dce] [unchecked] [stub=f1,f2] [src=FILE] [set=K:V,K:V] [export] [spirv] [noinline=all|f1,f2]`
//!
//! - `features`: display variant bits as `create_preview_pipeline` sets them
//!   (with `export`: as an export pipeline, i.e. without prepared layers).
//! - `dce`: fold every `if` whose condition the override constants decide
//!   and drop the code that becomes unreachable, before the driver sees it.
//! - `unchecked`: no bounds checks and no loop bounding (a trusted module).
//! - `stub`: replace these functions' bodies with `return` of their first
//!   argument (or a zero value): what a variant without that stage costs.
//! - `src`: another WGSL file instead of the built-in shader.wgsl.
//! - `set`: extra override constants.
//! - `spirv`: write the SPIR-V here (naga, as wgpu's Vulkan backend does) and
//!   hand it to the driver as is (Vulkan only; the pipeline layout is made
//!   from the module's bindings).
//! - `spv`: with `spirv`, writer options as wgpu-hal sets them: `nodebug`,
//!   `halflags`, `caps`, `bufrestrict`, `imgrestrict`, `zeronative`, or `hal`
//!   for all.
//! - `spvfile`: with `spirv`, pass this SPIR-V file through instead;
//!   `ranks=b,b,…`: the layout's bindings numbered as wgpu-hal does (rank
//!   among these WGSL bindings), for SPIR-V written that way.
//! - `spv=cache`: compile through an empty pipeline cache, as the app does.
//! - `spv=proc`: the app's device start and a processor; the variant compiles
//!   through the processor's display path. `spv=appdevice` (with `spirv`):
//!   the probe's SPIR-V and layout on the app's device.
//! - `noinline`: with `spirv`, mark these functions (or `all` but the entry
//!   point) DontInline: whether the driver compiles each once.
//!
//! Every candidate gets a unique nonce in its module, so the driver's own
//! on-disk shader cache never answers it (clear it anyway for a cold run).
//! Lines starting with `#` are skipped. A candidate is stopped after
//! `COMPILE_PROBE_TIMEOUT` seconds (default 300).

use std::io::Write as _;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--one") {
        let line = args.get(2).cloned().unwrap_or_default();
        match one(&line) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                println!("{{\"error\":{}}}", json_str(&error));
                std::process::exit(2);
            }
        }
        return;
    }
    let specs = args
        .get(1)
        .expect("usage: compile_probe SPECS_FILE [OUT.jsonl]");
    let out_path = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "compile_probe.jsonl".into());
    let timeout = Duration::from_secs(
        std::env::var("COMPILE_PROBE_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300),
    );
    let text = std::fs::read_to_string(specs).expect("read the specs file");
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)
        .expect("open the output file");
    let exe = std::env::current_exe().unwrap();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = line.split_whitespace().next().unwrap_or("?").to_string();
        eprintln!("{name}: compiling …");
        let started = Instant::now();
        let mut child = std::process::Command::new(&exe)
            .arg("--one")
            .arg(line)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("start a child");
        let result = loop {
            if let Some(status) = child.try_wait().unwrap() {
                let mut text = String::new();
                use std::io::Read as _;
                child.stdout.take().unwrap().read_to_string(&mut text).ok();
                let last = text.lines().last().unwrap_or("").to_string();
                break if last.starts_with('{') {
                    last
                } else {
                    format!("{{\"error\":{}}}", json_str(&format!("exit {status}")))
                };
            }
            if started.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                break format!("{{\"error\":\"timeout after {} s\"}}", timeout.as_secs());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let record = format!(
            "{{\"name\":{},\"spec\":{},\"result\":{}}}",
            json_str(&name),
            json_str(line),
            result
        );
        eprintln!("{record}");
        writeln!(out, "{record}").unwrap();
    }
}

fn json_str(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct Spec {
    features: usize,
    dce: bool,
    unchecked: bool,
    export: bool,
    stubs: Vec<String>,
    src: Option<String>,
    set: Vec<(String, f64)>,
    spirv: bool,
    noinline: Vec<String>,
    spv: Vec<String>,
    spvfile: Option<String>,
    ranks: Vec<u32>,
}

fn parse(line: &str) -> Result<Spec, String> {
    let mut spec = Spec {
        features: 0,
        dce: false,
        unchecked: false,
        export: false,
        stubs: vec![],
        src: None,
        set: vec![],
        spirv: false,
        noinline: vec![],
        spv: vec![],
        spvfile: None,
        ranks: vec![],
    };
    for word in line.split_whitespace().skip(1) {
        match word.split_once('=') {
            Some(("features", v)) => spec.features = v.parse().map_err(|_| "features")?,
            Some(("stub", v)) => spec.stubs = v.split(',').map(String::from).collect(),
            Some(("src", v)) => spec.src = Some(v.to_string()),
            Some(("noinline", v)) => spec.noinline = v.split(',').map(String::from).collect(),
            Some(("spv", v)) => spec.spv = v.split(',').map(String::from).collect(),
            Some(("spvfile", v)) => spec.spvfile = Some(v.to_string()),
            Some(("ranks", v)) => {
                spec.ranks = v
                    .split(',')
                    .map(|b| b.parse().map_err(|_| "ranks"))
                    .collect::<Result<_, _>>()?
            }
            Some(("set", v)) => {
                for pair in v.split(',') {
                    let (k, v) = pair.split_once(':').ok_or("set K:V")?;
                    spec.set
                        .push((k.to_string(), v.parse().map_err(|_| "set value")?));
                }
            }
            None if word == "dce" => spec.dce = true,
            None if word == "unchecked" => spec.unchecked = true,
            None if word == "export" => spec.export = true,
            None if word == "spirv" => spec.spirv = true,
            _ => return Err(format!("unknown word {word}")),
        }
    }
    Ok(spec)
}

/// `variant_constants` of gpu_processing.rs (display variant: direct curves,
/// prepared layers and mixer probes; export: neither).
fn constants(spec: &Spec) -> naga::back::PipelineConstants {
    let f = spec.features;
    let prepared = !spec.export;
    let mut c: Vec<(String, f64)> = [
        ("DIRECT_CURVES", 1.0),
        ("PREPARED_LAYERS", f64::from(u8::from(prepared))),
        ("LAYER_SUMS", 0.0),
        ("ENABLE_MASKS", ((f >> 3) & 1) as f64),
        ("ENABLE_NOISE_REDUCTION", ((f >> 4) & 1) as f64),
        ("ENABLE_SHARPEN", (f & 1) as f64),
        ("ENABLE_HIGHLIGHTS", ((f >> 1) & 1) as f64),
        ("ENABLE_CURVES", ((f >> 2) & 1) as f64),
        ("ENABLE_TONE", ((f >> 5) & 1) as f64),
        ("ENABLE_RELIGHT", ((f >> 6) & 1) as f64),
        ("NR_REFINED", ((f >> 7) & 1) as f64),
        ("RELIGHT_RANGE2", ((f >> 8) & 1) as f64),
        ("RELIGHT_RANGE3", ((f >> 9) & 1) as f64),
        ("RELIGHT_RANGE4", ((f >> 10) & 1) as f64),
        ("SHARPEN_V7", ((f >> 11) & 1) as f64),
        ("SHARPEN_V8", ((f >> 12) & 1) as f64),
        ("WHITES_V12", ((f >> 13) & 1) as f64),
        ("SHADOWS_V13", ((f >> 14) & 1) as f64),
        ("HIGHLIGHTS_V14", ((f >> 15) & 1) as f64),
        ("VIBRANCE_V2", ((f >> 16) & 1) as f64),
        ("BLACKS_V16", ((f >> 18) & 1) as f64),
        ("HSL_V2", ((f >> 17) & 1) as f64),
        ("PRESENCE_V3", ((f >> 19) & 1) as f64),
        ("DEHAZE_V2", ((f >> 20) & 1) as f64),
        ("ENABLE_EFFECTS", ((f >> 21) & 1) as f64),
        ("DEHAZE_V3", ((f >> 22) & 1) as f64),
        ("TONE_V5", ((f >> 23) & 1) as f64),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    if prepared {
        c.push(("MIXER_PROBES".into(), 1.0));
    }
    c.extend(spec.set.iter().cloned());
    c.into_iter().collect()
}

fn one(line: &str) -> Result<String, String> {
    let spec = parse(line)?;
    let source = match &spec.src {
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?,
        None => include_str!("../src/shaders/shader.wgsl").to_string(),
    };
    let module = naga::front::wgsl::parse_str(&source).map_err(|e| e.emit_to_string(&source))?;
    let validate = |m: &naga::Module| {
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(m)
        .map_err(|e| format!("{e:?}"))
    };
    let info = validate(&module)?;
    let (module, _) = naga::back::pipeline_constants::process_overrides(
        &module,
        &info,
        Some((naga::ShaderStage::Compute, "main")),
        &constants(&spec),
    )
    .map_err(|e| e.to_string())?;
    let mut module = module.into_owned();
    let folded = if spec.dce {
        rawtonic_gpu::gpu_processing::fold_decided_branches(&mut module)
    } else {
        0
    };
    for name in &spec.stubs {
        stub(&mut module, name)?;
    }
    add_nonce(&mut module);
    naga::compact::compact(&mut module, naga::compact::KeepUnused::No);
    validate(&module)?;
    let inlined = inlined_size(&module);
    // COMPILE_PROBE_REPORT=1: the largest inlined calls (main's direct
    // calls and the most-repeated functions) on stderr.
    if std::env::var("COMPILE_PROBE_REPORT").is_ok() {
        report_inlining(&module);
    }
    let functions = module.functions.len();

    let (spirv, marked) = if spec.spirv {
        let (words, marked) = match &spec.spvfile {
            // `spvfile`: SPIR-V written elsewhere (e.g. as the app writes it,
            // test `dump_display_variant_spirv`), passed through as is.
            Some(path) => {
                let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
                let words = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| u32::from_le_bytes(*c))
                    .collect();
                (words, 0)
            }
            None => spirv_words(&module, spec.unchecked, &spec.noinline, &spec.spv)?,
        };
        // COMPILE_PROBE_DUMP=DIR keeps the SPIR-V (for spirv-val / spirv-dis)
        // and stops there.
        if let Ok(dir) = std::env::var("COMPILE_PROBE_DUMP") {
            let name = line.split_whitespace().next().unwrap_or("probe");
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            std::fs::write(
                std::path::Path::new(&dir).join(format!("{name}.spv")),
                bytes,
            )
            .map_err(|e| e.to_string())?;
            return Ok(format!("{{\"dumped\":true,\"noinline_marked\":{marked}}}"));
        }
        (Some(words), marked)
    } else {
        (None, 0)
    };
    // `spv=proc`: the app's own device start (`GpuContext::headless`) and a
    // processor, the variant compiled through the processor's display path
    // (its layout, pipeline cache and passthrough choice).
    if knob_set(&spec.spv, "proc") {
        if log::set_logger(&StderrLog).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
        return through_processor(&spec, inlined, functions);
    }
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    if std::env::var("WGPU_BACKEND").is_err() {
        descriptor.backends = wgpu::Backends::VULKAN;
    }
    let instance = wgpu::Instance::new(descriptor);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|e| e.to_string())?;
    let adapter_info = adapter.get_info();
    let mut device_descriptor =
        rawtonic_gpu::gpu_processing::processing_device_descriptor(&adapter);
    if spec.spirv {
        if !adapter
            .features()
            .contains(wgpu::Features::PASSTHROUGH_SHADERS)
        {
            return Err("the adapter has no SPIR-V passthrough".into());
        }
        device_descriptor.required_features |= wgpu::Features::PASSTHROUGH_SHADERS;
    }
    // `spv=appdevice`: the device the app starts (`GpuContext::headless`)
    // instead of the probe's own.
    let app_context = if knob_set(&spec.spv, "appdevice") {
        Some(rawtonic_gpu::gpu_processing::GpuContext::headless()?)
    } else {
        None
    };
    let (device, _queue) = match &app_context {
        Some(context) => ((*context.device).clone(), (*context.queue).clone()),
        None => pollster::block_on(adapter.request_device(&device_descriptor))
            .map_err(|e| e.to_string())?,
    };
    if spec.spirv
        && !device
            .features()
            .contains(wgpu::Features::PASSTHROUGH_SHADERS)
    {
        return Err(
            "this device has no SPIR-V passthrough (the app's no longer asks for it)".into(),
        );
    }
    // `spv=explicit` without `spirv`: wgpu's own path with the probe's
    // explicit layout instead of the derived one.
    let layout = if spec.spirv || knob_set(&spec.spv, "explicit") {
        Some(layout_for(&device, &module, &spec.ranks)?)
    } else {
        None
    };
    let shader_descriptor = wgpu::ShaderModuleDescriptor {
        label: Some("probe"),
        source: wgpu::ShaderSource::Naga(std::borrow::Cow::Owned(module)),
    };
    // `spv=cache`: through an (empty) pipeline cache, as the app compiles.
    let cache = if knob_set(&spec.spv, "cache") {
        if !device.features().contains(wgpu::Features::PIPELINE_CACHE) {
            return Err("the device keeps no pipeline cache".into());
        }
        // SAFETY: no data is passed in.
        Some(unsafe {
            device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
                label: Some("probe"),
                data: None,
                fallback: true,
            })
        })
    } else {
        None
    };
    let private_before = memory::private_bytes();
    let sampler = memory::Sampler::start();
    let started = Instant::now();
    let shader = if let Some(words) = spirv {
        // SAFETY: a measurement; the pipeline is never dispatched.
        unsafe {
            device.create_shader_module_passthrough(wgpu::ShaderModuleDescriptorPassthrough {
                label: Some("probe"),
                entry_points: std::borrow::Cow::Owned(vec![wgpu::PassthroughShaderEntryPoint {
                    name: "main".into(),
                    workgroup_size: (8, 8, 1),
                }]),
                spirv: Some(std::borrow::Cow::Owned(words)),
                ..Default::default()
            })
        }
    } else if spec.unchecked {
        // SAFETY: a measurement; the pipeline is never dispatched.
        unsafe {
            device.create_shader_module_trusted(
                shader_descriptor,
                wgpu::ShaderRuntimeChecks::unchecked(),
            )
        }
    } else {
        device.create_shader_module(shader_descriptor)
    };
    let shader_ms = started.elapsed().as_secs_f64() * 1000.0;
    let pipeline_started = Instant::now();
    let _pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("probe"),
        layout: layout.as_ref(),
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: cache.as_ref(),
    });
    let pipeline_ms = pipeline_started.elapsed().as_secs_f64() * 1000.0;
    let peak_sampled = sampler.stop();
    let mb = |bytes: Option<usize>| bytes.map_or("null".to_string(), |b| (b >> 20).to_string());
    let peak = memory::peak_private_bytes().map(|p| p.max(peak_sampled));
    Ok(format!(
        "{{\"pipeline_ms\":{pipeline_ms:.0},\"shader_ms\":{shader_ms:.0},\"peak_mb\":{},\"before_mb\":{},\
\"inlined_exprs\":{inlined},\"functions\":{functions},\"folded_ifs\":{folded},\"noinline_marked\":{marked},\"adapter\":{},\"driver\":{}}}",
        mb(peak),
        mb(private_before),
        json_str(&adapter_info.name),
        json_str(&format!(
            "{:?} {} {}",
            adapter_info.backend, adapter_info.driver, adapter_info.driver_info
        )),
    ))
}

/// The module as wgpu's Vulkan backend writes it (SPIR-V 1.6, index bounds
/// restricted; buffers and images unchecked as with robust access), with the
/// functions in `noinline` (or every one for `all`) marked DontInline.
/// Returns the words and how many functions were marked.
fn spirv_words(
    module: &naga::Module,
    unchecked: bool,
    noinline: &[String],
    knobs: &[String],
) -> Result<(Vec<u32>, usize), String> {
    use naga::back::spv;
    use naga::proc::BoundsCheckPolicy as P;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| format!("{e:?}"))?;
    // Vulkan bindings as wgpu-hal numbers them for the layout `layout_for`
    // makes (the module's bindings, densely in WGSL order). Rounds 3–8 wrote
    // the WGSL numbers instead: with gaps among the used bindings the shader
    // then read other or missing descriptors, and the driver compiled that
    // broken pipeline fast — those passthrough timings are invalid.
    let mut used: Vec<u32> = module
        .global_variables
        .iter()
        .filter_map(|(_, g)| g.binding.as_ref().map(|b| b.binding))
        .collect();
    used.sort_unstable();
    used.dedup();
    let binding_map = used
        .iter()
        .enumerate()
        .map(|(vulkan, &binding)| {
            (
                naga::ResourceBinding { group: 0, binding },
                spv::BindingInfo {
                    descriptor_set: 0,
                    binding: vulkan as u32,
                    binding_array_size: None,
                },
            )
        })
        .collect();
    let mut options = spv::Options {
        lang_version: (1, 6),
        // DEBUG: OpName for the functions, so `noinline` can name them.
        flags: spv::WriterFlags::DEBUG | spv::WriterFlags::LABEL_VARYINGS,
        binding_map,
        fake_missing_bindings: false,
        ..Default::default()
    };
    options.bounds_check_policies = naga::proc::BoundsCheckPolicies {
        index: if unchecked { P::Unchecked } else { P::Restrict },
        buffer: P::Unchecked,
        image_load: P::Unchecked,
        binding_array: P::Unchecked,
    };
    if unchecked {
        options.force_loop_bounding = false;
    }
    // `spv=` knobs: the options wgpu-hal's Vulkan backend writes with
    // (`hal` = all of them), one at a time to find what a driver reacts to.
    let knob = |name: &str| knobs.iter().any(|k| k == name || k == "hal");
    if knob("nodebug") && noinline.iter().all(|n| n == "all") {
        options.flags.remove(spv::WriterFlags::DEBUG);
    }
    if knob("halflags") {
        // (wgpu-hal maps every binding to itself instead of faking missing
        // ones: the same decorations.)
        options.flags |= spv::WriterFlags::FORCE_POINT_SIZE;
    }
    if knob("caps") {
        options.capabilities = Some(
            [
                spv::Capability::Shader,
                spv::Capability::Matrix,
                spv::Capability::Sampled1D,
                spv::Capability::Image1D,
                spv::Capability::ImageQuery,
                spv::Capability::DerivativeControl,
                spv::Capability::StorageImageExtendedFormats,
                spv::Capability::SampledCubeArray,
                spv::Capability::DotProductInputAllKHR,
                spv::Capability::DotProductInput4x8BitKHR,
                spv::Capability::DotProductInput4x8BitPackedKHR,
                spv::Capability::DotProductKHR,
                spv::Capability::Int8,
            ]
            .into_iter()
            .collect(),
        );
    }
    if knob("bufrestrict") {
        options.bounds_check_policies.buffer = P::Restrict;
    }
    if knob("imgrestrict") {
        options.bounds_check_policies.image_load = P::Restrict;
    }
    if knob("zeronative") {
        options.zero_initialize_workgroup_memory = spv::ZeroInitializeWorkgroupMemoryMode::Native;
    }
    let pipeline = spv::PipelineOptions {
        entry_point: "main".into(),
        shader_stage: naga::ShaderStage::Compute,
    };
    // `noentry`: no per-pipeline options, as wgpu-hal writes a module that
    // has no overrides left (its `ShaderModule::Raw` path).
    let pipeline = (!knob_set(knobs, "noentry")).then_some(pipeline);
    let mut words =
        spv::write_vec(module, &info, &options, pipeline.as_ref()).map_err(|e| e.to_string())?;
    if noinline.is_empty() {
        return Ok((words, 0));
    }
    // Instruction walk: OpName (5) gives id → name, OpEntryPoint (15) the
    // entry's id, OpFunction (54) has its control mask in word 3.
    const DONT_INLINE: u32 = 0x2;
    let mut names = std::collections::HashMap::new();
    let mut entry = None;
    let mut functions = vec![];
    let mut at = 5;
    while at < words.len() {
        let count = (words[at] >> 16) as usize;
        let opcode = words[at] & 0xffff;
        if count == 0 {
            return Err("malformed SPIR-V".into());
        }
        match opcode {
            5 => {
                let bytes: Vec<u8> = words[at + 2..at + count]
                    .iter()
                    .flat_map(|w| w.to_le_bytes())
                    .take_while(|b| *b != 0)
                    .collect();
                names.insert(words[at + 1], String::from_utf8_lossy(&bytes).into_owned());
            }
            15 => entry = Some(words[at + 2]),
            54 => functions.push(at),
            _ => {}
        }
        at += count;
    }
    let all = noinline.iter().any(|n| n == "all");
    let mut marked = 0;
    for at in functions {
        let id = words[at + 2];
        if Some(id) == entry {
            continue;
        }
        let named = names.get(&id).is_some_and(|n| noinline.contains(n));
        if all || named {
            words[at + 3] |= DONT_INLINE;
            marked += 1;
        }
    }
    Ok((words, marked))
}

/// `spv=proc`: see `one`.
fn through_processor(spec: &Spec, inlined: usize, functions: usize) -> Result<String, String> {
    use rawtonic_gpu::gpu_processing::{GpuContext, GpuProcessor};
    let context = GpuContext::headless()?;
    let adapter = format!(
        "{:?}",
        context
            .device
            .features()
            .contains(wgpu::Features::PASSTHROUGH_SHADERS)
    );
    let processor = GpuProcessor::for_exports(context, 1808, 1356)?;
    let private_before = memory::private_bytes();
    let sampler = memory::Sampler::start();
    let started = Instant::now();
    processor.compile_display_variant(spec.features)?;
    let pipeline_ms = started.elapsed().as_secs_f64() * 1000.0;
    let peak_sampled = sampler.stop();
    let mb = |bytes: Option<usize>| bytes.map_or("null".to_string(), |b| (b >> 20).to_string());
    let peak = memory::peak_private_bytes().map(|p| p.max(peak_sampled));
    Ok(format!(
        "{{\"pipeline_ms\":{pipeline_ms:.0},\"peak_mb\":{},\"before_mb\":{},\"inlined_exprs\":{inlined},\
\"functions\":{functions},\"path\":\"processor\",\"device_passthrough\":{adapter}}}",
        mb(peak),
        mb(private_before),
    ))
}

/// The app's log lines (`rawtonic*` targets) on stderr, for `spv=proc`.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info && metadata.target().starts_with("rawtonic")
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }
    fn flush(&self) {}
}

fn knob_set(knobs: &[String], name: &str) -> bool {
    knobs.iter().any(|k| k == name)
}

/// A pipeline layout with one entry per global the module binds (group 0).
fn layout_for(
    device: &wgpu::Device,
    module: &naga::Module,
    ranks: &[u32],
) -> Result<wgpu::PipelineLayout, String> {
    use naga::{AddressSpace, ImageClass, ImageDimension, TypeInner};
    let mut entries = vec![];
    for (_, global) in module.global_variables.iter() {
        let Some(binding) = &global.binding else {
            continue;
        };
        if binding.group != 0 {
            return Err("bindings outside group 0".into());
        }
        let ty = match (global.space, &module.types[global.ty].inner) {
            (AddressSpace::Uniform, _) => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            (AddressSpace::Storage { access }, _) => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage {
                    read_only: !access.contains(naga::StorageAccess::STORE),
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            (_, TypeInner::Sampler { comparison }) => wgpu::BindingType::Sampler(if *comparison {
                wgpu::SamplerBindingType::Comparison
            } else {
                wgpu::SamplerBindingType::Filtering
            }),
            (
                _,
                TypeInner::Image {
                    dim,
                    arrayed,
                    class,
                },
            ) => {
                let view_dimension = match (dim, arrayed) {
                    (ImageDimension::D1, _) => wgpu::TextureViewDimension::D1,
                    (ImageDimension::D2, false) => wgpu::TextureViewDimension::D2,
                    (ImageDimension::D2, true) => wgpu::TextureViewDimension::D2Array,
                    (ImageDimension::D3, _) => wgpu::TextureViewDimension::D3,
                    (ImageDimension::Cube, false) => wgpu::TextureViewDimension::Cube,
                    (ImageDimension::Cube, true) => wgpu::TextureViewDimension::CubeArray,
                };
                match class {
                    ImageClass::Storage { format, access } => wgpu::BindingType::StorageTexture {
                        access: if access.contains(naga::StorageAccess::LOAD) {
                            wgpu::StorageTextureAccess::ReadWrite
                        } else {
                            wgpu::StorageTextureAccess::WriteOnly
                        },
                        format: match format {
                            naga::StorageFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
                            naga::StorageFormat::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
                            naga::StorageFormat::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
                            naga::StorageFormat::R32Float => wgpu::TextureFormat::R32Float,
                            naga::StorageFormat::R32Uint => wgpu::TextureFormat::R32Uint,
                            other => return Err(format!("storage format {other:?}")),
                        },
                        view_dimension,
                    },
                    _ => wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension,
                        multisampled: false,
                    },
                }
            }
            other => return Err(format!("binding type {:?}", other.0)),
        };
        entries.push(wgpu::BindGroupLayoutEntry {
            // `ranks`: the layout as wgpu-hal numbers Vulkan bindings (each
            // WGSL binding's rank among these), for SPIR-V written so.
            binding: if ranks.is_empty() {
                binding.binding
            } else {
                let mut sorted = ranks.to_vec();
                sorted.sort_unstable();
                sorted
                    .iter()
                    .position(|b| *b == binding.binding)
                    .ok_or_else(|| format!("binding {} not in ranks", binding.binding))?
                    as u32
            },
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        });
    }
    let group = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("probe"),
        entries: &entries,
    });
    Ok(
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("probe"),
            bind_group_layouts: &[Some(&group)],
            immediate_size: 0,
        }),
    )
}

/// Replaces `name`'s body with a return of its first argument when that has
/// the result's type, else of a zero value.
fn stub(module: &mut naga::Module, name: &str) -> Result<(), String> {
    let (_, function) = module
        .functions
        .iter_mut()
        .find(|(_, f)| f.name.as_deref() == Some(name))
        .ok_or_else(|| format!("no function {name}"))?;
    let span = naga::Span::UNDEFINED;
    function.expressions = naga::Arena::new();
    function.named_expressions.clear();
    function.local_variables = naga::Arena::new();
    let mut body = naga::Block::new();
    let value = match &function.result {
        None => None,
        Some(result) => {
            let same = function
                .arguments
                .first()
                .is_some_and(|a| a.ty == result.ty);
            Some(if same {
                function
                    .expressions
                    .append(naga::Expression::FunctionArgument(0), span)
            } else {
                function
                    .expressions
                    .append(naga::Expression::ZeroValue(result.ty), span)
            })
        }
    };
    body.push(naga::Statement::Return { value }, span);
    function.body = body;
    Ok(())
}

/// A local variable initialised with a per-run value at the top of `main`:
/// different SPIR-V each run, so no driver cache can answer the compile.
fn add_nonce(module: &mut naga::Module) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
        .unwrap_or(0)
        ^ std::process::id();
    let ty = module.types.insert(
        naga::Type {
            name: None,
            inner: naga::TypeInner::Scalar(naga::Scalar::U32),
        },
        naga::Span::UNDEFINED,
    );
    let function = &mut module.entry_points[0].function;
    let init = function.expressions.append(
        naga::Expression::Literal(naga::Literal::U32(nonce)),
        naga::Span::UNDEFINED,
    );
    function.local_variables.append(
        naga::LocalVariable {
            name: Some("probe_nonce".into()),
            ty,
            init: Some(init),
        },
        naga::Span::UNDEFINED,
    );
}

fn report_inlining(module: &naga::Module) {
    fn calls(block: &naga::Block, out: &mut Vec<naga::Handle<naga::Function>>) {
        use naga::Statement as S;
        for statement in block.iter() {
            match statement {
                S::Call { function, .. } => out.push(*function),
                S::Block(b) => calls(b, out),
                S::If { accept, reject, .. } => {
                    calls(accept, out);
                    calls(reject, out);
                }
                S::Loop {
                    body, continuing, ..
                } => {
                    calls(body, out);
                    calls(continuing, out);
                }
                S::Switch { cases, .. } => cases.iter().for_each(|c| calls(&c.body, out)),
                _ => {}
            }
        }
    }
    let mut size = std::collections::HashMap::new();
    for (handle, function) in module.functions.iter() {
        let mut callees = vec![];
        calls(&function.body, &mut callees);
        let total = function.expressions.len() + callees.iter().map(|c| size[c]).sum::<usize>();
        size.insert(handle, total);
    }
    let mut instances = std::collections::HashMap::<naga::Handle<naga::Function>, usize>::new();
    fn count(
        module: &naga::Module,
        block: &naga::Block,
        n: &mut std::collections::HashMap<naga::Handle<naga::Function>, usize>,
        calls: &dyn Fn(&naga::Block, &mut Vec<naga::Handle<naga::Function>>),
    ) {
        let mut callees = vec![];
        calls(block, &mut callees);
        for c in callees {
            *n.entry(c).or_default() += 1;
            count(module, &module.functions[c].body, n, calls);
        }
    }
    count(
        module,
        &module.entry_points[0].function.body,
        &mut instances,
        &calls,
    );
    let name =
        |h: naga::Handle<naga::Function>| module.functions[h].name.clone().unwrap_or_default();
    let mut direct = vec![];
    calls(&module.entry_points[0].function.body, &mut direct);
    let mut rows: Vec<_> = direct.iter().map(|c| (size[c], name(*c))).collect();
    rows.sort_by(|a, b| b.cmp(a));
    eprintln!("main's direct calls (inlined size):");
    for (s, n) in rows.iter().take(15) {
        eprintln!("  {s:>7} {n}");
    }
    let mut rows: Vec<_> = instances
        .iter()
        .map(|(h, k)| (module.functions[*h].expressions.len() * k, *k, name(*h)))
        .collect();
    rows.sort_by(|a, b| b.cmp(a));
    eprintln!("own size x instances:");
    for (t, k, n) in rows.iter().take(15) {
        eprintln!("  {t:>7} x{k:<4} {n}");
    }
}

/// Expressions of `main` with every call inlined (what a compiler that
/// inlines everything starts from).
fn inlined_size(module: &naga::Module) -> usize {
    fn calls(block: &naga::Block, out: &mut Vec<naga::Handle<naga::Function>>) {
        use naga::Statement as S;
        for statement in block.iter() {
            match statement {
                S::Call { function, .. } => out.push(*function),
                S::Block(b) => calls(b, out),
                S::If { accept, reject, .. } => {
                    calls(accept, out);
                    calls(reject, out);
                }
                S::Loop {
                    body, continuing, ..
                } => {
                    calls(body, out);
                    calls(continuing, out);
                }
                S::Switch { cases, .. } => cases.iter().for_each(|c| calls(&c.body, out)),
                _ => {}
            }
        }
    }
    let mut sizes = std::collections::HashMap::new();
    for (handle, function) in module.functions.iter() {
        let mut callees = vec![];
        calls(&function.body, &mut callees);
        let size = function.expressions.len() + callees.iter().map(|c| sizes[c]).sum::<usize>();
        sizes.insert(handle, size);
    }
    let main = &module.entry_points[0].function;
    let mut callees = vec![];
    calls(&main.body, &mut callees);
    main.expressions.len() + callees.iter().map(|c| sizes[c]).sum::<usize>()
}

#[cfg(windows)]
mod memory {
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
    }
    fn counters() -> Counters {
        let mut c = Counters {
            cb: std::mem::size_of::<Counters>() as u32,
            ..Default::default()
        };
        // SAFETY: a valid out-pointer of the size given.
        unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
        c
    }
    pub fn private_bytes() -> Option<usize> {
        Some(counters().private_usage)
    }
    /// Peak commit charge of the process so far (Task Manager's peak of
    /// "Commit size", the figure the G76 measurements used).
    pub fn peak_private_bytes() -> Option<usize> {
        Some(counters().peak_pagefile_usage)
    }
    pub use super::sampler::Sampler;
}

#[cfg(not(windows))]
mod memory {
    /// Not measured here: reported as `null`, never as 0.
    pub fn private_bytes() -> Option<usize> {
        None
    }
    pub fn peak_private_bytes() -> Option<usize> {
        None
    }
    pub use super::sampler::Sampler;
}

mod sampler {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Samples private bytes every 50 ms (a cross-check of the OS peak).
    pub struct Sampler {
        stop: Arc<AtomicBool>,
        peak: Arc<AtomicUsize>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Sampler {
        pub fn start() -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let peak = Arc::new(AtomicUsize::new(0));
            let thread = {
                let (stop, peak) = (Arc::clone(&stop), Arc::clone(&peak));
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        peak.fetch_max(
                            super::memory::private_bytes().unwrap_or(0),
                            Ordering::Relaxed,
                        );
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                })
            };
            Self {
                stop,
                peak,
                thread: Some(thread),
            }
        }
        pub fn stop(mut self) -> usize {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            self.peak.load(Ordering::Relaxed)
        }
    }
}
