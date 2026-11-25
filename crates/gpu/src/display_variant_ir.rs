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
    /// Whether its variants read glow and halation from the terms pass
    /// (`with_effect_passes`, `EFFECT_TERMS`).
    effect_passes: bool,
    /// Whether its main passes run after the parts of `main` before and in
    /// the tone stage (`with_tone_split`), and those parts' pipelines.
    tone_split: Option<crate::gpu_processing::ToneSplitParts>,
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
/// against 77.0k and 4.69 GB (G76/WCC8, probe round 16; a copy of the tone
/// stage with only Whites for glow, 67.7k, compiled into the same 4.69 GB,
/// round 17, so glow keeps the display stage). Each switch only
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

/// The display source's switches and bindings for effects in passes of
/// their own (`with_effect_terms`): `EFFECT_TERMS_PASS` makes `main` the
/// terms pass, `EFFECT_TERMS` makes it read what that pass wrote.
const EFFECT_TERMS_DECLARATIONS: &str = "override EFFECT_TERMS: bool = false;\n\
     override EFFECT_TERMS_PASS: bool = false;\n\
     override FINISH_APART: bool = false;\n\
     override FINISH_PASS: bool = false;\n\
     @group(0) @binding(33) var finish_views: texture_2d<f32>;\n\
     @group(0) @binding(34) var finish_views_out: texture_storage_2d<rgba32float, write>;\n\
     @group(0) @binding(35) var finish_colour_out: texture_storage_2d<rgba32float, write>;\n\
     @group(0) @binding(29) var glow_terms_out: texture_storage_2d<rgba32float, write>;\n\
     @group(0) @binding(30) var halation_terms_out: texture_storage_2d<rgba32float, write>;\n\
     @group(0) @binding(31) var glow_terms: texture_2d<f32>;\n\
     @group(0) @binding(32) var halation_terms: texture_2d<f32>;\n\
     // A drag's frame (detail_cache.z, G76/GRN3, GRN4): the terms once per\n\
     // block of 2 x 2 or 4 x 4 pixels of the photo (z is the shift; blocks of\n\
     // absolute coordinates), stored and read at the block's index in the tile.\n\
     // The grain field pass and the finish pass that reads it (`FILM_GRAIN_EDITS`),\n\
     // over blocks of 2 x 2 or 4 x 4 pixels (detail_cache.w is the shift,\n\
     // G76/GRN6).\n\
     override GRAIN_FIELD_PASS: bool = false;\n\
     override GRAIN_FROM_FIELD: bool = false;\n\
     @group(0) @binding(40) var grain_field_out: texture_storage_2d<rgba32float, write>;\n\
     @group(0) @binding(41) var grain_field: texture_2d<f32>;\n\
     fn tile_offset() -> vec2<u32> {\n    \
     return vec2<u32>(adjustments.tile_offset_x, adjustments.tile_offset_y);\n}\n\
     // The grain field's block (of absolute coordinates) of tile pixel `p`,\n\
     // at its index in the tile.\n\
     fn block_index(p: vec2<u32>) -> vec2<u32> {\n    \
     return block_index_of(p, adjustments.detail_cache.w);\n}\n\
     // The same for blocks of `1 << shift` pixels.\n\
     fn block_index_of(p: vec2<u32>, shift: u32) -> vec2<u32> {\n    \
     return ((p + tile_offset()) >> vec2<u32>(shift)) - (tile_offset() >> vec2<u32>(shift));\n}\n\
     fn terms_half() -> bool {\n    \
     return adjustments.detail_cache.z != 0u;\n}\n\
     fn terms_index(p: vec2<u32>) -> vec2<u32> {\n    \
     if (!terms_half()) { return p; }\n    \
     return block_index_of(p, adjustments.detail_cache.z);\n}\n\
     // A pass over blocks: invocation `p` is the block at that index\n\
     // in the tile, at its first pixel in the tile.\n\
     fn terms_pixel(p: vec2<u32>) -> vec3<u32> {\n    \
     if (!((EFFECT_TERMS_PASS && terms_half()) || GRAIN_FIELD_PASS)) { return vec3<u32>(p, 0u); }\n    \
     let shift = select(adjustments.detail_cache.z, adjustments.detail_cache.w, GRAIN_FIELD_PASS);\n    \
     let first = (p + (tile_offset() >> vec2<u32>(shift))) << vec2<u32>(shift);\n    \
     // A 4 x 4 block's terms at its pixel (1, 1), the nearest to its centre\n    \
     // (G76/GRN4; from the first pixel the drag frame's mean shift doubled),\n    \
     // within the photo. The terms read no pixel the main pass renders. The\n    \
     // grain field reads the stored colour of the block's first pixel, which\n    \
     // the main pass renders in every region (`with_block_starts`).\n    \
     let at = min(first + vec2<u32>(select(0u, 1u, shift > 1u && !GRAIN_FIELD_PASS)), vec2<u32>(input_dims()) - vec2<u32>(1u));\n    \
     return vec3<u32>(max(at, tile_offset()) - tile_offset(), 0u);\n}\n";

