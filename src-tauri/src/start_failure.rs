// SPDX-License-Identifier: AGPL-3.0-or-later
//! A start that cannot go on says so instead of closing without a word.
//!
//! On Windows the app has no console, so what the start would print to
//! stderr is never seen. Two cases get a native message box there:
//! - the WebView2 runtime is missing (the portable zip carries none; the
//!   installers set it up): checked before any window is made, with the
//!   page where Microsoft offers it, opened only when the user says Yes;
//! - a panic that ends the start (before the window's page is ready): the
//!   box names the short reason; the crash records keep the details for
//!   the report the next start offers (`rawtonic_contribute::crash`).
//!
//! The boxes are in English only: the translations live in the webview
//! (i18next), which is not up at this point. Windows itself translates the
//! box's Yes/No/OK buttons. Elsewhere the reason goes to stderr as before
//! (a start from a terminal shows it; the crash record keeps it too).

use std::sync::atomic::{AtomicBool, Ordering};

/// Microsoft's page for the WebView2 runtime ("Evergreen" bootstrapper and
/// standalone installer). Opened only on the user's click.
#[cfg_attr(not(windows), allow(dead_code))] // the boxes are Windows-only
pub(crate) const WEBVIEW2_PAGE: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

/// Characters of a panic's message the box shows at most.
const REASON_CAP: usize = 400;

#[cfg_attr(not(windows), allow(dead_code))] // the boxes are Windows-only
pub(crate) const TITLE: &str = "Rawtonic could not start";

/// The page of the main window reported ready: from here on the start is
/// done and a panic is no start failure (the next start's crash report
/// covers it).
static STARTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn mark_started() {
    STARTED.store(true, Ordering::SeqCst);
}

pub(crate) fn started() -> bool {
    STARTED.load(Ordering::SeqCst)
}

/// What the box says when the WebView2 runtime is missing.
#[cfg_attr(not(windows), allow(dead_code))] // the boxes are Windows-only
pub(crate) fn webview2_missing_text() -> String {
    format!(
        "Rawtonic needs the Microsoft Edge WebView2 Runtime to show its window, and it is not \
         installed for this Windows account.\n\n\
         The Rawtonic installer sets it up; the portable version does not. Microsoft offers the \
         runtime (the \"Evergreen\" version) on this page:\n{WEBVIEW2_PAGE}\n\n\
         Open this page in your browser now?"
    )
}

/// What the box says when a panic ended the start; `recorded` when the
/// crash records noted it, so the next start offers a report.
#[cfg_attr(not(windows), allow(dead_code))] // the boxes are Windows-only
pub(crate) fn start_failed_text(reason: &str, recorded: bool) -> String {
    let mut text = format!("Rawtonic stopped while starting:\n\n{reason}");
    if recorded {
        text.push_str(
            "\n\nThe details are kept on this computer. At its next start Rawtonic offers to send \
             a crash report and shows what it would send first.",
        );
    }
    text
}

/// A panic's message, first lines only, capped for a box.
pub(crate) fn short_reason(payload: &(dyn std::any::Any + Send)) -> String {
    let message = if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        return "an internal error (no message)".to_string();
    };
    let lines: Vec<&str> = message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(3)
        .collect();
    let joined = lines.join("\n");
    if joined.chars().count() > REASON_CAP {
        let cut: String = joined.chars().take(REASON_CAP).collect();
        format!("{cut}…")
    } else if joined.is_empty() {
        "an internal error (no message)".to_string()
    } else {
        joined
    }
}

/// Whether the raw arguments ask for a run without a window (a headless
/// export, the installer's GPU warm-up): such runs never show a box. Used
/// before the arguments are parsed, so a panic while parsing them is
/// judged too.
pub(crate) fn looks_headless(args: &[String]) -> bool {
    args.first().is_some_and(|first| first == "export")
        || args.iter().any(|arg| arg == "--warm-gpu-cache")
}

/// Whether the WebView2 runtime is missing for this account (the check
/// Tauri itself makes before its first webview).
#[cfg(windows)]
pub(crate) fn webview2_missing() -> bool {
    tauri::webview_version().is_err()
}

