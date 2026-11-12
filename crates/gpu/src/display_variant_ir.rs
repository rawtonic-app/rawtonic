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

use std::sync::{Arc, OnceLock};

use naga::{Arena, Block, Expression, Handle, Literal, Module, Statement};

/// The parsed shader, shared by the display variants of a processor.
pub(crate) struct DisplayShader {
    source: Arc<str>,
    parsed: OnceLock<Result<(Module, naga::valid::ModuleInfo), String>>,
}

/// The tone gains display variants compile instead of shader.wgsl's
/// (`shaders/display_tone_gains.wgsl`): one call site of the unordered gain
/// each, far less for a compiler that inlines every call (G76/WCC3). The
/// functions they replace are renamed and dropped as unused; exports never
/// see this source. A source without them is left as it is.
const DISPLAY_TONE_GAINS: &str = include_str!("shaders/display_tone_gains.wgsl");
const REPLACED_TONE_GAINS: [&str; 2] = ["camera_raw_shadows_blacks_gain", "cr_whites_gain"];

fn with_display_tone_gains(source: &str) -> String {
    let definition = |name: &str| format!("fn {name}(");
    if !REPLACED_TONE_GAINS
        .iter()
        .all(|name| source.matches(&definition(name)).count() == 1)
    {
        return source.into();
    }
    let mut source = REPLACED_TONE_GAINS
        .iter()
        .fold(source.to_string(), |source, name| {
            source.replace(&definition(name), &format!("fn {name}_shared("))
        });
    source.push('\n');
    source.push_str(DISPLAY_TONE_GAINS);
    source
}