/// shader.wgsl's text (after `with_effects_apart`) and what the display
/// source of a processor with effect passes writes instead, each found
/// exactly once. Glow and halation each become a term, the blurred photo
/// through exposure and the tone stage (`glow_term`, `halation_term`), and
/// its use on the pixel's colour (`apply_glow_term`,
/// `apply_halation_term`); the operations and their order stay as they
/// are, so the terms pass and a main pass reading its Rgba32Float textures
/// compute what one pass did.
const EFFECT_TERMS_EDITS: [(&str, &str); 5] = [
    // The terms pass of a drag's frame (`terms_pixel`) works on
    // blocks: each invocation is one block, at the block's first pixel in
    // the tile.
    (
        "    let id = vec3<u32>(invocation + region.xy, 0u);\n",
        "    let id = terms_pixel(invocation + region.xy);\n",
    ),
    (
        ") -> vec3<f32> {\n    if (amount <= 0.0) {\n        return color;\n    }\n",
        ") -> vec3<f32> {\n    if (amount <= 0.0) {\n        return color;\n    }\n    \
         return apply_glow_term(color, glow_term(blurred_color_input_space, is_raw, exp, bright, wh), amount);\n}\n\n\
         fn glow_term(\n    blurred_color_input_space: vec3<f32>,\n    is_raw: u32,\n    \
         exp: f32, bright: f32, wh: f32\n) -> vec3<f32> {\n",
    ),
    (
        ") -> vec3<f32> {\n    if (amount <= 0.0) { return color; }\n",
        ") -> vec3<f32> {\n    if (amount <= 0.0) { return color; }\n    \
         return apply_halation_term(color, halation_term(blurred_color_input_space, is_raw, exp, bright, wh), amount);\n}\n\n\
         fn halation_term(\n    blurred_color_input_space: vec3<f32>,\n    is_raw: u32,\n    \
         exp: f32, bright: f32, wh: f32\n) -> vec3<f32> {\n",
    ),
    // `main` as the terms pass ends once it has the blurred photo and the
    // pixel's slider values; the rest of `main` is its other branch
    // (closed by `EFFECT_TERMS_MAIN_END`), which the fold drops.
    (
        "    let structure_blurred = textureLoad(structure_blur_texture, id.xy, 0).rgb;\n",
        "    let structure_blurred = textureLoad(structure_blur_texture, id.xy, 0).rgb;\n    \
         if (EFFECT_TERMS_PASS) {\n        \
         var glow = vec3<f32>(0.0);\n        \
         var halation = vec3<f32>(0.0);\n        \
         if (ENABLE_GLOW && t_glow > 0.0) {\n            \
         glow = glow_term(structure_blurred, is_raw, t_exposure, t_brightness, t_whites);\n        }\n        \
         if (ENABLE_HALATION && t_halation > 0.0) {\n            \
         halation = halation_term(clarity_blurred, is_raw, t_exposure, t_brightness, t_whites);\n        }\n        \
         textureStore(glow_terms_out, terms_index(id.xy), vec4<f32>(glow, 0.0));\n        \
         textureStore(halation_terms_out, terms_index(id.xy), vec4<f32>(halation, 0.0));\n    \
         } else {\n",
    ),
    // The photo's glow and halation: from the terms pass or computed here.
    (
        "        if (ENABLE_GLOW && t_glow > 0.0) {\n            \
         processed_rgb = apply_glow_bloom(\n                \
         processed_rgb, structure_blurred, t_glow, is_raw,\n                \
         t_exposure, t_brightness, t_contrast, t_whites\n            );\n        }\n        \
         if (ENABLE_HALATION && t_halation > 0.0) {\n            \
         processed_rgb = apply_halation(\n                \
         processed_rgb, clarity_blurred, t_halation, is_raw,\n                \
         t_exposure, t_brightness, t_contrast, t_whites\n            );\n        }\n",
        "        if (ENABLE_GLOW && t_glow > 0.0) {\n            \
         if (EFFECT_TERMS) {\n                \
         processed_rgb = apply_glow_term(processed_rgb, textureLoad(glow_terms, terms_index(id.xy), 0).rgb, t_glow);\n            \
         } else {\n                \
         processed_rgb = apply_glow_bloom(processed_rgb, structure_blurred, t_glow, is_raw, t_exposure, t_brightness, t_contrast, t_whites);\n            }\n        }\n        \
         if (ENABLE_HALATION && t_halation > 0.0) {\n            \
         if (EFFECT_TERMS) {\n                \
         processed_rgb = apply_halation_term(processed_rgb, textureLoad(halation_terms, terms_index(id.xy), 0).rgb, t_halation);\n            \
         } else {\n                \
         processed_rgb = apply_halation(processed_rgb, clarity_blurred, t_halation, is_raw, t_exposure, t_brightness, t_contrast, t_whites);\n            }\n        }\n",
    ),
];

