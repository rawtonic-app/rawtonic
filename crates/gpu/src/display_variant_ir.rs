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
//! Display variants also compile a reshaped source (`display_source`).
//! Under a compile limit export variants take only the fold, of shader.wgsl
//! as it is (`ExportShaders`; G76/WCC6: the Radeon 680M compiled a
//! many-feature export variant for 438 s and 52 GB before its device was
//! lost). Elsewhere exports and the general pipelines compile the shader as
//! before: the folded module, the same math, compiled to a few codes'
//! difference in some exports on NVIDIA and Metal.

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

/// The tone stage display variants compile (`shaders/display_tone_stage.wgsl`):
/// shader.wgsl's `apply_tonal_adjustments` and its copies for tone versions
/// 12 and 13 as one function, which every caller reaches (G76/WCC4).
const DISPLAY_TONE_STAGE: &str = include_str!("shaders/display_tone_stage.wgsl");
const REPLACED_TONE_STAGE: &str = "apply_tonal_adjustments";

/// The callers' branches to the tone stage's version-12 and -13 copies that
/// the display source drops (the merged stage takes their versions inside),
/// with how often each occurs in shader.wgsl: the photo's tone stage (13
/// and 12), glow's and halation's (12).
const DROPPED_TONE_ARMS: [(&str, &str, usize); 2] = [
    (
        "SHADOWS_V13 && adjustments.global.tone_version > 12.5",
        "apply_tonal_adjustments_v13(",
        1,
    ),
    (
        "WHITES_V12 && adjustments.global.tone_version > 11.5",
        "apply_tonal_adjustments_v12(",
        3,
    ),
];

/// Renames shader.wgsl's definitions of `names` to `NAME_shared` (dropped
/// as unused) and appends `replacement`; `None` when a definition is not
/// there exactly once.
fn replace_functions(source: &str, names: &[&str], replacement: &str) -> Option<String> {
    let definition = |name: &str| format!("fn {name}(");
    if !names
        .iter()
        .all(|name| source.matches(&definition(name)).count() == 1)
    {
        return None;
    }
    let mut source = names.iter().fold(source.to_string(), |source, name| {
        source.replace(&definition(name), &format!("fn {name}_shared("))
    });
    source.push('\n');
    source.push_str(replacement);
    Some(source)
}

/// Drops every `} else if (CONDITION) {` arm whose one statement calls
/// `callee`, keeping the `}` that closes the arm before it; `None` unless it
/// dropped `expected` arms.
fn drop_arms(source: &str, condition: &str, callee: &str, expected: usize) -> Option<String> {
    let opening = format!("}} else if ({condition}) {{\n");
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    let mut dropped = 0;
    while let Some(at) = rest.find(&opening) {
        let after = &rest[at + opening.len()..];
        let line_end = after.find('\n')?;
        if !after[..line_end].contains(callee) {
            return None;
        }
        // Up to the arm's closing `}`, which then closes the arm before.
        let next = &after[line_end + 1..];
        let close = next.len() - next.trim_start_matches(' ').len();
        if !next[close..].starts_with('}') {
            return None;
        }
        out.push_str(&rest[..at]);
        rest = &next[close..];
        dropped += 1;
    }
    out.push_str(rest);
    (dropped == expected).then_some(out)
}

/// The tone stage as one function: `None` when shader.wgsl no longer has
/// the shape this expects.
fn with_display_tone_stage(source: &str) -> Option<String> {
    let source = DROPPED_TONE_ARMS
        .iter()
        .try_fold(source.to_string(), |source, (condition, callee, n)| {
            drop_arms(&source, condition, callee, *n)
        })?;
    replace_functions(&source, &[REPLACED_TONE_STAGE], DISPLAY_TONE_STAGE)
}

/// Glow's and halation's call of the tone stage as shader.wgsl writes it
/// (twice): Whites only, every other tone argument zero.
const EFFECT_TONE_CALL: &str = "apply_tonal_adjustments(blurred_linear, blurred_color_input_space, 0.0, 0.0, is_raw, 0.0, 0.0, wh, 0.0, exp2(exp))";
/// What display variants call there instead (`effect_tone_stage`).
const EFFECT_TONE_DISPLAY_CALL: &str =
    "apply_effect_tone(blurred_linear, blurred_color_input_space, is_raw, wh, exp2(exp))";
/// The tone stage's parameters that `EFFECT_TONE_CALL` passes as zero.
const EFFECT_TONE_ZEROS: [&str; 5] = [
    "wide_luma_input_space",
    "whites_offset_log",
    "con",
    "sh",
    "bl_in",
];

