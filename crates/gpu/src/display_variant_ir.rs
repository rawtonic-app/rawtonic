//! Display variants as the driver should see them: the image-processing
//! shader with its pipeline constants applied and every branch those
//! constants decide removed before it reaches the driver.
//!
//! naga turns override constants into plain constants but keeps the code
//! behind `if (false)`, so a driver receives the whole shader for every
//! variant. AMD's Windows Vulkan compiler inlines every call before it drops
//! that code: the RAW default display variant (4113) took 97 s and 7.8 GB of
//! private memory on a Radeon 680M as compiled before, 63 s / 4.1 GB with
//! the decided branches removed, and 8.1 s / 0.7 GB with them removed and
//! without the glow, halation and grain code it never runs (`ENABLE_EFFECTS`;
//! G76/WCC round 1, `examples/compile_probe.rs`).
//!
//! Only display variants take this path; exports and the general pipelines
//! compile the shader as before, so their code is unchanged.
//!
//! On that driver the folded variants are also handed over as SPIR-V written
//! here (`variant_spirv`, wgpu's SPIR-V passthrough) instead of through
//! wgpu's own Vulkan path: the tone variant 4145 compiled in 6.3–7.9 s at
//! 2.5 GB that way, whatever writer options wgpu-hal would set, and in
//! 132–135 s at 11.6 GB through wgpu, reproducibly (G76/WCC2 rounds 3–5; the
//! cause inside wgpu's path is not found).

use std::sync::{Arc, OnceLock};

use naga::{Arena, Block, Expression, Handle, Literal, Module, Statement};

/// The parsed shader, shared by the display variants of a processor.
pub(crate) struct DisplayShader {
    source: Arc<str>,
    parsed: OnceLock<Result<(Module, naga::valid::ModuleInfo), String>>,
    /// Whether variants go to the driver as our own SPIR-V
    /// (`variant_spirv`): where `spirv_passthrough` says so.
    passthrough: std::sync::atomic::AtomicBool,
    /// The bindings of the pipeline layout the variants are made for
    /// (group 0), in any order.
    bindings: Vec<u32>,
}

/// Whether display variants of new processors are passed through as SPIR-V
/// (`set_spirv_passthrough`, from the device start).
static SPIRV_PASSTHROUGH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Sets whether display variants are passed through as SPIR-V on devices
/// that allow it; `RAWTONIC_GPU_SPIRV_PASSTHROUGH=0`/`1` overrides.
pub(crate) fn set_spirv_passthrough(on: bool) {
    let on = match std::env::var("RAWTONIC_GPU_SPIRV_PASSTHROUGH").as_deref() {
        Ok("0") => false,
        Ok("1") => true,
        _ => on,
    };
    if on {
        log::info!("GPU display variants are passed to the driver as SPIR-V");
    }
    SPIRV_PASSTHROUGH.store(on, std::sync::atomic::Ordering::Relaxed);
}

impl DisplayShader {
    pub(crate) fn new(source: &str, bindings: Vec<u32>) -> Arc<Self> {
        Arc::new(Self {
            source: source.into(),
            bindings,
            parsed: OnceLock::new(),
            passthrough: std::sync::atomic::AtomicBool::new(
                SPIRV_PASSTHROUGH.load(std::sync::atomic::Ordering::Relaxed),
            ),
        })
    }

