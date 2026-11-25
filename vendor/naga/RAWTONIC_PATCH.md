# HLSL image-size helpers are written once per HLSL type

Origin: naga 30.0.1 from crates.io (archive SHA-256
a616d2fb8c89516ac2723a581f69d6c18576046bed761bd6b305e5618e6ae130), part of
wgpu 30. Original MIT and Apache-2.0 license texts remain. Rawtonic's change
is AGPL-3.0-or-later (`LICENSE_RAWTONIC_AGPL-3.0`). `Cargo.lock` of the
published crate is removed; nothing else changes outside the patch.

Only `src/back/hlsl/help.rs` changes: the `Expression::ImageQuery` case of
`write_wrapped_functions` keys its dedup set with `hlsl_query_class` (new, at
the end of the file), which drops the storage access and maps each storage
format to one format of the same HLSL element type
(`StorageFormat::to_hlsl_str`). The helper's name (`NagaRWDimensions2D`) and
its parameter type (`RWTexture2D<float4>`) depend only on those, so two
storage textures that differ in access or in a format sharing an HLSL type
(`rgba16float` and `rgba32float` are both `float4`) wrote the same function
twice. FXC accepted that; DXC fails with `error: redefinition of
'NagaRWDimensions2D'`, so with DXC every Rawtonic display variant fell back
to the general shader (gap G76, Radeon 680M, 2026-11-24). The HLSL written
for any other shader is unchanged; the Metal, Vulkan (SPIR-V) and other
backends do not use this code.

Test: `rawtonic-gpu` `display_variant_ir::tests::hlsl_defines_each_image_query_helper_once`
(fails with the unpatched crate).

Upstream: not fixed in naga 30.0.2 nor on wgpu trunk (2026-11-25), no issue
found. Remove this copy and its `[patch.crates-io]` entry when a released
naga dedups the helper by its HLSL signature.
