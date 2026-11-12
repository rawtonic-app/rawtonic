# Architecture

## Overview

Rawtonic has two halves, still mid-migration (ADR 0001, ADR 0009):

- **Core and engine crates (`crates/`)**: desktop-independent, with real tests. SQLite is authoritative for managed
  assets; the engine layers `runtime` → `rawdecode` → `engine` → `gpu` → `export` hold the RAW pipeline.
- **Desktop adapter (`src-tauri`, crate `rawtonic-app`) and React workspace (`src`)**: Tauri commands, `AppState`,
  caches and the UI, built on the attributed RapidRAW renderer.

**Rendering:** native wgpu presents *under* the transparent webview. **IPC rule:** only renderer commands and small
thumbnails cross IPC; Develop pixels stay on the GPU. Engine and GPU functions take data, never `AppState`;
`scripts/check_layering.sh` fails if an engine or core crate reaches Tauri. Architecture decisions are in
`docs/decisions/`; the bounded contexts, the shared kernel and the typed-recipe target are ADR 0021
(`docs/decisions/0021-architecture-contexts-and-recipe-model.md`).

Dependencies below are the normal workspace dependencies from `cargo metadata` (2026-11-03, `main` @ `aa8e7f3c`);
every crate also uses external crates. `rawler`, the vendored RAW library (`third_party/rawler`), is left out
of the lists (rawdecode, engine, export, ingest and merge use it).

## Layer tables

### Core crates (`crates/`)

| Crate | Owns | Depends on | Key modules | Tests/hooks |
|---|---|---|---|---|
| `catalog` | SQLite schema, sessions/catalogs, recipes, search | `fs`, `metadata` | `lib`, `query`, `add` | `core:test` |
| `fs` | file-safety kernel: atomic write, no-clobber publish, temporary and staging files; volume detection | none | `lib`, `no_clobber`, `temp`, `staging`, `volume` | `core:test` |
| `metadata` | XMP sidecars, provenance, labels, photo identity (`PhotoRef`: `<source>?vc=<copy>` codec, TS twin `src/entities/photo/photoRef.ts`, shared vectors) | `fs`, `recipe` | `lib`, `provenance`, `labels`, `photo_ref` | `core:test` |
| `fileops` | journaled rename/move/trash, `purge` | `fs` | `lib`, `volume`, `tree` | `core:test`; crash hooks |
| `naming` | strict filename templates | none | `lib` | `core:test` |
| `ingest` | verified no-clobber copy, queue | catalog, fs, metadata, naming | `copy`, `queue` | `core:test` |
| `interop` | other editors' settings → ours | `metadata` | `lightroom`, `capture_one` | `-p rawtonic-interop` |
| `tether` | camera tethering helper (ADR 0012) | `fs` | `protocol`, `staging`, `client` | `--mock` camera |
| `contribute` | scrubbing, relay client (ADR 0017) | `fs` | `scrub`, `payload`, `crash`, `native` | panic/abort tests; Windows crash-in-event-log test |
| `merge` | HDR, panorama, focus stack, DNG | `engine`, `rawdecode`, `rawler` | `hdr`, `panorama`, `dng` | `examples/` |
| `cli` | `rawtonic` binary, benchmarks | most core, engine, AI crates | `main`, `search_bench` | `crates/cli/tests` |

### Engine layers (ADR 0009), lowest first

| Crate | Owns | Depends on | Key modules | Tests/hooks |
|---|---|---|---|---|
| `runtime` | stage timing, allocator release | none | `performance`, `allocator` | none special |
| `rawdecode` | RAW bytes → oriented linear RGB | `rawler`, `runtime` | `raw_processing`, `demosaic` | fixtures |
| `recipe` | stored + resolved develop recipe, typed keys | serde, serde_json only | `stored`, `tone`, `color`, `detail`, `geometry`, `effects`, `lens`, `masks`, `resources` | `-p recipe` |
| `engine` | develop model, masks, looks, lens | fs, metadata, rawdecode, recipe, runtime | `image_processing` | `-p engine` |
| `gpu` | wgpu pipeline, shaders, display | engine, fs, rawdecode, runtime | `gpu_processing` | `--ignored` GPU |
| `export` | render, encoders, queue | engine, fs, gpu, metadata, naming, rawdecode, recipe, runtime | `file_export` | exports |

### AI crates

| Crate | Owns | Depends on | Key modules | Tests/hooks |
|---|---|---|---|---|
| `onnx` | every ONNX model load, providers, the bundled VC++ runtime and DirectX Shader Compiler on Windows | none | `runtime`, `vc_runtime`, `dxc`, `check`, `background`, `watch`, `bench` | `rawtonic ai-bench` |
| `denoise` | AI Denoise on the Bayer mosaic | `onnx`, `rawdecode` | `coreml`, `tiles`, `noise` | model pack |
| `faces` | detection, landmarks, parsing, retouch | `onnx` | `detect`, `parts`, `retouch` | ignored `real_photos` |
| `segment` | Sky and Subject networks | `denoise`, `onnx` | `models`, `focus` | model pack |
| `superres` | Super Resolution (2× at export) | `denoise` (Core ML), `onnx` | `Model`, `enlarge_with` | unit tests, model pack |

Workspace `default-members` leave out `src-tauri`, `denoise`, `faces`, `segment`, `superres`, `onnx` and `third_party/rawler`;
use `--workspace` or `-p <crate>` for them.

### Desktop adapter (`src-tauri`)

| Area | Owns | Depends on | Key modules | Tests/hooks |
|---|---|---|---|---|
| `rawtonic-app` | Tauri commands, `AppState` | engine, core, AI crates | `lib`, `app_state` | `-p rawtonic-app` |
| glue modules | same-named thin modules | the crate they wrap | `camera_profile`, `merges` | `fixture_render` |
| inherited | Tauri-bound code | `AppState` | `file_management`, `ai_*`, `inpainting` | per module |

### Frontend (`src`)

| Area | Owns | Depends on | Key modules | Tests/hooks |
|---|---|---|---|---|
| `components/studio` | Library/Develop workspace | stores, hooks | `StudioDevelop` | vitest (jsdom) |
| `panel`, `modals` | inherited RapidRAW UI, canvas | stores, hooks | `panel/Editor.tsx` | vitest |
| `store`, `hooks`, `utils` | zustand stores, IPC hooks | `Invokes` enum | `useImageLoader` | `npx vitest run` |
| `activity` | activity centre, `notify` | `store` | `bridges.ts`, `notify.ts` | `noToastFailures.test.ts` |

Tauri command names are collected in the `Invokes` enum in `src/components/ui/AppProperties.tsx`.

## Crate details

Read the part for the crate you change. The crate map was moved verbatim from AGENTS.md (2026-10-17) so agents load it
only when needed. Each section below keeps the facts of the former single-paragraph map.

### Baseline and target

The desktop adapter lives in `src-tauri`, using the attributed RapidRAW renderer and React workspace in `src`. Native
wgpu presents under the transparent webview. Renderer commands and small thumbnails cross IPC; native Develop output
stays on GPU.

New Rust workspace crates isolate data and file safety.


### Core crates

**Core crates (`crates/`)** — new, standalone, desktop-independent, with real tests:

#### `catalog`

`catalog`: migrations, session/catalog manifest, relative paths, identity, metadata and recipe history.

`catalog` — SQLite schema/migrations, session and catalog modes (one schema), relative-path validation
(traversal/symlinks), asset identity, metadata, versioned recipe history. SQLite is authoritative for managed assets.