    /// Whether this shader's variants go to `device` as our own SPIR-V.
    pub(crate) fn passes_through(&self, device: &wgpu::Device) -> bool {
        if !self.passthrough.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let available = device
            .features()
            .contains(wgpu::Features::PASSTHROUGH_SHADERS);
        if !available {
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| {
                log::warn!(
                    "GPU display variants: SPIR-V passthrough wanted, but the device has no \
                     PASSTHROUGH_SHADERS feature; compiling through wgpu"
                );
            });
        }
        available
    }

    #[cfg(test)]
    pub(crate) fn bindings_for_test(&self) -> &[u32] {
        &self.bindings
    }

    #[cfg(test)]
    pub(crate) fn set_passthrough(&self, on: bool) {
        self.passthrough
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// `variant_module` as Vulkan SPIR-V, written as wgpu's Vulkan backend
    /// writes it for a Vulkan 1.3 device without robust buffer or image
    /// access: with its binding numbers (wgpu-hal numbers a bind group's
    /// Vulkan bindings 0, 1, 2 … in the order of the WGSL bindings; with the
    /// WGSL numbers the first frame read the wrong resources and lost the
    /// device, WGT5/WGT6), SPIR-V 1.6, indices, buffers and image loads bounds-restricted,
    /// loops bounded, integer division checked. Image loads must stay
    /// restricted: unchecked, the first frame on a Radeon 680M lost the device
    /// (WGT5; the shader reads texels outside a tile's image at its edges).
    /// spirv-val rejects one constant naga writes for that restriction
    /// (OpConstantComposite of int into a uvec2); wgpu sends the same, and the
    /// driver compiles it (WCC2 round 4).
    pub(crate) fn variant_spirv(&self, constants: &[(&str, f64)]) -> Result<Vec<u32>, String> {
        use naga::back::spv;
        use naga::proc::BoundsCheckPolicy as P;
        let module = self.variant_module(constants)?;
        let info = validate(&module)?;
        let mut sorted = self.bindings.clone();
        sorted.sort_unstable();
        sorted.dedup();
        let binding_map = sorted
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
        let options = spv::Options {
            lang_version: (1, 6),
            binding_map,
            // A binding the layout lacks is an error, never a guess.
            fake_missing_bindings: false,
            flags: spv::WriterFlags::LABEL_VARYINGS | spv::WriterFlags::FORCE_POINT_SIZE,
            bounds_check_policies: naga::proc::BoundsCheckPolicies {
                index: P::Restrict,
                buffer: P::Restrict,
                image_load: P::Restrict,
                binding_array: P::Unchecked,
            },
            ..Default::default()
        };
        let pipeline = spv::PipelineOptions {
            entry_point: "main".into(),
            shader_stage: naga::ShaderStage::Compute,
        };
        spv::write_vec(&module, &info, &options, Some(&pipeline)).map_err(|e| e.to_string())
    }

    /// The shader module of a variant: `constants` applied and the code they
    /// rule out removed. Parsed once, on the first variant's compile thread.
    pub(crate) fn variant_module(&self, constants: &[(&str, f64)]) -> Result<Module, String> {
        let (module, info) = self
            .parsed
            .get_or_init(|| {
                let module = naga::front::wgsl::parse_str(&self.source)
                    .map_err(|e| e.emit_to_string(&self.source))?;
                let info = validate(&module)?;
                Ok((module, info))
            })
            .as_ref()
            .map_err(Clone::clone)?;
        let constants: naga::back::PipelineConstants = constants
            .iter()
            .map(|(name, value)| ((*name).to_string(), *value))
            .collect();
        let (module, _) = naga::back::pipeline_constants::process_overrides(
            module,
            info,
            Some((naga::ShaderStage::Compute, "main")),
            &constants,
        )
        .map_err(|e| e.to_string())?;
        let mut module = module.into_owned();
        fold(&mut module);
        naga::compact::compact(&mut module, naga::compact::KeepUnused::No);
        validate(&module)?;
        Ok(module)
    }
}

fn validate(module: &Module) -> Result<naga::valid::ModuleInfo, String> {
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| format!("{e:?}"))
}

/// Replaces every `if` whose condition the constants decide with the branch
/// taken. Returns how many it replaced.
pub(crate) fn fold(module: &mut Module) -> usize {
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
        // A name keeps its expression alive through compaction: drop the
        // names of expressions only the removed code computed (a `let` of a
        // removed call would keep that function), keep every other.
        let mut live = HandleSet::default();
        live_expressions(&function.body, &mut live);
        let expressions = &function.expressions;
        function
            .named_expressions
            .retain(|handle, _| expressions[*handle].needs_pre_emit() || live.contains(*handle));
    }
    count
}

