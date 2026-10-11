# ADR 0005 — XML dependency remediation and advisory evidence
Status: accepted, 2026-10-01.

The inherited lockfile was necessary to retain compatible jxl-encoder SIMD dependencies (magetypes 0.9.26 / archmage 0.9.28). A broad update initially broke compilation, so dependency changes are targeted and verified.

A full RustSec scan found rustls RUSTSEC-2026-0285 and quick-xml RUSTSEC-2026-0194/0195, plus unmaintained dependencies. Upgrade rustls to 0.23.45 and yanked chacha20 0.10.1 to 0.10.2. The application already uses quick-xml 0.41; little_exif 0.6.23 pins 0.37.5. Vendor that small MIT/Apache crate, retain its source/licenses, and change only its quick-xml dependency to 0.41.0. This removes the vulnerable XML version without rewriting the EXIF codec. Record the local patch in THIRD_PARTY_NOTICES; return to upstream when a compatible release incorporates the update.

The remaining full advisory gate fails for unmaintained transitive packages: paste (2024-0436), proc-macro-error (2024-0370), ttf-parser (2026-0192), unic-char-property (2025-0081), unic-char-range (2025-0075), unic-common (2025-0080), unic-ucd-ident (2025-0100), unic-ucd-version (2025-0098). They arrive through image codecs, GTK/Tauri and text dependencies. Do not ignore them or call the full audit green. Migrate/fork their upstream consumers, then re-run platform builds and the full audit before production release. License/bans/source checks are separate and pass.

## Update — 2026-10-08

Six of the eight unmaintained packages are gone: Tauri 2.11.5 → 2.12.1 (with `@tauri-apps/api` and `@tauri-apps/cli` 2.12.1) brings `urlpattern` 0.6, which drops all five `unic-*` crates; `imageproc` is used without its default `text` feature (Rawtonic draws no text with it), which removes `ab_glyph`/`ttf-parser`; the vendored little_exif uses `pastey` instead of `paste`. `cargo deny check advisories` now reports two: paste (2024-0436), still pulled by `image` → `exr` → `pulp`, and proc-macro-error (2024-0370), pulled by GTK 0.18 (`glib-macros`, Linux only) through Tauri's `muda`. Both need upstream releases (an `exr`/`pulp` without paste; Tauri on gtk-rs 0.19+). Exports of three RAWs (TIFF, and JPEG with metadata through little_exif) are byte-identical before and after; licenses, bans and sources pass.
