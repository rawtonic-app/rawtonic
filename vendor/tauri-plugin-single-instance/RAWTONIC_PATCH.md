# Rawtonic patch of tauri-plugin-single-instance 2.5.4

tauri-plugin-single-instance 2.5.4 as published on crates.io (SHA-256
561cd99484561bc39c954768e7b41a66cebbe1ac3d9d081c34361f740c489e2c,
tauri-apps/plugins-workspace, Copyright Tauri Programme within The Commons
Conservancy, Apache-2.0 OR MIT; `LICENSE_APACHE-2.0`, `LICENSE_MIT` and
`LICENSE.spdx` kept unchanged). On Linux it finds a running instance over
D-Bus through zbus.

One change: it uses zbus 6 instead of zbus 5. zbus 5 depends on zvariant,
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
`tests/dbus_errors.rs` builds its test bus owner the same way. Nothing else
differs from the published crate.

Checked on Linux (CachyOS, zbus 6.0.0): upstream's D-Bus test
(`dbus-run-session -- cargo test --test dbus_errors -- --ignored`, run in a
standalone copy of this folder because a patched crate's dev-dependencies are
not in our lockfile) passes 10 of 10 runs — a missing or invalid bus address
launches normally, an invalid id fails setup, a rejecting primary fails the
second instance's setup, and a second instance hands its arguments and working
directory to the primary over the real bus and exits.

It is patched in through `[patch.crates-io]` in the workspace `Cargo.toml`.
Remove it when a tauri-plugin-single-instance release uses zbus 6. ADR 0005.