#[derive(Default)]
struct HandleSet(std::collections::HashSet<Handle<Expression>>);

impl HandleSet {
    fn contains(&self, handle: Handle<Expression>) -> bool {
        self.0.contains(&handle)
    }
}

/// The expressions `block` emits or whose results its statements produce.
fn live_expressions(block: &Block, live: &mut HandleSet) {
    for statement in block.iter() {
        match statement {
            Statement::Emit(range) => live.0.extend(range.clone()),
            Statement::Block(inner) => live_expressions(inner, live),
            Statement::If { accept, reject, .. } => {
                live_expressions(accept, live);
                live_expressions(reject, live);
            }
            Statement::Loop {
                body, continuing, ..
            } => {
                live_expressions(body, live);
                live_expressions(continuing, live);
            }
            Statement::Switch { cases, .. } => {
                for case in cases {
                    live_expressions(&case.body, live);
                }
            }
            Statement::Call {
                result: Some(result),
                ..
            }
            | Statement::Atomic {
                result: Some(result),
                ..
            }
            | Statement::WorkGroupUniformLoad { result, .. } => {
                live.0.insert(*result);
            }
            _ => {}
        }
    }
}

fn literal(
    module: &Module,
    arena: &Arena<Expression>,
    handle: Handle<Expression>,
) -> Option<Literal> {
    match arena[handle] {
        Expression::Literal(l) => Some(l),
        Expression::Constant(c) => {
            literal(module, &module.global_expressions, module.constants[c].init)
        }
        _ => None,
    }
}

