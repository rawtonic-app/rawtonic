# Rawtonic patch of glib-macros 0.18.5

glib-macros 0.18.5 as published on crates.io (gtk-rs-core commit
42b9caf98e03ded086362d9653ca58fe94dc8658, Copyright the gtk-rs Project
Developers, MIT; `LICENSE` and `COPYRIGHT` kept unchanged). It is a
compile-time proc-macro dependency of glib 0.18, which Tauri 2 uses on Linux
through gtk 0.18 (muda, tao, wry, webkit2gtk).

One change: it no longer depends on the unmaintained `proc-macro-error` 1.0
(RUSTSEC-2024-0370). Its fork `proc-macro-error2` is unmaintained as well
(RUSTSEC-2026-0173), so the three things this crate used from it are
implemented in `src/abort.rs` on `syn::Error` (MIT, like the crate):
`#[proc_macro_error]` entry points (now `crate::abort::entry_point(move || ..)`
around the same bodies), `abort!` and `abort_call_site!` (same arguments,
same messages; a `note = span => ".."` is appended to the message as
proc-macro-error did on stable Rust). The `use proc_macro_error::..` lines
import from `crate::abort` instead, and `Cargo.toml`/`Cargo.toml.orig` drop
the dependency. Nothing else differs from the published crate; the changed
entry points were formatted with rustfmt.

Checked on macOS (Homebrew GLib 2.90) against glib 0.18.5:

- the crate's own test suite, including its 29 trybuild compile-fail cases,
  passes as it does with the original;
- a test program with seven wrong macro uses (Enum/Boxed/flags on the wrong
  item, async closure, two `@watch`, unknown keyword, `self` capture) gives
  byte-identical compiler output with the original and the patched crate.

It is patched in through `[patch.crates-io]` in the workspace `Cargo.toml`.
Remove it when Tauri moves to gtk 0.19 (glib 0.22, whose glib-macros no
longer use proc-macro-error): <https://github.com/tauri-apps/tauri/issues/10910>,
<https://github.com/tauri-apps/muda/issues/219>. ADR 0005.
