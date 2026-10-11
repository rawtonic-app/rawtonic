# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Rules

- **One tool per job.** For every tool and system, take the *better* approach of the leading professional RAW editors — decided by measurement and use — not everything from all of them. Never per-editor modes or "editor A / editor B" switches. Importers may translate another editor's settings into that one tool. The goal is the best RAW development tool, not a clone.
- **The bar is law.** Every tool and system must be equal to or better than the better of the leading professional RAW editors (quality, speed, workflow), shown by measurement or a documented side-by-side use test. A tool below that bar is not done: record the gap and the evidence, never call it finished.
- Rust 2024 (toolchain pinned to 1.98.1 in `rust-toolchain.toml`), Node 24+. AGPL-3.0-or-later for original additions; the editor derives from RapidRAW (commit `a82251f`), so preserve upstream copyright/license notices on inherited code.
- Never alter originals implicitly. The user's RAW files must never be deleted or modified; QA uses separate working copies. Every file write must use the atomic/no-clobber helpers (`rawtonic-metadata`, `rawtonic-ingest`, catalog). Unsupported destructive operations must fail clearly rather than fall back to deletion or overwrite.
- No false completion claims or fake benchmark results. A budget is only green with measured evidence; label what a measurement does and does not cover (e.g. IPC round trip ≠ pointer-to-photon).
- `APP_FIXTURES` / `RAWTONIC_RAW_FIXTURES` point to private RAW fixtures — never commit photographs, private paths, manifests, raw app logs, credentials or keys. Keep `refs/` gitignored. Commits use a noreply address; the pre-push hook `scripts/git-hooks/pre-push` refuses other addresses and gitleaks findings.
- Conventional commits at green milestones; run `scripts/check.sh` before merging into main. Do not publish or tag a production release while parity or benchmark gates are open.
- Maintainers may keep extra, private instructions in `CLAUDE.local.md` (gitignored); follow it when present.

## Legal guardrails (check before implementing, so work is not redone)

- **No other products' content.** Never copy, convert or fit to other editors' or camera makers' profiles, DCP/ICC files, look tables, LUTs, presets, styles or curves (Adobe, Capture One, Fujifilm, …). Observing another program's *output* on our own test files to understand its behaviour is fine; the result must be our own implementation and our own numbers. Plain numerical camera facts (colour matrices, white levels, baseline exposure) are fine with their source stated in `THIRD_PARTY_NOTICES.md`.
- **No third-party names for our features.** Tools, looks, presets and profiles get descriptive names, never another company's product or trademark (no film-simulation, film-stock or editor-look names). Naming another product is fine only to say what we import from or work with (`TRADEMARKS.md`).
- **Licenses.** Code only under AGPL-3.0-compatible licenses (no GPL-2.0-only, no proprietary or decompiled code), with notices kept. New third-party code, data or assets go into `THIRD_PARTY_NOTICES.md` (and its copy in `src-tauri/resources/licenses/`, checked by `scripts/check.sh`) and the About page credits (`src/components/panel/AboutSettings.tsx`). Keep the DNG notice ("This product includes DNG technology under license by Adobe.") in About, NOTICE and README. AGPL means no Mac App Store distribution.
- **AI models.** Only weights with an explicit MIT/Apache-2.0/BSD-style license from the rights holder — no non-commercial, research-only, use-restricted (RAIL) or unlicensed weights. Host them in the model pack (`scripts/models/build_model_pack.py`, pinned SHA-256) and record them in `docs/MODELS.md` with license texts in `licenses/models/`.
- **Generated content.** Anything a generative model adds to a photo must be an AI patch (`aiPatches`) so exports carry the IPTC Digital Source Type marking (EU AI Act Art. 50; `has_generated_areas`, `export_metadata_with`). A new generative feature must keep that marking working.
- **Privacy.** No network access the user did not ask for: no telemetry, update checks or background uploads. Any new network access needs a user action and an entry in `PRIVACY.md`.

## Commands