/// The value of a condition when the constants decide it.
fn decide(module: &Module, arena: &Arena<Expression>, handle: Handle<Expression>) -> Option<bool> {
    use naga::BinaryOperator as B;
    if let Some(Literal::Bool(b)) = literal(module, arena, handle) {
        return Some(b);
    }
    match arena[handle] {
        Expression::Unary {
            op: naga::UnaryOperator::LogicalNot,
            expr,
        } => decide(module, arena, expr).map(|b| !b),
        // Conditions are pure expressions: a decided side decides `&&`/`||`
        // whatever the other one is.
        Expression::Binary { op, left, right } => match op {
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

fn fold_block(module: &Module, arena: &Arena<Expression>, block: &mut Block, count: &mut usize) {
    for (statement, span) in std::mem::take(block).span_into_iter() {
        let statement = match statement {
            Statement::If {
                condition,
                mut accept,
                mut reject,
            } => match decide(module, arena, condition) {
                Some(taken) => {
                    *count += 1;
                    let mut kept = if taken { accept } else { reject };
                    fold_block(module, arena, &mut kept, count);
                    Statement::Block(kept)
                }
                None => {
                    fold_block(module, arena, &mut accept, count);
                    fold_block(module, arena, &mut reject, count);
                    Statement::If {
                        condition,
                        accept,
                        reject,
                    }
                }
            },
            Statement::Block(mut inner) => {
                fold_block(module, arena, &mut inner, count);
                Statement::Block(inner)
            }
            Statement::Loop {
                mut body,
                mut continuing,
                break_if,
            } => {
                fold_block(module, arena, &mut body, count);
                fold_block(module, arena, &mut continuing, count);
                Statement::Loop {
                    body,
                    continuing,
                    break_if,
                }
            }
            Statement::Switch {
                selector,
                mut cases,
            } => {
                for case in &mut cases {
                    fold_block(module, arena, &mut case.body, count);
                }
                Statement::Switch { selector, cases }
            }
            other => other,
        };
        block.push(statement, span);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHADER: &str = include_str!("shaders/shader.wgsl");

    fn has_function(module: &Module, name: &str) -> bool {
        module
            .functions
            .iter()
            .any(|(_, f)| f.name.as_deref() == Some(name))
    }

    #[test]
    fn decided_branches_and_the_functions_only_they_call_are_removed() {
        let shader = DisplayShader::new(SHADER, (0..=28).collect());
        let without = shader
            .variant_module(&[("ENABLE_EFFECTS", 0.0), ("ENABLE_RELIGHT", 0.0)])
            .unwrap();
        assert!(!has_function(&without, "apply_glow_bloom"));
        assert!(!has_function(&without, "apply_halation"));
        assert!(!has_function(&without, "apply_film_grain"));
        assert!(!has_function(&without, "apply_relight"));
        let with = shader.variant_module(&[("ENABLE_EFFECTS", 1.0)]).unwrap();
        assert!(has_function(&with, "apply_glow_bloom"));
        assert!(has_function(&with, "apply_film_grain"));
        assert!(with.overrides.is_empty() && without.overrides.is_empty());
    }

    #[test]
    fn variants_are_written_as_spir_v_without_the_removed_code() {
        let shader = DisplayShader::new(SHADER, (0..=28).collect());
        let words = shader
            .variant_spirv(&[("ENABLE_EFFECTS", 0.0), ("ENABLE_TONE", 1.0)])
            .unwrap();
        assert_eq!(words[0], 0x0723_0203, "SPIR-V magic");
        assert_eq!(words[1], 0x0001_0600, "SPIR-V 1.6");
        let with_effects = shader.variant_spirv(&[("ENABLE_EFFECTS", 1.0)]).unwrap();
        assert!(with_effects.len() > words.len());
    }

    #[test]
    fn spir_v_bindings_are_numbered_as_wgpus_vulkan_layouts() {
        let src = "@group(0) @binding(9) var<storage, read_write> c: array<u32>;
            @group(0) @binding(0) var<storage, read_write> a: array<u32>;
            @group(0) @binding(5) var<storage, read_write> b: array<u32>;
            @compute @workgroup_size(8, 8, 1) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                a[id.x] = b[id.x] + c[id.x];
            }";
        let words = DisplayShader::new(src, vec![9, 0, 5, 7])
            .variant_spirv(&[])
            .unwrap();
        // OpDecorate (71) target Binding (33) n, in instruction order.
        let mut bindings = vec![];
        let mut at = 5;
        while at < words.len() {
            let count = (words[at] >> 16) as usize;
            if words[at] & 0xffff == 71 && words[at + 2] == 33 {
                bindings.push(words[at + 3]);
            }
            at += count;
        }
        bindings.sort_unstable();
        // Layout bindings 0, 5, 7, 9 are Vulkan bindings 0, 1, 2, 3.
        assert_eq!(bindings, vec![0, 1, 3]);
        let missing = DisplayShader::new(src, vec![0, 5]).variant_spirv(&[]);
        assert!(missing.is_err());
    }

    #[test]
    fn runtime_conditions_stay() {
        let src = "override ON: bool = true;
            @group(0) @binding(0) var<storage, read_write> out: array<u32>;
            fn f(x: u32) -> u32 { return x * 3u; }
            @compute @workgroup_size(1) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
                if (ON && id.x > 2u) { out[0] = f(id.x); }
                if (!ON || id.x == 7u) { out[1] = 2u; }
                if (ON) { let kept = f(id.y); out[2] = kept; } else { let gone = f(id.z); out[3] = gone; }
            }";
        let module = DisplayShader::new(src, vec![0])
            .variant_module(&[("ON", 1.0)])
            .unwrap();
        let body = &module.entry_points[0].function.body;
        let ifs = body
            .iter()
            .filter(|s| matches!(s, Statement::If { .. }))
            .count();
        // The first two depend on the invocation; the third is decided.
        assert_eq!(ifs, 2);
        let names: Vec<_> = module.entry_points[0]
            .function
            .named_expressions
            .values()
            .cloned()
            .collect();
        // The removed branch's `let` loses its name; the rest keep theirs.
        assert!(names.contains(&"kept".to_string()) && !names.contains(&"gone".to_string()));
    }
}
