//! Measures what one compute-pipeline compile of the image-processing shader
//! costs the GPU driver: wall time and the process's peak private memory
//! (Windows: commit charge; elsewhere: peak resident size). A tool for
//! finding where a driver's compiler spends its time (G76/WCC: the Radeon
//! 680M's Windows Vulkan driver took ~70 s and ~7 GB for one display
//! variant), not part of the app.
//!
//! cargo run --release -p rawtonic-gpu --example compile_probe -- SPECS_FILE [OUT.jsonl]
//!
//! Each line of SPECS_FILE is one candidate, compiled in its own child
//! process (clean peak memory, no in-process reuse):
//!
//! `NAME features=N [dce] [unchecked] [stub=f1,f2] [src=FILE] [set=K:V,K:V] [export]`
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
    };
    for word in line.split_whitespace().skip(1) {
        match word.split_once('=') {
            Some(("features", v)) => spec.features = v.parse().map_err(|_| "features")?,
            Some(("stub", v)) => spec.stubs = v.split(',').map(String::from).collect(),
            Some(("src", v)) => spec.src = Some(v.to_string()),
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
    let folded = if spec.dce { fold(&mut module) } else { 0 };
    for name in &spec.stubs {
        stub(&mut module, name)?;
    }
    add_nonce(&mut module);
    naga::compact::compact(&mut module, naga::compact::KeepUnused::No);
    validate(&module)?;
    let inlined = inlined_size(&module);
    let functions = module.functions.len();

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
    let (device, _queue) = pollster::block_on(adapter.request_device(
        &rawtonic_gpu::gpu_processing::processing_device_descriptor(&adapter),
    ))
    .map_err(|e| e.to_string())?;
    let shader_descriptor = wgpu::ShaderModuleDescriptor {
        label: Some("probe"),
        source: wgpu::ShaderSource::Naga(std::borrow::Cow::Owned(module)),
    };
    let private_before = memory::private_bytes();
    let sampler = memory::Sampler::start();
    let started = Instant::now();
    let shader = if spec.unchecked {
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
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let pipeline_ms = pipeline_started.elapsed().as_secs_f64() * 1000.0;
    let peak_sampled = sampler.stop();
    let peak = memory::peak_private_bytes().max(peak_sampled);
    Ok(format!(
        "{{\"pipeline_ms\":{pipeline_ms:.0},\"shader_ms\":{shader_ms:.0},\"peak_mb\":{},\"before_mb\":{},\
\"inlined_exprs\":{inlined},\"functions\":{functions},\"folded_ifs\":{folded},\"adapter\":{},\"driver\":{}}}",
        peak >> 20,
        private_before >> 20,
        json_str(&adapter_info.name),
        json_str(&format!(
            "{:?} {} {}",
            adapter_info.backend, adapter_info.driver, adapter_info.driver_info
        )),
    ))
}

/// Folds every `if` whose condition the (now constant) overrides decide.
fn fold(module: &mut naga::Module) -> usize {
    let snapshot = module.clone();
    let mut count = 0;
    let functions = module
        .functions
        .iter_mut()
        .map(|(_, f)| f)
        .chain(module.entry_points.iter_mut().map(|e| &mut e.function));
    for function in functions {
        fold_block(
            &snapshot,
            &function.expressions,
            &mut function.body,
            &mut count,
        );
        // Names keep expressions alive through compaction (a `let` inside a
        // removed branch would keep its call); they only matter to debuggers.
        function.named_expressions.clear();
    }
    count
}

fn literal(
    module: &naga::Module,
    arena: &naga::Arena<naga::Expression>,
    handle: naga::Handle<naga::Expression>,
) -> Option<naga::Literal> {
    match arena[handle] {
        naga::Expression::Literal(l) => Some(l),
        naga::Expression::Constant(c) => {
            literal(module, &module.global_expressions, module.constants[c].init)
        }
        _ => None,
    }
}

fn decide(
    module: &naga::Module,
    arena: &naga::Arena<naga::Expression>,
    handle: naga::Handle<naga::Expression>,
) -> Option<bool> {
    use naga::{BinaryOperator as B, Expression as E};
    if let Some(naga::Literal::Bool(b)) = literal(module, arena, handle) {
        return Some(b);
    }
    match arena[handle] {
        E::Unary {
            op: naga::UnaryOperator::LogicalNot,
            expr,
        } => decide(module, arena, expr).map(|b| !b),
        E::Binary { op, left, right } => match op {
            B::LogicalAnd => match (decide(module, arena, left), decide(module, arena, right)) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            B::LogicalOr => match (decide(module, arena, left), decide(module, arena, right)) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
            B::Equal | B::NotEqual => {
                let equal = literal(module, arena, left)? == literal(module, arena, right)?;
                Some(equal == (op == B::Equal))
            }
            _ => None,
        },
        _ => None,
    }
}

fn fold_block(
    module: &naga::Module,
    arena: &naga::Arena<naga::Expression>,
    block: &mut naga::Block,
    count: &mut usize,
) {
    use naga::Statement as S;
    for (statement, span) in std::mem::take(block).span_into_iter() {
        let statement = match statement {
            S::If {
                condition,
                mut accept,
                mut reject,
            } => match decide(module, arena, condition) {
                Some(taken) => {
                    *count += 1;
                    let mut kept = if taken { accept } else { reject };
                    fold_block(module, arena, &mut kept, count);
                    S::Block(kept)
                }
                None => {
                    fold_block(module, arena, &mut accept, count);
                    fold_block(module, arena, &mut reject, count);
                    S::If {
                        condition,
                        accept,
                        reject,
                    }
                }
            },
            S::Block(mut inner) => {
                fold_block(module, arena, &mut inner, count);
                S::Block(inner)
            }
            S::Loop {
                mut body,
                mut continuing,
                break_if,
            } => {
                fold_block(module, arena, &mut body, count);
                fold_block(module, arena, &mut continuing, count);
                S::Loop {
                    body,
                    continuing,
                    break_if,
                }
            }
            S::Switch {
                selector,
                mut cases,
            } => {
                for case in &mut cases {
                    fold_block(module, arena, &mut case.body, count);
                }
                S::Switch { selector, cases }
            }
            other => other,
        };
        block.push(statement, span);
    }
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
                let zero = function
                    .expressions
                    .append(naga::Expression::ZeroValue(result.ty), span);
                body.push(
                    naga::Statement::Emit(function.expressions.range_from(0)),
                    span,
                );
                zero
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
    pub fn private_bytes() -> usize {
        counters().private_usage
    }
    /// Peak commit charge of the process so far (Task Manager's peak of
    /// "Commit size", the figure the G76 measurements used).
    pub fn peak_private_bytes() -> usize {
        counters().peak_pagefile_usage
    }
    pub use super::sampler::Sampler;
}

#[cfg(not(windows))]
mod memory {
    pub fn private_bytes() -> usize {
        0
    }
    pub fn peak_private_bytes() -> usize {
        0
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
                        peak.fetch_max(super::memory::private_bytes(), Ordering::Relaxed);
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