- `migrations`: the schema's one table of steps (`MIGRATIONS`, the SQL scripts in `crates/catalog/migrations/`) and its
  one runner; a new library is migrated from schema 0, an opened one from its `user_version`, in one immediate
  transaction. Adding schema 26 = `migrations/0026.sql` + one `MIGRATIONS` line (`crates/catalog/README.md`).
  `tests/migration_fixtures.rs` upgrades a populated catalog of every schema (1–26) and compares it with the recorded
  result (schema, rows, foreign keys, searches). `fileops` keeps its journal's steps in its own `migrations.rs` of the
  same shape.


- `query`: the library search and smart albums (ADR 0014; rules over text, keywords, culling, file type, camera, lens,
  exposure, dates, albums, folders, compiled to one SQL query; value lists with counts; schema 17 adds `Field::Ai`
  (recipe facts across versions, maintained by SQLite triggers calling `rawtonic_ai_facts`) and `Field::SourceType` (the
  file’s IPTC Digital Source Type); smart albums are `collections` of kind `smart` with their rules, schema 11);
- `facts`: capture facts per photograph (`CaptureFacts`, recorded by verified ingest, a background pass reads the rest:
  `src-tauri/src/library_search.rs`, which also holds the search commands; the camera's own rating becomes the
  photograph's while nobody set its metadata; schema 13 read again what the earlier reader missed, schema 14 the undated
  ones once the reader followed TIFF-based files' late IFDs); keywords one row each in `asset_keywords` (kept by
  triggers), text in the trigram index `asset_search`.
- `capture_time`: corrected capture times (Shift Capture Time, schema 19: `capture_overrides`, apart from
  `assets.capture_time`, so a facts reread never undoes one and deleting the row restores the file's time; exact shift
  arithmetic keeping fraction digits and UTC offset; the editor state's EXIF map carries the correction on read and
  save; search uses the effective time; app glue `src-tauri/src/capture_time.rs` with preview/plan/apply commands,
  exports take it through `PinnedPhoto::capture` and `rawtonic_export::exif_processing::apply_corrected_capture_time`,
  UI `CaptureTimeDialog`). The XMP sidecar carries it as `exif:DateTimeOriginal` + `photoshop:DateCreated`
  (`capture_write` → `rawtonic_metadata::CaptureWrite`; `metadata.xmp_capture`, schema 27, is the value the library
  owns there, so a reset removes only that); a sidecar's differing `exif:DateTimeOriginal` becomes the correction on
  import, Use XMP, adopt and the first facts read (`adopt_sidecar`), compared with the file's whole time (facts keep its
  fraction and offset, `assets.capture_subsec`/`capture_offset`, schema 28); Use XMP and its undo bring the `.rrdata`
  EXIF along (`src-tauri/src/capture_time.rs::follow_correction`).
- `session_move`: whether a whole session folder may move into a folder (G40: photos never move out of their session; a
  catalog is not moved this way) — not into itself or another library, no name clash, its database not open in another
  connection (an exclusive-lock probe); the move itself is one `fileops` step, app glue `src-tauri/src/session_move.rs`
  (`move_session`, the offer in `plan_move_files`/`plan_move_folder`, undo/redo reopen the session where it is,
  `session-moved`).
- `face_eyes`: eyes per face for culling (schema 21: `eye_reads` per photograph with the reader version and fingerprint,
  `face_eyes` per face with box, call, scores and the photographer's `override_state`, carried to the same face by box
  overlap when read again), `eye_backlog`, `record_eyes`, `photos_eyes`, `set_eye_override` and the search field `Eyes`;
  the reader is `rawtonic_faces::eyes` (stage A rules and thresholds, notes `benchmark-results/fqb-eyes-2026-10-31/`),
  app glue `src-tauri/src/face_eyes.rs` (background pass behind the `eyeCheck` setting, never downloading by itself;
  Loupe faces and grid badges `src/components/studio/StudioEyes.tsx`; the strip's face box on the photograph
  `src/components/panel/editor/FaceBoxOverlay.tsx`, carried through the recipe's geometry by
  `rawtonic_engine::red_eye::box_outline_shown`, command `face_boxes_shown`).
- `content`: content search's index (G37, schema 25: `content_embeddings`, one row per photograph with the model id —
  the vision file's SHA-256 — the file's fingerprint, 768 int8 values and their scale, or the read error),
  `content_backlog`, `record_content`, `content_status`, `content_ranking` (dot product, best first; a ranking, never a
  filter or a smart-album rule); app glue `src-tauri/src/content_search.rs` (background pass from the cached thumbnail
  with SigLIP 2's image encoder on two utility-QoS ONNX Runtime threads behind the `contentSearch` setting, waiting for
  exports, batch writes and auto tags, which hand it their embeddings; the text encoder as ONNX external data loaded on
  the first query and dropped after 5 min idle; never downloading by itself), UI `src/components/studio/StudioSearchBox.tsx`
  and `src/hooks/useContentSearch.ts` (the view's order: `useSortedLibrary`).
- `duplicates` + `image_hash`: Find Duplicates stage 1 — a hash of each photograph's image data as its file stores it
  (TIFF-based RAWs: the IFD with the most strip/tile bytes; RAF: the CFA section; ISO media/CR3: the largest track's
  samples; JPEG: all but APPn/COM segments; PNG: IHDR/PLTE/tRNS/IDAT; else the whole file, marked `file`), recorded in
  `image_hashes` (schema 20) with the file size and nanosecond modification time it was read at and dropped by a trigger
  when the registered size, time or fingerprint changes; `fill_image_hashes` (worker threads, cancellable),
  `duplicate_groups` (each member with its `owner` — `Library::owner_elsewhere`, the library whose folder holds the file
  — and whether it lies in this library's own folder), `suggested_trash` (keep the copy with the most work, on a tie the
  oldest import; pre-mark only own-folder copies, never another library's), `confirm_duplicates` (re-read before
  trashing; refuses another library's photographs) and the search field `Duplicate`; app glue
  `src-tauri/src/duplicates.rs` (one background pass with progress events, started by Find Duplicates or by a
  search/smart album using the rule), UI `DuplicatesDialog`, trashing through the journaled file operations.
- `subset`: Export as Catalog (G39) — a `VACUUM INTO` snapshot of a session or catalog database pruned to the chosen
  photographs (`retain_only`: their rows cascade, albums left empty and their groups go, as do jobs, the file-operation
  journal, the hot folder and the drive cache), compacted so nothing of the others stays behind (the full-text index
  made again from its rows, then `VACUUM`); asset ids are kept for Import from Catalog; file copying (verified originals
  into `Photos/<folder>/` with *Include original files*; without, photographs inside the source library's folder, which
  belong to it, go edits only — at their place under `Photos/` without their file (`Library::place_without_original`),
  offline in the new catalog; photographs inside another library's folder (`owned_elsewhere` over
  `Library::owner_elsewhere`: their latest edit is there) are refused or left out —, edit resources, a pre-publish hook
  the app uses for Standard previews, staged beside the destination and renamed into place) is
  `rawtonic_ingest::catalog_export`, app glue `src-tauri/src/catalog_subset.rs` (previews of photographs left outside go
  to `.cache/carried-previews`, adopted into the app's preview cache when the catalog opens:
  `preview_proxy::adopt_carried`), UI `CatalogExportDialog`.
- `catalog_import`: Import from Catalog (G39) — `review_import` matches each photograph of another library's snapshot
  (`rawtonic_ingest::catalog_import::open_source`: `VACUUM INTO` the catalog's `.cache`, migrated there, the other
  library never opened for writing) by asset id, else by SHA-256 (recorded, or read from both files when size and
  fingerprint agree; a photograph here is claimed once, the earlier-registered copy first) and compares edit, copies,
  metadata, capture time, eye calls, snapshots and albums with resource references named by their content;
  `apply_import` adds new ones with all their rows (`ATTACH`, recipe and snapshot keys offset, same asset ids, an import
  batch), replaces changed ones (only what differs; the edit a new history step, the replaced one kept as a named
  snapshot; snapshots of the photo and its copies inserted or updated by id; an incoming unedited state stored as an
  explicit default edit), the app writing the touched photographs' derived sidecars, their copies' too, again
  (`session_bridge::rederive`, which also runs when a photograph comes back online), makes virtual copies or keeps, and
  adds album memberships (albums by id, else by name and group), all in one transaction; photographs inside a third
  library's folder are left out, those inside the other library's folder are copied. Which side changed a photograph
  (UXR3 D2): Export as Catalog writes `export-baseline.json` (a digest per photograph and part at the export, with the
  source catalog's id and, as `lineage`, the source's own common states with other catalogs, so a catalog exported from
  an exported catalog still tells the sides apart back in the first), and each import records the parts it left the same
  on both sides in this catalog's `import-baselines/<other catalog id>.json` (and, for an exported catalog's parts
  unchanged since its export, under the catalog it was exported from); each photograph's record carries its time
  (`@recorded`) and the review compares both sides with the latest common state (`Side`: there → Replace by default,
  here or both → Keep current; no record → both). Replace returns what puts the replaced metadata back (`travel_import`,
  one Undo step in the app); Keep current over a change there keeps the other catalog's edit as a snapshot, and metadata
  either choice leaves out of the photograph goes into the receipt and `import-reports/<time> <catalog id>.json`
  (`SetAside`). New photographs without their original in the other catalog (an edits-only export) are left out and
  counted. App glue `catalog_subset.rs`, UI `CatalogImportDialog`.