/// Where glow's and then halation's term ends in shader.wgsl: the first
/// line after the tone stage of the blurred photo, in that order. Cut
/// there, the main pass keeps each effect's own arithmetic: with the cut
/// after it (the bloom colour as the term) NVIDIA's compiler rounded glow
/// differently in the small module, 1/65535 in up to 0.1 % of an export's
/// 16-bit values (G76/EFX).
const EFFECT_TERM_END: &str =
    "    let linear_luma = get_luma(max(blurred_linear, vec3<f32>(0.0)));\n";
const EFFECT_TERM_USES: [&str; 2] = ["apply_glow_term", "apply_halation_term"];

/// The end of `main` in shader.wgsl (the text after it), where the branch
/// `EFFECT_TERMS_EDITS` opens is closed.
const EFFECT_TERMS_MAIN_END: &str = "}\n\n// The colours `main` stored with DEFER_OUTPUT";

/// Where the end of `main` starts in the display source (grain, then the
/// clipping warnings, the views, the output encoding and the histogram's
/// image), and the pixel's values it takes from the code before it.
const FINISH_START: &str =
    "    if (ENABLE_EFFECTS && ENABLE_GRAIN) {\n        if (adjustments.global.grain_amount > 0.0";
const FINISH_FUNCTION: &str = "fn finish_pixel(\n    \
     id: vec3<u32>, absolute_coord: vec2<u32>, scale: f32, original_alpha: f32,\n    \
     range_view: f32, sharpen_view_mask: f32, colour: vec3<f32>\n) {\n    \
     let absolute_coord_i = vec2<i32>(absolute_coord);\n    \
     var final_rgb = colour;\n";
/// `main`'s end with the finish apart: where a frame asks for it
/// (`FINISH_APART_FLAG`, set per frame: with grain, or for an export that
/// encodes each output in its own pass) a `FINISH_APART` variant stores the
/// colour before grain (and the two view values, when a texture for them
/// is bound) for the finish pass; else it finishes the pixel itself, as
/// one pass does. A frame without grain so costs what it did: on a Radeon
/// 680M the stored tile and its read added 12 ms to a 2560 x 1440 frame
/// (53 ms) when every frame took the finish pass (G76/WEFX).
const FINISH_MAIN_END: &str = "    if (FINISH_APART && adjustments.detail_cache.y != 0u) {\n        \
     textureStore(finish_colour_out, id.xy, vec4<f32>(final_rgb, original_alpha));\n        \
     if (textureDimensions(finish_views_out).x > 1u) {\n            \
     textureStore(finish_views_out, id.xy, vec4<f32>(range_view, sharpen_view_mask, 0.0, 0.0));\n        }\n    \
     } else {\n        \
     finish_pixel(id, absolute_coord, scale, original_alpha, range_view, sharpen_view_mask, final_rgb);\n    }\n    }\n";
/// Where `main` starts its work on a pixel, and `main` as the finish pass
/// before it: the stored colour through `finish_pixel`.
const FINISH_PASS_AT: &str = "    let original_alpha = load_input(vec2<i32>(absolute_coord)).a;\n";
const FINISH_PASS: &str = "    if (GRAIN_FIELD_PASS) {\n        \
     let stored = textureLoad(deferred_texture, vec2<i32>(id.xy), 0);\n        \
     let grain = apply_film_grain(stored.rgb, absolute_coord >> vec2<u32>(adjustments.detail_cache.w));\n        \
     textureStore(grain_field_out, block_index(id.xy), vec4<f32>(grain, 0.0));\n    \
     } else if (FINISH_PASS) {\n        \
     let stored = textureLoad(deferred_texture, vec2<i32>(id.xy), 0);\n        \
     var views = vec2<f32>(-1.0, 1.0);\n        \
     if (textureDimensions(finish_views).x > 1u) {\n            \
     views = textureLoad(finish_views, vec2<i32>(id.xy), 0).xy;\n        }\n        \
     finish_pixel(id, absolute_coord, scale, stored.a, views.x, views.y, stored.rgb);\n    \
     } else {\n";
const MAIN_START: &str = "@compute @workgroup_size(8, 8, 1)\nfn main(";

