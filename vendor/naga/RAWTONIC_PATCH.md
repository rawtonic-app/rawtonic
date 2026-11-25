# HLSL image-size helpers are written once per HLSL type

Origin: naga 30.0.1 from crates.io (archive SHA-256
a616d2fb8c89516ac2723a581f69d6c18576046bed761bd6b305e5618e6ae130), part of
wgpu 30. Original MIT and Apache-2.0 license texts remain. Rawtonic's change
is AGPL-3.0-or-later (`LICENSE_RAWTONIC_AGPL-3.0`). `Cargo.lock` of the
published crate is removed; nothing else changes outside the patch.

Only `src/back/hlsl/help.rs` changes, in two places:

1. The `Expression::ImageQuery` case of `write_wrapped_functions` keys its
   dedup set with `hlsl_query_class` (new, at the end of the file), which
   drops the storage access and maps each storage format to one format of the
   same HLSL element type (`StorageFormat::to_hlsl_str`). Two storage
   textures differing only in access or in formats sharing an HLSL type
   (`rgba16float` and `rgba32float` are both `float4`) wrote the same
   function twice.
2. `write_wrapped_image_query_function_name` names the helpers of `unorm` and
   `snorm` storage textures `NagaRWUnorm…` / `NagaRWSnorm…`. HLSL does not
   overload on those modifiers: `NagaRWDimensions2D(RWTexture2D<unorm float4>)`
   and `NagaRWDimensions2D(RWTexture2D<float4>)` (`rgba8unorm` and
   `rgba32float` outputs of Rawtonic's processing shader) are one function to
   DXC.

FXC accepted both duplicates; DXC fails with `error: redefinition of
'NagaRWDimensions2D'`, so with DXC every Rawtonic display variant fell back to
the general shader (gap G76, Radeon 680M, 2026-11-25). Call sites use the
same name function, so they follow. Only HLSL output changes; the Metal,
Vulkan (SPIR-V) and other backends do not use this code.

Checked with Microsoft's DXC 1.8.2502 (Linux build, wgpu's arguments
`-HV 2018 -Ges`, `cs_6_0`) on the HLSL of the processing shader, its display,
export and display-variant sources and every file in `crates/gpu/src/shaders`:
22 redefinition errors before, every entry point compiles after.

Tests (`rawtonic-gpu`, fail with the unpatched crate):
`display_variant_ir::tests::hlsl_defines_each_image_query_helper_once`,
`gpu_processing::hlsl_tests::every_shader_defines_each_hlsl_helper_once`.

Upstream: not fixed in naga 30.0.2 nor on wgpu trunk (2026-11-25), no issue
found. Remove this copy and its `[patch.crates-io]` entry when a released
naga dedups the helper by its HLSL signature.