- `stacks`: stacks (G36, schema 22: `stacks` with the collapsed flag, `stack_members` with asset, copy and position —
  the lowest position is the top, so a top that leaves promotes the next; triggers dissolve a stack left with one member
  and drop a forgotten copy), each command one transaction returning the touched stacks before/after for undo
  (`restore_stacks`), and Auto-Stack (`auto_stack_plan`: EXIF auto-bracket runs, maker burst frame numbers
  (`assets.sequence`, Fujifilm today), else the capture-time gap; never re-stacks); app glue `src-tauri/src/stacks.rs`,
  grid arrangement `src/store/useStackStore.ts` (collapse in folder views without a filter only), commands
  `src/utils/stacks.ts`, UI `AutoStackDialog`. `assets.stack` stays Edit With's render link.
- `snapshots`: named snapshots of a version's adjustments (ADR 0015, schema 12; their own table, carried by the editor
  state the library hands out and so by the derived `.rrdata`, adopted from sidecars; app glue
  `src-tauri/src/snapshots.rs`, UI `StudioSnapshots`).

#### `fs`

`fs` — the file-safety kernel every crate that writes beside the user's files uses (`rawtonic_fs`): `atomic_write`
(same-folder temporary file, sync, atomic replace, never through a symlink); `no_clobber::persist` publishes a finished
temporary file under a new name, never replacing one (export outputs, ingest copies, generated images; tether transfer
slots use `persist_in_own_folder`), with an O_EXCL copy where the volume has no exclusive rename or hard links
(exFAT/FAT; the copy's short non-atomic window is documented there); `temp` (recognisable `.rawtonic-tmp-` names and
their sweep) and `staging` (locked staging folders of jobs, removed only when abandoned). `volume` is the one place that
asks the system about mounts (`statfs`/`getfsstat`/`getattrlist` on macOS, `/proc/self/mountinfo` and
`/dev/disk/by-*` on Linux, the Windows volume API): the mount of a path and every mount (`Mount`), `VolumeInfo` (name,
file system, free space, network), the one list of network file systems, removable media, free space and mount-change
notifications; the catalog builds volume identity from it, file operations plan moves with it.

#### `metadata`

`metadata`: XMP sidecar IO (written through `rawtonic_fs::atomic_write`) and shared recipe provenance predicates
(`provenance`, consumed by the renderer, export and catalog).

#### `naming`

`naming`: strict templates.

`naming` — strict filename templates.


#### `ingest`

`ingest`: streamed verified no-clobber copying.

`ingest` — verified copies (`copy`: one read of a source for the library copy and the backup, SHA-256 recorded, each
copy and the source read again compared byte for byte, or by SHA-256 above 256 MiB; the next files read ahead; one
drive-cache flush per photograph before it is recorded, ADR 0019), no-clobber publish, optional backup destination,
cancellation; `examples/ingest_card.rs` times an ingest of a card folder; card XMP sidecars matched ignoring letter
case; RAW+JPEG pairs imported as one photograph with the JPEG as its companion (ADR 0013; `catalog::companion`:
`assets.raw_jpeg_pair`, never registered again, left out of listings by `session_bridge::companions_in_folder`, moved
and trashed with its RAW by `file_operations::associated_files`); every copy ingest is a resumable job (`ingest::queue`,
`catalog::ingest_jobs`: `tasks` + `ingest_items`, planned copies recorded before copying; app glue
`session_ingest_jobs`/`_resume`/`_forget`).

`folder_add`: Folders › Add Folder… in a catalog (G39) — `plan_folder_add` reads the folder (subfolders optional) and
lists the files to register, counting those already registered (at their place or as a copy of held content, so a scan
cut at the limit still advances), refusing those inside another library's folder by owner, naming unreadable folders,
and bringing a registered RAW along for a new JPEG beside it so the two pair; the files go through the one Add path
(`batch::add_batch_with_metadata`, `session_ingest` mode `add`); app glue `session_bridge::catalog_add_folder_scan`, UI
`AddFolderDialog` (projects only show the folder, with Copy into this project…) and `UnaddedFolderBanner` for browse
roots added before.


#### `fileops`

