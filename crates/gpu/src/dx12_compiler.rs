// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which shader compiler wgpu's Direct3D 12 backend uses (gap G76).
//!
//! wgpu's default (`Dx12Compiler::Auto`) looks for `dxcompiler.dll` by bare
//! name and otherwise uses FXC, the legacy compiler in Windows. FXC needs
//! about 48 s for one processing variant on a Radeon 680M at every start
//! (Direct3D 12 has no pipeline cache in wgpu) and aborts on others, so the
//! start compiled with FXC even though the Windows builds bundle Microsoft's
//! DirectX Shader Compiler in `resources\` (`src-tauri/build.rs`). The app
//! names that folder at startup (`set_bundled_dir`); the processing device
//! then uses the bundled DXC by full path when both of its libraries are
//! there, and `Auto` otherwise. `WGPU_DX12_COMPILER`, when set, wins.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The compiler library and its validator, which the compiler opens by
/// bare name (loaded first by full path, so that lookup finds it).
pub const LIBRARIES: [&str; 2] = ["dxil.dll", "dxcompiler.dll"];

static BUNDLED_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The folder holding the bundled DXC (the app's `resources`). First call
/// wins; call before the GPU starts.
pub fn set_bundled_dir(dir: &Path) {
    let _ = BUNDLED_DIR.set(dir.to_path_buf());
}

/// The compiler to use and the log line saying why. `env_set`: the user
/// chose one with `WGPU_DX12_COMPILER`; `dir`: the bundled folder, when
/// known; `exists`: whether a file is there.
pub(crate) fn choose(
    env_set: bool,
    dir: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> (wgpu::Dx12Compiler, String) {
    if env_set {
        let chosen = wgpu::Dx12Compiler::from_env().unwrap_or_default();
        return (
            chosen,
            "GPU: DX12 shader compiler from WGPU_DX12_COMPILER".into(),
        );
    }
    let Some(dir) = dir else {
        return (
            wgpu::Dx12Compiler::Auto,
            "GPU: DX12 shader compiler Auto (no bundled DXC folder given)".into(),
        );
    };
    let missing: Vec<&str> = LIBRARIES
        .into_iter()
        .filter(|name| !exists(&dir.join(name)))
        .collect();
    if !missing.is_empty() {
        return (
            wgpu::Dx12Compiler::Auto,
            format!(
                "GPU: DX12 shader compiler Auto ({} not in {})",
                missing.join(", "),
                dir.display()
            ),
        );
    }
    let dxc = dir.join("dxcompiler.dll");
    (
        wgpu::Dx12Compiler::DynamicDxc {
            dxc_path: dxc.display().to_string(),
        },
        format!("GPU: DX12 shader compiler DXC from {}", dxc.display()),
    )
}

/// Sets the DX12 compiler on `descriptor` and logs the choice once.
/// Returns whether it is the bundled DXC, which the caller drops for
/// `Auto` when it exposes no adapter (`without_bundled`): wgpu hides an
/// adapter whose driver reports only Shader Model 5.1 under DXC, while FXC
/// can still drive it.
pub(crate) fn apply(descriptor: &mut wgpu::InstanceDescriptor) -> bool {
    if !descriptor.backends.contains(wgpu::Backends::DX12) {
        return false;
    }
    let env_set = std::env::var_os("WGPU_DX12_COMPILER").is_some();
    let (mut compiler, mut line) = choose(env_set, BUNDLED_DIR.get().map(PathBuf::as_path), |p| {
        p.is_file()
    });
    let bundled = !env_set && matches!(compiler, wgpu::Dx12Compiler::DynamicDxc { .. });
    let mut bundled_loaded = bundled;
    if bundled
        && let Some(dir) = BUNDLED_DIR.get()
        && let Err(e) = load_validator(&dir.join(LIBRARIES[0]))
    {
        bundled_loaded = false;
        compiler = wgpu::Dx12Compiler::Auto;
        line = format!("GPU: DX12 shader compiler Auto (dxil.dll could not be loaded: {e})");
    }
    static LOGGED: OnceLock<()> = OnceLock::new();
    if LOGGED.set(()).is_ok() {
        log::info!("{line}");
    }
    descriptor.backend_options.dx12.shader_compiler = compiler;
    bundled_loaded
}

/// `descriptor` with wgpu's own choice (`Auto`) instead of the bundled DXC.
pub(crate) fn without_bundled(descriptor: &mut wgpu::InstanceDescriptor) {
    log::warn!(
        "GPU: no DX12 adapter with the bundled DXC (Shader Model 6 not reported?); retrying with Auto"
    );
    descriptor.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::Auto;
}

#[cfg(windows)]
fn load_validator(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // Never unloaded: the compiler keeps using it.
    let module = unsafe {
        LoadLibraryExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            LOAD_WITH_ALTERED_SEARCH_PATH,
        )
    };
    if module.is_null() {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn load_validator(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_dxc_is_used_by_full_path_when_both_libraries_are_there() {
        let dir = Path::new("R/resources");
        let (compiler, line) = choose(false, Some(dir), |_| true);
        let expected = dir.join("dxcompiler.dll").display().to_string();
        assert!(
            matches!(&compiler, wgpu::Dx12Compiler::DynamicDxc { dxc_path } if *dxc_path == expected),
            "{compiler:?}"
        );
        assert!(line.contains(&expected), "{line}");
    }

    #[test]
    fn a_missing_library_or_folder_keeps_auto() {
        let dir = Path::new("R/resources");
        let (compiler, line) = choose(false, Some(dir), |p| !p.ends_with("dxil.dll"));
        assert!(matches!(compiler, wgpu::Dx12Compiler::Auto));
        assert!(line.contains("dxil.dll not in"), "{line}");
        assert!(matches!(
            choose(false, None, |_| true).0,
            wgpu::Dx12Compiler::Auto
        ));
    }

    #[test]
    fn the_environment_wins() {
        let (_, line) = choose(true, Some(Path::new("R")), |_| true);
        assert!(line.contains("WGPU_DX12_COMPILER"), "{line}");
    }
}
