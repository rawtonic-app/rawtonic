# Verification and fixtures

Core tests use synthetic bytes and XMP. They cover traversal, source preservation, no-clobber races, cancellation, full-hash duplicate verification, dual destination, SQLite history, moved sessions, standard color labels, metadata adoption and external XMP conflicts. Native adapter tests ensure there is one managed XMP writer and prevent rendered output from replacing an existing image. WGSL tests parse and validate all four shader modules and the f16 export variant without requiring an adapter. Frontend tests exercise session creation, errors, text entry and shortcut ownership.

`cargo test --workspace --locked` also runs a separate fixture harness. With no `APP_FIXTURES`, it prints **SKIP** instead of claiming a RAW test passed. With fixtures but no wgpu adapter, it reports adapter absence. No tests are counted as visual-parity certification.

## Private RAW/golden manifest

Set `APP_FIXTURES` to a directory containing `fixtures.json`:

```json
[
  {
    "source": "sony-a7iv-lossless.ARW",
    "golden": "golden/sony-a7iv-neutral.png",
    "max_rgb_rmse": 0.01,
    "adjustments": {"exposure": 0}
  }
]
```

```sh
APP_FIXTURES=/path/to/private-fixtures cargo test -p rawtonic-app --test fixture_render --locked
```

The runner copies each input to a temporary workspace, uses the real headless GPU exporter, checks dimensions and normalized RGB RMSE against a supplied golden, and verifies original and working-source SHA-256. Source and golden paths cannot escape the fixture directory. Goldens must be licensed, versioned reference renders with the intended profile and scene settings; generating expected images from the same run would not test correctness. RGB RMSE is a regression metric, not a perceptual/color-accuracy certificate. A perceptual metric and preview/export comparison are still needed.

Record camera/body/firmware, compression variant, lens, ISO, original SHA-256 and reference process/profile. Required matrix: Sony A7 IV/A7 III compressed and lossless ARW; DJI/Adobe DNG; Canon CR3; Nikon NEF; Fuji RAF; Panasonic RW2; JPEG/HEIC/TIFF. Do not commit private originals. The current run had no camera fixtures, physical camera or real LR/C1 XMP samples.

Synthetic native QA used a 1536×1024 authored color/gray chart: create session, ingest, open Develop, rate, pick, change exposure, reopen, export, reject output collision, compare source hashes.

Automated interface checks in a running app use the [QA control channel](qa-control.md) (debug builds only).

## Decode fingerprints

A rawler update, a new camera entry or a change to the decode path can silently change what a RAW decodes to. `rawtonic fingerprint` records, per file, what the decoder and Rawtonic's decode preparation (`raw_processing::prepare_raw_image`, the same code the development runs) make of it: make/model as decoded and resolved (clean and canonical model, rawler's mode), format and TIFF compression, buffer size, samples per pixel, bit depth, photometric interpretation, colour pattern, active area and default crop (incl. in-camera aspect ratios and DNG user crops), black levels (exact rationals with their repeat pattern) and white levels, the decoder's as-shot white balance and the multipliers the development uses, the decoder's colour matrices and the selected/adapted one, DNG baseline exposure, the applied orientation, ISO, lens make/model/spec and rawler's lens match, the engine's base look (baseline EV, embedded curve) and embedded lens corrections, a SHA-256 of the decoder's samples, and (unless `--no-develop`) the size, SHA-256 and mean RGB of a full-resolution development at default settings (as `rawtonic decode`). Files that fail report the step (`decoder`, `unpack`, `prepare`, `develop`) and the error; decoder panics are caught.

Entries are keyed by the file's SHA-256 and hold no paths or file names (only the extension and size), so a baseline describes the files without naming them. It still describes private photographs: keep baselines outside this repository (e.g. in the private notes repository).

```sh
cargo build --release -p rawtonic-cli
# Before the change: write a baseline (never overwrites).
target/release/rawtonic fingerprint /path/to/raw-copies --out /private/baselines/decode-<commit>.json
# After it (e.g. a rawler bump): one line per changed field, exit code 1 on any change.
target/release/rawtonic fingerprint /path/to/raw-copies --check /private/baselines/decode-<commit>.json
```

Files are only read. Run the check over the same set of files: new and missing files count as differences too. Everything but `developed` is exact and should match on every machine; the development hash may differ between CPU architectures, so `--check` compares it only when both runs come from the same platform (and both developed). A summary (files, failures by camera, time per file) goes to stderr and is not part of the baseline. `--no-develop` checks the decoder facts alone, much faster.
