// SPDX-License-Identifier: AGPL-3.0-or-later
//! The DirectX Shader Compiler ONNX Runtime's WebGPU plugin needs on
//! Windows (gap G76).
//!
//! Dawn, inside the plugin, compiles its shaders for Direct3D 12 with
//! Microsoft's DirectX Shader Compiler: `dxcompiler.dll` and the validator
//! `dxil.dll`, which Windows does not ship. Without them the plugin fails on
//! Direct3D 12 ("dxil.dll error 87" on a Radeon 680M) and every model runs
//! on the CPU. The Windows builds bundle Microsoft's release DLLs beside the
//! plugin in `resources\` (`src-tauri/build.rs`, pinned in
//! `src-tauri/windows/dxc.sha256`), where Dawn looks beside its own module;
//! `prepare` also loads them by full path before ONNX Runtime starts, so a
//! lookup by bare name gets the same modules, and logs what is in use.
//! Missing or unloadable, nothing else changes: the WebGPU provider fails
//! as before and AI uses the CPU, with a log line that says why.

/// Load order: the validator first, since the compiler opens it by name.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const LIBRARIES: [&str; 2] = ["dxil.dll", "dxcompiler.dll"];

/// What happened to one library, for the log.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// In the process (`path`, file version if readable).
    Loaded(String, Option<String>),
    /// Not beside the runtime.
    Missing,
    /// There but not loadable (the system's error).
    Failed(String),
}

/// The log line for the outcomes of `LIBRARIES` in `dir`: info when all
/// loaded, a warning that names what is missing otherwise.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn summary(dir: &str, outcomes: &[(&str, Outcome)]) -> (log::Level, String) {
    let mut loaded = Vec::new();
    let mut problems = Vec::new();
    for (name, outcome) in outcomes {
        match outcome {
            Outcome::Loaded(path, version) => loaded.push(format!(
                "{name} {} loaded from {path}",
                version.as_deref().unwrap_or("(version unknown)")
            )),
            Outcome::Missing => problems.push(format!("{name} is not in {dir}")),
            Outcome::Failed(why) => {
                problems.push(format!("{name} in {dir} could not be loaded: {why}"))
            }
        }
    }
    if problems.is_empty() {
        return (log::Level::Info, format!("AI: DXC {}", loaded.join(", ")));
    }
    (
        log::Level::Warn,
        format!(
            "AI: DXC unavailable ({}): ONNX Runtime's WebGPU provider cannot compile its shaders on Direct3D 12 without it, so AI runs on the CPU",
            problems.join("; ")
        ),
    )
}

#[cfg(windows)]
pub(crate) fn prepare(dir: &std::path::Path) {
    use crate::vc_runtime::windows::{file_version, load_by_path, loaded, module_path};
    let mut outcomes = Vec::new();
    for name in LIBRARIES {
        let module = match loaded(name) {
            Some(module) => module,
            None => {
                let bundled = dir.join(name);
                if !bundled.is_file() {
                    outcomes.push((name, Outcome::Missing));
                    continue;
                }
                match load_by_path(&bundled) {
                    Ok(module) => module,
                    Err(e) => {
                        outcomes.push((name, Outcome::Failed(e.to_string())));
                        continue;
                    }
                }
            }
        };
        let path = module_path(module).unwrap_or_else(|| dir.join(name));
        let version = file_version(&path).map(|v| v.to_string());
        outcomes.push((name, Outcome::Loaded(path.display().to_string(), version)));
    }
    let (level, line) = summary(&dir.display().to_string(), &outcomes);
    log::log!(level, "{line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_loaded_is_one_info_line_with_versions_and_paths() {
        let (level, line) = summary(
            r"C:\R\resources",
            &[
                (
                    "dxil.dll",
                    Outcome::Loaded(r"C:\R\resources\dxil.dll".into(), Some("1.8.2502.8".into())),
                ),
                (
                    "dxcompiler.dll",
                    Outcome::Loaded(r"C:\R\resources\dxcompiler.dll".into(), None),
                ),
            ],
        );
        assert_eq!(level, log::Level::Info);
        assert_eq!(
            line,
            r"AI: DXC dxil.dll 1.8.2502.8 loaded from C:\R\resources\dxil.dll, dxcompiler.dll (version unknown) loaded from C:\R\resources\dxcompiler.dll"
        );
    }

    #[test]
    fn a_missing_or_broken_library_warns_that_ai_uses_the_cpu() {
        let (level, line) = summary(
            r"C:\R\resources",
            &[
                ("dxil.dll", Outcome::Missing),
                ("dxcompiler.dll", Outcome::Failed("error 193".into())),
            ],
        );
        assert_eq!(level, log::Level::Warn);
        assert!(
            line.contains(r"dxil.dll is not in C:\R\resources"),
            "{line}"
        );
        assert!(
            line.contains("dxcompiler.dll in C:\\R\\resources could not be loaded: error 193"),
            "{line}"
        );
        assert!(line.ends_with("so AI runs on the CPU"), "{line}");
    }
}
