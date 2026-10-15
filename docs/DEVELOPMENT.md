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
./target/debug/rawtonic list /path/Job
./target/debug/rawtonic inspect /path/Job
./target/debug/rawtonic rate /path/Job ASSET_UUID 5
./target/debug/rawtonic flag /path/Job ASSET_UUID 1
./target/debug/rawtonic sync /path/Job
./target/debug/rawtonic bench --iterations 1000
```

Ingest supplies original name, sequence, job/session and an mtime date fallback for naming templates. EXIF camera/lens token hydration, live preview and saved templates remain unfinished. Missing context tokens fail explicitly.

### Full-resolution export

```sh
./target/debug/rawtonic export /path/Job/Capture/photo.ARW --output /path/Job/Output/photo.jpg --quality 100 --lensfun-db /path/to/lensfun_db
./target/debug/rawtonic export /path/Job/Capture --output /path/Job/Output --format tiff --tiff-bit-depth 16 --lensfun-db /path/to/lensfun_db
```

The CLI needs the app's `lensfun_db` folder: pass `--lensfun-db`, set `RAWTONIC_LENSFUN_DB`, or place it beside the binary. It reads saved session edits or `.rrdata` sidecars. `--keep-metadata` opts into metadata retention; `--color-space srgb|display-p3|adobe-rgb|prophoto` picks the output colour space; `--adjustments` accepts renderer JSON. Existing output files are refused and failures return a nonzero exit status. Use `rawtonic export --help` for all options.

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
npm run typecheck
npm test
npm run build
npm run tauri -- build
```

The advisory check currently reports transitive unmaintained crates; it is not waived. See [ADR 0005](decisions/0005-dependency-remediation.md) for the remediation history. Before a merge, `scripts/check.sh` runs every gate locally; CI (`.github/workflows/ci.yml`) repeats them on Linux after each merge into main.

### GPU tests (manual)

The renderer's GPU tests need a GPU adapter, so they are `#[ignore]`d: neither `scripts/check.sh` nor CI runs them. Run them by hand on a machine with a GPU before merging any change to the shaders (`crates/gpu/src/shaders/`), the GPU pipeline or the tone, colour and look code they render:

```sh
cargo test -p rawtonic-gpu --locked -- --ignored gpu_processing::preview_tests
```

They check that the display shader variants, the fallback variant and the CPU-readback export path give the same pixels, and properties of the renderer such as tone order (`camera_raw_shadow_gains_stay_smooth_down_to_black`). A run takes a few minutes; `post_idle_partial_frame_probe` alone runs for over a minute. Compare full-resolution exports of the changed build against the previous commit as well (`rawtonic export` on copies of public sample RAWs, byte for byte): edits the change should not touch must stay identical.

[Fixture instructions](testing/README.md) describe `APP_FIXTURES`, GPU gating and golden comparisons. [Packaging](../packaging/README.md) records signing, Linux and tethering blockers. AI is optional; missing ONNX runtime produces an error, with an opt-in build path documented there.

## Architecture and implementation

[Architecture](ARCHITECTURE.md) · [Recipe schema](RECIPE_SCHEMA.md) · [AI models](MODELS.md) · [Contributing](../CONTRIBUTING.md)
