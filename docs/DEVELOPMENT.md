# Development guide

For the photography workflow and feature overview, start with the [README](../README.md).

## Run the desktop app

Prerequisites: Node.js 24+, Rust 1.98.1 (`rustup` honors `rust-toolchain.toml`), and the native Tauri SDK. macOS needs Xcode command-line tools and CMake (libjpeg-turbo is built from source; `brew install cmake`, optionally `nasm`). On Ubuntu 24.04:

```sh
sudo apt-get install build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev libssl-dev librsvg2-dev patchelf libxdo-dev cmake nasm
```

On Arch Linux / CachyOS (`xdotool` provides libxdo):

```sh
sudo pacman -S --needed base-devel pkgconf webkit2gtk-4.1 gtk3 openssl librsvg xdotool cmake nasm patchelf
```

Linux x86_64 builds require NASM for libjpeg-turbo's SIMD code. If dependencies were built before installing NASM, run `cargo clean -p turbojpeg-sys` once before rebuilding; installing NASM alone does not invalidate the cached CMake build.

```sh
source "$HOME/.cargo/env"  # if cargo is not already on PATH
npm ci
npm start
```

`npm run dev` alone serves the UI; it cannot decode or render photographs without the Tauri process. The first native build downloads dependencies and takes several minutes. Core editing needs no account, cloud service or AI runtime.

## Command line

The standalone CLI can manage sessions and export photographs without opening the desktop app:

```sh
cargo build -p rawtonic-cli
./target/debug/rawtonic create /path/Job --name Job
./target/debug/rawtonic ingest /path/Job /card/DSC0001.ARW --template '{job}_{seq:4}.{ext|lower}' --backup /backup/Job
./target/debug/rawtonic name /card/DCIM/100CANON/*.CR3 --template '{yyyy}{mm}{dd}_{camera}_{orig_num}.{ext}'
./target/debug/rawtonic list /path/Job
./target/debug/rawtonic inspect /path/Job
./target/debug/rawtonic rate /path/Job ASSET_UUID 5
./target/debug/rawtonic flag /path/Job ASSET_UUID 1
./target/debug/rawtonic sync /path/Job
./target/debug/rawtonic bench --iterations 1000
```

Ingest names each copy from the source's own metadata, read from its header only (no decode; plain reads, never a memory map of a card). Tokens (`rawtonic_naming::TOKENS`; older templates keep working, tokens are only added):