/// Film grain of a drag's frame (G76/GRN3): shader.wgsl's text in
/// `apply_film_grain` and what the display source writes instead. With
/// `GRAIN_FIELD_PASS` the function returns the grain of a 2 x 2 or 4 x 4
/// block (`detail_cache.w`, G76/GRN6) at the block's coordinates, twice or
/// four times the texels per pixel (the grain field rendered at half or a
/// quarter of the resolution); with `GRAIN_FROM_FIELD` it applies the
/// block's grain (`grain_field`) to the pixel's own colour. Without either
/// constant it computes what it did.
const FILM_GRAIN_EDITS: [(&str, &str); 3] = [
    (
        "    let s_q = cells * (FILM_TEXELS_PER_CELL >> 12u) + ((cells * (FILM_TEXELS_PER_CELL & 4095u)) >> 12u);\n",
        "    var s_q = cells * (FILM_TEXELS_PER_CELL >> 12u) + ((cells * (FILM_TEXELS_PER_CELL & 4095u)) >> 12u);\n    \
         if (GRAIN_FIELD_PASS) {\n        s_q = s_q << adjustments.detail_cache.w;\n    }\n",
    ),
    (
        "    var grain: vec3<f32>;\n    if (rho >= 1.0) {\n        // One neutral layer at the luminance's density.\n",
        "    var grain: vec3<f32>;\n    if (GRAIN_FROM_FIELD) {\n        \
         grain = textureLoad(grain_field, block_index(coord - tile_offset()), 0).rgb;\n    \
         } else if (rho >= 1.0) {\n        // One neutral layer at the luminance's density.\n",
    ),
    (
        "    let printed = 1.0 / (1.0 + exp(-FILM_PRINT_SLOPE * (x + grain * (FILM_DMAX * amount))));\n",
        "    if (GRAIN_FIELD_PASS) {\n        return grain;\n    }\n    \
         let printed = 1.0 / (1.0 + exp(-FILM_PRINT_SLOPE * (x + grain * (FILM_DMAX * amount))));\n",
    ),
];

/// The end of `main` as a function the main pass or a finish pass runs
/// (G76/EFX): grain is 29.4k inlined expressions (four versions), the rest
/// of the end 6.4k. A `FINISH_APART` variant holds the end without grain
/// and can store the colour before it in an Rgba32Float tile instead
/// (`FINISH_MAIN_END`; 66.2k for the Basic-panel superset with glow and
/// halation, against 117.4k for one pass with the three effects); with
/// `FINISH_PASS` `main` is the pass that finishes a stored colour (35.9k
/// with grain). The values the end takes are stored as they are, so it
/// computes what one pass did.
/// Before `with_effect_terms`. `None` when the source has another shape.
fn with_finish_pass(source: &str) -> Option<String> {
    if [
        FINISH_START,
        EFFECT_TERMS_MAIN_END,
        FINISH_PASS_AT,
        MAIN_START,
    ]
    .iter()
    .any(|text| source.matches(text).count() != 1)
    {
        return None;
    }
    let source = FILM_GRAIN_EDITS
        .iter()
        .try_fold(source.to_string(), |source, (from, to)| {
            (source.matches(from).count() == 1).then(|| source.replacen(from, to, 1))
        })?;
    let source = source.as_str();
    let start = source.find(FINISH_START)?;
    let end = source.find(EFFECT_TERMS_MAIN_END)?;
    let entry = source.find(MAIN_START)?;
    if !(entry < start && start < end) {
        return None;
    }
    let mut out = String::with_capacity(source.len() + 4096);
    out.push_str(&source[..entry]);
    out.push_str(FINISH_FUNCTION);
    out.push_str(&source[start..end]);
    out.push_str("}\n\n");
    out.push_str(&source[entry..start].replacen(
        FINISH_PASS_AT,
        &format!("{FINISH_PASS}{FINISH_PASS_AT}"),
        1,
    ));
    out.push_str(FINISH_MAIN_END);
    out.push_str(&source[end..]);
    Some(out)
}

/// Glow and halation as terms a pass of its own computes (G76/EFX): each
/// carries the tone stage for the blurred photo (10.9k inlined expressions
/// each), so the Basic-panel superset with both grew from 66.1k to 88.0k,
/// beyond what a Radeon 680M compiles within the memory bar (about 77k).
/// With `EFFECT_TERMS` the main pass reads the terms (66.2k with both);
/// with `EFFECT_TERMS_PASS` `main` is the pass that writes them (23.1k).
/// Without either constant the source computes what it did. `None` when
/// shader.wgsl has another shape.
fn with_effect_terms(source: &str) -> Option<String> {
    let mut source = EFFECT_TERMS_EDITS
        .iter()
        .try_fold(source.to_string(), |source, (from, to)| {
            (source.matches(from).count() == 1).then(|| source.replace(from, to))
        })?;
    if source.matches(EFFECT_TERM_END).count() != EFFECT_TERM_USES.len()
        || source.matches(EFFECT_TERMS_MAIN_END).count() != 1
    {
        return None;
    }
    for name in EFFECT_TERM_USES {
        source = source.replacen(
            EFFECT_TERM_END,
            &format!(
                "    return blurred_linear;\n}}\n\n\
                 fn {name}(color: vec3<f32>, blurred_linear: vec3<f32>, amount: f32) -> vec3<f32> {{\n\
                 {}",
                EFFECT_TERM_END.replace("    let linear_luma", "    let  linear_luma")
            ),
            1,
        );
    }
    source = source.replace("    let  linear_luma", "    let linear_luma");
    source = source.replace(
        EFFECT_TERMS_MAIN_END,
        &format!("    }}\n{EFFECT_TERMS_MAIN_END}"),
    );
    source.push_str(EFFECT_TERMS_DECLARATIONS);
    Some(source)
}

