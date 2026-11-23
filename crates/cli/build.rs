//! Windows main-thread stack of the CLI binary (gap G74).

/// Main-thread stack reserve of the executables: `RENDER_STACK_SIZE`
/// (rawtonic-runtime; a test checks they agree). Linux and macOS give the main
/// thread 8 MiB, Windows only 1 MiB unless the linker sets more, and a render
/// or GPU start-up on it would then overflow silently at start (gap G74).
const MAIN_STACK_SIZE: u64 = 8 * 1024 * 1024;

fn link_main_stack() {
    println!("cargo:rustc-env=RAWTONIC_MAIN_STACK_SIZE={MAIN_STACK_SIZE}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:{MAIN_STACK_SIZE}"),
        _ => println!("cargo:rustc-link-arg-bins=-Wl,--stack,{MAIN_STACK_SIZE}"),
    }
}

/// The released app's version (`src-tauri/Cargo.toml`, bumped by every
/// release): what the export records fingerprint with, so an update
/// re-exports with Replace if changed (`existing_files::inputs_hash`).
fn app_version() {
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let version = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("version = \"")?
                    .strip_suffix('"')
                    .map(str::to_owned)
            })
        })
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
    println!("cargo:rustc-env=RAWTONIC_APP_VERSION={version}");
}

fn main() {
    link_main_stack();
    app_version();
}
