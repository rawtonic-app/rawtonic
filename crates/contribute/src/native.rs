// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native crashes on Windows: what ended a run that left neither a panic
//! note nor a clean exit (an access violation, a stack overflow, a
//! fail-fast abort), read at the next start from the "Application Error"
//! entry (event 1000) Windows writes to its Application log for every
//! crashing process.
//!
//! Why the event log and not a handler in the crashing process: an
//! unhandled-exception filter does not see a fail-fast (`abort`, Rust's
//! out-of-memory and double-panic aborts, `/GS` stack checks: 0xC0000409),
//! another library may replace it, and after a stack overflow on a driver's
//! thread it would run on a stack that is already gone. WER's local dumps
//! need administrator rights (the key is under HKLM) and keep the process's
//! memory, photographs included. Windows' own entry is written outside the
//! crashing process for every one of these, needs no rights, and holds
//! exactly what a report needs: the exception code and the faulting module
//! with its offset. The record keeps the module's file name only.

/// One native crash, as a record describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCrash {
    /// The exception code (NTSTATUS), 0xC0000005 for an access violation.
    pub code: u32,
    /// The faulting module's file name (no folder).
    pub module: Option<String>,
    /// The fault's offset in that module.
    pub offset: Option<u64>,
}

impl NativeCrash {
    /// "an access violation (0xC0000005)".
    pub fn what(&self) -> String {
        let name = match self.code {
            0xC000_0005 => "an access violation",
            0xC000_00FD => "a stack overflow",
            0xC000_0409 => "a fail-fast abort (abort, out of memory or a stack check)",
            0xC000_0374 => "a heap corruption",
            0xC000_001D => "an illegal instruction",
            0xC000_0094 => "an integer division by zero",
            0xC000_0006 => "an in-page error (a file or drive the code was read from went away)",
            0x8000_0003 => "a breakpoint",
            0xC000_0602 => "a fail-fast exception",
            _ => "an exception",
        };
        format!("{name} (0x{:08X})", self.code)
    }

    /// "nvoglv64.dll+0x12e940", or "native code" without a module.
    pub fn place(&self) -> String {
        match (&self.module, self.offset) {
            (Some(module), Some(offset)) => format!("{module}+0x{offset:x}"),
            (Some(module), None) => module.clone(),
            _ => "native code".to_string(),
        }
    }
}

/// The fields of an "Application Error" entry (event 1000) that matter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppErrorEvent {
    pub app_name: String,
    pub pid: u32,
    /// When the crashed process started (Unix seconds).
    pub process_started: Option<u64>,
    pub crash: NativeCrash,
}

/// The order of the entry's data when Windows renders it without names
/// (the provider's template), else matched by name.
const FIELDS: [&str; 10] = [
    "AppName",
    "AppVersion",
    "AppTimeStamp",
    "ModuleName",
    "ModuleVersion",
    "ModuleTimeStamp",
    "ExceptionCode",
    "FaultingOffset",
    "ProcessId",
    "ProcessCreationTime",
];

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The `<Data>` elements of an event's XML, as (name, text).
fn data_elements(xml: &str) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<Data") {
        rest = &rest[at + 5..];
        // `<DataItem>` or similar is not a data element.
        let Some(next) = rest.chars().next() else {
            break;
        };
        if !(next == '>' || next == '/' || next.is_whitespace()) {
            continue;
        }
        let Some(tag_end) = rest.find('>') else { break };
        let attributes = &rest[..tag_end];
        let name = attributes.find("Name=").and_then(|n| {
            let value = &attributes[n + 5..];
            let quote = value.chars().next()?;
            let value = &value[1..];
            value.find(quote).map(|end| unescape(&value[..end]))
        });
        if attributes.trim_end().ends_with('/') {
            out.push((name, String::new()));
            rest = &rest[tag_end + 1..];
            continue;
        }
        rest = &rest[tag_end + 1..];
        let Some(close) = rest.find("</Data>") else {
            break;
        };
        out.push((name, unescape(rest[..close].trim())));
        rest = &rest[close + 7..];
    }
    out
}