/// The display source's switches and bindings for the main pass split at
/// the tone stage (`with_tone_split`).
const TONE_SPLIT_DECLARATIONS: &str = "override SPLIT_STORE1: bool = false;\n\
     override SPLIT_LOAD1: bool = false;\n\
     override SPLIT_STORE2: bool = false;\n\
     override SPLIT_LOAD2: bool = false;\n\
     @group(0) @binding(36) var split_out1: texture_storage_2d<rgba32float, write>;\n\
     @group(0) @binding(37) var split_in1: texture_2d<f32>;\n\
     @group(0) @binding(38) var split_out2: texture_storage_2d<rgba32float, write>;\n\
     @group(0) @binding(39) var split_in2: texture_2d<f32>;\n";

/// Where `main`'s work before the tone stage starts (after the blurred
/// photos are read), and the lines it changes so the colour and the
/// sharpening view's mask live beyond that part: `exposure_gain` (which the
/// tone stage reads too) moves before it.
const SPLIT_PART1_START: &str = "    // Layers that change noise reduction or sharpening need their own amounts.\n    let cached_detail";
const SPLIT_PART1_EDITS: [(&str, &str); 3] = [
    (
        "    var sharpen_view_mask = 1.0;\n    if (SHARPEN_VIEW && locally_contrasted_rgb.x",
        "    if (SHARPEN_VIEW && locally_contrasted_rgb.x",
    ),
    (
        "    let exposure_gain = exp2(t_exposure);\n    if (PRESENCE_V3 && adjustments.global.structure_version",
        "    if (PRESENCE_V3 && adjustments.global.structure_version",
    ),
    (
        "    var composite_rgb_linear = apply_dehaze(processed_rgb",
        "    composite_rgb_linear = apply_dehaze(processed_rgb",
    ),
];
/// The first cut: right before the tone stage.
const SPLIT_CUT1: &str = "    composite_rgb_linear = apply_centre_tonal_and_color(composite_rgb_linear, adjustments.global.centre, absolute_coord_i);\n    if (ENABLE_TONE) {\n";
/// The second cut: right after Highlights.
const SPLIT_CUT2: &str = "    composite_rgb_linear = apply_color_calibration(composite_rgb_linear, adjustments.global.color_calibration);\n";

/// The main pass split at the tone stage (G76/COLD4): the Radeon 680M's
/// Windows Vulkan compiler costs grow far faster than a shader's size (the
/// Basic-panel superset, 70.5k inlined expressions, 38.6 s / 4.4 GB; its
/// part before the tone stage, 25.9k, 2.6 s / 449 MB; the tone stage and
/// Highlights, 13.9k, 3.2 s / 732 MB; the rest, 33.3k, 4.5 s / 543 MB;
/// compile_probe round 21). `SPLIT_STORE1` makes `main` the pass before the
/// tone stage, which stores the colour and the sharpening view's mask in an
/// Rgba32Float tile; `SPLIT_LOAD1` + `SPLIT_STORE2` the tone pass, which
/// reads that tile and stores the colour after Highlights; `SPLIT_LOAD2` the
/// main pass that reads it and does the rest. Every pass sums the layers'
/// sliders as one pass does; the operations and their order are those of
/// one pass, and without the constants the source computes what it did.
/// After `with_effect_terms`. `None` when the source has another shape.
fn with_tone_split(source: &str) -> Option<String> {
    let mut source =
        SPLIT_PART1_EDITS
            .iter()
            .try_fold(source.to_string(), |source, (from, to)| {
                (source.matches(from).count() == 1).then(|| source.replacen(from, to, 1))
            })?;
    if [
        SPLIT_PART1_START,
        SPLIT_CUT1,
        SPLIT_CUT2,
        EFFECT_TERMS_MAIN_END,
    ]
    .iter()
    .any(|text| source.matches(text).count() != 1)
    {
        return None;
    }
    let (start, cut1, cut2) = (
        source.find(SPLIT_PART1_START)?,
        source.find(SPLIT_CUT1)?,
        source.find(SPLIT_CUT2)?,
    );
    if !(start < cut1 && cut1 < cut2 && cut2 < source.find(EFFECT_TERMS_MAIN_END)?) {
        return None;
    }
    let load = |n: u32| {
        format!(
            "    if (SPLIT_LOAD{n}) {{\n        \
             let stored = textureLoad(split_in{n}, id.xy, 0);\n        \
             composite_rgb_linear = stored.rgb;\n        \
             sharpen_view_mask = stored.a;\n    }}\n"
        )
    };
    let store = |n: u32| {
        format!(
            "    if (SPLIT_STORE{n}) {{\n        \
             textureStore(split_out{n}, id.xy, vec4<f32>(composite_rgb_linear, sharpen_view_mask));\n    \
             }} else {{\n"
        )
    };
    let tone_stage = SPLIT_CUT1.len() - "    if (ENABLE_TONE) {\n".len();
    source = format!(
        "{}    let exposure_gain = exp2(t_exposure);\n    \
         var composite_rgb_linear = vec3<f32>(0.0);\n    \
         var sharpen_view_mask = 1.0;\n    \
         if (!(SPLIT_LOAD1 || SPLIT_LOAD2)) {{\n{}{}    }}\n{}{}    if (!SPLIT_LOAD2) {{\n{}    }}\n{}{}{}",
        &source[..start],
        &source[start..cut1],
        &SPLIT_CUT1[..tone_stage],
        load(1),
        store(1),
        &source[cut1 + tone_stage..cut2],
        load(2),
        store(2),
        &source[cut2..],
    );
    source = source.replace(
        EFFECT_TERMS_MAIN_END,
        &format!("    }}\n    }}\n{EFFECT_TERMS_MAIN_END}"),
    );
    source.push_str(TONE_SPLIT_DECLARATIONS);
    Some(source)
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
    with_display_tone_stage(&source).unwrap_or(source)
}

