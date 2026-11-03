# ADR 0021 — Bounded contexts, shared kernel and the typed recipe

Status: Accepted, 2026-11-03. Refines ADR 0001 and ADR 0009; records Part 2 and the decisions D1–D5 of the
architecture review and cleanup plan (notes repo `docs/plans/architecture-cleanup-2026-11.md`, approved by the
second-model review in round 3).

## Context

Measured on `main` @ `aa8e7f3c` (about 320k lines of Rust): the develop recipe is a `serde_json::Value` read by
string key in 35 of 63 engine files; version gating and recipe defaulting are scattered; four files collect every
new adjustment; the desktop adapter holds 94 flat modules around a 57-field `AppState`; preview and export
assemble the same render separately and the CLI exports a second way; safety-critical file primitives (exclusive
rename, directory sync, verified copy, volume detection) exist in several copies with different contracts;
cancellation has four representations; three crates keep a process-wide wgpu device registry; the IPC contract is
written by hand.

The work is a behaviour-preserving refactor: byte-identical exports, unchanged saved recipes, catalogs and
settings, move-only commits apart from logic, one context per pull request. No rewrite, no big bang, no ceremony:
a port or trait appears only where two implementations exist or a test needs one, and no crate is split for
compile time alone.

## Decision

### Bounded contexts

| Context | Owns | Crates (target) |
|---|---|---|
| Capture (source facts) | reading RAW/image files: decode, capture facts, embedded lens data, camera crop | `rawdecode` (the only crate importing rawler), `codecs` |
| Develop | the Recipe aggregate (typed, versioned, normalised), stages Recipe → uniforms, masks, retouch, lens | `recipe` (new, pure), `engine`, `gpu` |
| Render & Export | one render plan for preview and export; encoders, layout, export metadata | `gpu`, `export` |
| Library | Library aggregate, assets, collections, stacks, snapshots, search | `catalog` |
| Sidecars | XMP and `.rrdata` read/merge/write, provenance | `metadata` (XMP only) |
| File operations | journaled rename/move/trash, moves between volumes | `fileops` |
| Ingest | verified copy pipeline, queue, hot folders, import from other catalogs | `ingest` |
| Interop | translating other editors' settings into our one tool | `interop` |
| Inference | ONNX Runtime / Core ML backends, model registry and verification | `inference` (`onnx` renamed) ← `denoise`, `faces`, `segment` |
| Tethering, Contribution | unchanged | `tether`, `contribute` |

### Shared kernel

Small, stable, one owner each:

- `rawtonic-fs`: `atomic_replace`, `persist_noclobber`, one `exclusive_rename` that is exclusive or fails with
  `Unsupported` (it never checks, then renames), `sync_dir`, `verified_copy` (one method), volume information and
  free space (one FFI layer), `sha256_hex`.
- `runtime` grows a `CancelToken` and one `Cancelled` error (no heavy dependencies), and a `GpuHandle` (device,
  queue, pipeline cache) replacing the three registries, behind a non-default `gpu` feature. A check keeps wgpu out
  of `cargo tree` for catalog, fileops and ingest.
- No shared domain crate. It would become another dependency hub.

### Value objects stay with their owner

`Rating`, `Flag`, `ColorLabel` live in `metadata`; `AssetId`, `RelPath`, `LibraryLayout` in `catalog`; `PhotoRef`
next to the virtual-copy code. Each keeps its stored form through an explicit codec: `PhotoRef::parse/format`
reproduce the existing `path?vc=id` string exactly (a two-field struct cannot do that with `serde(transparent)`).
A type moves to a shared crate only when a second context demonstrably needs it. The SQLite migration runner is
separate machinery, not a value object.

### StoredRecipe and ResolvedRecipe

The recipe becomes a typed, versioned aggregate in the new `recipe` crate. `StoredRecipe` is what is saved
(sidecars, catalog history, presets): it keeps every stored key and value as written, so saved recipes stay
readable and byte-stable. `ResolvedRecipe` is the stored recipe after version gating and defaulting, the one input
of every render, preview and export alike. "Make a recipe renderable" happens in one place instead of five.
Stages translate their slice of the resolved recipe to uniforms; the GPU keeps its `PreparedAdjustments` boundary.

### Application services

No separate use-case crate. A workflow's service lives in the crate of the context that owns it
(`export::PhotoExporter`, `ingest` as today, `fileops` plans with the catalog update through a port). The app and
the CLI both call them. Revisit only if the CLI still duplicates app logic after the render and export phase.

### Adapter and frontend targets

- `src-tauri`: `lib.rs` is wiring of at most about 150 lines; modules group by context (`develop`, `library`,
  `export`, `ai`, `tether`, `interop`, `system`), each with its own `commands.rs` and its own sub-state in place of
  one `AppState`; functions take the sub-state they need, not `AppHandle`; one `CommandError {code, message,
  details}`; event names and payloads defined once.
- `src` is sliced by feature: `app/`, `shared/` (IPC wrappers, ui, lib, i18n), `entities/` (recipe, mask, photo,
  library, settings: no React, no invoke), `features/*` with a public `index.ts`. Features import only entities,
  shared and other features' `index.ts`; entities import only shared.
- Engine and GPU: per-stage modules (`develop/{tone,color,detail,geometry,effects}`), shader split per stage and
  concatenated in a fixed order, `pub(crate)` by default.

### Decisions D1–D5 (2026-11-03, taken by the coordinator on the user's delegation)

- **D1 — new crates `rawtonic-fs`, `recipe`, `codecs`; `onnx` → `inference`.** Each is created when its phase
  starts and never empty; no shared domain crate. Each has real content and removes a measured duplication or leak.
- **D2 — tauri-specta for the IPC contract, behind a trial.** Typed commands, arguments and events are the
  measured gap. The prerelease is pinned exactly; a trial on one context (tether, about 20 commands) measures build
  time, generated-code noise and runtime change. If it fails (blocking bug, more than 10 % build time, churn),
  fall back to ts-rs for types plus a contract test for names.
- **D3 — exclusive rename, or refuse; measured first.** Measure which of the user's volume types support
  `RENAME_EXCL`/`RENAME_NOREPLACE` and exclusive `link()`. Then: syscall rename where supported; else an exclusive
  hard link plus unlink of the old name; else a clear refusal naming the volume. No check-then-rename anywhere. If a
  common professional setup would lose in-place renames, escalate with the numbers before removing the fallback.
- **D4 — Prettier with the majority style (single quotes), formatted once now**, during the pause with no active
  frontend branches; parked branches are reformatted with the same config on rebase.
- **D5 — all phases, with a value checkpoint.** After the shared kernel and error model, the typed recipe core and
  the adapter state split, re-measure the end-state signals and the churn on the hot files. Continue with the
  expensive phases only if churn is falling and no preservation gate had to be loosened.

## Consequences

- Day-one content for every new crate; the layering check (`scripts/check_layering.sh`) and `docs/ARCHITECTURE.md`
  follow each move.
- Every step is judged by the preservation run: byte-identical exports, unchanged saved recipes, catalogs and
  settings.
- Value objects stay local, so a rename of a stored form is impossible by accident: the codec is the contract.
- The check-then-rename fallback stays at its current caller as a recorded safety exception until D3's measurement
  is done.
