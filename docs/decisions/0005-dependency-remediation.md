# ADR 0005 — XML dependency remediation and advisory evidence
Status: accepted, 2026-10-01.

The inherited lockfile was necessary to retain compatible jxl-encoder SIMD dependencies (magetypes 0.9.26 / archmage 0.9.28). A broad update initially broke compilation, so dependency changes are targeted and verified.

A full RustSec scan found rustls RUSTSEC-2026-0285 and quick-xml RUSTSEC-2026-0194/0195, plus unmaintained dependencies. Upgrade rustls to 0.23.45 and yanked chacha20 0.10.1 to 0.10.2. The application already uses quick-xml 0.41; little_exif 0.6.23 pins 0.37.5. Vendor that small MIT/Apache crate, retain its source/licenses, and change only its quick-xml dependency to 0.41.0. This removes the vulnerable XML version without rewriting the EXIF codec. Record the local patch in THIRD_PARTY_NOTICES; return to upstream when a compatible release incorporates the update.

The remaining full advisory gate fails for unmaintained transitive packages: paste (2024-0436), proc-macro-error (2024-0370), ttf-parser (2026-0192), unic-char-property (2025-0081), unic-char-range (2025-0075), unic-common (2025-0080), unic-ucd-ident (2025-0100), unic-ucd-version (2025-0098). They arrive through image codecs, GTK/Tauri and text dependencies. Do not ignore them or call the full audit green. Migrate/fork their upstream consumers, then re-run platform builds and the full audit before production release. License/bans/source checks are separate and pass.

## Update — 2026-10-08

Six of the eight unmaintained packages are gone: Tauri 2.11.5 → 2.12.1 (with `@tauri-apps/api` and `@tauri-apps/cli` 2.12.1) brings `urlpattern` 0.6, which drops all five `unic-*` crates; `imageproc` is used without its default `text` feature (Rawtonic draws no text with it), which removes `ab_glyph`/`ttf-parser`; the vendored little_exif uses `pastey` instead of `paste`. `cargo deny check advisories` now reports two: paste (2024-0436), still pulled by `image` → `exr` → `pulp`, and proc-macro-error (2024-0370), pulled by GTK 0.18 (`glib-macros`, Linux only) through Tauri's `muda`. Both need upstream releases (an `exr`/`pulp` without paste; Tauri on gtk-rs 0.19+). Exports of three RAWs (TIFF, and JPEG with metadata through little_exif) are byte-identical before and after; licenses, bans and sources pass.

## Update — 2026-10-16

`cargo deny check advisories` passes, without ignore entries. Before this change it reported seven advisories: paste (2024-0436) and proc-macro-error (2024-0370) as above, ttf-parser (2026-0192) again, now through `ab_glyph` in the text watermark (`rawtonic-export`), and four rustls 0.23.45 vulnerabilities (2026-0346 to 0349).

- rustls 0.23.45 → 0.23.46 (`cargo update -p rustls`).
- Text watermarks read fonts with skrifa 0.48 (outlines, metrics, `kern` pairs; Google Fonts' maintained fontations project, MIT OR Apache-2.0) instead of ab_glyph/ttf-parser, and fill the outlines with the same ab_glyph_rasterizer as before. Of 96 test renders (12 macOS fonts, two texts, two widths, shadow on/off) 93 are byte-identical to the ab_glyph renders; the other three differ in a handful of pixels by at most 2/255.
- paste: no `exr`/`pulp` release drops it yet (exr 1.74.2 needs pulp ^0.22.3; pulp's main branch already uses pastey but is unreleased as 0.23). `vendor/paste` takes `paste`'s name through `[patch.crates-io]` and re-exports the macros of pastey 0.2, the maintained successor already used by our little_exif copy. OpenEXR input stays supported (the EXR round-trip test passes).
- proc-macro-error: Tauri 2.12.4 and 3.0.0-alpha.5 still require gtk ^0.18 on Linux (gtk3-rs is maintained again and released gtk 0.19.0 on glib 0.22 in 2026-09; Tauri plans the move, tauri-apps/tauri#10910, tauri-apps/muda#219). Its maintained fork proc-macro-error2 is itself unmaintained now (RUSTSEC-2026-0173), so `vendor/glib-macros` (0.18.5) and `vendor/gtk3-macros` (0.18.2) implement the three items they used (`#[proc_macro_error]` entry, `abort!`, `abort_call_site!`) in a small `src/abort.rs` on `syn::Error`. glib-macros' own test suite (including 29 compile-fail cases) passes, and seven wrong macro uses give byte-identical compiler output with the original and patched crate; the Linux workspace builds and passes clippy (Ubuntu 24.04 container, aarch64). syn 1 leaves the build with them.

Each patch is documented in its folder and in THIRD_PARTY_NOTICES; remove it when the upstream consumer releases the fix. Full-resolution exports of four RAWs (Fujifilm X-T3, Sony A7R IV, Nikon D5300, Canon R10; TIFF, and JPEG with metadata) are byte-identical before and after. Licenses, bans and sources pass.

## Gate policy — 2026-10-16 (user decision)

- Merges: `scripts/check.sh` and CI (`ci.yml`) run `cargo deny check -W unmaintained advisories`. Vulnerabilities and unsound crates fail the gate; an unmaintained transitive crate is a warning, because it often needs an upstream release first and should not block unrelated work.
- Watch: `.github/workflows/advisories.yml` runs the full `cargo deny check advisories` after every merge into main and weekly, so a new advisory shows as a failed run.
- Releases: the full check must pass. `npm run package:mac` runs it before building, and the Linux AppImage workflow's build needs its `advisories` job. Ignore entries in `deny.toml` stay a user decision, each with a reason and an exit condition.