/// `display_source` with glow and halation as terms of a pass of their own
/// (`with_effect_terms`) and the main pass split at the tone stage
/// (`with_tone_split`): what display variants compile under a compile
/// limit. `None` when shader.wgsl has another shape.
pub(crate) fn display_source_with_effect_passes(source: &str) -> Option<String> {
    with_tone_split(&display_source_with_effect_terms(source)?)
}

/// `display_source` with the effects in passes of their own, the main pass
/// not split.
fn display_source_with_effect_terms(source: &str) -> Option<String> {
    with_effect_terms(&with_finish_pass(&display_source(source))?)
}

impl DisplayShader {
    pub(crate) fn new(source: &str) -> Arc<Self> {
        Self::exact(&display_source(source))
    }

    /// The display shader of a processor that renders glow and halation
    /// through the terms pass (`with_effect_terms`), its main passes split
    /// (`with_passes`); `new` when the source has another shape (its
    /// variants then compute the effects themselves).
    #[cfg(test)]
    pub(crate) fn with_effect_passes(source: &str) -> Arc<Self> {
        Self::with_passes(source, true)
    }

    /// `with_effect_passes`, with the main pass split at the tone stage
    /// (`split`; a source whose shape does not allow the split keeps one
    /// main pass) or not.
    pub(crate) fn with_passes(source: &str, split: bool) -> Arc<Self> {
        let Some(terms) = display_source_with_effect_terms(source) else {
            log::warn!("Display variants: no effect passes (shader shape)");
            return Self::new(source);
        };
        let split_source = split.then(|| with_tone_split(&terms)).flatten();
        if split && split_source.is_none() {
            log::warn!("Display variants: the main pass is not split (shader shape)");
        }
        let tone_split = split_source
            .is_some()
            .then(crate::gpu_processing::ToneSplitParts::default);
        Arc::new(Self {
            source: split_source.unwrap_or(terms).into(),
            parsed: OnceLock::new(),
            effect_passes: true,
            tone_split,
        })
    }

    /// The parts of `main` this shader's main passes run after
    /// (`with_tone_split`): `None` when they are not split.
    pub(crate) fn tone_split(&self) -> Option<&crate::gpu_processing::ToneSplitParts> {
        self.tone_split.as_ref()
    }

    /// Whether variants of this shader read glow and halation from the
    /// terms pass, which then has to run before them.
    pub(crate) fn effect_passes(&self) -> bool {
        self.effect_passes
    }