- Capture date and time: `{yyyy}` `{yy}` `{mm}` (month) `{month}` `{mon}` `{dd}` `{doy}` `{weekday}` `{hh}` `{MM}` (minute) `{ss}` `{subsec}` `{tz}` `{date}` (`2026-10-01`) `{time}` (`14-37-08`). The date comes, best first, from EXIF DateTimeOriginal (with SubSecTimeOriginal/OffsetTimeOriginal; for maker formats as the RAW decoder reads their embedded EXIF, and a CRW's CIFF time stamp), DateTimeDigitized, the EXIF file date (IFD0 DateTime), the file's modification time, and only without any of these the import time. The import dialog's example shows which one was used.
- Original file: `{orig}` `{orig_base}` `{orig_num}` (the digits the name ends with) `{folder}` (the card folder) `{ext}`; counter `{seq}` / `{seq:N}` `{seq_start}`; `{job}` / `{session}`.
- Camera and exposure: `{camera}` (make and model, the make not repeated) `{make}` `{model}` `{serial}` `{lens}` `{iso}` `{focal}` `{aperture}` `{shutter}` (`1-250`, `0.3s`, `2s`).
- Metadata: `{rating}` `{color}` `{title}` from an XMP sidecar on the card, `{creator}` `{copyright}` from it or else EXIF Artist/Copyright.

A token without a value renders empty; unknown tokens and transforms fail. A name that does not end in the source's extension (any case) gets it appended, so `{date}_{orig_base}` still makes `2026-10-01_DSC0042.ARW`. Transforms: `|upper`, `|lower`, `|pad:N`. A source's XMP sidecar (`NAME.EXT.xmp`, else `NAME.xmp`; for a JPEG, HEIF or other developed image only when no other file shares its name, as in a RAW+JPEG pair the sidecar is the RAW's) is copied, verified and never replacing a file, beside the copy as `<new name>.xmp` (and beside the backup), and the library adopts its rating, label, keywords and IPTC fields when it registers the photograph. `rawtonic name SOURCES --template T` shows the names an ingest would give, read-only.

### Full-resolution export

```sh
./target/debug/rawtonic export /path/Job/Capture/photo.ARW --output /path/Job/Output/photo.jpg --quality 100 --lensfun-db /path/to/lensfun_db
./target/debug/rawtonic export /path/Job/Capture --output /path/Job/Output --format tiff --tiff-bit-depth 16 --lensfun-db /path/to/lensfun_db
```

The CLI needs the app's `lensfun_db` folder: pass `--lensfun-db`, set `RAWTONIC_LENSFUN_DB`, or place it beside the binary. It reads saved session edits or `.rrdata` sidecars. `--keep-metadata` retains all metadata. Without it, creator, copyright and usage terms remain; creator contacts (including postal address) are omitted unless `--keep-creator-contact` is explicitly given (also supported by the desktop headless exporter, ADR 0018); `--color-space srgb|display-p3|adobe-rgb|prophoto` picks the output colour space; `--adjustments` accepts renderer JSON. Existing output files are refused and failures return a nonzero exit status. Use `rawtonic export --help` for all options.

Summary of the CLI surface (moved from AGENTS.md): `cargo build -p rawtonic-cli` then `./target/debug/rawtonic {create,ingest,name,add,list,search,facts,inspect,rate,flag,sync,sync-folder,checksums,adopt-xmp,bench,search-bench,decode,export,fingerprint}`. `rawtonic search LIB '{"field":"iso","op":"ge","value":3200}'` prints the paths a library search rule matches (the rule JSON smart albums store, ADR 0014); `rawtonic facts LIB` reads the capture facts of photographs not read yet; `rawtonic search-bench --photos 50000` times the search, value lists and smart album counts on a synthetic library in a temporary folder (catalog only, no IPC or rendering). `rawtonic fingerprint DIR --out baseline.json` / `--check baseline.json` records decode facts per RAW (keyed by content hash) and diffs them after decoder changes (docs/testing/README.md). `rawtonic export SRC --output OUT.tif [--adjustments recipe.json] [--keep-metadata] [--color-space display-p3|adobe-rgb|prophoto] [--models DIR] [--face-retouch '{"blemishes":60,"evenSkin":50}']` (or a folder: `rawtonic export DIR --output OUTDIR --format tiff`) renders without Tauri, byte-identical to the app's export: it reads each photo's recipe (session database, else `.rrdata`) and exports virtual copies (photos a session keeps outside its folder, e.g. moved to another drive: `--library SESSION`, so their recipes and the session's `profiles/` and `resources/` come from it; a recipe naming such a resource for a photo of no library fails instead of rendering without it), like the app's headless export (`rawtonic-app export <src> --output <dst> --format jpeg|tiff ...`, see README). `--face-retouch` finds the faces with the face models in `--models` (ONNX Runtime from `ORT_DYLIB_PATH`) and renders that retouch, as the Retouch tab would. It needs the app's `lensfun_db` folder: beside the binary, `$RAWTONIC_LENSFUN_DB` or `--lensfun-db`.

The desktop binary also supports headless export with its packaged resources:

```sh
npm run tauri -- build --debug --bundles app
'./target/debug/bundle/macos/Rawtonic.app/Contents/MacOS/rawtonic-app' export /path/Job/Capture/photo.ARW --output /path/Job/Output/photo.jpg --format jpeg --quality 100
```

## Project and catalog files, transfer and recovery

A project contains `session.json`, `session.sqlite`, `Capture/`, `Selects/`, `Output/`, `Trash/`, `masks/` and `.cache/`. A new catalog uses `catalog.json`, `catalog.sqlite` and `Photos/`. Existing manifests keep their capture-folder name. SQLite is authoritative for metadata, primary edit history and registered virtual-copy states; sidecars remain an editor compatibility layer.

Catalog ingest defaults to verified copies under `Photos/YYYY/YYYY-MM-DD/`, using EXIF capture date (then digitized/image date, then file modification time). An explicitly selected folder keeps its exact destination. **Add where they are** is optional for disk sources and keeps external references. Albums are virtual collections and do not move files.

The library menu offers **Create portable copy**, **Create full backup** and **Restore backup**. Copy/backup flushes pending editor saves and takes a consistent SQLite snapshot including committed WAL edits. It gathers all registered originals, sidecars, albums, metadata, full primary history, virtual copies, embedded masks, and file dependencies named by `lutPath`, `cameraProfile` or `raster_path`. External originals are collected under `Photos/Imported/<asset-id>/`; dependencies are stored in `resources/<SHA-256>/` and recipe paths are made relative. This works for both projects and catalogs. Generated previews/caches are omitted and rebuilt. Machine-specific queued tasks and filesystem undo operations are not replayed in the copy.

Each copy is created in a temporary sibling folder, checked for database integrity and verified with SHA-256, then published without overwriting an existing destination. Missing/changed originals, unsupported symlinks, failures and cancellation abort publication. An edit resource that is already missing (a LUT deleted after an old undo step, a preset or even the current recipe used it) cannot be copied and does not abort: its reference is kept unchanged, and `rawtonic-copy.json` counts such files (`missing_resources`, `missing_current_resources` for those current edits use), which the dialog reports. A resource that is there but cannot be read still aborts. `rawtonic-copy.json` records hashes. Full backups get timestamped folder names; they are independent full snapshots, not incremental backups. Restore verifies the snapshot before opening any database and writes a new working folder; the backup is not migrated or modified. Keep backup folders unchanged and use Restore to work from them.

Managed original paths and collected resources resolve relative to the current library folder. Close Rawtonic before manually moving a working library; move the entire folder, including SQLite WAL files if present, sidecars and support folders. A library with external originals/resources needs **Create portable copy** to gather those dependencies. Independently edited copies do not synchronize. Use a compatible Rawtonic version; cross-platform rendering parity and every inherited mask format are not yet certified.

Derived `filename.ext.rrdata` files bridge the inherited editor. Standard metadata and the Rawtonic recipe are also written to `filename.xmp`. Preserve both until the engine migration is complete.

Ingest copies through temporary files, verifies source and destination with SHA-256, and publishes without overwriting. An optional second destination receives a verified backup. Cancellation removes unfinished temporary copies; completed copies remain. If registration or the primary copy fails after a backup succeeds, the result retains the successful paths. Card files are never removed.

Rename, move and trash operations now use an undo journal. In a session, deleted photographs go to its `Trash/` folder; Library Undo restores supported operations when the original location is still available.

An **XMP pending** count means library edits are saved but sidecar synchronization needs attention. External changes are not silently replaced. With the editor closed, explicitly adopt external metadata while retaining the Rawtonic develop recipe, then synchronize:

```sh
./target/debug/rawtonic adopt-xmp /path/Job ASSET_UUID
./target/debug/rawtonic sync /path/Job
```

RAW/JPEG files sharing a sidecar stem require pair-ownership resolution and are refused. The conflict-resolution UI and full interoperability certification remain open. Session/catalog conversion is not implemented. Catalogs already support external originals, adding or copying photographs, albums, metadata, edits, volume tracking and relinking missing files. Offline RAW editing uses previously generated Standard or 1:1 preview proxies; without a cached proxy the original must be available. This is implemented in `preview_proxy.rs` and `image_loader.rs`, rather than through the catalog's reserved `smart-previews/` directory. Full verified transfer/backup/restore is implemented and tested with moved mixed libraries, edit history, virtual copies and resources. Catalog mode is functional, not feature-complete; automatic/incremental backups and project/catalog conversion remain open.

## Build and verify

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check licenses bans sources
cargo deny check advisories
cargo shear                     # unused dependencies (cargo install --locked cargo-shear); an unused dependency that must stay goes into [package.metadata.cargo-shear] ignored, with the reason
npm run typecheck
npm test
npm run build
npm run tauri -- build
```

The advisory check passes without waivers (2026-10-19); a few unmaintained transitive crates are replaced through patched copies in `vendor/` (`[patch.crates-io]` in the workspace Cargo.toml: `vendor/paste` → pastey; `vendor/glib-macros`, `vendor/gtk3-macros` without proc-macro-error; `vendor/tauri-plugin-single-instance` on zbus 6 without zvariant/zbus_names; ADR 0005). Remove each patch when its upstream releases the fix; new advisories need the same treatment, not an ignore entry. Gates: merges fail on vulnerabilities and unsound crates (`cargo deny check -W unmaintained advisories` in `scripts/check.sh` and CI), unmaintained crates only warn there; `.github/workflows/advisories.yml` runs the full check after each merge and weekly, and releases need it green (`npm run package:mac` and the AppImage workflow run it). See [ADR 0005](decisions/0005-dependency-remediation.md) for the policy and remediation history. Before a merge, `scripts/check.sh` runs every gate locally; CI (`.github/workflows/ci.yml`) repeats them on Linux after each merge into main, except shellcheck, and with `cargo test` instead of nextest.

`scripts/ship.sh` ships a branch from its clean worktree in one step: the full check (skipped when `check.sh --brief` already passed on that HEAD with a clean tree), the second-model review (`scripts/review.sh`: Codex reviews `origin/main..HEAD` read-only against `scripts/review/prompt.md` and must approve that exact HEAD; the verdict and findings go into the pull request; `--no-review REASON` skips it with the reason in the PR; needs `codex` installed and logged in on the machine; model `gpt-6.1-sol` at reasoning effort `high` on every host, overridable with `RAWTONIC_REVIEW_MODEL` / `RAWTONIC_REVIEW_EFFORT`; a change shipped by Codex (detected from `CODEX_THREAD_ID`, Claude Code from `CLAUDECODE`; `--author codex|claude|human` overrides) is reviewed by Claude instead — `claude -p` with only Read/Grep/Glob, no settings or MCP servers, model `claude-opus-5-5`, `RAWTONIC_CLAUDE_REVIEW_MODEL` — and the PR names reviewer and author; `scripts/review.sh --default claude|codex` switches the reviewer for the whole clone when Codex's limit runs short, marking Claude's reviews of Claude's changes as same-model), push, pull request, local `merge --no-ff` into the main checkout, push of main, deletion of the remote branch. The worktree and its local branch are removed only when ship.sh is called from outside that worktree. When the caller's working directory is inside it, the caller runs in an Orca worktree (`ORCA_WORKTREE_ID`) or `--keep-worktree` is given, both stay: an agent running there would otherwise lose its working directory before it can send its final report, and a checked-out branch cannot be deleted. The last output line names the outcome and, for a kept worktree, the command that removes it and the branch later:

```text
ship: merged PR #95 as 1a2b3c4 (https://github.com/…/pull/95); worktree kept at /path/wt (running in an Orca worktree); remove it later with: git -C '/path/main' worktree remove '/path/wt' && git -C '/path/main' branch -d 'feature'
```

Other outcomes: `ship: opened PR #N (…); not merged (--no-merge)`, `ship: dry run OK; …` (`--dry-run` checks the preconditions and prints the plan without pushing, opening, merging or deleting anything) and `ship: FAILED during <step> (exit N)` with a non-zero exit code. Only an open pull request of the branch is reused (a merged one of the same branch name gets a new pull request). The merge holds a lock in the clone (`rawtonic-ship.lock` in the common git directory), so ships from its worktrees take turns on the shared main checkout; it stops when that checkout has commits that are not on `origin/main`, and undoes its local merge when the push of main is refused. Once main is pushed, a failed clean-up step only warns. `scripts/ship_test.sh` (part of `check.sh`) tests these paths against a throwaway repository with a local bare origin and a stubbed `gh`; `check.sh` also runs `shellcheck` on the scripts when it is installed.

### Moves between drives (manual on macOS)

The copy-move tests of `rawtonic-fileops` (ADR 0016) run on two file systems where the machine has a second one at `/dev/shm` (Linux, CI). On macOS, attach disk images (no root needed) and name a folder on one:

```sh
hdiutil create -size 512m -fs "MS-DOS FAT32" -volname FAT -layout GPTSPUD fat.dmg   # also -fs APFS, -fs ExFAT
hdiutil attach -nobrowse -mountpoint /path/mnt/fat fat.dmg
RAWTONIC_TEST_OTHER_VOLUME=/path/mnt/fat cargo test -p rawtonic-fileops --locked --lib copy_move -- --test-threads=1   # serially on FAT/exFAT images
RAWTONIC_TEST_EXFAT_DIR=/path/mnt/exfat cargo test -p rawtonic-fileops --locked --lib exfat
RAWTONIC_TEST_TRASH_DIR=/path/mnt/fat cargo test -p rawtonic-app --locked --lib platform_trash_round_trip -- --ignored
hdiutil detach /path/mnt/fat
```

### GPU tests (manual)

The renderer's GPU tests need a GPU adapter, so they are `#[ignore]`d: neither `scripts/check.sh` nor CI runs them. Run them by hand on a machine with a GPU before merging any change to the shaders (`crates/gpu/src/shaders/`), the GPU pipeline or the tone, colour and look code they render:

```sh
cargo test -p rawtonic-gpu --locked -- --ignored gpu_processing::preview_tests
```

They check that the display shader variants, the fallback variant and the CPU-readback export path give the same pixels, and properties of the renderer such as tone order (`camera_raw_shadow_gains_stay_smooth_down_to_black`). A run takes a few minutes; `post_idle_partial_frame_probe` alone runs for over a minute.

Many layers: `many_layer_frame_probe` times one preview job stage by stage as the editor runs it (IPC JSON parse, reading the layers, mask cache, adjustments, GPU frame) for a slider move on the first and the last layer and a live brush stroke on the top layer, on a real RAW at a 2304 px preview. `RAWTONIC_M6_RAW` names the RAW, `RAWTONIC_M6_RECIPES` comma-separated layer recipes (`{"masks": [...]}`), `RAWTONIC_M6_REFS=1` sends brush strokes by reference as the editor does for slider moves, `RAWTONIC_M6_EXPORT_MASKS=1` times each layer's full-resolution mask instead:

```sh
RAWTONIC_M6_RAW=/path/photo.NEF RAWTONIC_M6_RECIPES=/path/32-layers.json RAWTONIC_M6_REFS=1 \
  cargo test -p rawtonic-gpu --release --lib --locked many_layer_frame_probe -- --ignored --nocapture
```

Compare full-resolution exports of the changed build against the previous commit as well (`rawtonic export` on copies of public sample RAWs, byte for byte): edits the change should not touch must stay identical.

### Benchmarks (hot CPU paths)

Criterion benchmarks guard the CPU paths whose speed users feel; run them A/B against the base before shipping a change that could make any of them slower:

| Bench target | Package | Groups |
|---|---|---|
| `decode` | `rawtonic-rawdecode` | `raw_decode/{bayer,xtrans}/…` (file bytes in memory → oriented linear RGB, CPU demosaic), `demosaic/{bayer,xtrans}/…` (demosaic alone, X-Trans with its fine-grain smoothing) |
| `develop` | `rawtonic-engine` | `color_nr_v6/…`, `color_nr_v7/{pyramid,cpu_filter_6mp}` (the chroma pyramid every development builds; the whole CPU filter on a 6 MP centre), `lens/{edge_fringe_v2,edge_fringe_v3,edge_fringe_v4,lateral_ca_estimate_v2}`, `mask/{brush_stroke_preview,auto_mask_v2_stroke_preview,refine_edge_10_preview,refine_edge_40_full}` (one whole stroke on a 1536 px preview with the brush cache emptied; Refine edge on a radial layer) |
| `encode` | `rawtonic-export` | `export_encode/{jpeg_q90,jpeg_q100,tiff_16bit}` (full-resolution sRGB encode with ICC profile) |

Inputs: public RAW files from raw.pixls.us (CC0: Nikon D7200 NEF, Canon EOS R10 CR3, Fujifilm X-T3 RAF), fetched outside the repository and checked by SHA-256 — never commit them. Without them every benchmark runs on a synthetic 6000 × 4000 stand-in (an uncompressed DNG, or the scene it is made from), and its id says `synthetic_…`, so results on fixtures and on stand-ins never mix. The develop and encode groups work on the developed NEF.

```sh
scripts/bench/fetch_fixtures.sh ~/bench-fixtures      # ~117 MB; prints the export line
export RAWTONIC_BENCH_FIXTURES=~/bench-fixtures
cargo bench -p rawtonic-engine --features test-support --bench develop -- color_nr   # one target, filtered
scripts/bench/ab.sh                                   # working tree vs origin/main, all targets
scripts/bench/ab.sh --base HEAD~1 --bench develop --filter mask --rounds 3
```

`scripts/bench/ab.sh` builds the bench targets of the base commit (in a temporary worktree) and of the working tree into the same target directory (path crates do not clobber each other; only the changed crates are rebuilt), then runs them alternately (base, head, head, base, … with `--rounds N` runs per side, default 2) and prints a table: median of each side (median of its runs' medians), change, noise band and verdict. The band is the larger of the spread between one side's runs and criterion's 95 % confidence interval of a run's median, at least 3 % (`--min-band`; an A/A run of the same binaries differed by up to 1.6 %); a change inside it says `noise`, outside it `faster` or `SLOWER`. `--fail-on-slower` exits 3 on any `SLOWER` row, and when no benchmark was measured on both sides (a filter that matches nothing); `scripts/check.sh --bench` runs exactly that against `origin/main` (or `$CHECK_BASE`) after the other gates. It is opt-in because a full A/B takes about 20 minutes on a 6-core Ryzen 5 3600X (one pass of all targets ~300 s), plus two release builds of about 2 minutes each. The table is kept in `target/bench-ab/ab.md` (and as JSON in `ab.json`), also after `check.sh --brief --bench`, which shows no output of a passing step.

How to read it: measure on an idle machine (the script prints the load average) and compare only runs from the same machine; a `SLOWER` row needs the cause found or the cost accepted and recorded, never a re-run until it passes. A benchmark added after the base shows as `only head`. Benchmark ids carry their input and version (`color_nr_v6`, `edge_fringe_v2`), so a new filter version gets its own entry beside the old one rather than replacing it. The benchmarks never register a GPU, so X-Trans decode and demosaic time the CPU path (the app demosaics X-Trans on the GPU when it has one). They are type-checked by `clippy --all-targets` in every check run; the `test-support` features they need (the synthetic DNG writer, emptying the brush cache) are not enabled in the app.

**Tethering** (ADR 0012): the app starts the helper `rawtonic-tether` from its own folder (`target/debug/` in development; `RAWTONIC_TETHER_HELPER` overrides the path). `cargo build --workspace` builds it without camera support; for a real camera build it with `cargo build -p rawtonic-tether --features gphoto2` (Homebrew's or the system's libgphoto2 through pkg-config, or `GPHOTO2_LIB_DIR`/`GPHOTO2_INCLUDE_DIR` for `target/gphoto2` from `scripts/tether/build_libgphoto2.sh`). Without a camera, `RAWTONIC_TETHER_MOCK='{}' npm start` starts the scripted mock camera (`crates/tether/src/mock.rs`: numbering restarts, body shots, late or lost files, busy, unplug, read-only settings). `rawtonic-tether probe --out DIR [--shots N] [--body-seconds S]` asks a real body what it does through the same helper process (settings, set ISO and back, app shots with their timings, AF, live view) and prints one JSON report; `rawtonic-tether version` checks that libgphoto2 finds its camera drivers.

[Fixture instructions](testing/README.md) describe `APP_FIXTURES`, GPU gating and golden comparisons. [Packaging](../packaging/README.md) records signing, Linux and tethering blockers. AI is optional; missing ONNX runtime produces an error, with an opt-in build path documented there.

## Architecture and implementation

[Architecture](ARCHITECTURE.md) · [Recipe schema](RECIPE_SCHEMA.md) · [AI models](MODELS.md) · [Contributing](../CONTRIBUTING.md)
