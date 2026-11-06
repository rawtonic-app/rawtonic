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

fn main() {
    link_main_stack();
}