    /// `source` as it is, without the display-only reshapes: what export
    /// variants compile, folded (`variant_module`).
    pub(crate) fn exact(source: &str) -> Arc<Self> {
        Arc::new(Self {
            source: source.into(),
            parsed: OnceLock::new(),
            effect_passes: false,
            tone_split: None,
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
pub(crate) mod tests {
    use super::*;

    const SHADER: &str = include_str!("shaders/shader.wgsl");

    fn has_function(module: &Module, name: &str) -> bool {
        module
            .functions
            .iter()
            .any(|(_, f)| f.name.as_deref() == Some(name))
    }

    /// The HLSL wgpu's DX12 backend gets: names of helper functions defined
    /// more than once. DXC rejects a redefinition (FXC did not), so every
    /// display variant fell back to the general shader with DXC.
    pub(crate) fn hlsl_redefinitions(module: &Module) -> Vec<String> {
        let info = validate(module).unwrap();
        // Overrides at their defaults, as wgpu applies a pipeline's constants.
        let (module, info) = naga::back::pipeline_constants::process_overrides(
            module,
            &info,
            None,
            &Default::default(),
        )
        .map(|(m, i)| (m.into_owned(), i.into_owned()))
        .unwrap();
        let module = &module;
        let options = naga::back::hlsl::Options {
            shader_model: naga::back::hlsl::ShaderModel::V6_0,
            restrict_indexing: true,
            force_loop_bounding: true,
            zero_initialize_workgroup_memory: true,
            ..Default::default()
        };
        // Every entry point at once: the helpers of all of them, a superset
        // of what one pipeline's HLSL holds (wgpu-hal writes one entry point).
        let pipeline = naga::back::hlsl::PipelineOptions::default();
        let mut hlsl = String::new();
        let mut writer = naga::back::hlsl::Writer::new(&mut hlsl, &options, &pipeline);
        writer.write(module, &info, None).unwrap();
        if let Ok(dir) = std::env::var("RAWTONIC_HLSL_DUMP") {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            std::fs::write(format!("{dir}/{n}.hlsl"), &hlsl).unwrap();
        }
        let mut seen = std::collections::HashSet::new();
        let mut twice = Vec::new();
        for line in hlsl.lines().filter(|l| !l.starts_with(char::is_whitespace)) {
            let Some(name) = line
                .split(['(', ' '])
                .find(|w| w.starts_with("Naga"))
                .filter(|_| line.trim_end().ends_with(')') || line.ends_with('{'))
            else {
                continue;
            };
            // DXC does not overload on the `unorm`/`snorm` modifiers: a
            // `RWTexture2D<unorm float4>` parameter redefines `RWTexture2D<float4>`.
            let signature = line.replace("unorm ", "").replace("snorm ", "");
            if !seen.insert(signature) {
                twice.push(name.to_string());
            }
        }
        twice
    }

    #[test]
    fn hlsl_defines_each_image_query_helper_once() {
        let source = "
            @group(0) @binding(0) var a: texture_storage_2d<rgba16float, write>;
            @group(0) @binding(1) var b: texture_storage_2d<rgba32float, write>;
            @group(0) @binding(2) var c: texture_storage_2d<rgba8unorm, write>;
            @compute @workgroup_size(1) fn main() {
                let s = textureDimensions(a) + textureDimensions(b) + textureDimensions(c);
                textureStore(a, s, vec4<f32>(0.0));
                textureStore(b, s, vec4<f32>(0.0));
                textureStore(c, s, vec4<f32>(0.0));
            }";
        let module = naga::front::wgsl::parse_str(source).unwrap();
        assert_eq!(hlsl_redefinitions(&module), Vec::<String>::new());
        let shader = DisplayShader::new(SHADER);
        let effects = [("ENABLE_EFFECTS", 1.0)];
        for constants in [&[][..], &effects[..]] {
            let module = shader.variant_module(constants).unwrap();
            assert_eq!(hlsl_redefinitions(&module), Vec::<String>::new());
        }
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
            // The photo's; with the effects glow's and halation's too.
            assert_eq!(calls, if effects > 0.0 { 3 } else { 1 });
        }
        // A source of another shape keeps its tone stage as it is.
        assert_eq!(with_display_tone_stage("fn other() {}"), None);
    }

    /// Each effect behind its own switch: the code a Glow edit compiles
    /// (G76/WCC8).
    #[test]
    fn display_variants_compile_only_their_effects() {
        let source = display_source(SHADER);
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
        // Sources of another shape keep their code.
        assert_eq!(with_effects_apart("fn other() {}"), None);
    }

    /// The bindings a module's code uses.
    fn bindings(module: &Module) -> Vec<u32> {
        let mut bindings: Vec<u32> = module
            .global_variables
            .iter()
            .filter_map(|(_, variable)| variable.binding.as_ref().map(|b| b.binding))
            .collect();
        bindings.sort_unstable();
        bindings
    }

    /// Under a compile limit the display source has the effects in passes
    /// of their own (G76/EFX): the main pass without their code, the terms
    /// pass with the tone stage alone, the finish pass with grain and the
    /// output's end alone. A shader.wgsl of another shape fails here.
    #[test]
    fn effect_passes_split_the_display_source() {
        let shader = DisplayShader::with_effect_passes(SHADER);
        assert!(shader.effect_passes());
        let all = [
            ("ENABLE_TONE", 1.0),
            ("WHITES_V12", 1.0),
            ("TONE_V5", 0.0),
            ("ENABLE_EFFECTS", 1.0),
            ("ENABLE_GLOW", 1.0),
            ("ENABLE_HALATION", 1.0),
            ("ENABLE_GRAIN", 1.0),
        ];
        let with = |extra: &[(&'static str, f64)]| {
            shader.variant_module(&[&all[..], extra].concat()).unwrap()
        };
        let one_pass = with(&[]);
        let main = with(&[
            ("EFFECT_TERMS", 1.0),
            ("FINISH_APART", 1.0),
            ("ENABLE_GRAIN", 0.0),
        ]);
        let terms = with(&[("EFFECT_TERMS_PASS", 1.0)]);
        let finish = with(&[("FINISH_PASS", 1.0)]);
        for name in [
            "glow_term",
            "apply_glow_term",
            "apply_film_grain",
            "finish_pixel",
        ] {
            assert!(has_function(&one_pass, name), "one pass: {name}");
        }
        // The main pass: no tone stage for the blurred photo, no grain; it
        // can finish a pixel itself.
        for gone in ["glow_term", "halation_term", "apply_film_grain"] {
            assert!(!has_function(&main, gone), "main: {gone}");
        }
        for kept in ["apply_glow_term", "apply_halation_term", "finish_pixel"] {
            assert!(has_function(&main, kept), "main: {kept}");
        }
        for gone in [
            "apply_glow_term",
            "apply_film_grain",
            "finish_pixel",
            "apply_hsl_panel",
        ] {
            assert!(!has_function(&terms, gone), "terms: {gone}");
        }
        assert!(has_function(&terms, "glow_term") && has_function(&terms, "halation_term"));
        for gone in [
            "glow_term",
            "apply_glow_term",
            "apply_tonal_adjustments",
            "apply_hsl_panel",
        ] {
            assert!(!has_function(&finish, gone), "finish: {gone}");
        }
        assert!(has_function(&finish, "apply_film_grain") && has_function(&finish, "finish_pixel"));
        let sizes = [&one_pass, &main, &terms, &finish].map(inlined_size);
        println!(
            "one pass {}, main {}, terms {}, finish {}",
            sizes[0], sizes[1], sizes[2], sizes[3]
        );
        assert!(sizes[1] * 3 < sizes[0] * 2 && sizes[2] < sizes[1] / 2 && sizes[3] < sizes[1]);
        // The finish pass reads the stored colour, the views and what grain
        // and the output's end need (gpu_processing `effect_passes`).
        assert_eq!(bindings(&finish), [0, 1, 2, 16, 25, 27, 28, 33]);
        for binding in [1, 28, 31, 32, 34, 35] {
            assert!(
                bindings(&main).contains(&binding),
                "main: binding {binding}"
            );
        }
        // Sources of another shape keep one pass.
        assert_eq!(display_source_with_effect_passes("fn other() {}"), None);
        assert!(!DisplayShader::with_effect_passes("fn other() {}").effect_passes());
    }

    /// Under a compile limit the main pass runs after its parts before and
    /// in the tone stage (G76/COLD4): each part holds only its own stages,
    /// and binds only the tiles it writes and reads.
    #[test]
    fn the_tone_split_parts_hold_their_stages() {
        let shader = DisplayShader::with_passes(SHADER, true);
        assert!(shader.effect_passes() && shader.tone_split().is_some());
        let basic = [
            ("ENABLE_TONE", 1.0),
            ("ENABLE_HIGHLIGHTS", 1.0),
            ("ENABLE_SHARPEN", 1.0),
            ("ENABLE_NOISE_REDUCTION", 1.0),
            ("TONE_V5", 0.0),
            ("EFFECT_TERMS", 1.0),
            ("FINISH_APART", 1.0),
            ("ENABLE_GRAIN", 0.0),
        ];
        let with = |extra: &[(&'static str, f64)]| {
            shader
                .variant_module(&[&basic[..], extra].concat())
                .unwrap()
        };
        let one = with(&[]);
        let before = with(&[("SPLIT_STORE1", 1.0)]);
        let tone = with(&[("SPLIT_LOAD1", 1.0), ("SPLIT_STORE2", 1.0)]);
        let main = with(&[("SPLIT_LOAD2", 1.0)]);
        for (part, has, lacks) in [
            (
                &before,
                &["detail_stage", "apply_dehaze"][..],
                &["apply_tonal_adjustments", "apply_hsl_panel", "finish_pixel"][..],
            ),
            (
                &tone,
                &["apply_tonal_adjustments", "apply_highlights_adjustment"],
                &[
                    "detail_stage",
                    "apply_dehaze",
                    "apply_hsl_panel",
                    "finish_pixel",
                ],
            ),
            (
                &main,
                &["apply_hsl_panel", "finish_pixel"],
                &["detail_stage", "apply_dehaze", "apply_tonal_adjustments"],
            ),
        ] {
            for name in has {
                assert!(has_function(part, name), "{name} missing");
            }
            for name in lacks {
                assert!(!has_function(part, name), "{name} kept");
            }
        }
        let split_bindings = |module: &Module| {
            bindings(module)
                .into_iter()
                .filter(|binding| (36..40).contains(binding))
                .collect::<Vec<_>>()
        };
        assert_eq!(split_bindings(&one), Vec::<u32>::new());
        assert_eq!(split_bindings(&before), [36]);
        assert_eq!(split_bindings(&tone), [37, 38]);
        assert_eq!(split_bindings(&main), [39]);
        let sizes = [&one, &before, &tone, &main].map(inlined_size);
        println!(
            "one pass {}, parts {} + {} + {}",
            sizes[0], sizes[1], sizes[2], sizes[3]
        );
        assert!(
            sizes[1..].iter().all(|size| size * 2 < sizes[0]),
            "{sizes:?}"
        );
        // Without the split, the passes' source as before.
        assert!(
            DisplayShader::with_passes(SHADER, false)
                .tone_split()
                .is_none()
        );
        assert!(with_tone_split("fn other() {}").is_none());
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
