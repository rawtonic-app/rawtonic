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
