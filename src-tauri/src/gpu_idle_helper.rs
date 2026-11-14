// SPDX-License-Identifier: AGPL-3.0-or-later
//! `rawtonic-app --gpu-idle-compile JOB FOLDER`: the helper process of the
//! idle shader compiles under a compile limit (AMD's Windows Vulkan driver;
//! `rawtonic_gpu::gpu_processing::set_idle_compile_helper`, G76/WCC11). It
//! starts the device as the app does, compiles one display variant into a
//! pipeline cache in FOLDER and exits; the app kills it the moment a
//! compile or an export needs the compile slot, so no edit waits behind an
//! idle compile. It runs before anything else of the app (no window, log,
//! settings, crash flag or panic note: a helper killed or failing is never
//! taken for a crash of the app); why it failed goes into FOLDER
//! (`HELPER_ERROR_FILE`) for the app's log.

use std::path::Path;
use std::time::Duration;

pub const ARG: &str = "--gpu-idle-compile";

/// A helper still running after this ends itself (the app stops it long
/// before; this covers an app that is gone).
const LIMIT: Duration = Duration::from_secs(900);

/// Runs the helper when the process was started as one: its exit code.
pub fn run_if_asked() -> Option<i32> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some(ARG) {
        return None;
    }
    let (Some(job), Some(folder)) = (args.next(), args.next()) else {
        return Some(2);
    };
    let _ = std::thread::Builder::new()
        .name("gpu-idle-compile-limit".into())
        .spawn(|| {
            std::thread::sleep(LIMIT);
            std::process::exit(3);
        });
    let folder = Path::new(&folder);
    match rawtonic_gpu::gpu_processing::compile_in_helper_process(
        &job,
        folder,
        crate::gpu_processing::PIPELINE_CACHE_BUILD,
    ) {
        Ok(()) => Some(0),
        Err(error) => {
            let _ = std::fs::write(
                folder.join(rawtonic_gpu::gpu_processing::HELPER_ERROR_FILE),
                error,
            );
            Some(1)
        }
    }
}

/// Lets the idle compiles start helpers: this program with `ARG`, without
/// a console window and below normal priority on Windows (idle work).
pub fn register() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    rawtonic_gpu::gpu_processing::set_idle_compile_helper(Box::new(move |job, folder| {
        let mut command = std::process::Command::new(&exe);
        command
            .arg(ARG)
            .arg(job)
            .arg(folder)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
            command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
        }
        command.spawn()
    }));
}
