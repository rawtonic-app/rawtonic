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
/// before; this covers an app that is gone, where a Windows job object
/// does not end it, `kill_with_app`).
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
            let _ = rawtonic_fs::atomic_write(
                &folder.join(rawtonic_gpu::gpu_processing::HELPER_ERROR_FILE),
                error,
            );
            Some(1)
        }
    }
}

/// Lets the idle compiles start helpers: this program with `ARG`, without
/// a console window on Windows. At normal priority: an edit whose variant
/// the running helper compiles waits for it (G76/WSMOKE16: 27.5 s at below
/// normal priority against 23–25 s in the app).
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
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let child = command.spawn()?;
        kill_with_app(&child);
        Ok(child)
    }));
}

/// Puts `child` in a job object the app holds open until it ends, which
/// kills the job's processes when it closes: a helper ends with the app,
/// also when the app is killed or crashes, instead of compiling on until
/// its `LIMIT` (G76/HCF). Helpers run only under a compile limit, which
/// only AMD's Windows Vulkan driver has; elsewhere a helper of an app that
/// is gone still ends at its `LIMIT`. Failing that, the helper runs as
/// before (logged).
#[cfg(windows)]
fn kill_with_app(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    /// The job's handle (never closed: the system closes it when the app
    /// process ends, which ends the helpers in it).
    static JOB: std::sync::OnceLock<Result<isize, String>> = std::sync::OnceLock::new();
    let job = JOB.get_or_init(|| {
        // SAFETY: plain Win32 calls; `info` lives across the call that
        // reads it, with its true size.
        unsafe {
            let job = CreateJobObjectW(None, windows::core::PCWSTR::null())
                .map_err(|e| format!("{e}"))?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(|e| format!("{e}"))?;
            Ok(job.0)
        }
    });
    let assigned = job.as_ref().map_err(String::clone).and_then(|&job| {
        // SAFETY: `job` is the job created above and still open; the child's
        // handle is valid while `child` lives.
        unsafe {
            AssignProcessToJobObject(HANDLE(job), HANDLE(child.as_raw_handle() as isize))
                .map_err(|e| format!("{e}"))
        }
    });
    if let Err(error) = assigned {
        log::warn!(
            "GPU idle compile helper {} does not end with the app ({error}); it ends after {} s at the latest",
            child.id(),
            LIMIT.as_secs()
        );
    }
}

#[cfg(not(windows))]
fn kill_with_app(_child: &std::process::Child) {}