/// The display tone stage (`DISPLAY_TONE_STAGE`) as glow and halation call
/// it: the arguments `EFFECT_TONE_CALL` passes as zero become constants, so
/// the fold drops the Contrast, Shadows and Blacks code it never runs.
/// Glow and halation each inlined the whole stage: with Glow 30 on the
/// WGT16 edit the display variant had 117.1k inlined expressions and its
/// compile peaked at 5.95 GB on a Radeon 680M; with only the glow code
/// (`GLOW_FEATURE`) 77.0k / 4.69 GB, and with this stage 67.9k against the
/// edit's 65.8k without Glow (G76/WCC8, probe round 16). The same
/// arithmetic for those arguments: every removed branch tests one of them
/// against zero. `None` when the stage has another shape.
fn effect_tone_stage(stage: &str) -> Option<String> {
    let header = "fn apply_tonal_adjustments(";
    let start = stage.find(header)?;
    let open = start + stage[start..].find(") -> vec3<f32> {")? + ") -> vec3<f32> {".len();
    let mut depth = 1usize;
    let mut end = None;
    for (i, c) in stage[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &stage[open..end?];
    let blacks = "let bl = select(bl_in, 0.0, blacks_v9_active(bl_in, is_raw));";
    if body.matches(blacks).count() != 1 {
        return None;
    }
    let mut out = String::from(
        "fn apply_effect_tone(\n    color: vec3<f32>,\n    blurred_color_input_space: vec3<f32>,\n    \
         is_raw: u32,\n    wh: f32,\n    exposure_gain: f32\n) -> vec3<f32> {\n",
    );
    for zero in EFFECT_TONE_ZEROS {
        out.push_str(&format!("    const {zero} = 0.0;\n"));
    }
    out.push_str(&body.replace(blacks, "const bl = 0.0;"));
    out.push('\n');
    Some(out)
}

/// Glow's and halation's tone stage calls `apply_effect_tone`
/// (`effect_tone_stage`); `None` when the source has another shape.
fn with_effect_tone_stage(source: &str) -> Option<String> {
    if source.matches(EFFECT_TONE_CALL).count() != 2 {
        return None;
    }
    let mut source = source.replace(EFFECT_TONE_CALL, EFFECT_TONE_DISPLAY_CALL);
    source.push('\n');
    source.push_str(&effect_tone_stage(DISPLAY_TONE_STAGE)?);
    Some(source)
}

/// The display source's switches for each effect (gpu_processing.rs
/// `display_effect_constants`): shader.wgsl's `ENABLE_EFFECTS` is one for
/// glow, halation and grain, which exports keep unchanged.
const EFFECT_OVERRIDES: &str = "override ENABLE_GLOW: bool = true;\n\
     override ENABLE_HALATION: bool = true;\n\
     override ENABLE_GRAIN: bool = true;\n";

/// shader.wgsl's glow and halation calls and its grain block (`if
/// (ENABLE_EFFECTS)`), each of which the display source puts behind its
/// own switch.
const GLOW_BLOCK: &str =
    "        if (t_glow > 0.0) {\n            processed_rgb = apply_glow_bloom(";
const HALATION_BLOCK: &str =
    "        if (t_halation > 0.0) {\n            processed_rgb = apply_halation(";
const GRAIN_BLOCK: &str =
    "    if (ENABLE_EFFECTS) {\n        if (adjustments.global.grain_amount > 0.0";

/// Glow, halation and grain behind a switch each (`EFFECT_OVERRIDES`), so a
/// display variant compiles only the effects its recipe has: with one
/// switch Glow 30 on the WGT16 edit compiled halation and four grain
/// versions too, 117.1k inlined expressions and 5.95 GB on a Radeon 680M
/// against 77.0k and 4.69 GB (G76/WCC8, probe round 16). Each switch only
/// adds a condition decided by a constant around code that ran under
/// `ENABLE_EFFECTS` already. `None` when shader.wgsl has another shape;
/// the switches are declared either way (`display_source`).
fn with_effects_apart(source: &str) -> Option<String> {
    let mut source = source.to_string();
    for (block, switch) in [
        (GLOW_BLOCK, "ENABLE_GLOW"),
        (HALATION_BLOCK, "ENABLE_HALATION"),
    ] {
        if source.matches(block).count() != 1 {
            return None;
        }
        // `if (t > 0.0) {` → `if (SWITCH && t > 0.0) {`: the fold decides
        // the condition when the switch is off.
        let condition = block.trim_start().split(" {").next()?;
        let inner = condition.strip_prefix("if (")?.strip_suffix(')')?;
        source = source.replace(
            block,
            &block.replacen(condition, &format!("if ({switch} && {inner})"), 1),
        );
    }
    if source.matches(GRAIN_BLOCK).count() != 1 {
        return None;
    }
    Some(source.replace(
        GRAIN_BLOCK,
        &GRAIN_BLOCK.replace("ENABLE_EFFECTS", "ENABLE_EFFECTS && ENABLE_GRAIN"),
    ))
}

fn with_display_tone_gains(source: &str) -> String {
    replace_functions(source, &REPLACED_TONE_GAINS, DISPLAY_TONE_GAINS)
        .unwrap_or_else(|| source.into())
}

/// The callers' arms to the tone stage's version-13 and version-12 copies
/// as shader.wgsl writes them, what an export variant with the arm's
/// feature bit writes instead, and how often each occurs (the photo's tone
/// stage, and glow's and halation's for version 12). The bit implies the
/// arm's tone version (`shadows_v13`/`whites_v12` of gpu_processing.rs set
/// it only from tone version 13/12 on), so the run-time test always passes
/// in such a variant; without it the fold could not decide the arm and the
/// variant kept every copy of the tone stage.
const DECIDED_TONE_ARMS: [(&str, &str, usize); 2] = [
    (
        "SHADOWS_V13 && adjustments.global.tone_version > 12.5",
        "SHADOWS_V13",
        1,
    ),
    (
        "WHITES_V12 && adjustments.global.tone_version > 11.5",
        "WHITES_V12",
        3,
    ),
];

/// The source an export variant compiles: `source` with the tone arms its
/// bits decide written as decided (`DECIDED_TONE_ARMS`; `[shadows_v13,
/// whites_v12]`). `None` when shader.wgsl no longer has that shape.
fn export_source(source: &str, decided: [bool; 2]) -> Option<String> {
    DECIDED_TONE_ARMS.iter().zip(decided).try_fold(
        source.to_string(),
        |source, ((condition, decided_condition, n), decided)| {
            if !decided {
                return Some(source);
            }
            (source.matches(condition).count() == *n)
                .then(|| source.replace(condition, decided_condition))
        },
    )
}

/// `export_source` for a variant's `[shadows_v13, whites_v12]` bits, or
/// `source` itself when it has another shape.
pub(crate) fn export_variant_source(source: &str, decided: [bool; 2]) -> String {
    export_source(source, decided).unwrap_or_else(|| source.into())
}

/// The parsed sources export variants compile, by output precision and the
/// tone arms their bits decide, each made on first use (`export_source`).
pub(crate) struct ExportShaders {
    sources: [Arc<str>; 2],
    shaders: [[OnceLock<Arc<DisplayShader>>; 4]; 2],
}

impl ExportShaders {
    /// From the 8-bit and the high-precision shader source.
    pub(crate) fn new(eight_bit: &str, high_precision: &str) -> Self {
        Self {
            sources: [eight_bit.into(), high_precision.into()],
            shaders: Default::default(),
        }
    }

    /// The shader of an export variant: `high_precision` output, and whether
    /// its bits hold raised Shadows of tone version 13 and lowered Whites of
    /// tone version 12. A source of another shape is only folded.
    pub(crate) fn get(
        &self,
        high_precision: bool,
        shadows_v13: bool,
        whites_v12: bool,
    ) -> &DisplayShader {
        let precision = usize::from(high_precision);
        let slot = usize::from(shadows_v13) * 2 + usize::from(whites_v12);
        self.shaders[precision][slot].get_or_init(|| {
            let source = &self.sources[precision];
            if export_source(source, [true, true]).is_none() {
                log::warn!("Export variants: the tone arms are not decided (shader shape)");
            }
            DisplayShader::exact(&export_variant_source(source, [shadows_v13, whites_v12]))
        })
    }
}

/// The source display variants compile from `source` (shader.wgsl).
/// A source of another shape gets only the reshapes that fit it.
pub(crate) fn display_source(source: &str) -> String {
    let mut source = with_display_tone_gains(source);
    source = with_effects_apart(&source).unwrap_or(source);
    source.push('\n');
    source.push_str(EFFECT_OVERRIDES);
    let Some(source) = with_display_tone_stage(&source) else {
        return source;
    };
    with_effect_tone_stage(&source).unwrap_or(source)
}

impl DisplayShader {
    pub(crate) fn new(source: &str) -> Arc<Self> {
        Self::exact(&display_source(source))
    }

    /// `source` as it is, without the display-only reshapes: what export
    /// variants compile, folded (`variant_module`).
    pub(crate) fn exact(source: &str) -> Arc<Self> {
        Arc::new(Self {
            source: source.into(),
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

/// Expressions of `main` with every call inlined (what a compiler that
/// inlines everything starts from; G76: the Radeon 680M's Windows Vulkan
/// driver does, and its compile time and memory grow with this size).
/// Functions come before their callers in a naga module.
pub(crate) fn inlined_size(module: &Module) -> usize {
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

    /// With every tone version's code, a display variant keeps one tone
    /// stage, reached from the photo's tone stage, glow and halation; a
    /// shader.wgsl of another shape fails here instead of losing the
    /// reshape unnoticed.
    #[test]
    fn display_variants_take_one_tone_stage() {
        let source = display_source(SHADER);
        assert!(source.contains("fn apply_tonal_adjustments_shared("));
        assert!(!source.contains("= apply_tonal_adjustments_v12("));
        assert!(!source.contains("= apply_tonal_adjustments_v13("));
        let shader = DisplayShader::new(SHADER);
        for effects in [0.0, 1.0] {
            let module = shader
                .variant_module(&[
                    ("ENABLE_TONE", 1.0),
                    ("WHITES_V12", 1.0),
                    ("SHADOWS_V13", 1.0),
                    ("BLACKS_V16", 1.0),
                    ("ENABLE_EFFECTS", effects),
                ])
                .unwrap();
            for gone in [
                "apply_tonal_adjustments_v12",
                "apply_tonal_adjustments_v13",
                "apply_tonal_adjustments_shared",
                "whites_v10_lower",
                "whites_v12_lower",
                "shadows_v13_gain",
            ] {
                assert!(!has_function(&module, gone), "{gone}");
            }
            let (stage, _) = module
                .functions
                .iter()
                .find(|(_, f)| f.name.as_deref() == Some("apply_tonal_adjustments"))
                .unwrap();
            let calls: usize = module
                .functions
                .iter()
                .map(|(_, f)| count_calls(&f.body, stage))
                .sum::<usize>()
                + count_calls(&module.entry_points[0].function.body, stage);
            // The photo's only: glow and halation call their own stage
            // (`effect_tone_stage`).
            assert_eq!(calls, 1);
            assert_eq!(has_function(&module, "apply_effect_tone"), effects > 0.0);
        }
        // A source of another shape keeps its tone stage as it is.
        assert_eq!(with_display_tone_stage("fn other() {}"), None);
    }

    /// Each effect behind its own switch, and glow's and halation's tone
    /// stage with Whites only: the code a Glow edit compiles (G76/WCC8).
    #[test]
    fn display_variants_compile_only_their_effects() {
        let source = display_source(SHADER);
        assert!(!source.contains(EFFECT_TONE_CALL));
        assert_eq!(source.matches(EFFECT_TONE_DISPLAY_CALL).count(), 2);
        assert!(source.contains("if (ENABLE_GLOW && t_glow > 0.0) {"));
        assert!(source.contains("if (ENABLE_HALATION && t_halation > 0.0) {"));
        assert!(source.contains("if (ENABLE_EFFECTS && ENABLE_GRAIN) {"));
        let shader = DisplayShader::new(SHADER);
        let module = |glow: f64, halation: f64, grain: f64| {
            shader
                .variant_module(&[
                    ("ENABLE_TONE", 1.0),
                    ("WHITES_V12", 1.0),
                    ("TONE_V5", 0.0),
                    ("ENABLE_EFFECTS", 1.0),
                    ("ENABLE_GLOW", glow),
                    ("ENABLE_HALATION", halation),
                    ("ENABLE_GRAIN", grain),
                ])
                .unwrap()
        };
        let glow = module(1.0, 0.0, 0.0);
        assert!(has_function(&glow, "apply_glow_bloom"));
        for gone in ["apply_halation", "apply_film_grain", "apply_portable_grain"] {
            assert!(!has_function(&glow, gone), "{gone}");
        }
        let all = module(1.0, 1.0, 1.0);
        for kept in ["apply_glow_bloom", "apply_halation", "apply_film_grain"] {
            assert!(has_function(&all, kept), "{kept}");
        }
        // The Whites-only stage: its Contrast and Shadows code is decided.
        let none = module(0.0, 0.0, 0.0);
        assert!(inlined_size(&glow) < inlined_size(&none) + 4000);
        // Sources of another shape keep their code.
        assert_eq!(with_effects_apart("fn other() {}"), None);
        assert_eq!(with_effect_tone_stage("fn other() {}"), None);
        assert_eq!(effect_tone_stage("fn other() {}"), None);
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