`fileops` — journaled, undoable and redoable rename/move/trash (validated plan, no-clobber renames, rollback, crash
recovery; redo re-validates; the system Trash is a trait the app implements) moves to another volume as a verified copy
published by an exclusive rename, the originals then trashed, undone by copying back (ADR 0016; `volume`: free space
check, exclusive-rename probe, copy + read-back compare; `tree`: a whole folder as one step —
listing without links, tree copy, unchanged checks); and `purge`, the only deletion: emptying one real folder (a
session's `Trash/`) — entries set aside first, committed, then deleted without following links; not undoable, it ends
Undo for earlier operations. App glue: `src-tauri/src/file_operations.rs`; renames are planned once by
`file_management::rename_plan` (final names, sidecars, XMP owner, companion, conflicts), previewed in the rename dialog
and run as previewed.


#### `cli`

`cli`: desktop-independent session operations and measured benchmarks.

`cli` — the `rawtonic` binary over the above, plus measured benchmarks (`decode` times rawdecode without the app).


#### `merge`

`merge` (HDR, panorama and focus stack on scene-linear base images, written as 16-bit float linear DNGs with the
source's colour matrices, As Shot balance, EXIF and an equivalent baseline, so a merge develops like a RAW; used by the
app's merge dialog and `rawtonic merge`).

`merge` — photo merges on scene-linear frames (`frame`: base images without colour NR/sharpening, `load_corrected` for
lens-corrected, rebalanced frames): `hdr` (Poisson-optimal merge, alignment, deghosting), `panorama` (own keypoints,
bundle adjustment, projections, seams cut where frames agree, two-band blending, boundary warp/crop applied after the
stitch), `hdr_panorama` (brackets grouped per position → HDR → stitch), `focus_stack` (the moved stacker +
`align`/`planes`), and `dng` writing the result as a 16-bit float linear DNG that develops like the source RAW
(matrices, As Shot, EXIF, `dng_baseline_equivalent`; lens data in the starting `.rrdata` via `initial_adjustments`). App
glue `src-tauri/src/merges.rs` (`merge_photos`/`save_merge`, one `MergeModal`); collage rendering
`src-tauri/src/collage.rs`. CLI `rawtonic merge {hdr,pano,hdrpano,focus,single} … [--auto-settings]`; synthetic test
sets and comparisons in `crates/merge/examples/` (notes: reference/merges.md).


#### `tether`

`tether` — tethered shooting (ADR 0012): the helper program `rawtonic-tether` (libgphoto2 behind the `gphoto2` feature,
dynamically linked, never in the app; `--mock` scripted camera; `probe`, `queue` (the camera's transfer queue, read
only) and `version` commands; photos left in a camera's transfer queue are fetched after each connect), its protocol and
framing, the crash-safe transfer folder (`staging`: `<session>/.tether/incoming`, partial downloads quarantined in
`incomplete/`) and the app's client (`client`: request timeouts, helper death detected by exit, not only by EOF). App
glue `src-tauri/src/camera_tethering.rs`: starts the helper, ingests its photos through `hot_folder::ingest_capture`
with a transfer record written first, `tether-*` events; UI `src/components/session/TetherPanel.tsx`, its guided camera
test `TetherCameraTest.tsx` (anonymous report: `cameraTest.ts`) and the compatibility list `tetherCameras.json`
(docs/TETHER_CAMERAS.md). Packaging: `scripts/tether/build_libgphoto2.sh`, `bundle_helper.sh`.


#### `contribute`

`contribute` — the contribution channels' shared core (ADR 0017): `scrub` (a path start redacts to the end of its line,
naming only a known photo file type; Rust frames keep `lib.rs:12:5`; serial values to the end of the line; account and
host name, serial numbers, e-mail and IP addresses, GPS positions become placeholders; scrubbing twice changes nothing
more), `payload` (`prepare` fixes the scrubbed, pretty-printed body and its SHA-256 for review; `approve` refuses a body
changed since or not clean), `client` (one attempt per send, no redirects, 10 s connect / 30 s total timeouts, a 20-bit
proof-of-work stamp; the relay URL is fixed at build time: `relay.rawtonic.cc` in optimized builds, `RAWTONIC_RELAY_URL`
overrides (empty: none), debug builds and tests have none), `crash` (`CrashStore` in the config folder's
`crash-reports`: a run marker, the panic hook's note (`note_panic`: message, location and backtrace reduced to file
names) and an async-signal-safe note of a fatal signal (`arm`, chained to the handlers before it), both written only
while `set_recording` is on (off after Never); the marker holds the start phase (`note_phase`); `start` turns notes of a
process that is gone into a scrubbed record with the previous run's log tail, `start_with` also asks `native` how a run
gone without a note ended, `clean_exit` removes them, records expire after 30 days; tests run a real panic hook and a
real `abort()` in a child process), `native` (Windows: the "Application Error" entry (event 1000) Windows writes for a
crashed process, read at the next start for the run's process id and start time: exception code, module file name and
offset; chosen over an in-process exception filter, which misses fail-fast aborts and runs on a broken stack, and over WER
local dumps, which need administrator rights and keep the process memory; its Windows-only test crashes a child), `stats` (`StatsStore` in the config folder's `usage-stats.json`, channel 4: per ISO
week (UTC) the cameras opened once each, the tools from the relay's fixed list used, app/OS/GPU vendor; kept only while
on, deleted on off; `due` gives the finished weeks the relay still takes, older ones are dropped). App glue
`src-tauri/src/contribute.rs` (`contribution_available`/`_prepare`/`_send`/`_delete`,
`problem_report_context`/`_screenshot`,
`crash_reports_pending`/`crash_report_mark_asked`/`_remove`/`crash_reports_set_enabled`; `start_crash_records` first in
the setup (before the log is replaced; `startup::install_early_panic_hook` notes panics until the logger's hook),
`note_phase`/`note_gpu_phase` for the marker, `clean_exit` on every exit path, `note_gpu` from the GPU set-up; `start_usage_stats` sends the
finished weeks 30 s after a start when statistics are on, `note_camera` from `load_image`,
`usage_stats_set_enabled`/`_note_tools`/`_preview`), window capture `src-tauri/src/window_capture.rs` (shared with the
QA channel); UI `src/components/contribute/` (`ReviewSendDialog`, `ReportDialog` with Copy report / Open on GitHub,
`ProblemReportDialog`, `CrashReportDialog`, `ContributionHost` for the Help menu, the bar's "…" menu and the question at
start; `CameraHelpDialog`; `usageStats.ts`: tools used, from each history step's changed keys
(`useEditorStore.pushHistory`) and a few actions; Preferences `panel/UsageStatsSettings.tsx`). The relay itself:
`services/relay/` (docs/RELAY.md).


### AI crates

#### `denoise`

`denoise` — AI Denoise: Nonlocal MatchFilter on the Bayer mosaic (Core ML on macOS, ONNX Runtime elsewhere), blind noise
estimate (scaled by `MosaicDenoise::noise_scale`), tiles of the model's input size, a content-keyed f16 cache; plugs
into rawdecode's `mosaic_denoise` hook (before highlight reconstruction and demosaicing; from version 4 the hook
puts the sensor's local means back into the network's result, `keep_means`). App glue
`src-tauri/src/ai_denoise.rs` (one denoiser per model: versions 1–2 the first Core ML package, versions 3–4 the
mixed-precision 640 px one; no fallback, so a recipe always renders with its version's model); the recipe's `aiDenoise`
(0–100) and `aiDenoiseVersion` decide (`rawtonic_engine::image_loader::ai_denoise_settings`), Develop re-develops via
`apply_ai_denoise`. Its `coreml::CoreMlModel` (compile once to `.mlmodelc`, float inputs, one float output) also runs
the app's BiRefNet Subject model on macOS (`ai_processing::SubjectModel`, ONNX fallback; masks computed ahead when the
masking tools open, `precompute_ai_foreground_mask`) and Moebius, the guide of Remove's large holes
(`src-tauri/src/moebius.rs`: DDIM sampler, crop and model files; MI-GAN otherwise and away from macOS).


#### `faces`

`faces` (face detection, landmarks and parsing on ONNX Runtime, and People-mask rasterisation; the app downloads its
models and stores the masks as bitmaps).

`faces` — faces for People masks and the face retouch: YuNet detection (two passes), MediaPipe Face Mesh V2 landmarks,
MediaPipe multiclass parsing per face and of the whole photo, and `part_mask` (skin, hair, clothes, eyes, brows, lips,
teeth, entire person; everyone or one person). Models from the pack (`scripts/models/export_faces.py`, docs/MODELS.md);
app glue `src-tauri/src/ai_faces.rs` (`detect_people`, `generate_ai_people_mask`, the open photo's analysis cached in
`AppState.faces`); the mask is stored as an `ai-people` bitmap. `retouch` makes the face retouch's per-face data (crop,
skin and under-eye weights, blemishes with prominence) for `analyze_face_retouch`; the engine's `face_retouch` renders
it (blemishes healed without a source, even skin/texture, dark circles, shine; `face_retouch/protect.rs` puts painted
protected areas back) after spot removal. Ignored tests `crates/faces/tests/real_photos.rs` (`FACE_MODELS`,
`FACE_PHOTOS`, `FACE_OUT`, `ORT_DYLIB_PATH`) write part overlays and `retouch_photos.rs` blemish overlays; `cargo test
-p rawtonic-app --release --lib renders_face_retouch -- --ignored` renders before/after crops (`FACE_SETTINGS`),
`protects_painted_areas_of_real_photos` measures the protection brush the same way (measured invariants and open
preview/performance limits: notes `benchmark-results/fp-protection-2026-10-27/REPORT.md`, G69).