```sh
npm ci
npm start                       # = tauri dev; full app. `npm run dev` serves UI only and cannot decode/render photos
npm run typecheck
npm run lint
npm test                        # vitest (jsdom), src/**/*.test.{ts,tsx}
npx vitest run src/hooks/useThumbnails.test.ts   # single frontend test file (-t "name" to filter)
npm run build
npm run i18n:check
scripts/check.sh               # every gate before a merge into main (--quick skips Rust tests); CI repeats it on Linux after the merge

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
npm run core:test               # core crates only: catalog, naming, ingest, metadata, fileops
cargo test -p rawtonic-catalog --test session <name>   # single core test
cargo test -p rawtonic-app <name>                     # single native/adapter test
cargo test -p rawtonic-engine <name>                  # develop model, masks, looks (also -p rawtonic-rawdecode / rawtonic-gpu)
cargo test -p rawtonic-gpu -- --ignored gpu_processing::preview_tests   # GPU equivalence tests (needs a GPU)
scripts/check_layering.sh       # engine/core crates must not reach Tauri
APP_FIXTURES=/path/to/private-fixtures cargo test -p rawtonic-app --test fixture_render --locked
cargo deny check licenses bans sources
npm run tauri -- build          # optimized app → target/release/bundle/
scripts/models/install_models.sh [--runtime] cc.rawtonic.qa   # AI models from the private pack (gh), see docs/MODELS.md
```

Gotcha: workspace `default-members` excludes `src-tauri`, so plain `cargo test`/`cargo build` only cover the core and engine crates (incl. `rawtonic-gpu`) and the CLI. Use `--workspace` or `-p rawtonic-app` for the desktop crate. Native SDK prerequisites (Xcode CLT / Ubuntu webkit2gtk etc.) are listed in README. The `fixture_render` harness prints SKIP without `APP_FIXTURES`; that is not a pass. `cargo deny check advisories` currently fails on two transitive unmaintained crates (paste, proc-macro-error) — a known, unwaived release blocker (ADR 0005).

CLI without the desktop app: `cargo build -p rawtonic-cli` then `./target/debug/rawtonic {create,ingest,list,inspect,rate,flag,sync,adopt-xmp,bench,decode,export}`. `rawtonic export SRC --output OUT.tif [--adjustments recipe.json] [--keep-metadata] [--color-space display-p3|adobe-rgb|prophoto] [--models DIR] [--face-retouch '{"blemishes":60,"evenSkin":50}']` (or a folder: `rawtonic export DIR --output OUTDIR --format tiff`) renders without Tauri, byte-identical to the app's export: it reads each photo's recipe (session database, else `.rrdata`) and exports virtual copies, like the app's headless export (`rawtonic-app export <src> --output <dst> --format jpeg|tiff ...`, see README). `--face-retouch` finds the faces with the face models in `--models` (ONNX Runtime from `ORT_DYLIB_PATH`) and renders that retouch, as the Retouch tab would. It needs the app's `lensfun_db` folder: beside the binary, `$RAWTONIC_LENSFUN_DB` or `--lensfun-db`.

## Architecture

Two halves, mid-migration (ADR 0001, `docs/ARCHITECTURE.md`):