impl DisplayShader {
    pub(crate) fn new(source: &str) -> Arc<Self> {
        Arc::new(Self {
            source: with_display_tone_gains(source).into(),
            parsed: OnceLock::new(),
        })
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
        // Repeated while it removes code: naga lowers `A && B` (B evaluated
        // at run time) to `var t; if (A) { t = B; } else { t = false; }`
        // and `if (t)`, so `if (SHADOWS_V13 && …)` is decided only once the
        // folded `if (A)` leaves `t` a single known value (`decided_locals`);
        // without that each tone variant kept the code of tone versions 12
        // and 13 (44k of 128k inlined expressions in variant 4145).
        loop {
            let known = decided_locals(&snapshot, function);
            let before = count;
            fold_block(
                &snapshot,
                &known,
                &function.expressions,
                &mut function.body,
                &mut count,
            );
            if count == before {
                break;
            }
        }
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
/// The short-circuit temporaries naga makes for `&&`/`||` (unnamed local
/// variables without an initializer) that every remaining store sets to the
/// same decided value: loading one gives that value.
fn decided_locals(
    module: &Module,
    function: &naga::Function,
) -> std::collections::HashMap<Handle<naga::LocalVariable>, bool> {
    fn stores(
        module: &Module,
        known: &std::collections::HashMap<Handle<naga::LocalVariable>, bool>,
        arena: &Arena<Expression>,
        block: &Block,
        out: &mut std::collections::HashMap<Handle<naga::LocalVariable>, Vec<Option<bool>>>,
    ) {
        for statement in block.iter() {
            match statement {
                Statement::Store { pointer, value } => {
                    if let Expression::LocalVariable(var) = arena[*pointer] {
                        out.entry(var)
                            .or_default()
                            .push(decide(module, known, arena, *value));
                    }
                }
                Statement::Block(inner) => stores(module, known, arena, inner, out),
                Statement::If { accept, reject, .. } => {
                    stores(module, known, arena, accept, out);
                    stores(module, known, arena, reject, out);
                }
                Statement::Loop {
                    body, continuing, ..
                } => {
                    stores(module, known, arena, body, out);
                    stores(module, known, arena, continuing, out);
                }
                Statement::Switch { cases, .. } => {
                    for case in cases {
                        stores(module, known, arena, &case.body, out);
                    }
                }
                _ => {}
            }
        }
    }
    let empty = std::collections::HashMap::new();
    let mut all = std::collections::HashMap::new();
    stores(
        module,
        &empty,
        &function.expressions,
        &function.body,
        &mut all,
    );
    all.into_iter()
        .filter(|(var, _)| {
            let local = &function.local_variables[*var];
            local.name.is_none() && local.init.is_none()
        })
        .filter_map(|(var, values)| {
            let first = (*values.first()?)?;
            values
                .iter()
                .all(|v| *v == Some(first))
                .then_some((var, first))
        })
        .collect()
}

fn decide(
    module: &Module,
    known: &std::collections::HashMap<Handle<naga::LocalVariable>, bool>,
    arena: &Arena<Expression>,
    handle: Handle<Expression>,
) -> Option<bool> {
    use naga::BinaryOperator as B;
    if let Some(Literal::Bool(b)) = literal(module, arena, handle) {
        return Some(b);
    }
    match arena[handle] {
        Expression::Load { pointer } => match arena[pointer] {
            Expression::LocalVariable(var) => known.get(&var).copied(),
            _ => None,
        },
        Expression::Unary {
            op: naga::UnaryOperator::LogicalNot,
            expr,
        } => decide(module, known, arena, expr).map(|b| !b),
        // Conditions are pure expressions: a decided side decides `&&`/`||`
        // whatever the other one is.
        Expression::Binary { op, left, right } => match op {
            B::LogicalAnd => match (
                decide(module, known, arena, left),
                decide(module, known, arena, right),
            ) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            B::LogicalOr => match (
                decide(module, known, arena, left),
                decide(module, known, arena, right),
            ) {
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
    module: &Module,
    known: &std::collections::HashMap<Handle<naga::LocalVariable>, bool>,
    arena: &Arena<Expression>,
    block: &mut Block,
    count: &mut usize,
) {
    for (statement, span) in std::mem::take(block).span_into_iter() {
        let statement = match statement {
            Statement::If {
                condition,
                mut accept,
                mut reject,
            } => match decide(module, known, arena, condition) {
                Some(taken) => {
                    *count += 1;
                    let mut kept = if taken { accept } else { reject };
                    fold_block(module, known, arena, &mut kept, count);
                    Statement::Block(kept)
                }
                None => {
                    fold_block(module, known, arena, &mut accept, count);
                    fold_block(module, known, arena, &mut reject, count);
                    Statement::If {
                        condition,
                        accept,
                        reject,
                    }
                }
            },
            Statement::Block(mut inner) => {
                fold_block(module, known, arena, &mut inner, count);
                Statement::Block(inner)
            }
            Statement::Loop {
                mut body,
                mut continuing,
                break_if,
            } => {
                fold_block(module, known, arena, &mut body, count);
                fold_block(module, known, arena, &mut continuing, count);
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
                    fold_block(module, known, arena, &mut case.body, count);
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
        let shader = DisplayShader::new(SHADER);
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
    fn display_variants_take_the_tone_gains_with_one_call_site() {
        let shader = DisplayShader::new(SHADER);
        let module = shader
            .variant_module(&[("ENABLE_EFFECTS", 0.0), ("ENABLE_RELIGHT", 0.0)])
            .unwrap();
        for name in REPLACED_TONE_GAINS {
            assert!(has_function(&module, name), "{name}");
            assert!(!has_function(&module, &format!("{name}_shared")), "{name}");
        }
        // Each calls its unordered gain from one place.
        for (gain, unordered) in [
            (
                "camera_raw_shadows_blacks_gain",
                "camera_raw_shadows_blacks_gain_unordered",
            ),
            ("cr_whites_gain", "cr_whites_gain_unordered"),
        ] {
            let (_, function) = module
                .functions
                .iter()
                .find(|(_, f)| f.name.as_deref() == Some(gain))
                .unwrap();
            let calls = function
                .expressions
                .iter()
                .filter(|(_, e)| matches!(e, naga::Expression::CallResult(_)))
                .count();
            let target = module
                .functions
                .iter()
                .find(|(_, f)| f.name.as_deref() == Some(unordered))
                .map(|(handle, _)| handle)
                .unwrap();
            let to_unordered = count_calls(&function.body, target);
            assert_eq!(to_unordered, 1, "{gain}: {calls} calls in all");
        }
        // A source without them stays as it is (exports never use this).
        assert_eq!(with_display_tone_gains("fn other() {}"), "fn other() {}");
    }

    fn count_calls(block: &naga::Block, target: naga::Handle<naga::Function>) -> usize {
        block
            .iter()
            .map(|statement| match statement {
                Statement::Call { function, .. } => usize::from(*function == target),
                Statement::Block(b) => count_calls(b, target),
                Statement::If { accept, reject, .. } => {
                    count_calls(accept, target) + count_calls(reject, target)
                }
                Statement::Loop {
                    body, continuing, ..
                } => count_calls(body, target) + count_calls(continuing, target),
                Statement::Switch { cases, .. } => {
                    cases.iter().map(|c| count_calls(&c.body, target)).sum()
                }
                _ => 0,
            })
            .sum()
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
        let module = DisplayShader::new(src)
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