fn hex(text: &str) -> Option<u64> {
    let digits = text
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    u64::from_str_radix(digits, 16).ok()
}

/// A FILETIME (100 ns since 1601) as Unix seconds.
fn filetime_secs(filetime: u64) -> Option<u64> {
    (filetime / 10_000_000).checked_sub(11_644_473_600)
}

fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// Reads an "Application Error" entry rendered as XML (`EvtRender`).
pub fn parse_application_error(xml: &str) -> Option<AppErrorEvent> {
    let data = data_elements(xml);
    // By name; else by position, among elements without a name.
    let field = |wanted: &str| -> Option<&str> {
        if let Some((_, value)) = data
            .iter()
            .find(|(name, _)| name.as_deref() == Some(wanted))
        {
            return Some(value.as_str());
        }
        let index = FIELDS.iter().position(|f| *f == wanted)?;
        match data.get(index) {
            Some((None, value)) => Some(value.as_str()),
            _ => None,
        }
    };
    let code = u32::try_from(hex(field("ExceptionCode")?)?).ok()?;
    let pid = u32::try_from(hex(field("ProcessId")?)?).ok()?;
    let module = field("ModuleName")
        .map(file_name)
        .and_then(|name| crate::crash::plain_name(&name, 100));
    Some(AppErrorEvent {
        app_name: file_name(field("AppName")?),
        pid,
        process_started: field("ProcessCreationTime")
            .and_then(hex)
            .and_then(filetime_secs),
        crash: NativeCrash {
            code,
            module,
            offset: field("FaultingOffset").and_then(hex),
        },
    })
}

/// How far the start of the crashed process may lie before the time its
/// marker was written (its start-up up to the crash records).
pub const MARKER_AFTER_START_SECS: u64 = 600;

/// Whether `event` is the crash of the run with process id `pid` of the
/// executable `exe_name`, whose marker was written at `marker_started`.
pub fn is_crash_of(event: &AppErrorEvent, exe_name: &str, pid: u32, marker_started: u64) -> bool {
    event.pid == pid
        && event.app_name.eq_ignore_ascii_case(exe_name)
        && event.process_started.is_none_or(|started| {
            started <= marker_started + 5
                && marker_started.saturating_sub(started) <= MARKER_AFTER_START_SECS
        })
}

/// The native crash of the run with process id `pid` (marker written at
/// `marker_started`) from Windows' Application log, if Windows recorded
/// one. Elsewhere: none (Unix crashes leave a signal note).
pub fn find_crash(exe_name: &str, pid: u32, marker_started: u64) -> Option<NativeCrash> {
    #[cfg(windows)]
    {
        events::application_errors(KEEP_LOOKING_SECS, 200)
            .into_iter()
            .filter_map(|xml| parse_application_error(&xml))
            .find(|event| is_crash_of(event, exe_name, pid, marker_started))
            .map(|event| event.crash)
    }
    #[cfg(not(windows))]
    {
        let _ = (exe_name, pid, marker_started);
        None
    }
}

/// Entries this old at most are read (the records' own limit).
#[cfg_attr(not(windows), allow(dead_code))]
const KEEP_LOOKING_SECS: u64 = crate::crash::KEEP_DAYS * 86_400;