#### `segment`

`segment` (the Sky (UperNet) and Subject (BiRefNet) networks on ONNX Runtime or Core ML and their masks, `Models` to
load them from a model folder; the app loads them for the editor and its exports, the CLI from `--models`, so both
compute the same masks; Select Subject is `subject_selection` at the component's `subjectVersion`: version 2
(`select_subject`) is BiRefNet's foreground without separate objects clearly out of focus next to the sharpest
(`focus.rs`), version 1 and Relight's matte the network's plain output, `run_subject_model`).


#### `superres`

`superres` — Super Resolution: the export's 2× enlargement (Compact network, CC BY 4.0) in 512 px tiles with 24 px of
context, on Core ML on macOS or ONNX Runtime, the pixel type kept (`enlarge_with`). The export crate calls it through
its `Enlarger` trait (`enlarge_for_export`, before resize, border and output sharpening when `superResolution` is set);
glue `src-tauri/src/super_resolution.rs` (`prepare_super_resolution` fetches the model when the option is ticked,
`ExportEnlarger` for the export workers) and the CLI's `--super-resolution`. Whether it marks exports as generated
content is the one switch `export_marks_generated`.


#### `onnx`

`onnx` — every ONNX model loads through `rawtonic_onnx::Model`: GPU plugin providers (CUDA if installed, WebGPU on
Windows/Linux) checked once against the CPU, CPU fallback, per-model exceptions, preference `aiGpu` (ADR 0011).
`rawtonic ai-bench` times and compares providers.


### Engine layers

Engine layers are extracted from the adapter per ADR 0009, dependencies pointing downwards only and checked by
`scripts/check_layering.sh` in CI. Extracted, lowest first: `runtime` (opt-in stage timing, allocator release)

Engine layers extracted from the adapter (ADR 0009), lowest first: `runtime` (stage timing behind
`performance-profiling`, allocator release);


#### `rawdecode`

`rawdecode` (RAW bytes to oriented linear RGB: rawler front end, CPU and GPU X-Trans demosaic, DNG opcodes, highlight
reconstruction (colour ratios inside large partly clipped areas from `renderVersion` 9), hot pixels, unbiased shadow
blacks of `renderVersion` 8 (`black_bias`), maker-note capture data, camera colour and per-unit calibration).

`rawdecode` (raw_processing, source_frame (the frame of the RAW each `sourceFrameVersion` develops, ADR 0020),
camera_crop (the aspect ratio set in the camera where the RAW keeps the whole sensor: Canon AspectInfo, the Exif image
size), demosaic, gpu_demosaic, DNG opcodes/colour, capture_info, camera colour/exposure tables);


#### `recipe`

`recipe` (plan 2.2, ADR 0021) holds the develop recipe in two representations. `StoredRecipe` (owned) and
`StoredRef` (borrowed) are the JSON value exactly as saved — key presence, explicit `null`s, unknown and
nested fields — read and written only through typed keys (`Key<T> { name, default, since_version }`, one
registry per section module); nothing is filled in on load or save. `ResolvedRecipe` holds the typed sections
as they render (value objects such as `tone::Ev` and `tone::Amount`, which carry the Develop slider ranges but
do not clamp), produced only by `rawtonic_recipe::normalize` from a stored recipe and never saved. Sections
move in one per PR; moved so far: tone (exposure, contrast, highlights, shadows, whites, blacks, brightness,
centré), colour (white balance, presence, mixer, grading, calibration, black-and-white and colour ranges),
detail (sharpening, noise reduction and refinements, clarity/dehaze/structure, manual CA, defringe, moiré and
diffraction request), geometry (crop/aspect ratio, straighten, orientation, flips and perspective/transform),
effects (post-crop vignette, grain, LUT references/amount, glow/halation/flares and depth-based lens blur),
and lens (correction mode, profile parameters, amounts and switches, manual lens vignetting, the automatic
lateral CA switch, `lensProfileFor` and the lens version key names), and masks (containers and nested local
adjustments, using the same tone/colour/detail/effects keys with their own defaults and visibility).
`image_processing::get_global_adjustments_resolved` reads them from `ResolvedRecipe::tone`,
`ResolvedRecipe::color`, `ResolvedRecipe::detail` and `ResolvedRecipe::effects`; the warp reads its lens
parameters through `lens::resolve` (lens, like geometry, has no visibility gate). The mask stack is not part
of `ResolvedRecipe`: only the stages that read it resolve it (`masks::resolve`), so `normalize` does not pay
for it. The geometry warp and spatial preparation read `ResolvedRecipe::geometry`; geometry has no visibility
gate, and its value objects retain stored numbers until the existing casts and rendering conversions.
`normalize(stored, is_raw)` applies RAW capture defaults to missing detail amounts; hidden amounts become
neutral, sharpening parameters keep their defaults, and noise refinements remain stored. Detail value objects
do not clamp; the renderer retains its existing conversions. Absolute white balance keeps the renderer's
existing clamp; colour sliders otherwise retain their stored values until the existing rendering conversions.
The crate is below the engine and depends on serde and serde_json only (`scripts/check_layering.sh`). Effects
amounts keep their existing visibility rules: hidden effects use defaults, grain structure/colour remain
stored, and lens blur's stage retains its own visibility/enable gate. LUT and depth-map strings are borrowed
from the stored recipe, so `ResolvedRecipe` carries its lifetime and normalization never clones those
payloads. Lens blur's missing amount/fades remain 50/15, distinct from the frontend's initial 40/20. CPU
stages that need just effects, geometry, lens, colour or the mask containers use narrow section resolvers
(`effects::resolve`, `geometry::resolve`, `lens::resolve`, `color::resolve`, `masks::resolve`), so each
resolves only the sections it reads. The recipe owns all three file-valued keys (`resources::FILE_KEYS`: LUT,
camera profile and raster path); the recursive resource visitor in `metadata` consumes that complete list, and
LUT loading in `export` uses the same LUT key (re-exported by `effects`). This dependency expresses ownership
of the recipe grammar rather than a single constant beside two literals. The version types stay in
`engine::recipe_versions` for now (their newest versions and `is_new_recipe` live with the engine's tools);
the grain version key is in the effects registry. The frontend's types of these keys are generated from the
registry (plan 2.3): each section lists its keys with their TypeScript type and doc (`tone::TS`, `color::TS`,
`detail::TS`, `geometry::TS`, `effects::TS`, `lens::TS`, `masks::TS`, one entry per section in
`ts::SECTIONS`), and `cargo run -p rawtonic-recipe --example ts_type` writes `src/generated/recipeKeys.ts`
(checked in; `scripts/check.sh` fails when it is stale). Every generated key is optional, as in a saved
recipe; `Adjustments` in `src/utils/adjustments.ts` marks those its objects always hold. The mask-local type
and container fields are generated too; their required/optional contract stays in `adjustments.ts` and is
independently pinned by a frozen type check. Nested value types (`Hsl`, `ColorRange`, curves, …) remain named
in `adjustments.ts`.

`masks::resolve` validates containers with the former all-or-nothing serde rules, borrowing their strings,
parameter JSON and curve payloads. Each layer's adjustments are resolved on demand (`Mask::adjustments`,
`masks::resolve_local`) through a nested `StoredRef`, so reading only the containers
(`mask_definitions_from_json`) resolves none of them: missing sharpening threshold stays 0 (the photo's is
15); RAW capture defaults never apply. The engine's owned `MaskDefinition`/`SubMask` remain rasterization
inputs, including their existing version/bitmap methods; they are materialized from resolved containers only
when needed. Type-specific rasterization math and its serde parameter decoders stay in the engine, addressed
by the mask registry's geometry keys. Photo and local colour ranges share one `ColorRange` decoder and one
shader packer. Whether a layer renders a bitmap is one rule (`masks::produces_bitmap`) for resolved layers and
`MaskDefinition`. A preview or export reads the layers once (`mask_definitions_from_json`) and the recipe's
versions once (`RecipeVersions`, from `rawtonic_export::normalize` for stored recipes):
`get_all_adjustments_with_masks` takes both instead of reading them again.

