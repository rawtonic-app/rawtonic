# Rawtonic patch of gtk3-macros 0.18.2

gtk3-macros 0.18.2 as published on crates.io (gtk3-rs commit
00133512bfc8bbb8e9be59b20e93406c6a3eeb21, Copyright the gtk-rs Project
Developers, MIT; `LICENSE` and `COPYRIGHT` kept unchanged). It is a
compile-time proc-macro dependency of gtk 0.18, which Tauri 2 uses on Linux
(muda, tao, wry, webkit2gtk).

The same change as in `vendor/glib-macros` (see its RAWTONIC_PATCH.md): no
dependency on the unmaintained `proc-macro-error` (RUSTSEC-2024-0370) or its
equally unmaintained fork `proc-macro-error2` (RUSTSEC-2026-0173);
`src/abort.rs` (identical to glib-macros' copy) provides the entry point,
`abort!` and `abort_call_site!` on `syn::Error`. Nothing else differs from the
published crate (which ships no tests); it builds with the same two upstream
dead-code warnings as the original.

It is patched in through `[patch.crates-io]` in the workspace `Cargo.toml`.
Remove it when Tauri moves to gtk 0.19 (gtk3-rs is maintained again since
2026-09; gtk 0.19.0 builds on glib 0.22):
<https://github.com/tauri-apps/tauri/issues/10910>,
<https://github.com/tauri-apps/muda/issues/219>. ADR 0005.