#[cfg(windows)]
mod native {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MESSAGEBOX_STYLE,
        MessageBoxW, SW_SHOWNORMAL,
    };
    use windows::core::{HSTRING, PCWSTR, w};

    fn show(
        text: &str,
        style: MESSAGEBOX_STYLE,
    ) -> windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_RESULT {
        let text = HSTRING::from(text);
        let title = HSTRING::from(super::TITLE);
        // SAFETY: NUL-terminated wide strings that outlive the call; no
        // owner window (none exists yet, or it is gone).
        unsafe {
            MessageBoxW(
                None,
                &text,
                &title,
                style | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
            )
        }
    }

    pub(super) fn webview2_missing() {
        if show(&super::webview2_missing_text(), MB_YESNO) == IDYES {
            let page = HSTRING::from(super::WEBVIEW2_PAGE);
            // SAFETY: wide strings that outlive the call; the default
            // browser opens the page (the user's click above).
            unsafe {
                ShellExecuteW(
                    None,
                    w!("open"),
                    &page,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                );
            }
        }
    }

    pub(super) fn start_failed(text: &str) {
        show(text, MB_OK);
    }
}

/// Tells the user the WebView2 runtime is missing (and opens Microsoft's
/// page if they want it). Windows only.
#[cfg(windows)]
pub(crate) fn tell_webview2_missing() {
    eprintln!("{}", webview2_missing_text());
    native::webview2_missing();
}

/// Tells the user the start failed: a message box on Windows, stderr
/// elsewhere (the default panic output is there already).
pub(crate) fn tell_start_failed(reason: &str, recorded: bool) {
    #[cfg(windows)]
    native::start_failed(&start_failed_text(reason, recorded));
    #[cfg(not(windows))]
    let _ = (reason, recorded);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_webview2_text_names_the_runtime_and_the_page() {
        let text = webview2_missing_text();
        assert!(text.contains("Microsoft Edge WebView2 Runtime"));
        assert!(text.contains(WEBVIEW2_PAGE));
        assert!(text.contains("portable"));
        assert!(text.ends_with("Open this page in your browser now?"));
        assert!(WEBVIEW2_PAGE.starts_with("https://developer.microsoft.com/"));
    }

    #[test]
    fn a_start_failure_names_the_reason_and_the_report_only_when_kept() {
        let kept = start_failed_text("Failed to build window: access denied", true);
        assert!(kept.contains("Failed to build window: access denied"));
        assert!(kept.contains("crash report"));
        let not_kept = start_failed_text("boom", false);
        assert!(not_kept.contains("boom"));
        assert!(!not_kept.contains("crash report"));
    }

    #[test]
    fn reasons_are_short() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("a\n\n b \nc\nd".to_string());
        assert_eq!(short_reason(payload.as_ref()), "a\nb\nc");
        let payload: Box<dyn std::any::Any + Send> = Box::new("static");
        assert_eq!(short_reason(payload.as_ref()), "static");
        let long: Box<dyn std::any::Any + Send> = Box::new("é".repeat(1000));
        let reason = short_reason(long.as_ref());
        assert_eq!(reason.chars().count(), REASON_CAP + 1);
        assert!(reason.ends_with('…'));
        let other: Box<dyn std::any::Any + Send> = Box::new(7_u32);
        assert_eq!(
            short_reason(other.as_ref()),
            "an internal error (no message)"
        );
        let empty: Box<dyn std::any::Any + Send> = Box::new(String::new());
        assert_eq!(
            short_reason(empty.as_ref()),
            "an internal error (no message)"
        );
    }

    #[test]
    fn headless_runs_are_known_before_parsing() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(looks_headless(&args(&[
            "export", "a.NEF", "--output", "b.jpg"
        ])));
        assert!(looks_headless(&args(&["--warm-gpu-cache"])));
        assert!(!looks_headless(&args(&[])));
        assert!(!looks_headless(&args(&["C:\\photos\\export"])));
        assert!(!looks_headless(&args(&["a.NEF"])));
    }

    #[test]
    fn the_start_is_done_once_marked() {
        mark_started();
        assert!(started());
    }
}