#### `engine`

`engine` (the develop model and its JSON-to-uniform translation, geometry, CPU detail stages, scopes, auto analysis,
camera defaults and DCP profiles, looks and colour tools, Lensfun parsing and lens maths, the edge-fringe pass (CPU
versions 1–4; version 5 on the GPU in `edge_fringe/gpu.rs` with its WGSL, using the device the app or
`GpuContext::headless` registers, as rawdecode's GPU demosaic does), the geometry warp of `renderVersion` 7 on that
device (`warp_gpu.rs`), spot removal, mask layer definitions; `masks::paste_layers` and `pending_detections`: the
copy/sync layer rule and the export refusal of AI selections not detected yet, ADR 0015; `recipe_versions`: every
recipe `*Version` key read once, with its clamp and a named predicate per gate). The adapter re-exports them
under their former module names and keeps their Tauri commands in thin modules of the same name. `engine` also holds
mask rasterisation and the recipe hashes.

`engine::ai_selections` computes the Sky and Subject selections a never-opened photo (an import, pasted settings) holds
without a bitmap at export, as the editor does on open (same warped photo, same parameters), through a
`SelectionDetector` (`ai_commands::ExportSelections` in the app, `SelectionModels` in the CLI); the app keeps the result
in the stored recipe (`export_processing::keep_detected_selections`), a missing model fails that photo, and
`prepare_export_source` refuses any shown Sky or Subject selection still without its bitmap.

`engine` (image_processing core = adjustment model/geometry/detail, scopes (histogram, waveform, parade, vectorscope of
the shown frame's `analytics_output` in its output space, with that space's luma/Cb/Cr weights; drawn by
`StudioScopes`), baseline_exposure, auto_adjust, series_match (match light across a series: per-photo statistics,
histogram-overlap and skin exposure match, white balance carried relative to each photo's light,
brackets/silhouettes/coloured light kept with a reason and confidence; app glue `src-tauri/src/series_match.rs` with
face skin from `rawtonic_faces::face_skin`, writes through `set_recipe_values` as one undo step, UI
`SeriesMatchDialog`), camera_profile, looks/colour tools, lens_correction + embedded_lens maths, edge_fringe (version 5:
`edge_fringe/gpu.rs` + `src/shaders/edge_fringe.wgsl`, registered device, no CPU fallback), warp_gpu (the geometry warp
of `renderVersion` 7 for every use, display and export alike: `src/shaders/warp.wgsl` on the same registered device; the
CPU f32 warp only without a device or for frames beyond its storage buffers), lens_recipe (a recipe's lens parameters:
`lensProfileFor` stamp, save-time `LensLookup`, `resolve_unresolved` before rendering), red_eye (red/pet eye detection
and correction), applied_style (`appliedStyle`: a style replaced or taken back on each photo of a selection, same rules
as src/utils/appliedStyle.ts), spot_removal, dust (Find Dust: sensor dust found on flat areas of the base image, placed
as heal spots, synced to the same camera through `lightroom_retouch`'s pending form; app glue
`src-tauri/src/spot_removal.rs` with face boxes from `rawtonic_faces`), lightroom_retouch (imported Lightroom spots and
red eyes placed from Lightroom's geometry on the base image), lightroom_crop (Lightroom's crop and straighten as
`crop`/`rotation` in the photo's frame, from the sensor area `rawdecode::source_frame::frame_and_sensor` reads),
face_retouch, mask_generation rasterisation + masks definitions + mask_parameters (brush strokes a preview request
refers to by token), cache_utils hashes, lut, source_frame (a recipe's frame version, the frames a file's versions
develop alike, a Reset's recipe), camera_crop (that aspect ratio as the crop of new recipes, ADR 0020 option C));

Engine also holds image_loader's decode path (`load_base_image` + `BaseImageOptions`), adjustment_utils and the LUT
parsers.


#### `gpu`

`gpu` (the wgpu pipeline, shaders, readback and native display presentation onto a surface the app creates; GPU
equivalence tests).

`gpu` (gpu_processing: GpuProcessor, shader variants + WGSL in `crates/gpu/src/shaders/`, readback, WgpuDisplay
presentation, `GpuContext::headless`, GPU equivalence tests; `device_start`: the device with a backend fallback, Windows
Vulkan/DX12 and never OpenGL (`usable_backends`: wgpu's WGL backend panics on a context lock held over 1 s), every
failure named, backends to leave out (`avoid`) and each step announced (`on_step`) for the app's start guard
`src-tauri/src/gpu_start_guard.rs` (a flag naming backends and step while a windowed start's device starts and its first
processor compiles; after a crash the next start leaves those backends out with a notice, the one after retries the
choice once, a second crash keeps the fallback until Preferences → Processing → Use automatic again; it never leaves
out every usable backend (then only the newest crash's); headless runs follow it read-only and write no flag; state in
the config folder's `gpu-start.json`, the processing backend preference is never changed silently; when no backend
starts the window offers Reset graphics settings and retry; `RAWTONIC_TEST_CRASH` in `qa_crash.rs`, `qa-control`
builds only); `pipeline_cache`: the app's own Vulkan pipeline cache file, keyed by adapter, driver and app version,
filled by the first start (shown as "Preparing graphics" in the activity centre) or ahead by
`rawtonic-app --warm-gpu-cache` (`src-tauri/src/gpu_warmup.rs`, a QA/benchmark tool; no installer runs it));


#### `export`

`export` (render pipeline with an explicit `RenderEnv`, resize/border/watermark, encoders, EXIF/XMP export metadata,
no-clobber output, single-file export used by `rawtonic export`, and the persistent export queue's state in `queue`:
jobs and their photos with state, output, error and timing; pause, cancel, resume, retry, recovery after a crash).

`export` (export settings/geometry/encoders, render pipeline with `RenderEnv`, exif_processing/tiff_exif/xmp_embed with
`export_metadata_with` + `encode_with_metadata`, `sidecars` (virtual paths, `.rrdata` reading, library walk, export
naming), `output::write_new_output`, `file_export::FileExporter`, and `normalize::normalize`, the one entry that makes a
stored or editor recipe renderable: bitmaps, offline lens, new-edit crop and versions, camera lens defaults, Lightroom
orientation, in that order; every render caller in the app and the CLI calls it once).


#### Layering check and colour pipeline

`scripts/check_layering.sh` (CI) fails if any of these crates reaches Tauri.

The actual pipeline is not yet the requested ProPhoto/DCP pipeline. Working values are linear sRGB primaries with a
ProPhoto gamut (negative channels beyond sRGB); every output (sRGB, Display P3, Adobe RGB, ProPhoto exports, the Display
P3 view on macOS) is that one rendering clipped at the end (ADR 0010).


### Where to edit

Where to edit: develop logic → the crate (`crates/engine`, `crates/rawdecode`, `crates/gpu`); a Tauri command or
`AppState` access for it → the same-named thin module in `src-tauri/src` (e.g. `src-tauri/src/camera_profile.rs` = `pub
use rawtonic_engine::camera_profile::*` + commands; `src-tauri/src/gpu_processing.rs` = window surface, the
context and processors in `state::gpu::GpuState`, render wrappers taking `&GpuState`). Engine/GPU functions take data (device,
caches, images), never `AppState`. Test-only helpers for another crate's tests go behind a `test-support` feature.


### Desktop adapter (`src-tauri`)

Still in the adapter: the window surface and the processor/cache slots in `state::gpu::GpuState`, the export queue's runner
(`export_queue`: one job at a time, per-job pause/cancel, `export-item-progress` stage events incl. AI Denoise tiles
through `MosaicDenoise::with_progress`, read by the studio's queue panel `StudioExportQueue`; a job carries each photo's
recipe and embedded metadata, read once at submission through the library it was submitted from
(`export_processing::pin_photos`), and its runner reads no library), editor-sidecar and session catalog reads, the AI
models, `tether`, file management and the session bridge.

**Desktop adapter (`src-tauri/`, crate `rawtonic-app`, lib `rapidraw_lib`)** — Tauri commands, `AppState`, image
loading/caches, file management, export, AI, tether, culling, merge/collage glue and the thin glue modules over the
engine crates.

`export_processing`, `image_loader`, `exif_processing` and the other same-named modules are now thin glue over the
engine/export crates; still inherited and Tauri-bound: file_management, ai_*, inpainting, tagging, camera_tethering.

`src-tauri/src/state/` holds the sub-states split out of `AppState` (plan step 3.3), each managed on its own:
`state::settings::SettingsStore` reads and migrates `settings.json` once per run and serves it from memory
(`current` / `try_current`; changes through `update`, one read-modify-write under one lock).
`state::develop::DevelopSession` owns the caches built from the open photograph (decodes, preview bases and their
GPU upload, masks, warps, geometry bases, the editor's patches); they are dropped only through
`DevelopSession::invalidate(Scope)` (`OtherPhoto`, `Source`, `Decoder`, `DecodedCache`, `PreviewUpload`).
`state::gpu::GpuState` holds the GPU's handles: the processing context (created on first use by
`gpu_processing::get_or_init_gpu_context`, or early by `start_early`) and its adapter, the display and export
processors, the scopes' upload, the device start's crash flag, the start error and the window's newest size.
`state::library_files::{AlbumsStore, PresetsStore}` own `albums.json` and `presets.json`: each file is read once per
run and every change (save, add to album, import, a file operation's rename or delete) is one read-modify-write
under the store's write lock, written with the same atomic helper and format as before. The commands run on blocking
workers; the frontend sends its saves in order (`src/store/libraryFiles.ts`) and quitting waits for them. Its
`README.md` has the lock policy (`state::lock::lock_recover`) and the lock order.

ADR 0009 moves AI/tether only when a consumer outside the app needs them, by its move-don't-rewrite method and
byte-identical-export gate.

Moved modules stay reachable under their old `crate::<module>` names through those glue modules or a `use
rawtonic_…::{…}` line in `lib.rs`. Don't add empty crates pretending a separation exists.

`lib.rs` is wiring only: module declarations, `run()` and `generate_handler!`; the Develop source/preview pipeline lives
in `editor_preview.rs`, start-up services in `startup.rs`, merges in `merges.rs` over rawtonic-merge, Open With / Edit
With in `open_with.rs` (programs from LaunchServices on macOS and from the shell's association handlers on Windows,
`open_with/windows_shell.rs` with its plain-data part in `open_with/handlers.rs`; disabled on Linux; Edit With queues one export per photo
through the export queue, snapshotted at the click like any queued export, to `NAME_edit.<ext>` beside the original,
registers each render via `catalog::derived` and groups it with the original in `catalog::stacks` (the grid stack), then watches
it, so a save in the other program refreshes its thumbnail).

There are no crate-root re-exports: import from the owning module (`crate::app_state::AppState`,
`crate::app_settings::load_settings`).

`file_management.rs` re-exports its submodules
(`file_management/{listing,folders,albums,thumbnails,edits,operations,presets,xmp_sync}.rs`).

`session_bridge.rs` is the transitional adapter between the inherited editor (which uses `filename.ext.rrdata` sidecars)
and the portable SQLite session; it also writes standard metadata + recipe to `filename.xmp`.

The library tracks each sidecar's state (`metadata.xmp_status`: synced, pending, conflict, with the digest it last wrote
or read); `catalog::xmp_state` says why each unsynced one is not written (`XmpProblem`, one check for the list and Try
again `write_pending_xmp`), `catalog::xmp_review` lists them and resolves sidecars changed elsewhere
(`keep_library_xmp`, `use_xmp` with `undo_use_xmp`), and `src-tauri/src/xmp_review.rs` serves the session bar's XMP
status panel (`SessionXmpStatus`; Use XMP is a step of `utils/appUndo`, all of it on the photo write queue).

Sidecars changed elsewhere while in sync are found without a write (`Library::check_xmp_sidecars` reads only new or
changed ones, by size and modification time, then `mark_changed_elsewhere` under the write lock; `session_xmp_check`,
run by `utils/xmpCheck.ts` when a library opens, the app is back in front or a folder is shown; no file watching or
polling).

XMP a quit or crash left pending are written once when the library opens, part by part on the photo write queue
(`writeLeftoverXmp` in `SessionXmpStatus`, `write_pending_xmp_of`).

Batch writes of several photos (paste/preset/style, rating, label, keyword) record themselves before their first photo
in `src-tauri/src/batch_journal.rs` (a job file per batch in the app's data folder with each photo's value before;
removed when the batch returns); one an earlier run left is offered when its library opens
(`src/utils/interruptedBatches.ts`: Apply to the rest, Undo the applied part, Dismiss).

Metadata writes never change the look: `Library::adopt_editor_sidecar` takes an edit that is only in a photo's `.rrdata`
into the library before `set_metadata`, flags, Use XMP and Keep library.


## Cross-cutting rules

### Storage

Opening a session or catalog requires SQLite WAL mode on every platform. Existing WAL mode is read before any transition
is requested; if WAL cannot be enabled, opening fails with a database configuration error rather than silently
proceeding in another journal mode.

Sessions contain session.json, session.sqlite (WAL), Capture, Selects, Output, Trash, masks and disposable .cache. The
Capture, Selects and Output folders are configurable per session (G38, `crates/catalog/src/session_folders.rs`; app glue
`src-tauri/src/session_folders.rs`, UI `SessionFoldersDialog`): stored in `session.json` relative when inside the
session, absolute when outside it (a folder absolute on another system opens as not available); a folder outside that is
not there fails capture, import, Move to Selects and export (`Library::usable_folder`), never made in its place.
Photographs in a session's folders outside it are registered like a catalog's (an external root per folder,
`place_for`), owned by the open session (`photo_library::outside_folder_owner` in the app, used by `culling_library_for` and the
file operations' `session_above`); changing a folder moves nothing, "Move contents" runs
`rawtonic_fileops::plan_move_contents` as journaled operations. A complete library copy (`transfer`) resets folders
outside to their defaults.

Mask definitions and bitmaps are kept in the database (`recipe_blobs`), so a closed session copied anywhere renders the
same (`crates/catalog/tests/mask_portability.rs`, `crates/cli/tests/moved_session_masks.rs`).

Asset references in a session are relative, validated against traversal and symlinks. Catalog bundles use catalog.sqlite
and absolute external roots. One schema supports both modes. SQLite metadata and versioned recipes are authoritative for
managed assets. Recipe strings of 16 KB and more (AI mask bitmaps, Relight analyses) are stored once by SHA-256 in
`recipe_blobs` and referenced from every history step (`rawtonic-blob:sha256:<hex>`, schema 7); reads restore them,
removals prune unreferenced ones. Legacy sidecars form an explicit adapter bridge during migration.

Nothing removes an asset because its file is gone: it is marked missing (offline), keeps its edits, metadata and albums,
stays listed in its folder and albums and in the Missing Photographs view, and opens on its previews; edits made offline
go to SQLite only and the derived sidecars follow when it comes back.

External roots record their volume (UUID, name, mount point; `crates/catalog/src/volume.rs`, from the mount facts of
`rawtonic_fs::volume`), so a disconnected drive is
named and a drive mounted elsewhere is followed. Volume kind (startup/local/removable/network) and configured roots
without photographs keep their last observed drive in SQLite (schema 15, `folder_root_volumes`); the Folders panel
orders the startup disk first and marks network shares.

Relinking (one photograph with its folder neighbours, or a folder) accepts a file only with the registered size and
content fingerprint and changes only database rows, in one transaction.

Adding photographs (`crates/catalog/src/add.rs`) is walk → plan → insert: callers find the files, `plan_add` reads and
validates what each brings (editor sidecar, else XMP), and `insert_planned` stores all of them with recipe and metadata
in one transaction, into the current import batch (`import_batches`, `asset_imports`; the newest is Previous Import).

Synchronize Folder (`crates/catalog/src/folder_sync.rs`, app glue `src-tauri/src/folder_sync.rs`) is scan → review →
apply: a read-only, cancellable scan lists a folder (or every root), matches files no asset references against missing
and present assets by size and sampled fingerprint, and proves matches with the full SHA-256 recorded per asset
(`assets.sha256`, `checksum.rs`: written by verified ingest and a throttled background pass, since a missing file cannot
be hashed any more). It classifies verified and probable moves, copies, contested files, new files, removed and
unavailable (disconnected drive: never moved or removed) photographs. A photograph is only removed when its folder was
listed and its library folder (the library's own, or a catalog folder it is in) is there and not empty; below a catalog
folder that is not there (a drive or share without a volume identity — Windows, SMB/NAS — looks just like that) it is
unavailable, and apply refuses its removal too. Apply checks every accepted action against disk and database again,
commits moves and removals in one transaction (the asset id stays, so edits, metadata, versions and albums follow) and
adds new files through `add_files` as a "sync" batch; when adding fails, the committed moves and removals are still
reported (and followed by previews), the files as skipped with `addError`. Opening a library follows verified moves only
among the new files of its own folder (`scan_files_sync`: no folder listed, only files matching an asset by size and
fingerprint hashed). Files are never moved, modified or deleted.

A library's hot folder is kept in it (`hot_folder`, `hot_folder_files`, schema 8; `crates/catalog/src/hot_folder.rs`):
the watched folder, template, subfolder option and next-capture settings, the last capture, and the files taken (path
relative to the watched folder, size, modification time), so watching resumes when the library opens again and files
that arrived meanwhile are taken once without hashing the earlier ones again; transferred or restored copies drop it.


#### Preferences → Storage

Preferences → Storage (`src-tauri/src/storage.rs`, `storage_fs.rs`) inventories logical file bytes in blocking workers.
Each category walk has a 2 s budget (partial totals are labelled), is cancellable, and is reused for 5 s. The cache root
owns thumbnails, previews outside libraries, metadata reads and model first-run markers; exact app-data subdirectories
own denoise results and downloaded models, the app log directory owns logs, and only the active library's `.cache` is
eligible there. Clear validates these fixed roots, refuses descendant links, keeps the active log, staging/recent files
and active readers, and leaves directory structure in place. Unix removal is anchored with directory descriptors; the
Windows implementation pins ancestors against renames and refuses reparse points; native Windows execution remains
unqualified. Model removal needs confirmation and runs before model loading at the next launch; installed
execution-provider plug-ins are preserved, and system Core ML caches are read-only.

Thumbnail and denoise-result limits (2/5 decimal GB by default, 0 unlimited) are applied once per minute and on
preference changes; active or recently used entries may keep a cache above its limit temporarily. Standard previews
retain their existing LRU/offline protection and separate 1:1 expiry policy. Successful webview thumbnail loads
(including the decoded-preview path) refresh cache-family last-use timestamps through one batched IPC accepting only
validated opaque cache keys. Preview clearing and size limits live only in Storage; Preview settings link there, and the
all/1:1 clear choices share one confirmed native action.

Cached thumbnail images recover an expired asset URL on demand without replacing the viewport queue; decoder work for
recovery is admitted through one additional slot before entering the blocking pool; the shared file URLs are refreshed
after recovery, and failed recovery shows a placeholder with no retry loop.

Vanished staging files are skipped while scanning; an undeletable file is kept while eviction continues with other
candidates. Staging files are counted with their owning cache or model directory; there is no separate app scratch tree.
Decoded-image storage is RAM, shown separately with its existing count limit.


### File safety

Copy into a unique temporary file beside the destination, flush, compare source and destination SHA-256, publish with
no-clobber atomic persistence. Never overwrite on collision. Source files are opened read-only. Metadata writes are
atomic. Unsupported destructive operations must fail clearly rather than fall back to deletion.


### Concurrency

Native renderer retains cancellation generations, coalescing, workers and bounded preview strategies. Catalog
transactions run in blocking workers for IPC; no SQLite connection is shared between threads. Ingest is background work;
each copy ingest is a job in the library database with explicit item states (pending, copying with its planned copies,
done, failed), resumed when the library is opened again (ADR 0013).

Every write of a photograph's recipe, sidecar or XMP from the webview (editor autosave, batch paste/preset/sync/reset,
lens and Auto corrections, recipe undo/redo, ratings, labels, flags, keywords, EXIF and text fields, snapshots) goes
through that photograph's one write queue (`src/utils/photoWrites.ts`, keyed by the file, so virtual copies share it): a
write starts after every write submitted before it for any of its photographs, its dependencies taken at submission, and
each settled write, failed or partly failed, is announced once (`PHOTO_WRITES_SETTLED`, which the session bar's XMP
count listens to). The backend keeps each sidecar read-modify-write atomic (`sidecar_field_edit`); the order is the
webview's.

Background work and its outcomes are reported in one place, the activity centre in the session bar beside the library
name (`src/store/useActivityStore.ts`, `src/components/activity/ActivityCentre.tsx`, ⌥⌘A / Ctrl+Alt+A, View →
Activity…): running tasks with progress and Cancel, recent results (they expire after 15 minutes) and what needs
attention (warnings and failures with their actions, kept until resolved or dismissed). `src/activity/bridges.ts`
follows the paths with their own state (export queue, AI tasks and AI Denoise, model downloads, indexing, batch
commands, the photo write queue); everything else calls `notify` (`src/activity/notify.ts`, shaped like the toast API):
`error`/`warn` become entries, `success`/`info` confirm the user's own action with a ~3 s toast and are logged under
Done. Only `notify` and `AppNotices` use react-toastify (`src/activity/noToastFailures.test.ts`); background failures
never open a dialog.


### Release gates

Target hardware and fixtures are named in the maintainers' benchmark notes. No budget is green without measured
evidence. Core tests, UI type checks, production bundle, native compilation and real sample render are separate gates.


