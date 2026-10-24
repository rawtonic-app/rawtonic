# Third-party notices

Rawtonic additions: Copyright 2026 Rawtonic contributors, AGPL-3.0-or-later.

## RapidRAW
The initial `src/`, `src-tauri/`, `public/`, `data/` and frontend tooling derive from [CyberTimon/RapidRAW](https://github.com/CyberTimon/RapidRAW), snapshot a82251f699333daf3648292c756485272be86187, Copyright Timon Käch and contributors. Engine modules moved out of `src-tauri/` into `crates/` (ADR 0009, e.g. `crates/rawdecode`) keep that origin and license. Distributed under its preserved GNU AGPL v3 license (`licenses/RapidRAW-AGPL-3.0.txt`). Rawtonic is an independent derivative, not an official RapidRAW release. Existing third-party notices and data licenses remain applicable. No claim is made that upstream owners endorse Rawtonic.

## Vendored rawler
`third_party/rawler` contains rawler 0.7.1 from [CyberTimon/RapidRAW-DngLab](https://github.com/CyberTimon/RapidRAW-DngLab) (RapidRAW's fork of [dnglab/dnglab](https://github.com/dnglab/dnglab)), commit 934af4b213a12b95f77d69529627067056066294, Copyright Daniel Vogelbacher, Pedro Côrte-Real and contributors, GNU LGPL v2.1; the license text and AUTHORS file are preserved there. Rawtonic's decoder fixes and camera additions are marked in the source and listed in `third_party/rawler/RAWTONIC_PATCHES.md` (ADR 0007). It replaces the git dependency through a `[patch]` entry in the workspace Cargo.toml; the LGPL terms apply to the vendored crate as before.

`third_party/rawler/src/decompressors/sony_arw6.rs` is a Rust port of LibRaw's Sony ARW 6 decoder (`src/decoders/sony_arw6.cpp`, LibRaw master after 0.22.2, pull request #824 and commit e419de0): Copyright 2026 LibRaw LLC, Copyright (C) 2026 Jiang Yaogeng (code generated with assistance from GLM-5). LibRaw distributes it under LGPL 2.1 or CDDL 1.0; the port is used under LGPL 2.1, like the rest of the vendored rawler. The notices are kept in the file header.

## libjpeg-turbo
JPEG exports are encoded with libjpeg-turbo, built from the source bundled in the `turbojpeg-sys` crate (crates `turbojpeg`/`turbojpeg-sys`: Unlicense OR MIT). libjpeg-turbo is covered by the IJG License and the Modified (3-clause) BSD License (SIMD code: zlib License); its LICENSE.md and README.ijg ship with that crate. This software is based in part on the work of the Independent JPEG Group.

## AVIF decoding
AVIF sources are decoded with [rav1d](https://github.com/memorysafety/rav1d) 1.1.0, the Rust port of the dav1d AV1 decoder (Copyright VideoLAN, dav1d authors and Internet Security Research Group, BSD-2-Clause; built without its assembly), and their container is read with [avif-parse](https://github.com/kornelski/avif-parse) 2.1.0 (Kornel Lesiński and the Mozilla mp4parse authors, MPL-2.0, used unmodified). Both crates' license texts ship with them and in `THIRD_PARTY_LICENSES.txt`.

## Local dependency patch
`vendor/little_exif` contains little_exif 0.6.23, Copyright Tobias J. Prisching and contributors, MIT OR Apache-2.0; both license texts are preserved there. Only two dependencies were changed: quick-xml from 0.37.5 to 0.41.0 to remove known XML denial-of-service advisories, and the unmaintained `paste` macro crate for its maintained drop-in successor `pastey` 0.2 (MIT OR Apache-2.0; `use pastey::paste`) (ADR 0005). It is patched through the workspace Cargo.toml.

More crates are patched the same way to keep unmaintained crates out of the build (ADR 0005):
- `vendor/paste` is a Rawtonic stand-in under the `paste` 1.0.15 name for crates that still depend on `paste` (pulp, through image → exr). It contains no code of `paste`; it re-exports the macros of `pastey` 0.2 (Aditya Kumar and David Tolnay, MIT OR Apache-2.0; both license texts kept there).
- `vendor/glib-macros` contains glib-macros 0.18.5 and `vendor/gtk3-macros` gtk3-macros 0.18.2 (Copyright the gtk-rs Project Developers, MIT; LICENSE and COPYRIGHT preserved), the proc-macro crates of the GTK 3 bindings Tauri uses on Linux. Both no longer depend on proc-macro-error (or its fork proc-macro-error2, also unmaintained): the few parts they used are reimplemented in each crate's `src/abort.rs` (MIT, like the crates); the changes are listed in their `RAWTONIC_PATCH.md`.
- `vendor/tauri-plugin-single-instance` contains tauri-plugin-single-instance 2.5.4 (Copyright Tauri Programme within The Commons Conservancy, Apache-2.0 OR MIT; both license texts and LICENSE.spdx preserved). Its Linux D-Bus code uses zbus 6 instead of zbus 5, whose zvariant, zvariant_derive and zbus_names are unmaintained since zbus 6 merged them; the changes are listed in its `RAWTONIC_PATCH.md`.

Original Rawtonic work permits AGPL v3 or later. Inherited AGPL-3.0-only components, including jxl-encoder, retain their original terms; the combined development distribution uses AGPL v3. Our license does not relicense those dependencies.

## DNG
This product includes DNG technology under license by Adobe. Rawtonic reads files that comply with Adobe's Digital Negative (DNG) Specification under Adobe's royalty-free DNG Specification patent license (`licenses/Adobe-DNG-Specification-Patent-License.txt`). Rawtonic contains no Adobe source code.

## ONNX Runtime and its GPU plugins
The opt-in AI build (`RAWTONIC_BUNDLE_ONNX=1`) bundles Microsoft's [ONNX Runtime](https://github.com/microsoft/onnxruntime) library, unchanged: 1.23.2 on macOS, 1.30.0 on Windows and Linux. MIT (`licenses/models/onnxruntime-MIT.txt`), with its third-party notices (`licenses/models/onnxruntime-1.23.2-ThirdPartyNotices.txt` for 1.23.2, `licenses/models/onnxruntime-1.30.0-ThirdPartyNotices.txt` for 1.30.0); the build copies both into the app's `licenses` folder. On Windows and Linux it also bundles Microsoft's WebGPU plugin execution provider 0.4.0 (`onnxruntime_providers_webgpu`, MIT, from the `onnxruntime-ep-webgpu` package), which contains Google's [Dawn](https://dawn.googlesource.com/dawn) (BSD-3-Clause) and the other components listed in the 1.30.0 notices. The DirectX Shader Compiler libraries (`dxcompiler.dll`, `dxil.dll`, Microsoft Software License Terms) in that package's Windows build are not distributed. NVIDIA's CUDA plugin (Microsoft, MIT) is an optional download (`scripts/models/install_models.sh --cuda`); it loads the CUDA and cuDNN libraries the user installed under NVIDIA's terms, none of which Rawtonic distributes. Choice and licenses: `docs/decisions/0011-gpu-ai.md`.

## libgphoto2 (tethered shooting)
Tethered shooting talks to cameras through [libgphoto2](https://github.com/gphoto/libgphoto2) 2.5.34 (libgphoto2 and libgphoto2_port, Copyright the gphoto project, Lutz Müller, Marcus Meissner and contributors), [libusb](https://libusb.info/) 1.0.30 (Linux only; Copyright Johannes Erdfelt, Daniel Drake, Peter Stuge, Nathan Hjelm, Chris Dickens and contributors) and libltdl from [GNU Libtool](https://www.gnu.org/software/libtool/) 2.6.2 (Copyright Free Software Foundation, Inc.), all GNU LGPL v2.1 or later (`licenses/LGPL-2.1.txt`). They run only in the separate helper program `rawtonic-tether`, never in the app, and are linked dynamically: the app bundle carries them as separate shared libraries (macOS: `Contents/Frameworks`, the camera driver `ptp2` and the port driver in `Contents/Resources/gphoto2`), so they can be replaced with other builds of the same versions. They are built unmodified from the upstream release tarballs by `scripts/tether/build_libgphoto2.sh` (download addresses and SHA-256 checksums in that script; every release that ships them also offers those source tarballs from the same place as the app, as release assets), configured with only the PTP camera driver and one port driver — no network (PTP/IP), serial or disk port drivers. On Linux the port driver is libgphoto2's USB driver `usb1` on libusb; on macOS it is Rawtonic's own `icc` port driver (part of Rawtonic, AGPL), which passes PTP through macOS's ImageCaptureCore, and libusb is not shipped. The helper's Rust bindings are the `gphoto2` and `gphoto2-sys` crates from [CyberTimon/libgphoto2-rs](https://github.com/CyberTimon/libgphoto2-rs) (commit 878ca523, a fork of vibe-libgphoto2-rs; MIT OR Apache-2.0, vibe-libgphoto2-rs contributors).

## Data and assets
**Lensfun database** in `src-tauri/lensfun_db`: Lensfun contributors, CC BY-SA 3.0 (`src-tauri/lensfun_db/COPYING.CC_BY-SA_3.0`, `licenses/Lensfun-CC-BY-SA-3.0.txt`, https://lensfun.github.io/license/). Taken unmodified from the Lensfun repository (https://github.com/lensfun/lensfun, `data/db`) at commit 43f9e001ab66c4fcdd4f400463f13b72f94b2288 (2026-10-08). It is shipped as separate data files; no Lensfun library code is linked. Changes to the database files are listed in their git history and stay under CC BY-SA 3.0.

**spektrafilm LUTs** in `src-tauri/resources/film_luts`: Andrea Volpato, CC BY-SA 4.0 (`SPEKTRAFILM_LICENSE.txt`, unmodified). The UI labels and TITLE headers use descriptive names without stock-like numbers; filenames remain stable identifiers for saved edits. Header lines naming film and print stocks were removed; the LUT data is unchanged (`CHANGELOG.txt` in that folder).

**Camera calibration values.** Per-camera colour matrices (`crates/rawdecode/src/camera_matrices.rs`), baseline exposures (`camera_exposure.rs`) and white-balance references (`unit_calibration_table.rs`) are numerical facts read from DNG files written by Adobe DNG Converter for sample RAW files, the same source dcraw, LibRaw and rawler use, and from Rawtonic's own measurements. The CHDK raw geometry and colour matrices of the Canon PowerShot SX710 HS, SX20 IS and IXUS 160 (`third_party/rawler/data/cameras/canon/`) are numerical facts from LibRaw's tables (`src/metadata/identify.cpp`, `src/tables/colordata.cpp`, LibRaw LLC, LGPL 2.1 / CDDL 1.0); the PowerShot Pro70's mosaic, levels and matrices from Adobe DNG Converter 18.7's DNG of a raw.pixls.us sample. The cameras' own lens correction data are read from each RAW file itself; the layout of Panasonic's lateral CA data (RW2 tag 0x011b, undocumented) was worked out from Rawtonic's own measurements of lateral CA on raw.pixls.us sample files, whose tag values (one GX80 frame) also serve as a test case. Rawtonic claims no copyright in these values. Tone and colour behaviour compared with other editors was measured by observing their output on test files; no code, profiles or look files of those editors are included.

**MediaPipe face topology.** `crates/engine/src/relight_face_topology.rs` contains triangle indices from Google's [canonical face mesh](https://raw.githubusercontent.com/google-ai-edge/mediapipe/a908d668c730da128dfa8d9f6bd25d519d006692/mediapipe/modules/face_geometry/data/canonical_face_model.obj), Copyright 2020 The MediaPipe Authors, Apache-2.0 (`licenses/spdx/Apache-2.0.txt`, included in `THIRD_PARTY_LICENSES.txt`). Indices are converted to zero-based Rust arrays; canonical positions are retained only in `relight_face_fixture.rs` for tests, and textures are not included. Runtime Relight uses locally inferred face geometry.

**Depth Anything 3 export.** `scripts/models/relight_da3_dynamic.patch` adapts ByteDance Seed's Depth Anything 3 source at `3d835ec1a5802d64a8b8b15f817a1ab54809bfe4` for dynamic ONNX export. Copyright (c) 2025 ByteDance Ltd. and/or its affiliates, and Meta Platforms, Inc. and affiliates (the inherited DINOv2 backbone), Apache-2.0; upstream copyright notices remain in the patched source. The DA3 Base weights are explicitly Apache-2.0 in the rights holder's `depth-anything/DA3-BASE` model card. See `docs/MODELS.md` and `licenses/models/Depth-Anything-3-Apache-2.0.txt`. No weights are included in this repository.

**DAViD.** The Relight people-normal model is Microsoft's `normal-model-vitb16_384.onnx` from DAViD (Data-efficient and Accurate Vision models from synthetic Data, ICCV 2025, github.com/microsoft/DAViD), hosted unchanged in the model pack. Copyright (c) Microsoft Corporation, MIT ("DAViD models and runtime code are licensed under the MIT License"); it was trained on Microsoft's synthetic SynthHuman data (CDLA-2.0). `src-tauri/src/ai_processing/relight.rs` prepares its input as DAViD's runtime does (own code). See `docs/MODELS.md` and `licenses/models/DAViD-MIT.txt`. No weights are included in this repository.

**MatchFilter Core ML conversion.** `scripts/models/convert_nlmf_coreml.py` builds AI Denoise version 2's Core ML program from the authors' Nonlocal MatchFilter checkpoint (Marco Sánchez Beeckman and Antoni Buades, github.com/MIA-UIB/nonlocal-matchfilter, MIT). At conversion time it downloads the pure-PyTorch network and static packed sampler that RapidRAW vendors (github.com/sheldonxxxx/RapidRAW `denoise/rapidraw_denoise/` at commit 2375e17c6077e2a6bb3352f913f0d6dc894e222f: the authors' network under MIT with their notice, the sampler under GNU AGPL v3); neither is included in this repository or the app. See `docs/MODELS.md` and `licenses/models/nonlocal-matchfilter-MIT.txt`.

**AI models** are not part of the source or the app; they download on first use from Rawtonic's model pack, each under its own license (MIT or Apache-2.0). Origins, hashes and licenses: `docs/MODELS.md`, license texts in `licenses/models/`.

**Benchmark fixtures** are not part of the source or the app: `scripts/bench/fetch_fixtures.sh` downloads three sample RAW files from raw.pixls.us (CC0) for the hot-path benchmarks (docs/DEVELOPMENT.md "Benchmarks"); criterion (Apache-2.0 OR MIT), the benchmark harness, is a development dependency only and is not shipped.

**Icons and fonts** come with the inherited frontend; see the dependency inventory below for the npm packages that ship them.

Other open-source projects studied for ideas are not distributed; no code was taken from them unless listed above.

## Dependency inventory
Resolved npm and Cargo version/license inventories are below, including build/test and target-specific dependencies. Missing, custom or ambiguous licenses are release blockers, not implicit approvals. Lockfiles pin the shipped dependency set. The full license and notice texts of every shipped package are collected by `scripts/third_party_licenses.py` into `THIRD_PARTY_LICENSES.txt`, which every `tauri build` bundles and About shows; the build fails if a package's license text is missing.













<!-- generated inventory -->

## Resolved Rust dependencies

| Package | Version | Declared license |
|---|---|---|
| ab_glyph_rasterizer | 0.1.10 | Apache-2.0 |
| addr2line | 0.25.1 | Apache-2.0 OR MIT |
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| adler32 | 1.2.0 | Zlib |
| ahash | 0.8.12 | MIT OR Apache-2.0 |
| aho-corasick | 1.1.5 | Unlicense OR MIT |
| aligned | 0.4.3 | MIT OR Apache-2.0 |
| aligned-vec | 0.6.4 | MIT |
| alloc-no-stdlib | 2.0.4 | BSD-3-Clause |
| alloc-no-stdlib | 3.0.0 | BSD-3-Clause |
| alloc-stdlib | 0.2.4 | BSD-3-Clause |
| alloc-stdlib | 0.3.0 | BSD-3-Clause |
| alloca | 0.4.0 | MIT |
| allocator-api2 | 0.2.21 | MIT OR Apache-2.0 |
| android_system_properties | 0.1.6 | MIT OR Apache-2.0 |
| anes | 0.1.6 | MIT OR Apache-2.0 |
| anstream | 1.0.0 | MIT OR Apache-2.0 |
| anstyle | 1.0.14 | MIT OR Apache-2.0 |
| anstyle-parse | 1.0.0 | MIT OR Apache-2.0 |
| anstyle-query | 1.1.5 | MIT OR Apache-2.0 |
| anstyle-wincon | 3.0.11 | MIT OR Apache-2.0 |
| anyhow | 1.0.104 | MIT OR Apache-2.0 |
| approx | 0.5.1 | Apache-2.0 |
| arbitrary | 1.4.2 | MIT OR Apache-2.0 |
| archmage | 0.9.28 | MIT OR Apache-2.0 |
| archmage-macros | 0.9.28 | MIT OR Apache-2.0 |
| arg_enum_proc_macro | 0.3.4 | MIT |
| arrayvec | 0.7.8 | MIT OR Apache-2.0 |
| as-slice | 0.2.1 | MIT OR Apache-2.0 |
| ash | 0.38.0+1.3.281 | MIT OR Apache-2.0 |
| assert_matches | 1.5.0 | MIT/Apache-2.0 |
| async-trait | 0.1.92 | MIT OR Apache-2.0 |
| atk | 0.18.2 | MIT |
| atk-sys | 0.18.2 | MIT |
| atomic-waker | 1.1.2 | Apache-2.0 OR MIT |
| atomig | 0.4.3 | MIT/Apache-2.0 |
| atomig-macro | 0.4.0 | MIT/Apache-2.0 |
| autocfg | 1.5.1 | Apache-2.0 OR MIT |
| av-scenechange | 0.14.1 | MIT |
| av1-grain | 0.2.5 | BSD-2-Clause |
| avif-parse | 2.1.0 | MPL-2.0 |
| avif-serialize | 0.8.9 | BSD-3-Clause |
| aws-lc-rs | 1.18.0 | ISC AND (Apache-2.0 OR ISC) |
| aws-lc-sys | 0.44.0 | ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0) |
| backtrace | 0.3.76 | MIT OR Apache-2.0 |
| base64 | 0.13.1 | MIT/Apache-2.0 |
| base64 | 0.21.7 | MIT OR Apache-2.0 |
| base64 | 0.22.1 | MIT OR Apache-2.0 |
| base64 | 0.23.1 | MIT OR Apache-2.0 |
| bindgen | 0.71.1 | BSD-3-Clause |
| bit-set | 0.8.0 | Apache-2.0 OR MIT |
| bit-set | 0.9.1 | Apache-2.0 OR MIT |
| bit-vec | 0.8.0 | Apache-2.0 OR MIT |
| bit-vec | 0.9.1 | Apache-2.0 OR MIT |
| bit_field | 0.10.3 | Apache-2.0/MIT |
| bitflags | 1.3.2 | MIT/Apache-2.0 |
| bitflags | 2.13.2 | MIT OR Apache-2.0 |
| bitreader | 0.3.11 | MIT OR Apache-2.0 |
| bitstream-io | 4.10.0 | MIT/Apache-2.0 |
| blake3 | 1.8.7 | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 |
| block-buffer | 0.12.1 | MIT OR Apache-2.0 |
| block2 | 0.6.2 | MIT |
| brotli | 8.0.4 | BSD-3-Clause AND MIT |
| brotli | 9.0.0 | BSD-3-Clause AND MIT |
| brotli-decompressor | 5.0.3 | BSD-3-Clause/MIT |
| brotli-decompressor | 6.0.1 | BSD-3-Clause/MIT |
| bs58 | 0.5.1 | MIT/Apache-2.0 |
| built | 0.8.1 | MIT |
| bumpalo | 3.20.3 | MIT OR Apache-2.0 |
| butteraugli | 0.9.3 | BSD-3-Clause |
| bytemuck | 1.25.2 | Zlib OR Apache-2.0 OR MIT |
| bytemuck_derive | 1.12.0 | Zlib OR Apache-2.0 OR MIT |
| byteorder | 1.5.0 | Unlicense OR MIT |
| byteorder-lite | 0.1.0 | Unlicense OR MIT |
| bytes | 1.12.1 | MIT |
| cairo-rs | 0.18.5 | MIT |
| cairo-sys-rs | 0.18.2 | MIT |
| camino | 1.2.5 | MIT OR Apache-2.0 |
| cargo-platform | 0.1.9 | MIT OR Apache-2.0 |
| cargo_metadata | 0.19.2 | MIT |
| cargo_toml | 1.0.1 | Apache-2.0 OR MIT |
| cast | 0.3.0 | MIT OR Apache-2.0 |
| castaway | 0.2.4 | MIT |
| cc | 1.4.4 | MIT OR Apache-2.0 |
| cesu8 | 1.1.0 | Apache-2.0/MIT |
| cexpr | 0.6.0 | Apache-2.0/MIT |
| cfb | 0.14.0 | MIT |
| cfg-expr | 0.15.8 | MIT OR Apache-2.0 |
| cfg-if | 1.0.4 | MIT OR Apache-2.0 |
| cfg_aliases | 0.2.2 | MIT |
| chacha20 | 0.10.2 | MIT OR Apache-2.0 |
| chrono | 0.4.45 | MIT OR Apache-2.0 |
| ciborium | 0.2.2 | Apache-2.0 |
| ciborium-io | 0.2.2 | Apache-2.0 |
| ciborium-ll | 0.2.2 | Apache-2.0 |
| clang-sys | 1.9.1 | Apache-2.0 |
| clap | 4.6.7 | MIT OR Apache-2.0 |
| clap_builder | 4.6.7 | MIT OR Apache-2.0 |
| clap_derive | 4.6.7 | MIT OR Apache-2.0 |
| clap_lex | 1.1.1 | MIT OR Apache-2.0 |
| cmake | 0.1.58 | MIT OR Apache-2.0 |
| codespan-reporting | 0.13.1 | Apache-2.0 |
| color_quant | 1.1.0 | MIT |
| colorchoice | 1.0.5 | MIT OR Apache-2.0 |
| combine | 4.6.7 | MIT |
| compact_str | 0.9.1 | MIT |
| const-oid | 0.10.2 | Apache-2.0 OR MIT |
| constant_time_eq | 0.4.2 | CC0-1.0 OR MIT-0 OR Apache-2.0 |
| cookie | 0.18.2 | MIT OR Apache-2.0 |
| core-foundation | 0.10.1 | MIT OR Apache-2.0 |
| core-foundation-sys | 0.8.7 | MIT OR Apache-2.0 |
| core-graphics | 0.25.0 | MIT OR Apache-2.0 |
| core-graphics-types | 0.2.0 | MIT OR Apache-2.0 |
| cpufeatures | 0.2.17 | MIT OR Apache-2.0 |
| cpufeatures | 0.3.0 | MIT OR Apache-2.0 |
| crc | 3.4.0 | MIT OR Apache-2.0 |
| crc-catalog | 2.5.0 | MIT OR Apache-2.0 |
| crc32fast | 1.5.0 | MIT OR Apache-2.0 |
| criterion | 0.8.2 | Apache-2.0 OR MIT |
| criterion-plot | 0.8.2 | Apache-2.0 OR MIT |
| crossbeam-channel | 0.5.16 | MIT OR Apache-2.0 |
| crossbeam-deque | 0.8.7 | MIT OR Apache-2.0 |
| crossbeam-epoch | 0.9.20 | MIT OR Apache-2.0 |
| crossbeam-utils | 0.8.22 | MIT OR Apache-2.0 |
| crunchy | 0.2.4 | MIT |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 |
| crypto-common | 0.2.2 | MIT OR Apache-2.0 |
| cssparser | 0.37.0 | MPL-2.0 |
| cssparser-macros | 0.7.1 | MPL-2.0 |
| ctor | 1.0.13 | Apache-2.0 OR MIT |
| cty | 0.2.2 | MIT OR Apache-2.0 |
| daachorse | 1.0.1 | MIT OR Apache-2.0 |
| darling | 0.20.11 | MIT |
| darling | 0.23.0 | MIT |
| darling_core | 0.20.11 | MIT |
| darling_core | 0.23.0 | MIT |
| darling_macro | 0.20.11 | MIT |
| darling_macro | 0.23.0 | MIT |
| dary_heap | 0.3.9 | MIT OR Apache-2.0 |
| dbus | 0.9.12 | Apache-2.0/MIT |
| defmt | 1.1.1 | MIT OR Apache-2.0 |
| defmt-macros | 1.1.1 | MIT OR Apache-2.0 |
| defmt-parser | 1.0.0 | MIT OR Apache-2.0 |
| deranged | 0.5.8 | MIT OR Apache-2.0 |
| derive_builder | 0.20.2 | MIT OR Apache-2.0 |
| derive_builder_core | 0.20.2 | MIT OR Apache-2.0 |
| derive_builder_macro | 0.20.2 | MIT OR Apache-2.0 |
| derive_more | 2.1.1 | MIT |
| derive_more-impl | 2.1.1 | MIT |
| digest | 0.10.7 | MIT OR Apache-2.0 |
| digest | 0.11.3 | MIT OR Apache-2.0 |
| dirs | 7.0.0 | MIT OR Apache-2.0 |
| dirs-sys | 0.5.0 | MIT OR Apache-2.0 |
| dispatch2 | 0.3.1 | Zlib OR Apache-2.0 OR MIT |
| displaydoc | 0.2.7 | MIT OR Apache-2.0 |
| dlib | 0.5.3 | MIT |
| dlopen2 | 0.8.2 | MIT |
| dlopen2_derive | 0.4.3 | MIT |
| document-features | 0.2.12 | MIT OR Apache-2.0 |
| dom_query | 0.28.0 | MIT |
| dpi | 0.1.2 | Apache-2.0 AND MIT |
| dtoa | 1.0.11 | MIT OR Apache-2.0 |
| dtoa-short | 0.3.5 | MPL-2.0 |
| dunce | 1.0.5 | CC0-1.0 OR MIT-0 OR Apache-2.0 |
| dyn-clone | 1.0.20 | MIT OR Apache-2.0 |
| either | 1.18.0 | MIT OR Apache-2.0 |
| embed-resource | 3.0.11 | MIT |
| embed_plist | 1.2.2 | MIT OR Apache-2.0 |
| encoding_rs | 0.8.35 | (Apache-2.0 OR MIT) AND BSD-3-Clause |
| endi | 1.1.1 | MIT |
| enough | 0.4.4 | MIT OR Apache-2.0 |
| enumn | 0.1.14 | MIT OR Apache-2.0 |
| env_filter | 2.0.0 | MIT OR Apache-2.0 |
| env_logger | 0.11.11 | MIT OR Apache-2.0 |
| equator | 0.4.2 | MIT |
| equator-macro | 0.4.2 | MIT |
| equivalent | 1.0.2 | Apache-2.0 OR MIT |
| erased-serde | 0.4.10 | MIT OR Apache-2.0 |
| errno | 0.3.14 | MIT OR Apache-2.0 |
| esaxx-rs | 0.1.10 | Apache-2.0 |
| exr | 1.74.2 | BSD-3-Clause |
| fallible-iterator | 0.3.0 | MIT/Apache-2.0 |
| fallible-streaming-iterator | 0.1.9 | MIT/Apache-2.0 |
| fallible_collections | 0.5.2 | MIT OR Apache-2.0 |
| fastrand | 2.5.0 | Apache-2.0 OR MIT |
| fax | 0.2.7 | MIT |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 |
| fern | 0.7.1 | MIT |
| field-offset | 0.3.6 | MIT OR Apache-2.0 |
| filetime | 0.2.29 | MIT/Apache-2.0 |
| find-msvc-tools | 0.1.11 | MIT OR Apache-2.0 |
| flate2 | 1.1.9 | MIT OR Apache-2.0 |
| fnv | 1.0.7 | Apache-2.0 / MIT |
| foldhash | 0.1.5 | Zlib |
| foldhash | 0.2.0 | Zlib |
| font-types | 0.12.7 | MIT OR Apache-2.0 |
| fontdb | 0.24.0 | MIT |
| foreign-types | 0.5.0 | MIT/Apache-2.0 |
| foreign-types-macros | 0.2.4 | MIT/Apache-2.0 |
| foreign-types-shared | 0.3.1 | MIT/Apache-2.0 |
| form_urlencoded | 1.2.2 | MIT OR Apache-2.0 |
| fs_extra | 1.3.0 | MIT |
| futures | 0.3.34 | MIT OR Apache-2.0 |
| futures-channel | 0.3.34 | MIT OR Apache-2.0 |
| futures-core | 0.3.34 | MIT OR Apache-2.0 |
| futures-executor | 0.3.34 | MIT OR Apache-2.0 |
| futures-io | 0.3.34 | MIT OR Apache-2.0 |
| futures-macro | 0.3.34 | MIT OR Apache-2.0 |
| futures-sink | 0.3.34 | MIT OR Apache-2.0 |
| futures-task | 0.3.34 | MIT OR Apache-2.0 |
| futures-util | 0.3.34 | MIT OR Apache-2.0 |
| fuzzy-matcher | 0.3.7 | MIT |
| gcd | 2.3.0 | MIT/Apache-2.0 |
| gdk | 0.18.2 | MIT |
| gdk-pixbuf | 0.18.5 | MIT |
| gdk-pixbuf-sys | 0.18.0 | MIT |
| gdk-sys | 0.18.2 | MIT |
| gdkwayland-sys | 0.18.2 | MIT |
| gdkx11 | 0.18.2 | MIT |
| gdkx11-sys | 0.18.2 | MIT |
| generic-array | 0.14.7 | MIT |
| gethostname | 1.1.0 | Apache-2.0 |
| getrandom | 0.2.17 | MIT OR Apache-2.0 |
| getrandom | 0.3.4 | MIT OR Apache-2.0 |
| getrandom | 0.4.3 | MIT OR Apache-2.0 |
| gif | 0.14.2 | MIT OR Apache-2.0 |
| gimli | 0.32.3 | MIT OR Apache-2.0 |
| gio | 0.18.4 | MIT |
| gio-sys | 0.18.1 | MIT |
| gl_generator | 0.14.0 | Apache-2.0 |
| glam | 0.30.10 | MIT OR Apache-2.0 |
| glam | 0.31.1 | MIT OR Apache-2.0 |
| glam | 0.32.1 | MIT OR Apache-2.0 |
| glam | 0.33.5 | MIT OR Apache-2.0 |
| glib | 0.18.5 | MIT |
| glib-macros | 0.18.5 | MIT |
| glib-sys | 0.18.1 | MIT |
| glob | 0.3.4 | MIT OR Apache-2.0 |
| glow | 0.17.0 | MIT OR Apache-2.0 OR Zlib |
| glutin_wgl_sys | 0.6.1 | Apache-2.0 |
| gobject-sys | 0.18.0 | MIT |
| gphoto2 | 0.1.0 | MIT OR Apache-2.0 |
| gphoto2-sys | 0.1.0 | MIT OR Apache-2.0 |
| gpu-allocator | 0.28.0 | MIT OR Apache-2.0 |
| gpu-descriptor | 0.3.2 | MIT OR Apache-2.0 |
| gpu-descriptor-types | 0.2.0 | MIT OR Apache-2.0 |
| gtk | 0.18.2 | MIT |
| gtk-sys | 0.18.2 | MIT |
| gtk3-macros | 0.18.2 | MIT |
| half | 2.7.1 | MIT OR Apache-2.0 |
| hashbrown | 0.12.3 | MIT OR Apache-2.0 |
| hashbrown | 0.15.5 | MIT OR Apache-2.0 |
| hashbrown | 0.16.1 | MIT OR Apache-2.0 |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 |
| hashlink | 0.12.2 | MIT OR Apache-2.0 |
| heck | 0.4.1 | MIT OR Apache-2.0 |
| heck | 0.5.0 | MIT OR Apache-2.0 |
| hex | 0.4.3 | MIT OR Apache-2.0 |
| hexf-parse | 0.2.1 | CC0-1.0 |
| html5ever | 0.39.0 | MIT OR Apache-2.0 |
| http | 1.5.0 | MIT OR Apache-2.0 |
| http-body | 1.1.0 | MIT |
| http-body-util | 0.1.5 | MIT |
| http-range | 0.1.5 | MIT |
| httparse | 1.10.1 | MIT OR Apache-2.0 |
| hybrid-array | 0.4.14 | MIT OR Apache-2.0 |
| hyper | 1.11.0 | MIT |
| hyper-rustls | 0.27.9 | Apache-2.0 OR ISC OR MIT |
| hyper-util | 0.1.20 | MIT |
| iana-time-zone | 0.1.65 | MIT OR Apache-2.0 |
| iana-time-zone-haiku | 0.1.2 | MIT OR Apache-2.0 |
| ico | 0.5.0 | MIT |
| icu_collections | 2.3.0 | Unicode-3.0 |
| icu_locale_core | 2.3.0 | Unicode-3.0 |
| icu_normalizer | 2.3.0 | Unicode-3.0 |
| icu_normalizer_data | 2.3.0 | Unicode-3.0 |
| icu_properties | 2.3.0 | Unicode-3.0 |
| icu_properties_data | 2.3.0 | Unicode-3.0 |
| icu_provider | 2.3.1 | Unicode-3.0 |
| ident_case | 1.0.1 | MIT/Apache-2.0 |
| idna | 1.1.0 | MIT OR Apache-2.0 |
| idna_adapter | 1.2.2 | Apache-2.0 OR MIT |
| image | 0.25.10 | MIT OR Apache-2.0 |
| image-webp | 0.2.4 | MIT OR Apache-2.0 |
| image_hasher | 3.1.1 | MIT OR Apache-2.0 |
| imageproc | 0.27.0 | MIT |
| imgref | 1.12.2 | CC0-1.0 OR Apache-2.0 |
| indexmap | 1.9.3 | Apache-2.0 OR MIT |
| indexmap | 2.14.0 | Apache-2.0 OR MIT |
| infer | 0.22.0 | MIT |
| interpolate_name | 0.2.4 | MIT |
| ipnet | 2.12.1 | MIT OR Apache-2.0 |
| is-docker | 0.2.0 | MIT |
| is-wsl | 0.4.0 | MIT |
| is_terminal_polyfill | 1.70.2 | MIT OR Apache-2.0 |
| itertools | 0.13.0 | MIT OR Apache-2.0 |
| itertools | 0.14.0 | MIT OR Apache-2.0 |
| itoa | 1.0.18 | MIT OR Apache-2.0 |
| javascriptcore-rs | 1.1.2 | MIT |
| javascriptcore-rs-sys | 1.1.1 | MIT |
| jiff | 0.2.35 | Unlicense OR MIT |
| jiff-core | 0.1.0 | Unlicense OR MIT |
| jiff-static | 0.2.35 | Unlicense OR MIT |
| jiff-tzdb | 0.1.8 | Unlicense OR MIT |
| jiff-tzdb-platform | 0.1.3 | Unlicense OR MIT |
| jni | 0.21.1 | MIT/Apache-2.0 |
| jni | 0.22.4 | MIT OR Apache-2.0 |
| jni-macros | 0.22.4 | MIT OR Apache-2.0 |
| jni-sys | 0.3.1 | MIT OR Apache-2.0 |
| jni-sys | 0.4.1 | MIT OR Apache-2.0 |
| jni-sys-macros | 0.4.1 | MIT OR Apache-2.0 |
| jobserver | 0.1.35 | MIT OR Apache-2.0 |
| js-sys | 0.3.104 | MIT OR Apache-2.0 |
| json-patch | 4.2.0 | MIT/Apache-2.0 |
| jsonptr | 0.7.1 | MIT OR Apache-2.0 |
| jxl-bitstream | 1.1.0 | MIT OR Apache-2.0 |
| jxl-coding | 1.0.1 | MIT OR Apache-2.0 |
| jxl-color | 0.11.0 | MIT OR Apache-2.0 |
| jxl-encoder | 0.3.1 | AGPL-3.0-only OR LicenseRef-Imazen-Commercial |
| jxl-encoder-simd | 0.3.0 | AGPL-3.0-only OR LicenseRef-Imazen-Commercial |
| jxl-frame | 0.13.3 | MIT OR Apache-2.0 |
| jxl-grid | 0.6.2 | MIT OR Apache-2.0 |
| jxl-image | 0.13.0 | MIT OR Apache-2.0 |
| jxl-jbr | 0.2.1 | MIT OR Apache-2.0 |
| jxl-modular | 0.11.3 | MIT OR Apache-2.0 |
| jxl-oxide | 0.12.6 | MIT OR Apache-2.0 |
| jxl-oxide-common | 1.0.0 | MIT OR Apache-2.0 |
| jxl-render | 0.12.4 | MIT OR Apache-2.0 |
| jxl-threadpool | 1.0.0 | MIT OR Apache-2.0 |
| jxl-vardct | 0.11.1 | MIT OR Apache-2.0 |
| kamadak-exif | 0.6.1 | BSD-2-Clause |
| keyboard-types | 0.8.3 | MIT OR Apache-2.0 |
| khronos-egl | 6.0.0 | MIT/Apache-2.0 |
| khronos_api | 3.1.0 | Apache-2.0 |
| lazy_static | 1.5.0 | MIT OR Apache-2.0 |
| leb128 | 0.2.7 | MIT OR Apache-2.0 |
| lebe | 0.5.3 | BSD-3-Clause |
| libappindicator | 0.9.0 | Apache-2.0 OR MIT |
| libappindicator-sys | 0.9.0 | Apache-2.0 OR MIT |
| libc | 0.2.189 | MIT OR Apache-2.0 |
| libdbus-sys | 0.2.7 | Apache-2.0/MIT |
| libflate | 2.3.1 | MIT |
| libflate_lz77 | 2.3.0 | MIT |
| libfuzzer-sys | 0.4.13 | (MIT OR Apache-2.0) AND NCSA |
| libloading | 0.7.4 | ISC |
| libloading | 0.8.9 | ISC |
| libloading | 0.9.0 | ISC |
| libm | 0.2.16 | MIT |
| libmimalloc-sys | 0.1.49 | MIT |
| libredox | 0.1.20 | MIT |
| libsqlite3-sys | 0.38.2 | MIT |
| libwebp-sys | 0.9.6 | MIT |
| linear-srgb | 0.6.12 | MIT OR Apache-2.0 |
| linux-raw-sys | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| litemap | 0.8.3 | Unicode-3.0 |
| litrs | 1.0.0 | MIT OR Apache-2.0 |
| little_exif | 0.6.23 | MIT OR Apache-2.0 |
| lock_api | 0.4.14 | MIT OR Apache-2.0 |
| log | 0.4.34 | MIT OR Apache-2.0 |
| loop9 | 0.1.5 | MIT |
| lru-slab | 0.1.2 | MIT OR Apache-2.0 OR Zlib |
| macro_rules_attribute | 0.2.3 | Apache-2.0 OR MIT OR Zlib |
| macro_rules_attribute-proc_macro | 0.2.3 | Apache-2.0 OR MIT OR Zlib |
| magetypes | 0.9.26 | MIT OR Apache-2.0 |
| malloc_buf | 0.0.6 | MIT |
| markup5ever | 0.39.0 | MIT OR Apache-2.0 |
| matrixmultiply | 0.3.11 | MIT/Apache-2.0 |
| maybe-rayon | 0.1.1 | MIT |
| md5 | 0.8.1 | Apache-2.0 OR MIT |
| memchr | 2.8.3 | Unlicense OR MIT |
| memmap2 | 0.9.11 | MIT OR Apache-2.0 |
| memoffset | 0.9.1 | MIT |
| mimalloc | 0.1.52 | MIT |
| mime | 0.3.17 | MIT OR Apache-2.0 |
| mime_guess | 2.0.5 | MIT |
| minimal-lexical | 0.2.1 | MIT/Apache-2.0 |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 |
| mio | 1.2.2 | MIT |
| monostate | 0.1.18 | MIT OR Apache-2.0 |
| monostate-impl | 0.1.18 | MIT OR Apache-2.0 |
| moxcms | 0.8.1 | BSD-3-Clause OR Apache-2.0 |
| mozjpeg-rs | 0.9.2 | BSD-3-Clause |
| muda | 0.20.0 | Apache-2.0 OR MIT |
| multiversion | 0.8.0 | MIT OR Apache-2.0 |
| multiversion-macros | 0.8.0 | MIT OR Apache-2.0 |
| mutate_once | 0.1.2 | BSD-2-Clause |
| naga | 29.0.4 | MIT OR Apache-2.0 |
| nalgebra | 0.35.0 | Apache-2.0 |
| nalgebra-macros | 0.3.0 | Apache-2.0 |
| nasm-rs | 0.3.2 | MIT OR Apache-2.0 |
| ndarray | 0.17.2 | MIT OR Apache-2.0 |
| ndk | 0.9.0 | MIT OR Apache-2.0 |
| ndk-context | 0.1.1 | MIT OR Apache-2.0 |
| ndk-sys | 0.6.0+11769913 | MIT OR Apache-2.0 |
| new_debug_unreachable | 1.0.6 | MIT |
| nix | 0.31.3 | MIT |
| no_std_io2 | 0.9.4 | Apache-2.0 OR MIT |
| nom | 7.1.3 | MIT |
| nom | 8.0.0 | MIT |
| noop_proc_macro | 0.3.0 | MIT |
| ntapi | 0.4.3 | Apache-2.0 OR MIT |
| num | 0.4.3 | MIT OR Apache-2.0 |
| num-bigint | 0.4.8 | MIT OR Apache-2.0 |
| num-complex | 0.4.6 | MIT OR Apache-2.0 |
| num-conv | 0.2.2 | MIT OR Apache-2.0 |
| num-derive | 0.4.2 | MIT OR Apache-2.0 |
| num-integer | 0.1.47 | MIT OR Apache-2.0 |
| num-iter | 0.1.46 | MIT OR Apache-2.0 |
| num-rational | 0.4.2 | MIT OR Apache-2.0 |
| num-traits | 0.2.19 | MIT OR Apache-2.0 |
| num_enum | 0.7.6 | BSD-3-Clause OR MIT OR Apache-2.0 |
| num_enum_derive | 0.7.6 | BSD-3-Clause OR MIT OR Apache-2.0 |
| objc | 0.2.7 | MIT |
| objc2 | 0.6.4 | MIT |
| objc2-app-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-cloud-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-data | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-foundation | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-graphics | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-image | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-location | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-ml | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-text | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-core-video | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-encode | 4.1.0 | MIT |
| objc2-exception-helper | 0.1.1 | Zlib OR Apache-2.0 OR MIT |
| objc2-foundation | 0.3.2 | MIT |
| objc2-image-io | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-io-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-io-surface | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-metal | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-open-directory | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-quartz-core | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-ui-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-user-notifications | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| objc2-web-kit | 0.3.2 | Zlib OR Apache-2.0 OR MIT |
| object | 0.37.3 | Apache-2.0 OR MIT |
| once_cell | 1.21.4 | MIT OR Apache-2.0 |
| once_cell_polyfill | 1.70.2 | MIT OR Apache-2.0 |
| onig | 6.5.3 | MIT |
| onig_sys | 69.9.3 | MIT |
| oorandom | 11.1.5 | MIT |
| open | 5.4.1 | MIT |
| openssl-probe | 0.2.1 | MIT OR Apache-2.0 |
| option-ext | 0.2.0 | MPL-2.0 |
| ordered-float | 5.5.0 | MIT |
| ordered-stream | 0.2.0 | MIT OR Apache-2.0 |
| ort | 2.0.0-rc.12 | MIT OR Apache-2.0 |
| ort-sys | 2.0.0-rc.12 | MIT OR Apache-2.0 |
| os_info | 3.15.0 | MIT |
| os_pipe | 1.2.3 | MIT |
| page_size | 0.6.0 | MIT/Apache-2.0 |
| pango | 0.18.3 | MIT |
| pango-sys | 0.18.0 | MIT |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 |
| parking_lot_core | 0.9.12 | MIT OR Apache-2.0 |
| paste | 1.0.15 | MIT OR Apache-2.0 |
| pastey | 0.1.1 | MIT OR Apache-2.0 |
| pastey | 0.2.3 | MIT OR Apache-2.0 |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 |
| phf | 0.13.1 | MIT |
| phf_codegen | 0.13.1 | MIT |
| phf_generator | 0.13.1 | MIT |
| phf_macros | 0.13.1 | MIT |
| phf_shared | 0.13.1 | MIT |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT |
| pkg-config | 0.3.34 | MIT OR Apache-2.0 |
| plist | 1.10.0 | MIT |
| png | 0.17.16 | MIT OR Apache-2.0 |
| png | 0.18.1 | MIT OR Apache-2.0 |
| pollster | 1.0.1 | Apache-2.0/MIT |
| portable-atomic | 1.15.0 | Apache-2.0 OR MIT |
| portable-atomic-util | 0.2.7 | Apache-2.0 OR MIT |
| potential_utf | 0.1.6 | Unicode-3.0 |
| powerfmt | 0.2.0 | MIT OR Apache-2.0 |
| ppv-lite86 | 0.2.21 | MIT OR Apache-2.0 |
| precomputed-hash | 0.1.1 | MIT |
| presser | 0.3.1 | MIT OR Apache-2.0 |
| prettyplease | 0.2.37 | MIT OR Apache-2.0 |
| primal-check | 0.3.4 | MIT OR Apache-2.0 |
| proc-macro-crate | 1.3.1 | MIT OR Apache-2.0 |
| proc-macro-crate | 2.0.2 | MIT OR Apache-2.0 |
| proc-macro-crate | 3.5.0 | MIT OR Apache-2.0 |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 |
| profiling | 1.0.18 | MIT OR Apache-2.0 |
| profiling-procmacros | 1.0.18 | MIT OR Apache-2.0 |
| proptest | 1.11.0 | MIT OR Apache-2.0 |
| pulp | 0.22.3 | MIT |
| pulp-wasm-simd-flag | 0.1.1 | MIT |
| pxfm | 0.1.30 | BSD-3-Clause OR Apache-2.0 |
| qoi | 0.4.1 | MIT/Apache-2.0 |
| quick-error | 1.2.3 | MIT/Apache-2.0 |
| quick-error | 2.0.1 | MIT/Apache-2.0 |
| quick-xml | 0.41.0 | MIT |
| quinn | 0.11.11 | MIT OR Apache-2.0 |
| quinn-proto | 0.11.17 | MIT OR Apache-2.0 |
| quinn-udp | 0.5.15 | MIT OR Apache-2.0 |
| quote | 1.0.47 | MIT OR Apache-2.0 |
| r-efi | 5.3.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| rand | 0.10.2 | MIT OR Apache-2.0 |
| rand | 0.9.5 | MIT OR Apache-2.0 |
| rand_chacha | 0.9.0 | MIT OR Apache-2.0 |
| rand_core | 0.10.1 | MIT OR Apache-2.0 |
| rand_core | 0.9.5 | MIT OR Apache-2.0 |
| rand_distr | 0.6.0 | MIT OR Apache-2.0 |
| rand_pcg | 0.10.2 | MIT OR Apache-2.0 |
| rand_xorshift | 0.4.0 | MIT OR Apache-2.0 |
| range-alloc | 0.1.5 | MIT OR Apache-2.0 |
| rav1d | 1.1.0 | BSD-2-Clause |
| rav1e | 0.8.1 | BSD-2-Clause |
| ravif | 0.13.0 | BSD-3-Clause |
| raw-cpuid | 11.6.0 | MIT |
| raw-window-handle | 0.6.2 | MIT OR Apache-2.0 OR Zlib |
| raw-window-metal | 1.1.0 | MIT OR Apache-2.0 |
| rawler | 0.7.1 | LGPL-2.1 |
| rawpointer | 0.2.1 | MIT/Apache-2.0 |
| rawtonic-app | 0.1.0-alpha.4 | AGPL-3.0-or-later |
| rawtonic-catalog | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-cli | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-denoise | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-engine | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-export | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-faces | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-fileops | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-gpu | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-ingest | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-interop | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-merge | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-metadata | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-naming | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-onnx | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-rawdecode | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-runtime | 0.1.0 | AGPL-3.0-or-later |
| rawtonic-tether | 0.1.0 | AGPL-3.0-or-later |
| rayon | 1.12.0 | MIT OR Apache-2.0 |
| rayon-cond | 0.4.0 | Apache-2.0/MIT |
| rayon-core | 1.13.0 | MIT OR Apache-2.0 |
| read-fonts | 0.45.1 | MIT OR Apache-2.0 |
| reborrow | 0.5.5 | MIT |
| redox_syscall | 0.5.18 | MIT |
| redox_users | 0.5.2 | MIT |
| ref-cast | 1.0.27 | MIT OR Apache-2.0 |
| ref-cast-impl | 1.0.27 | MIT OR Apache-2.0 |
| regex | 1.13.1 | MIT OR Apache-2.0 |
| regex-automata | 0.4.18 | MIT OR Apache-2.0 |
| regex-syntax | 0.8.11 | MIT OR Apache-2.0 |
| renderdoc-sys | 1.1.0 | MIT OR Apache-2.0 |
| reqwest | 0.13.4 | MIT OR Apache-2.0 |
| rfd | 0.16.0 | MIT |
| rgb | 0.8.53 | MIT |
| ring | 0.17.14 | Apache-2.0 AND ISC |
| rle-decode-fast | 1.0.3 | MIT OR Apache-2.0 |
| roxmltree | 0.21.1 | MIT OR Apache-2.0 |
| rsqlite-vfs | 0.1.1 | MIT |
| rusqlite | 0.40.2 | MIT |
| rustc-demangle | 0.1.28 | MIT/Apache-2.0 |
| rustc-hash | 1.1.0 | Apache-2.0/MIT |
| rustc-hash | 2.1.3 | Apache-2.0 OR MIT |
| rustc_version | 0.4.1 | MIT OR Apache-2.0 |
| rustdct | 0.7.1 | MIT OR Apache-2.0 |
| rustfft | 6.4.1 | MIT OR Apache-2.0 |
| rustix | 1.1.5 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| rustls | 0.23.46 | Apache-2.0 OR ISC OR MIT |
| rustls-native-certs | 0.8.4 | Apache-2.0 OR ISC OR MIT |
| rustls-pki-types | 1.15.1 | MIT OR Apache-2.0 |
| rustls-platform-verifier | 0.7.0 | MIT OR Apache-2.0 |
| rustls-platform-verifier-android | 0.1.1 | MIT OR Apache-2.0 |
| rustls-webpki | 0.103.16 | ISC |
| rustversion | 1.0.23 | MIT OR Apache-2.0 |
| rusty-fork | 0.3.1 | MIT/Apache-2.0 |
| ryu | 1.0.23 | Apache-2.0 OR BSL-1.0 |
| safe_arch | 1.2.0 | Zlib OR Apache-2.0 OR MIT |
| safe_unaligned_simd | 0.2.5 | MIT OR Apache-2.0 |
| same-file | 1.0.6 | Unlicense/MIT |
| schannel | 0.1.29 | MIT |
| schemars | 0.8.22 | MIT |
| schemars | 0.9.0 | MIT |
| schemars | 1.2.2 | MIT |
| schemars_derive | 0.8.22 | MIT |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 |
| security-framework | 3.7.0 | MIT OR Apache-2.0 |
| security-framework-sys | 2.17.0 | MIT OR Apache-2.0 |
| selectors | 0.38.0 | MPL-2.0 |
| semver | 1.0.28 | MIT OR Apache-2.0 |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde-untagged | 0.1.9 | MIT OR Apache-2.0 |
| serde_core | 1.0.229 | MIT OR Apache-2.0 |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 |
| serde_derive_internals | 0.29.1 | MIT OR Apache-2.0 |
| serde_json | 1.0.151 | MIT OR Apache-2.0 |
| serde_repr | 0.1.21 | MIT OR Apache-2.0 |
| serde_spanned | 0.6.9 | MIT OR Apache-2.0 |
| serde_spanned | 1.1.1 | MIT OR Apache-2.0 |
| serde_with | 3.22.0 | MIT OR Apache-2.0 |
| serde_with_macros | 3.22.0 | MIT OR Apache-2.0 |
| serialize-to-javascript | 0.1.2 | MIT OR Apache-2.0 |
| serialize-to-javascript-impl | 0.1.2 | MIT OR Apache-2.0 |
| servo_arc | 0.4.3 | MIT OR Apache-2.0 |
| sha2 | 0.10.9 | MIT OR Apache-2.0 |
| sha2 | 0.11.0 | MIT OR Apache-2.0 |
| shared_child | 1.1.1 | MIT |
| shlex | 1.3.0 | MIT OR Apache-2.0 |
| shlex | 2.0.1 | MIT OR Apache-2.0 |
| sigchld | 0.2.4 | MIT |
| signal-hook | 0.3.18 | Apache-2.0/MIT |
| signal-hook-registry | 1.4.8 | MIT OR Apache-2.0 |
| simba | 0.10.2 | Apache-2.0 |
| simd-adler32 | 0.3.10 | MIT |
| simd_cesu8 | 1.2.0 | Apache-2.0 OR MIT |
| simd_helpers | 0.1.0 | MIT |
| simdutf8 | 0.1.5 | MIT OR Apache-2.0 |
| siphasher | 1.0.3 | MIT/Apache-2.0 |
| skrifa | 0.48.0 | MIT OR Apache-2.0 |
| slab | 0.4.12 | MIT |
| slotmap | 1.1.1 | Zlib |
| smallvec | 1.15.2 | MIT OR Apache-2.0 |
| socket2 | 0.6.5 | MIT OR Apache-2.0 |
| softbuffer | 0.4.8 | MIT OR Apache-2.0 |
| soup3 | 0.5.0 | MIT |
| soup3-sys | 0.5.0 | MIT |
| spirv | 0.4.0+sdk-1.4.341.0 | Apache-2.0 |
| spm_precompiled | 0.1.4 | Apache-2.0 |
| sqlite-wasm-rs | 0.5.5 | MIT |
| stable_deref_trait | 1.2.1 | MIT OR Apache-2.0 |
| static_assertions | 1.1.0 | MIT OR Apache-2.0 |
| strength_reduce | 0.2.4 | MIT OR Apache-2.0 |
| string_cache | 0.9.0 | MIT OR Apache-2.0 |
| string_cache_codegen | 0.6.1 | MIT OR Apache-2.0 |
| strsim | 0.11.1 | MIT |
| strum | 0.26.3 | MIT |
| strum_macros | 0.26.4 | MIT |
| subtle | 2.6.1 | BSD-3-Clause |
| swift-rs | 1.0.8 | MIT OR Apache-2.0 |
| syn | 2.0.119 | MIT OR Apache-2.0 |
| syn | 3.0.3 | MIT OR Apache-2.0 |
| sync_wrapper | 1.0.2 | Apache-2.0 |
| synstructure | 0.13.2 | MIT |
| sys-locale | 0.3.2 | MIT OR Apache-2.0 |
| sysinfo | 0.39.6 | MIT |
| system-deps | 6.2.2 | MIT OR Apache-2.0 |
| tao | 0.37.1 | Apache-2.0 |
| tao-macros | 0.1.4 | MIT OR Apache-2.0 |
| target-features | 0.1.6 | MIT OR Apache-2.0 |
| target-lexicon | 0.12.16 | Apache-2.0 WITH LLVM-exception |
| tauri | 2.12.1 | Apache-2.0 OR MIT |
| tauri-build | 2.7.1 | Apache-2.0 OR MIT |
| tauri-codegen | 2.7.1 | Apache-2.0 OR MIT |
| tauri-macros | 2.7.1 | Apache-2.0 OR MIT |
| tauri-plugin | 2.6.3 | Apache-2.0 OR MIT |
| tauri-plugin-dialog | 2.7.2 | Apache-2.0 OR MIT |
| tauri-plugin-fs | 2.5.1 | Apache-2.0 OR MIT |
| tauri-plugin-os | 2.3.2 | Apache-2.0 OR MIT |
| tauri-plugin-process | 2.3.1 | Apache-2.0 OR MIT |
| tauri-plugin-shell | 2.3.5 | Apache-2.0 OR MIT |
| tauri-plugin-single-instance | 2.5.4 | Apache-2.0 OR MIT |
| tauri-plugin-wayland-nvidia-quirk | 0.1.0 | Apache-2.0 |
| tauri-runtime | 2.12.1 | Apache-2.0 OR MIT |
| tauri-runtime-wry | 2.12.1 | Apache-2.0 OR MIT |
| tauri-utils | 2.10.1 | Apache-2.0 OR MIT |
| tauri-winres | 0.3.6 | MIT |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |
| tendril | 0.5.1 | MIT OR Apache-2.0 |
| termcolor | 1.4.1 | Unlicense OR MIT |
| thiserror | 1.0.69 | MIT OR Apache-2.0 |
| thiserror | 2.0.20 | MIT OR Apache-2.0 |
| thiserror-impl | 1.0.69 | MIT OR Apache-2.0 |
| thiserror-impl | 2.0.20 | MIT OR Apache-2.0 |
| thread_local | 1.1.10 | MIT OR Apache-2.0 |
| tiff | 0.11.3 | MIT |
| time | 0.3.55 | MIT OR Apache-2.0 |
| time-core | 0.1.9 | MIT OR Apache-2.0 |
| time-macros | 0.2.32 | MIT OR Apache-2.0 |
| tinystr | 0.8.4 | Unicode-3.0 |
| tinytemplate | 1.2.1 | Apache-2.0 OR MIT |
| tinyvec | 1.12.0 | Zlib OR Apache-2.0 OR MIT |
| tinyvec_macros | 0.1.1 | MIT OR Apache-2.0 OR Zlib |
| to_method | 1.1.0 | CC0-1.0 |
| tokenizers | 0.23.1 | Apache-2.0 |
| tokio | 1.53.3 | MIT |
| tokio-macros | 2.7.2 | MIT |
| tokio-rustls | 0.26.4 | MIT OR Apache-2.0 |
| tokio-util | 0.7.19 | MIT |
| toml | 0.8.2 | MIT OR Apache-2.0 |
| toml | 1.1.4+spec-1.1.0 | MIT OR Apache-2.0 |
| toml_datetime | 0.6.3 | MIT OR Apache-2.0 |
| toml_datetime | 1.1.1+spec-1.1.0 | MIT OR Apache-2.0 |
| toml_edit | 0.19.15 | MIT OR Apache-2.0 |
| toml_edit | 0.20.2 | MIT OR Apache-2.0 |
| toml_edit | 0.25.13+spec-1.1.0 | MIT OR Apache-2.0 |
| toml_parser | 1.1.3+spec-1.1.0 | MIT OR Apache-2.0 |
| toml_writer | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 |
| tower | 0.5.3 | MIT |
| tower-http | 0.6.11 | MIT |
| tower-layer | 0.3.3 | MIT |
| tower-service | 0.3.3 | MIT |
| tracing | 0.1.44 | MIT |
| tracing-attributes | 0.1.31 | MIT |
| tracing-core | 0.1.36 | MIT |
| transpose | 0.2.3 | MIT OR Apache-2.0 |
| trash | 5.2.6 | MIT |
| tray-icon | 0.25.1 | MIT OR Apache-2.0 |
| try-lock | 0.2.5 | MIT |
| turbojpeg | 1.5.1 | Unlicense OR MIT |
| turbojpeg-sys | 1.2.0 | Unlicense OR MIT |
| typeid | 1.0.3 | MIT OR Apache-2.0 |
| typenum | 1.20.1 | MIT OR Apache-2.0 |
| uds_windows | 1.2.1 | MIT |
| unarray | 0.1.4 | MIT OR Apache-2.0 |
| unicase | 2.9.0 | MIT OR Apache-2.0 |
| unicode-ident | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| unicode-normalization-alignments | 0.1.12 | MIT/Apache-2.0 |
| unicode-segmentation | 1.13.3 | MIT OR Apache-2.0 |
| unicode-width | 0.2.2 | MIT OR Apache-2.0 |
| unicode_categories | 0.1.1 | MIT OR Apache-2.0 |
| untrusted | 0.9.0 | ISC |
| url | 2.5.8 | MIT OR Apache-2.0 |
| urlencoding | 2.1.3 | MIT |
| urlpattern | 0.6.0 | MIT |
| utf8_iter | 1.0.4 | Apache-2.0 OR MIT |
| utf8parse | 0.2.2 | Apache-2.0 OR MIT |
| uuid | 1.25.0 | Apache-2.0 OR MIT |
| v_frame | 0.3.9 | BSD-2-Clause |
| vcpkg | 0.2.15 | MIT/Apache-2.0 |
| version-compare | 0.2.1 | MIT |
| version_check | 0.9.5 | MIT/Apache-2.0 |
| vswhom | 0.1.0 | MIT |
| vswhom-sys | 0.1.3 | MIT |
| wait-timeout | 0.2.1 | MIT/Apache-2.0 |
| walkdir | 2.5.0 | Unlicense/MIT |
| want | 0.3.1 | MIT |
| wasi | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| wasip2 | 1.0.4+wasi-0.2.12 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| wasm-bindgen | 0.2.127 | MIT OR Apache-2.0 |
| wasm-bindgen-futures | 0.4.77 | MIT OR Apache-2.0 |
| wasm-bindgen-macro | 0.2.127 | MIT OR Apache-2.0 |
| wasm-bindgen-macro-support | 0.2.127 | MIT OR Apache-2.0 |
| wasm-bindgen-shared | 0.2.127 | MIT OR Apache-2.0 |
| wasm-streams | 0.5.0 | MIT OR Apache-2.0 |
| wayland-sys | 0.31.11 | MIT |
| web-sys | 0.3.104 | MIT OR Apache-2.0 |
| web-time | 1.1.0 | MIT OR Apache-2.0 |
| web_atoms | 0.2.6 | MIT OR Apache-2.0 |
| webkit2gtk | 2.0.2 | MIT |
| webkit2gtk-sys | 2.0.2 | MIT |
| webp | 0.3.1 | MIT OR Apache-2.0 |
| webpki-root-certs | 1.0.10 | CDLA-Permissive-2.0 |
| webview2-com | 0.39.1 | MIT |
| webview2-com-macros | 0.8.1 | MIT |
| webview2-com-sys | 0.39.1 | MIT |
| weezl | 0.1.12 | MIT OR Apache-2.0 |
| wgpu | 29.0.4 | MIT OR Apache-2.0 |
| wgpu-core | 29.0.4 | MIT OR Apache-2.0 |
| wgpu-core-deps-apple | 29.0.4 | MIT OR Apache-2.0 |
| wgpu-core-deps-emscripten | 29.0.4 | MIT OR Apache-2.0 |
| wgpu-core-deps-windows-linux-android | 29.0.4 | MIT OR Apache-2.0 |
| wgpu-hal | 29.0.4 | MIT OR Apache-2.0 |
| wgpu-naga-bridge | 29.0.4 | MIT OR Apache-2.0 |
| wgpu-types | 29.0.4 | MIT OR Apache-2.0 |
| whereat | 0.1.5 | MIT OR Apache-2.0 |
| wide | 1.6.1 | Zlib OR Apache-2.0 OR MIT |
| winapi | 0.3.9 | MIT/Apache-2.0 |
| winapi-i686-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 |
| winapi-util | 0.1.11 | Unlicense OR MIT |
| winapi-x86_64-pc-windows-gnu | 0.4.0 | MIT/Apache-2.0 |
| winarm-cpufeatures | 0.1.2 | MIT OR Apache-2.0 |
| window-vibrancy | 0.8.1 | Apache-2.0 OR MIT |
| windows | 0.56.0 | MIT OR Apache-2.0 |
| windows | 0.62.2 | MIT OR Apache-2.0 |
| windows-collections | 0.3.2 | MIT OR Apache-2.0 |
| windows-core | 0.56.0 | MIT OR Apache-2.0 |
| windows-core | 0.62.2 | MIT OR Apache-2.0 |
| windows-future | 0.3.2 | MIT OR Apache-2.0 |
| windows-implement | 0.56.0 | MIT OR Apache-2.0 |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 |
| windows-interface | 0.56.0 | MIT OR Apache-2.0 |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 |
| windows-link | 0.2.1 | MIT OR Apache-2.0 |
| windows-numerics | 0.3.1 | MIT OR Apache-2.0 |
| windows-result | 0.1.2 | MIT OR Apache-2.0 |
| windows-result | 0.4.1 | MIT OR Apache-2.0 |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 |
| windows-sys | 0.45.0 | MIT OR Apache-2.0 |
| windows-sys | 0.52.0 | MIT OR Apache-2.0 |
| windows-sys | 0.59.0 | MIT OR Apache-2.0 |
| windows-sys | 0.60.2 | MIT OR Apache-2.0 |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 |
| windows-targets | 0.42.2 | MIT OR Apache-2.0 |
| windows-targets | 0.52.6 | MIT OR Apache-2.0 |
| windows-targets | 0.53.5 | MIT OR Apache-2.0 |
| windows-threading | 0.2.1 | MIT OR Apache-2.0 |
| windows-version | 0.1.7 | MIT OR Apache-2.0 |
| windows_aarch64_gnullvm | 0.42.2 | MIT OR Apache-2.0 |
| windows_aarch64_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_aarch64_gnullvm | 0.53.1 | MIT OR Apache-2.0 |
| windows_aarch64_msvc | 0.42.2 | MIT OR Apache-2.0 |
| windows_aarch64_msvc | 0.52.6 | MIT OR Apache-2.0 |
| windows_aarch64_msvc | 0.53.1 | MIT OR Apache-2.0 |
| windows_i686_gnu | 0.42.2 | MIT OR Apache-2.0 |
| windows_i686_gnu | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_gnu | 0.53.1 | MIT OR Apache-2.0 |
| windows_i686_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_gnullvm | 0.53.1 | MIT OR Apache-2.0 |
| windows_i686_msvc | 0.42.2 | MIT OR Apache-2.0 |
| windows_i686_msvc | 0.52.6 | MIT OR Apache-2.0 |
| windows_i686_msvc | 0.53.1 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.42.2 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_gnu | 0.53.1 | MIT OR Apache-2.0 |
| windows_x86_64_gnullvm | 0.42.2 | MIT OR Apache-2.0 |
| windows_x86_64_gnullvm | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_gnullvm | 0.53.1 | MIT OR Apache-2.0 |
| windows_x86_64_msvc | 0.42.2 | MIT OR Apache-2.0 |
| windows_x86_64_msvc | 0.52.6 | MIT OR Apache-2.0 |
| windows_x86_64_msvc | 0.53.1 | MIT OR Apache-2.0 |
| winnow | 0.5.40 | MIT |
| winnow | 1.0.4 | MIT |
| winreg | 0.55.0 | MIT |
| wit-bindgen | 0.57.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |
| writeable | 0.6.4 | Unicode-3.0 |
| wry | 0.57.0 | Apache-2.0 OR MIT |
| x11 | 2.21.0 | MIT |
| x11-dl | 2.21.0 | MIT |
| xml-rs | 0.8.29 | MIT |
| xxhash-rust | 0.8.19 | BSL-1.0 |
| y4m | 0.8.0 | MIT |
| yoke | 0.8.3 | Unicode-3.0 |
| yoke-derive | 0.8.2 | Unicode-3.0 |
| zbus | 6.0.0 | MIT |
| zbus_macros | 6.0.0 | MIT |
| zbus_utils | 6.0.0 | MIT |
| zcheapstr | 1.1.0 | MIT |
| zenyuv | 0.1.3 | MIT OR Apache-2.0 |
| zerocopy | 0.7.35 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zerocopy | 0.8.56 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zerocopy-derive | 0.7.35 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zerocopy-derive | 0.8.56 | BSD-2-Clause OR Apache-2.0 OR MIT |
| zerofrom | 0.1.8 | Unicode-3.0 |
| zerofrom-derive | 0.1.7 | Unicode-3.0 |
| zeroize | 1.9.0 | Apache-2.0 OR MIT |
| zerotrie | 0.2.5 | Unicode-3.0 |
| zerovec | 0.11.8 | Unicode-3.0 |
| zerovec-derive | 0.11.6 | Unicode-3.0 |
| zmij | 1.0.23 | MIT |
| zruntime | 0.2.0 | MIT |
| zune-core | 0.5.3 | MIT OR Apache-2.0 OR Zlib |
| zune-inflate | 0.2.54 | MIT OR Apache-2.0 OR Zlib |
| zune-jpeg | 0.5.15 | MIT OR Apache-2.0 OR Zlib |

## Resolved npm dependencies (including build and test tools)

| Package | Version | Declared license |
|---|---|---|
| @asamuzakjp/css-color | 7.1.2 | MIT |
| @asamuzakjp/dom-selector | 9.2.2 | MIT |
| @babel/code-frame | 7.29.7 | MIT |
| @babel/compat-data | 7.29.7 | MIT |
| @babel/core | 7.29.7 | MIT |
| @babel/generator | 7.29.8 | MIT |
| @babel/helper-compilation-targets | 7.29.7 | MIT |
| @babel/helper-globals | 7.29.7 | MIT |
| @babel/helper-module-imports | 7.29.7 | MIT |
| @babel/helper-module-transforms | 7.29.7 | MIT |
| @babel/helper-string-parser | 7.29.7 | MIT |
| @babel/helper-validator-identifier | 7.29.7 | MIT |
| @babel/helper-validator-option | 7.29.7 | MIT |
| @babel/helpers | 7.29.7 | MIT |
| @babel/parser | 7.29.9 | MIT |
| @babel/runtime | 7.29.7 | MIT |
| @babel/runtime | 8.0.0 | MIT |
| @babel/template | 7.29.7 | MIT |
| @babel/traverse | 7.29.8 | MIT |
| @babel/types | 7.29.8 | MIT |
| @bramus/specificity | 2.4.2 | MIT |
| @cloudflare/kv-asset-handler | 0.5.0 | MIT OR Apache-2.0 |
| @cloudflare/unenv-preset | 2.16.2 | MIT OR Apache-2.0 |
| @cloudflare/vitest-plugin | 1.4.7 | MIT |
| @cloudflare/workerd-darwin-arm64 | 1.20261019.1 | Apache-2.0 |
| @cloudflare/workers-types | 5.20261024.1 | MIT OR Apache-2.0 |
| @croct/json | 2.1.0 | MIT |
| @croct/json5-parser | 0.2.2 | MIT |
| @cspotcode/source-map-support | 0.8.1 | MIT |
| @csstools/color-helpers | 6.1.2 | MIT-0 |
| @csstools/css-calc | 3.4.2 | MIT |
| @csstools/css-color-parser | 4.2.5 | MIT |
| @csstools/css-parser-algorithms | 4.0.2 | MIT |
| @csstools/css-syntax-patches-for-csstree | 1.1.15 | MIT-0 |
| @csstools/css-tokenizer | 4.0.2 | MIT |
| @dnd-kit/accessibility | 3.1.1 | MIT |
| @dnd-kit/core | 6.3.1 | MIT |
| @dnd-kit/utilities | 3.2.2 | MIT |
| @esbuild/darwin-arm64 | 0.28.2 | MIT |
| @eslint-community/eslint-utils | 4.10.1 | MIT |
| @eslint-community/regexpp | 4.12.2 | MIT |
| @eslint/config-array | 0.23.5 | Apache-2.0 |
| @eslint/config-helpers | 0.7.0 | Apache-2.0 |
| @eslint/core | 1.2.1 | Apache-2.0 |
| @eslint/js | 10.0.1 | MIT |
| @eslint/object-schema | 3.0.5 | Apache-2.0 |
| @eslint/plugin-kit | 0.7.2 | Apache-2.0 |
| @exodus/bytes | 1.16.0 | MIT |
| @humanfs/core | 0.19.2 | Apache-2.0 |
| @humanfs/node | 0.16.8 | Apache-2.0 |
| @humanfs/types | 0.15.0 | Apache-2.0 |
| @humanwhocodes/module-importer | 1.0.1 | Apache-2.0 |
| @humanwhocodes/retry | 0.4.3 | Apache-2.0 |
| @img/colour | 1.1.0 | MIT |
| @img/sharp-darwin-arm64 | 0.35.5 | Apache-2.0 |
| @img/sharp-libvips-darwin-arm64 | 1.3.4 | LGPL-3.0-or-later |
| @inquirer/ansi | 2.0.7 | MIT |
| @inquirer/checkbox | 5.2.2 | MIT |
| @inquirer/confirm | 6.2.0 | MIT |
| @inquirer/core | 12.0.0 | MIT |
| @inquirer/editor | 5.3.0 | MIT |
| @inquirer/expand | 5.1.2 | MIT |
| @inquirer/external-editor | 3.0.4 | MIT |
| @inquirer/figures | 2.0.8 | MIT |
| @inquirer/input | 5.1.3 | MIT |
| @inquirer/number | 4.2.0 | MIT |
| @inquirer/password | 5.1.2 | MIT |
| @inquirer/prompts | 8.6.0 | MIT |
| @inquirer/rawlist | 5.3.2 | MIT |
| @inquirer/search | 4.3.0 | MIT |
| @inquirer/select | 5.2.2 | MIT |
| @inquirer/type | 4.0.7 | MIT |
| @jridgewell/gen-mapping | 0.3.13 | MIT |
| @jridgewell/remapping | 2.3.5 | MIT |
| @jridgewell/resolve-uri | 3.1.2 | MIT |
| @jridgewell/sourcemap-codec | 1.6.0 | MIT |
| @jridgewell/trace-mapping | 0.3.31 | MIT |
| @jridgewell/trace-mapping | 0.3.9 | MIT |
| @oxc-project/types | 0.146.0 | MIT |
| @oxc-project/types | 0.155.0 | MIT |
| @poppinss/colors | 4.1.6 | MIT |
| @poppinss/dumper | 0.6.5 | MIT |
| @poppinss/exception | 1.2.3 | MIT |
| @rolldown/binding-darwin-arm64 | 1.2.15 | MIT |
| @rolldown/binding-darwin-arm64 | 1.2.5 | MIT |
| @rolldown/pluginutils | 1.0.1 | MIT |
| @sec-ant/readable-stream | 0.4.1 | MIT |
| @sindresorhus/is | 7.2.0 | MIT |
| @sindresorhus/merge-streams | 4.0.0 | MIT |
| @speed-highlight/core | 1.2.24 | CC0-1.0 |
| @swc/core | 1.16.1 | Apache-2.0 |
| @swc/core-darwin-arm64 | 1.16.1 | Apache-2.0 AND MIT |
| @swc/counter | 0.1.3 | Apache-2.0 |
| @swc/types | 0.1.28 | Apache-2.0 |
| @tailwindcss/node | 4.3.3 | MIT |
| @tailwindcss/oxide | 4.3.3 | MIT |
| @tailwindcss/oxide-darwin-arm64 | 4.3.3 | MIT |
| @tailwindcss/vite | 4.3.3 | MIT |
| @tauri-apps/api | 2.12.1 | Apache-2.0 OR MIT |
| @tauri-apps/cli | 2.12.1 | Apache-2.0 OR MIT |
| @tauri-apps/cli-darwin-arm64 | 2.12.1 | Apache-2.0 OR MIT |
| @tauri-apps/plugin-dialog | 2.7.2 | MIT OR Apache-2.0 |
| @tauri-apps/plugin-os | 2.3.2 | MIT OR Apache-2.0 |
| @tauri-apps/plugin-process | 2.3.1 | MIT OR Apache-2.0 |
| @tauri-apps/plugin-shell | 2.3.5 | MIT OR Apache-2.0 |
| @testing-library/dom | 10.4.2 | MIT |
| @testing-library/react | 16.3.3 | MIT |
| @testing-library/user-event | 14.6.7 | MIT |
| @types/aria-query | 5.0.4 | MIT |
| @types/chai | 5.2.3 | MIT |
| @types/deep-eql | 4.0.2 | MIT |
| @types/esrecurse | 4.3.1 | MIT |
| @types/estree | 1.0.9 | MIT |
| @types/json-schema | 7.0.15 | MIT |
| @types/lodash | 4.17.25 | MIT |
| @types/lodash.debounce | 4.0.9 | MIT |
| @types/lodash.throttle | 4.1.9 | MIT |
| @types/react | 19.2.18 | MIT |
| @types/react-dom | 19.2.4 | MIT |
| @types/react-reconciler | 0.28.9 | MIT |
| @types/react-reconciler | 0.33.0 | MIT |
| @typescript-eslint/eslint-plugin | 8.67.0 | MIT |
| @typescript-eslint/parser | 8.67.0 | MIT |
| @typescript-eslint/project-service | 8.67.0 | MIT |
| @typescript-eslint/scope-manager | 8.67.0 | MIT |
| @typescript-eslint/tsconfig-utils | 8.67.0 | MIT |
| @typescript-eslint/type-utils | 8.67.0 | MIT |
| @typescript-eslint/types | 8.67.0 | MIT |
| @typescript-eslint/typescript-estree | 8.67.0 | MIT |
| @typescript-eslint/utils | 8.67.0 | MIT |
| @typescript-eslint/visitor-keys | 8.67.0 | MIT |
| @uiw/color-convert | 2.10.3 | MIT |
| @uiw/react-color-wheel | 2.10.3 | MIT |
| @uiw/react-drag-event-interactive | 2.10.3 | MIT |
| @vitejs/plugin-react | 6.1.0 | MIT |
| @vitest/mocker | 5.0.3 | MIT |
| @vitest/mocker | 5.0.5 | MIT |
| @vitest/spy | 5.0.3 | MIT |
| @vitest/spy | 5.0.5 | MIT |
| acorn | 8.18.0 | MIT |
| acorn-jsx | 5.3.2 | MIT |
| ajv | 6.15.0 | MIT |
| ansi-regex | 5.0.1 | MIT |
| ansi-regex | 6.3.0 | MIT |
| ansi-styles | 5.2.0 | MIT |
| aria-query | 5.3.0 | Apache-2.0 |
| assertion-error | 2.0.1 | MIT |
| balanced-match | 4.0.4 | MIT |
| baseline-browser-mapping | 2.11.27 | Apache-2.0 |
| bidi-js | 1.1.0 | MIT |
| blake3-wasm | 2.1.5 | MIT |
| brace-expansion | 5.0.12 | MIT |
| browserslist | 4.29.3 | MIT |
| caniuse-lite | 1.0.30001814 | CC-BY-4.0 |
| chai | 6.3.0 | MIT |
| chalk | 5.6.2 | MIT |
| chardet | 2.2.0 | MIT |
| chokidar | 5.0.0 | MIT |
| cjs-module-lexer | 1.2.3 | MIT |
| cli-cursor | 5.0.0 | MIT |
| cli-spinners | 3.4.0 | MIT |
| cli-width | 4.1.0 | ISC |
| clsx | 2.1.1 | MIT |
| commander | 15.0.0 | MIT |
| convert-source-map | 2.0.0 | MIT |
| cookie | 1.1.1 | MIT |
| cross-spawn | 7.0.6 | MIT |
| css-tree | 3.2.1 | MIT |
| csstype | 3.2.3 | MIT |
| data-urls | 7.0.0 | MIT |
| debug | 4.4.3 | MIT |
| decimal.js | 10.6.0 | MIT |
| deep-is | 0.1.4 | MIT |
| dequal | 2.0.3 | MIT |
| detect-libc | 2.1.2 | Apache-2.0 |
| dom-accessibility-api | 0.5.16 | MIT |
| electron-to-chromium | 1.5.444 | ISC |
| enhanced-resolve | 5.24.5 | MIT |
| entities | 8.1.0 | BSD-2-Clause |
| error-stack-parser-es | 1.0.5 | MIT |
| es-module-lexer | 2.3.2 | MIT |
| esbuild | 0.28.2 | MIT |
| escalade | 3.2.0 | MIT |
| escape-string-regexp | 4.0.0 | MIT |
| eslint | 10.9.0 | MIT |
| eslint-plugin-i18next | 6.1.5 | ISC |
| eslint-plugin-react-hooks | 7.1.1 | MIT |
| eslint-scope | 9.1.2 | BSD-2-Clause |
| eslint-visitor-keys | 3.4.3 | Apache-2.0 |
| eslint-visitor-keys | 5.0.1 | Apache-2.0 |
| espree | 11.2.0 | BSD-2-Clause |
| esquery | 1.7.0 | BSD-3-Clause |
| esrecurse | 4.3.0 | BSD-2-Clause |
| estraverse | 5.3.0 | BSD-2-Clause |
| estree-walker | 3.0.3 | MIT |
| esutils | 2.0.3 | BSD-2-Clause |
| execa | 10.0.1 | MIT |
| expect-type | 1.4.0 | Apache-2.0 |
| fast-deep-equal | 3.1.3 | MIT |
| fast-json-stable-stringify | 2.1.0 | MIT |
| fast-levenshtein | 2.0.6 | MIT |
| fast-string-truncated-width | 3.0.3 | MIT |
| fast-string-width | 3.0.2 | MIT |
| fast-wrap-ansi | 0.2.2 | MIT |
| fdir | 6.5.0 | MIT |
| figures | 6.1.0 | MIT |
| file-entry-cache | 8.0.0 | MIT |
| find-up | 5.0.0 | MIT |
| flat-cache | 4.0.1 | MIT |
| flatted | 3.4.4 | ISC |
| framer-motion | 13.1.1 | MIT |
| fsevents | 2.3.3 | MIT |
| gensync | 1.0.0-beta.2 | MIT |
| get-east-asian-width | 1.6.0 | MIT |
| get-stream | 9.0.1 | MIT |
| glob | 13.0.6 | BlueOak-1.0.0 |
| glob-parent | 6.0.2 | ISC |
| graceful-fs | 4.2.11 | ISC |
| hermes-estree | 0.25.1 | MIT |
| hermes-parser | 0.25.1 | MIT |
| html-encoding-sniffer | 7.0.0 | MIT |
| html-parse-stringify | 4.0.1 | MIT |
| human-signals | 8.0.1 | Apache-2.0 |
| i18next | 26.4.0 | MIT |
| i18next-cli | 1.71.0 | MIT |
| i18next-resources-for-ts | 2.1.0 | MIT |
| iconv-lite | 0.7.3 | MIT |
| ignore | 5.3.2 | MIT |
| ignore | 7.0.6 | MIT |
| imurmurhash | 0.1.4 | MIT |
| inquirer | 14.1.0 | MIT |
| is-extglob | 2.1.1 | MIT |
| is-glob | 4.0.3 | MIT |
| is-interactive | 2.0.0 | MIT |
| is-plain-obj | 4.1.0 | MIT |
| is-potential-custom-element-name | 1.0.1 | MIT |
| is-stream | 4.0.1 | MIT |
| is-unicode-supported | 2.1.0 | MIT |
| isexe | 2.0.0 | ISC |
| its-fine | 2.0.0 | MIT |
| jiti | 2.7.0 | MIT |
| js-tokens | 4.0.0 | MIT |
| jsdom | 30.1.1 | MIT |
| jsesc | 3.1.0 | MIT |
| json-buffer | 3.0.1 | MIT |
| json-schema-traverse | 0.4.1 | MIT |
| json-stable-stringify-without-jsonify | 1.0.1 | MIT |
| json5 | 2.2.3 | MIT |
| jsonc-parser | 3.3.1 | MIT |
| keyv | 4.5.4 | MIT |
| kleur | 4.1.5 | MIT |
| konva | 10.3.1 | MIT |
| levn | 0.4.1 | MIT |
| lightningcss | 1.32.0 | MPL-2.0 |
| lightningcss | 1.33.0 | MPL-2.0 |
| lightningcss-darwin-arm64 | 1.32.0 | MPL-2.0 |
| lightningcss-darwin-arm64 | 1.33.0 | MPL-2.0 |
| locate-path | 6.0.0 | MIT |
| lodash.debounce | 4.0.8 | MIT |
| lodash.throttle | 4.1.1 | MIT |
| log-symbols | 7.0.1 | MIT |
| lru-cache | 11.5.3 | BlueOak-1.0.0 |
| lru-cache | 5.1.1 | ISC |
| lucide-react | 1.33.0 | ISC |
| lz-string | 1.5.0 | MIT |
| magic-string | 0.30.21 | MIT |
| magic-string | 1.2.2 | MIT |
| magic-string | 1.4.2 | MIT |
| magic-string | 1.4.3 | MIT |
| mdn-data | 2.27.1 | CC0-1.0 |
| mimic-function | 5.0.1 | MIT |
| miniflare | 5.20261019.2-beta | MIT |
| minimatch | 10.2.6 | BlueOak-1.0.0 |
| minipass | 7.1.3 | BlueOak-1.0.0 |
| motion-dom | 13.1.1 | MIT |
| motion-utils | 13.0.0 | MIT |
| ms | 2.1.3 | MIT |
| mute-stream | 3.0.0 | ISC |
| nanoid | 3.3.18 | MIT |
| nanoid | 3.3.20 | MIT |
| natural-compare | 1.4.0 | MIT |
| node-releases | 2.0.57 | MIT |
| npm-run-path | 6.0.0 | MIT |
| obug | 2.2.1 | MIT |
| obug | 3.0.0 | MIT |
| onetime | 7.0.0 | MIT |
| optionator | 0.9.4 | MIT |
| ora | 9.4.1 | MIT |
| p-limit | 3.1.0 | MIT |
| p-locate | 5.0.0 | MIT |
| parse-ms | 4.0.0 | MIT |
| parse5 | 8.0.1 | MIT |
| path-exists | 4.0.0 | MIT |
| path-key | 3.1.1 | MIT |
| path-key | 4.0.0 | MIT |
| path-scurry | 2.0.2 | BlueOak-1.0.0 |
| path-to-regexp | 6.3.0 | MIT |
| pathe | 2.0.3 | MIT |
| picocolors | 1.1.1 | ISC |
| picomatch | 4.0.7 | MIT |
| postcss | 8.5.26 | MIT |
| postcss | 8.5.31 | MIT |
| prelude-ls | 1.2.1 | MIT |
| prettier | 3.9.6 | MIT |
| pretty-format | 27.5.1 | MIT |
| pretty-ms | 9.3.0 | MIT |
| punycode | 2.3.1 | MIT |
| react | 19.2.8 | MIT |
| react-dom | 19.2.8 | MIT |
| react-i18next | 17.0.12 | MIT |
| react-image-crop | 11.1.2 | ISC |
| react-is | 17.0.2 | MIT |
| react-konva | 19.2.5 | MIT |
| react-reconciler | 0.33.0 | MIT |
| react-toastify | 11.1.0 | MIT |
| readdirp | 5.1.1 | MIT |
| require-from-string | 2.0.2 | MIT |
| requireindex | 1.1.0 | MIT |
| restore-cursor | 5.1.0 | MIT |
| rolldown | 1.2.15 | MIT |
| rolldown | 1.2.5 | MIT |
| run-async | 4.0.6 | MIT |
| safer-buffer | 2.1.2 | MIT |
| saxes | 6.0.0 | ISC |
| scheduler | 0.27.0 | MIT |
| semver | 6.3.1 | ISC |
| semver | 7.8.5 | ISC |
| sharp | 0.35.5 | Apache-2.0 |
| shebang-command | 2.0.0 | MIT |
| shebang-regex | 3.0.0 | MIT |
| signal-exit | 4.1.0 | ISC |
| source-map-js | 1.2.1 | BSD-3-Clause |
| source-map-js | 1.2.2 | BSD-3-Clause |
| std-env | 4.3.0 | MIT |
| stdin-discarder | 0.3.2 | MIT |
| string-width | 8.2.2 | MIT |
| strip-ansi | 7.2.0 | MIT |
| strip-final-newline | 4.0.0 | MIT |
| supports-color | 10.2.2 | MIT |
| tailwindcss | 4.3.3 | MIT |
| tapable | 2.3.3 | MIT |
| tinybench | 6.2.0 | MIT |
| tinybench | 6.2.2 | MIT |
| tinyexec | 1.3.1 | MIT |
| tinyglobby | 0.2.17 | MIT |
| tldts | 7.4.16 | MIT |
| tldts-core | 7.4.16 | MIT |
| tough-cookie | 6.0.2 | BSD-3-Clause |
| tr46 | 6.0.0 | MIT |
| ts-api-utils | 2.5.0 | MIT |
| tslib | 2.8.1 | 0BSD |
| type-check | 0.4.0 | MIT |
| typescript | 5.9.3 | Apache-2.0 |
| typescript | 6.0.3 | Apache-2.0 |
| typescript-eslint | 8.67.0 | MIT |
| undici | 7.30.1 | MIT |
| undici | 8.11.2 | MIT |
| unenv | 2.0.0-rc.24 | MIT |
| unicorn-magic | 0.3.0 | MIT |
| update-browserslist-db | 1.3.3 | MIT |
| uri-js | 4.4.1 | BSD-2-Clause |
| use-sync-external-store | 1.6.0 | MIT |
| uuid | 14.0.2 | MIT |
| vite | 8.2.2 | MIT |
| vite | 8.3.5 | MIT |
| vitest | 5.0.3 | MIT |
| vitest | 5.0.5 | MIT |
| w3c-xmlserializer | 6.0.0 | MIT |
| webidl-conversions | 8.0.1 | BSD-2-Clause |
| whatwg-mimetype | 5.0.0 | MIT |
| whatwg-url | 16.0.1 | MIT |
| whatwg-url | 17.1.2 | MIT |
| which | 2.0.2 | ISC |
| which-command | 0.1.0 | MIT |
| why-is-node-running | 3.2.1 | MIT |
| word-wrap | 1.2.5 | MIT |
| workerd | 1.20261019.1 | Apache-2.0 |
| wrangler | 4.152.0 | MIT OR Apache-2.0 |
| ws | 8.21.0 | MIT |
| xml-name-validator | 5.0.0 | Apache-2.0 |
| xmlchars | 2.2.0 | MIT |
| yallist | 3.1.1 | ISC |
| yaml | 2.9.0 | ISC |
| yocto-queue | 0.1.0 | MIT |
| yoctocolors | 2.2.0 | MIT |
| youch | 4.1.0-beta.10 | MIT |
| youch-core | 0.3.3 | MIT |
| zod | 4.6.5 | MIT |
| zod | 4.6.6 | MIT |
| zod-validation-error | 4.0.2 | MIT |
| zustand | 5.0.15 | MIT |