#[cfg(windows)]
mod events {
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError};
    use windows_sys::Win32::System::EventLog::{
        EVT_HANDLE, EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection,
        EvtRender, EvtRenderEventXml,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    struct Handle(EVT_HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if self.0 != 0 {
                // SAFETY: a handle EvtQuery/EvtNext returned, closed once.
                unsafe { EvtClose(self.0) };
            }
        }
    }

    fn render(event: &Handle) -> Option<String> {
        let mut used = 0u32;
        let mut count = 0u32;
        // SAFETY: a size query with no buffer, then a render into one of
        // the size asked for (bytes, UTF-16).
        unsafe {
            if EvtRender(
                0,
                event.0,
                EvtRenderEventXml,
                0,
                std::ptr::null_mut(),
                &mut used,
                &mut count,
            ) == 0
                && GetLastError() != ERROR_INSUFFICIENT_BUFFER
            {
                return None;
            }
            let mut buffer = vec![0u16; (used as usize).div_ceil(2) + 1];
            let size = (buffer.len() * 2) as u32;
            if EvtRender(
                0,
                event.0,
                EvtRenderEventXml,
                size,
                buffer.as_mut_ptr().cast(),
                &mut used,
                &mut count,
            ) == 0
            {
                return None;
            }
            let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            Some(String::from_utf16_lossy(&buffer[..len]))
        }
    }

    /// The newest `max` "Application Error" entries of the last `secs`
    /// seconds, as XML, newest first.
    pub(super) fn application_errors(secs: u64, max: usize) -> Vec<String> {
        let query = wide(&format!(
            "*[System[Provider[@Name='Application Error'] and (EventID=1000) and TimeCreated[timediff(@SystemTime) <= {}]]]",
            secs * 1000
        ));
        let channel = wide("Application");
        // SAFETY: NUL-terminated wide strings that outlive the call.
        let results = Handle(unsafe {
            EvtQuery(
                0,
                channel.as_ptr(),
                query.as_ptr(),
                EvtQueryChannelPath | EvtQueryReverseDirection,
            )
        });
        let mut out = Vec::new();
        if results.0 == 0 {
            return out;
        }
        while out.len() < max {
            let mut batch = [0isize; 16];
            let mut returned = 0u32;
            // SAFETY: room for 16 handles; each returned one is closed.
            let ok = unsafe {
                EvtNext(
                    results.0,
                    batch.len() as u32,
                    batch.as_mut_ptr(),
                    1000,
                    0,
                    &mut returned,
                )
            };
            if ok == 0 || returned == 0 {
                break;
            }
            for &raw in &batch[..returned as usize] {
                let event = Handle(raw);
                if out.len() < max
                    && let Some(xml) = render(&event)
                {
                    out.push(xml);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Composed for the test in the shape Windows renders event 1000.
    fn event_xml(named: bool) -> String {
        let values = [
            "rawtonic-app.exe",
            "0.1.0.0",
            "6ae7bd5a",
            "nvoglv64.dll",
            "31.0.15.1234",
            "5fd39a05",
            "c00000fd",
            "0x000000000012c51a",
            "0x1d10",
            "0x1dd6e1174bfac9d",
            "C:\\Users\\anna\\AppData\\Local\\Rawtonic\\rawtonic-app.exe",
            "C:\\WINDOWS\\SYSTEM32\\nvoglv64.dll",
        ];
        let names = [
            "AppName",
            "AppVersion",
            "AppTimeStamp",
            "ModuleName",
            "ModuleVersion",
            "ModuleTimeStamp",
            "ExceptionCode",
            "FaultingOffset",
            "ProcessId",
            "ProcessCreationTime",
            "AppPath",
            "ModulePath",
        ];
        let data: String = values
            .iter()
            .zip(names)
            .map(|(value, name)| {
                if named {
                    format!("<Data Name='{name}'>{value}</Data>")
                } else {
                    format!("<Data>{value}</Data>")
                }
            })
            .collect();
        format!(
            "<Event xmlns='http://schemas.microsoft.com/win/2004/08/events/event'><System><Provider Name='Application Error'/><EventID>1000</EventID></System><EventData>{data}<Data Name='PackageFullName'/></EventData></Event>"
        )
    }

    #[test]
    fn application_errors_are_read_with_and_without_field_names() {
        for named in [true, false] {
            let event = parse_application_error(&event_xml(named)).unwrap();
            assert_eq!(event.app_name, "rawtonic-app.exe");
            assert_eq!(event.pid, 0x1d10);
            assert_eq!(event.crash.code, 0xC000_00FD);
            assert_eq!(event.crash.module.as_deref(), Some("nvoglv64.dll"));
            assert_eq!(event.crash.offset, Some(0x12c51a));
            // 0x1dd6e1174bfac9d is 2026-11-06 (a WINQ run's start).
            let started = event.process_started.unwrap();
            assert_eq!(crate::crash::utc_date(started), "2026-11-06");
        }
        assert_eq!(
            parse_application_error("<Event><EventData></EventData></Event>"),
            None
        );
    }

    #[test]
    fn a_crash_is_described_by_code_module_and_offset() {
        let crash = NativeCrash {
            code: 0xC000_0005,
            module: Some("vk_swiftshader.dll".into()),
            offset: Some(0x12e940),
        };
        assert_eq!(crash.what(), "an access violation (0xC0000005)");
        assert_eq!(crash.place(), "vk_swiftshader.dll+0x12e940");
        let unknown = NativeCrash {
            code: 0xE0434352,
            module: None,
            offset: None,
        };
        assert_eq!(unknown.what(), "an exception (0xE0434352)");
        assert_eq!(unknown.place(), "native code");
    }

    #[test]
    fn only_the_crash_of_that_run_matches() {
        let event = parse_application_error(&event_xml(false)).unwrap();
        let started = event.process_started.unwrap();
        assert!(is_crash_of(&event, "Rawtonic-App.exe", 0x1d10, started + 3));
        // Another process id, another program, or a process started long
        // before the marker (the id reused) is not that run.
        assert!(!is_crash_of(
            &event,
            "rawtonic-app.exe",
            0x1d11,
            started + 3
        ));
        assert!(!is_crash_of(&event, "other.exe", 0x1d10, started + 3));
        assert!(!is_crash_of(
            &event,
            "rawtonic-app.exe",
            0x1d10,
            started + 86_400
        ));
        assert!(!is_crash_of(
            &event,
            "rawtonic-app.exe",
            0x1d10,
            started - 60
        ));
    }

    /// Windows: a child process that crashes is found in the Application
    /// log with its exception code and module (task WSF). Needs Windows
    /// Error Reporting on (the default).
    #[cfg(windows)]
    #[test]
    // The GitHub Windows runner writes no Application Error entry for any
    // crashing process (a crashing pwsh neither; CI run 43132467361, G77).
    #[ignore = "needs Windows Error Reporting's Application Error entries, which the GitHub \
                Windows runner does not write (G77); run with --ignored on a Windows desktop"]
    fn a_crashing_child_is_found_in_the_application_log() {
        for (kind, code) in [
            ("access-violation", 0xC000_0005u32),
            ("stack-overflow", 0xC000_00FD),
        ] {
            let exe = std::env::current_exe().unwrap();
            let started = crate::crash::now_secs();
            let mut child = std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "native::tests::crash_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("RAWTONIC_TEST_NATIVE_CRASH", kind)
                .spawn()
                .unwrap();
            let pid = child.id();
            let status = child.wait().unwrap();
            assert_eq!(status.code().map(|c| c as u32), Some(code), "{kind}");
            let exe_name = exe.file_name().unwrap().to_string_lossy().into_owned();
            let mut found = None;
            for _ in 0..60 {
                found = find_crash(&exe_name, pid, started);
                if found.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            let crash = found
                .unwrap_or_else(|| panic!("no Application Error entry for {kind} (pid {pid})"));
            assert_eq!(crash.code, code, "{kind}");
            assert!(crash.module.is_some(), "{kind}: {crash:?}");
        }
    }

    /// The crashing child of the test above (does nothing on its own).
    #[cfg(windows)]
    #[test]
    #[ignore = "child process of a_crashing_child_is_found_in_the_application_log"]
    fn crash_child() {
        match std::env::var("RAWTONIC_TEST_NATIVE_CRASH").as_deref() {
            Ok("access-violation") => {
                // SAFETY: none; the write to the first page faults on
                // purpose (not null: that is a checked precondition).
                unsafe { std::ptr::write_volatile(std::ptr::without_provenance_mut::<u64>(16), 1) };
            }
            Ok("stack-overflow") => {
                #[allow(unconditional_recursion)]
                fn deeper(n: u64) -> u64 {
                    let pad = [n; 512];
                    std::hint::black_box(&pad);
                    deeper(n + 1) + pad[7]
                }
                std::hint::black_box(deeper(0));
            }
            _ => {}
        }
    }
}