**Core crates (`crates/`)** — new, standalone, desktop-independent, with real tests:
- `catalog` — SQLite schema/migrations, session and catalog modes (one schema), relative-path validation (traversal/symlinks), asset identity, metadata, versioned recipe history. SQLite is authoritative for managed assets.
- `fileops` — journaled, undoable rename/move/trash (validated plan, no-clobber renames, rollback, crash recovery; the system Trash is a trait the app implements). App glue: `src-tauri/src/file_operations.rs`.
- `metadata` — atomic XMP sidecar IO. `naming` — strict filename templates. `ingest` — streamed copy to temp file, SHA-256 verify both sides, no-clobber publish, optional backup destination, cancellation.
- `denoise` — AI Denoise: Nonlocal MatchFilter on the Bayer mosaic (Core ML on macOS, ONNX Runtime elsewhere), blind noise estimate, tiles, a content-keyed f16 cache; plugs into rawdecode's `mosaic_denoise` hook (before highlight reconstruction and demosaicing). App glue `src-tauri/src/ai_denoise.rs`; the recipe's `aiDenoise` (0–100) decides, Develop re-develops via `apply_ai_denoise`. Its `coreml::CoreMlModel` (compile once to `.mlmodelc`, one float input/output) also runs the app's BiRefNet Subject model on macOS (`ai_processing::SubjectModel`, ONNX fallback; masks computed ahead when the masking tools open, `precompute_ai_foreground_mask`).
- `faces` — faces for People masks and the face retouch: YuNet detection (two passes), MediaPipe Face Mesh V2 landmarks, MediaPipe multiclass parsing per face and of the whole photo, and `part_mask` (skin, hair, clothes, eyes, brows, lips, teeth, entire person; everyone or one person). Models from the pack (`scripts/models/export_faces.py`, docs/MODELS.md); app glue `src-tauri/src/ai_faces.rs` (`detect_people`, `generate_ai_people_mask`, the open photo's analysis cached in `AppState.faces`); the mask is stored as an `ai-people` bitmap. `retouch` makes the face retouch's per-face data (crop, skin and under-eye weights, blemishes with prominence) for `analyze_face_retouch`; the engine's `face_retouch` renders it (blemishes healed without a source, even skin/texture, dark circles, shine) after spot removal. Ignored tests `crates/faces/tests/real_photos.rs` (`FACE_MODELS`, `FACE_PHOTOS`, `FACE_OUT`, `ORT_DYLIB_PATH`) write part overlays and `retouch_photos.rs` blemish overlays; `cargo test -p rawtonic-app --release --lib renders_face_retouch -- --ignored` renders before/after crops (`FACE_SETTINGS`).
- `onnx` — every ONNX model loads through `rawtonic_onnx::Model`: GPU plugin providers (CUDA if installed, WebGPU on Windows/Linux) checked once against the CPU, CPU fallback, per-model exceptions, preference `aiGpu` (ADR 0011). `rawtonic ai-bench` times and compares providers.
- `cli` — the `rawtonic` binary over the above, plus measured benchmarks (`decode` times rawdecode without the app).
- `merge` — photo merges on scene-linear frames (`frame`: base images without colour NR/sharpening, `load_corrected` for lens-corrected, rebalanced frames): `hdr` (Poisson-optimal merge, alignment, deghosting), `panorama` (own keypoints, bundle adjustment, projections, seams cut where frames agree, two-band blending, boundary warp/crop applied after the stitch), `hdr_panorama` (brackets grouped per position → HDR → stitch), `focus_stack` (the moved stacker + `align`/`planes`), and `dng` writing the result as a 16-bit float linear DNG that develops like the source RAW (matrices, As Shot, EXIF, `dng_baseline_equivalent`; lens data in the starting `.rrdata` via `initial_adjustments`). App glue `src-tauri/src/merges.rs` (`merge_photos`/`save_merge`, one `MergeModal`); collage rendering `src-tauri/src/collage.rs`. CLI `rawtonic merge {hdr,pano,hdrpano,focus,single} … [--auto-settings]`; synthetic test sets and comparisons in `crates/merge/examples/` (notes: reference/merges.md).
- Engine layers extracted from the adapter (ADR 0009), lowest first: `runtime` (stage timing behind `performance-profiling`, allocator release); `rawdecode` (raw_processing, demosaic, gpu_demosaic, DNG opcodes/colour, capture_info, camera colour/exposure tables); `engine` (image_processing core = adjustment model/geometry/detail/scopes, baseline_exposure, auto_adjust, camera_profile, looks/colour tools, lens_correction + embedded_lens maths, spot_removal, face_retouch, mask_generation rasterisation + masks definitions, cache_utils hashes, lut); `gpu` (gpu_processing: GpuProcessor, shader variants + WGSL in `crates/gpu/src/shaders/`, readback, WgpuDisplay presentation, `GpuContext::headless`, GPU equivalence tests); `export` (export settings/geometry/encoders, render pipeline with `RenderEnv`, exif_processing/tiff_exif/xmp_embed with `export_metadata_with` + `encode_with_metadata`, `sidecars` (virtual paths, `.rrdata` reading, library walk, export naming), `output::write_new_output`, `file_export::FileExporter`). Engine also holds image_loader's decode path (`load_base_image` + `BaseImageOptions`), adjustment_utils and the LUT parsers. `scripts/check_layering.sh` (CI) fails if any of these crates reaches Tauri.
- Where to edit: develop logic → the crate (`crates/engine`, `crates/rawdecode`, `crates/gpu`); a Tauri command or `AppState` access for it → the same-named thin module in `src-tauri/src` (e.g. `src-tauri/src/camera_profile.rs` = `pub use rawtonic_engine::camera_profile::*` + commands; `src-tauri/src/gpu_processing.rs` = window surface, the processor/image cache in `AppState`, render wrappers taking `tauri::State`). Engine/GPU functions take data (device, caches, images), never `AppState`. Test-only helpers for another crate's tests go behind a `test-support` feature.

**Desktop adapter (`src-tauri/`, crate `rawtonic-app`, lib `rapidraw_lib`)** — Tauri commands, `AppState`, image loading/caches, file management, export, AI, tether, culling, merge/collage glue and the thin glue modules over the engine crates. `export_processing`, `image_loader`, `exif_processing` and the other same-named modules are now thin glue over the engine/export crates; still inherited and Tauri-bound: file_management, ai_*, inpainting, tagging, camera_tethering. ADR 0009 moves AI/tether only when a consumer outside the app needs them, by its move-don't-rewrite method and byte-identical-export gate. Moved modules stay reachable under their old `crate::<module>` names through those glue modules or a `use rawtonic_…::{…}` line in `lib.rs`. Don't add empty crates pretending a separation exists. `lib.rs` is wiring only: module declarations, `run()` and `generate_handler!`; the Develop source/preview pipeline lives in `editor_preview.rs`, start-up services in `startup.rs`, merges in `merges.rs` over rawtonic-merge. There are no crate-root re-exports: import from the owning module (`crate::app_state::AppState`, `crate::app_settings::load_settings`). `file_management.rs` re-exports its submodules (`file_management/{listing,folders,albums,thumbnails,edits,operations,presets,xmp_sync}.rs`). `session_bridge.rs` is the transitional adapter between the inherited editor (which uses `filename.ext.rrdata` sidecars) and the portable SQLite session; it also writes standard metadata + recipe to `filename.xmp`.

**Rendering:** native wgpu presents *under a transparent webview*. Develop pixels never cross IPC — only renderer commands and small thumbnails do. Do not replace rendering with CSS/canvas filters or fabricated previews. The display path uses specialized shader variants (compiled lazily, general shader as fallback); CPU-readback exports use the general shader and must stay pixel-exact. Changes to the pipeline are validated with the GPU equivalence tests in `crates/gpu/src/gpu_processing_tests.rs` (`cargo test -p rawtonic-gpu -- --ignored gpu_processing::preview_tests`) and byte-identical full-res export comparisons against the previous commit.

**Frontend (`src/`)** — React 19 + zustand stores (`src/store/`) + Tailwind 4, i18next. `src/components/studio/` is the original Rawtonic Library/Develop workspace; `components/panel`, `modals`, `adjustments` etc. are inherited RapidRAW UI, some still reachable via a separate "tools" dialog pending migration. Tauri command names are centralized in the `Invokes` enum in `src/components/ui/AppProperties.tsx`; most backend interaction lives in `src/hooks/` (`useImageLoader`, `useImageProcessing`, `useThumbnails`, `useTauriListeners`, …).

**Session layout on disk:** `session.json`, `session.sqlite` (WAL), `Capture/`, `Selects/`, `Output/`, `Trash/`, `masks/`, `.cache/`. Asset paths are relative so closed sessions can be moved. Catalog SQLite connections are never shared across threads; catalog work runs in blocking workers for IPC.

## Key docs

`docs/ARCHITECTURE.md`, `docs/DEVELOPMENT.md`, `docs/RECIPE_SCHEMA.md`, `docs/MODELS.md` (AI models, origins and licenses), `docs/testing/README.md` (fixture manifest format), `docs/decisions/` (ADRs), `CONTRIBUTING.md`, `PRIVACY.md`, `TRADEMARKS.md`, `THIRD_PARTY_NOTICES.md`.
