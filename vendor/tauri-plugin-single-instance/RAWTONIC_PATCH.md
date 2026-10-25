# Rawtonic patch of tauri-plugin-single-instance 2.5.4

tauri-plugin-single-instance 2.5.4 as published on crates.io (SHA-256
561cd99484561bc39c954768e7b41a66cebbe1ac3d9d081c34361f740c489e2c,
tauri-apps/plugins-workspace, Copyright Tauri Programme within The Commons
Conservancy, Apache-2.0 OR MIT; `LICENSE_APACHE-2.0`, `LICENSE_MIT` and
`LICENSE.spdx` kept unchanged). On Linux it finds a running instance over
D-Bus through zbus.

It uses zbus 6 instead of zbus 5. zbus 5 depends on zvariant,
zvariant_derive and zbus_names, which are unmaintained since zbus 6.0.0
merged them into zbus (RUSTSEC-2026-0370, -0371, -0372). zbus 6 has no
blocking API, so `src/platform_impl/linux.rs` runs the same async calls in
`zbus::block_on` (connection build, forwarding the arguments to the running
instance, releasing the name on exit); the default zbus runtime keeps serving
the connection on a helper thread after `block_on` returns. zbus 6 also defers
builder errors to `build()`, where every error but `NameTaken` means "launch
normally"; the D-Bus name and object path are therefore validated before the
build, so an invalid id still fails plugin setup as in 2.5.4. `Cargo.toml` and
`Cargo.toml.orig` require zbus 6; the copy of upstream's
`tests/dbus_errors.rs` builds its test bus owner the same way.

Before the invocation-directory change, checked on Linux (CachyOS, zbus 6.0.0): upstream's D-Bus test
(`dbus-run-session -- cargo test --test dbus_errors -- --ignored`, run in a
standalone copy of this folder because a patched crate's dev-dependencies are
not in our lockfile) passes 10 of 10 runs — a missing or invalid bus address
launches normally, an invalid id fails setup, a rejecting primary fails the
second instance's setup, and a second instance hands its arguments and working
directory to the primary over the real bus and exits.

It is patched in through `[patch.crates-io]` in the workspace `Cargo.toml`.
Remove it only when an upstream release both uses zbus 6 and supports forwarding
the invocation directory with the same AppImage rule. Otherwise preserve the
resolver and its app call site when updating the dependency. ADR 0005.

The Linux sender resolves its forwarded working directory against the AppImage
runtime's `OWD` (or the shell's `PWD` for `--appimage-extract-and-run`) before
the D-Bus call. `invocation_dir` is shared with Rawtonic's initial launch parser,
so relative paths follow the same rule for initial and forwarded launches.
The process cwd stays unchanged for WebKit helpers; non-AppImage launches
still forward `current_dir()`. This Rawtonic resolver is AGPL-3.0-or-later.
Keep this behavior when replacing the zbus patch with an upstream release.

The invocation-directory change has resolution-unit-test coverage with fake
environment values on macOS, not a live D-Bus or manual AppImage check. The
`PWD` fallback is unchanged from PR #345, which measured an extract-and-run
AppImage; this follow-up did not repeat that check for a forwarded launch.
The crate's combined license expression includes the AGPL resolver; upstream
license texts and `LICENSE.spdx` remain unchanged.
