// SPDX-License-Identifier: AGPL-3.0-or-later
//! Removes what names the user, their files or their camera from text the
//! app sends (ADR 0017, "Scrubbing"), before the user sees the preview.
//! Scrubbing twice changes nothing more, which is how a reviewed body is
//! checked again just before it is sent.

use regex::{Captures, Regex};
use serde_json::Value;
use std::sync::LazyLock;

/// JSON keys whose strings are kept verbatim: binary data and digests, and
/// the fixed protocol values the relay validates (versions, system, GPU and
/// camera names as the libraries report them, licence, week, dates) — a
/// host named "linux" must not turn `"os": "linux"` into a placeholder.
/// Everything else (free text, logs) is scrubbed.
const VERBATIM_KEYS: &[&str] = &[
    "data",
    "sha256",
    "mime",
    "version",
    "os",
    "osVersion",
    "arch",
    "vendor",
    "gpuVendor",
    "name",
    "make",
    "model",
    "firmware",
    "fileType",
    "licence",
    "week",
    "crashedOn",
];

/// The start of an absolute path — POSIX, `~/`, Windows with either slash,
/// UNC, `file:` URL — at the start of the text or after a space, bracket,
/// `=`, comma or quote. Names may hold spaces, quotes (escaped or not) and
/// any other punctuation, so a path runs to the end of its line; only
/// `path_in_text` ends it earlier, at a colon before a space or a line
/// number. One fail-closed rule instead of one pattern per spelling: a few
/// words after a path are dropped rather than part of a name kept.
static PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(^|[\s(\[=,"'`])((?:file:/+|~/|/[^/\s]|[A-Za-z]:[\\/]|\\\\[^\s\\])[^\n]*)"#)
        .expect("path pattern")
});
/// Where an unquoted path ends at the latest: a colon before a space, a
/// digit (line numbers) or the end.
static PATH_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":(?:\s|\d|$)").expect("path end pattern"));
/// A file extension at the end of a word.
static EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\S\.([A-Za-z0-9]{1,8})(?:\s|$)").expect("extension pattern"));
static SERIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b((?:body|internal|lens|camera)?\s?serial(?:\s?(?:number|no))?)(["']?\s*[:=]\s*["']?)([^\s"',;}\]]+)"#,
    )
    .expect("serial pattern")
});
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b").expect("e-mail pattern")
});
static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:25[0-5]|2[0-4]\d|1?\d?\d)(?:\.(?:25[0-5]|2[0-4]\d|1?\d?\d)){3}\b")
        .expect("IPv4 pattern")
});
/// IPv6 candidates (hex digits and colons), confirmed by parsing.
static IPV6: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[0-9A-Fa-f:]{2,39}").expect("IPv6 pattern"));
static GPS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"-?\b\d{1,2}\.\d{3,}\s*,\s*-?\d{1,3}\.\d{3,}\b|\b\d{1,3}\s?(?:deg|°)\s?\d{1,2}'\s?[\d.]+"?\s?[NSEW]\b"#,
    )
    .expect("GPS pattern")
});
/// What follows a path but is not part of it: line and column numbers,
/// closing punctuation.
static PATH_TAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:(?::\d+)+|[.,;:!?)\]}"'`])+$"#).expect("path tail pattern"));
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}_.-]+").expect("word"));

/// The placeholder for an unquoted path found in running text, with what
/// follows it (": denied", ":12:5", closing punctuation) kept. Everything
/// else up to the end of the line counts as the path, since names may hold
/// spaces: a few words too many are dropped rather than a name kept.
fn path_in_text(found: &str) -> String {
    let end = PATH_END.find(found).map_or(found.len(), |m| m.start());
    let (path, rest) = found.split_at(end);
    let tail = PATH_TAIL.find(path).map_or("", |m| m.as_str());
    let path = &path[..path.len() - tail.len()];
    format!("{}{tail}{rest}", path_placeholder(path))
}

/// `<path>` plus what may stay of the last segment: its extension (the file
/// type), or the whole name of a Rust source file (a backtrace's frame,
/// never a user's file).
fn path_placeholder(path: &str) -> String {
    let last = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    if last.ends_with(".rs")
        && last.len() <= 64
        && WORD.find(last).is_some_and(|m| m.len() == last.len())
    {
        return format!("<path>/{last}");
    }
    match EXTENSION.captures_iter(last).last() {
        Some(c) => format!("<path>.{}", &c[1]),
        None => "<path>".to_string(),
    }
}

/// Removes paths, the account and host name, serial numbers, e-mail and IP
/// addresses and GPS positions from text.
#[derive(Debug, Clone, Default)]
pub struct Scrubber {
    /// Names replaced wherever they stand alone as a word, with their
    /// placeholder: the account name, the host name.
    names: Vec<(Regex, &'static str)>,
}

impl Scrubber {
    /// A scrubber for these account and host names (each may be unknown).
    pub fn new(user: Option<&str>, host: Option<&str>) -> Self {
        let mut names = Vec::new();
        let mut add = |name: &str, placeholder| {
            let name = name.trim();
            // Every name counts, even one like "user": placeholders are
            // protected by their brackets (`word_pattern`).
            if !name.is_empty() {
                names.push((name.to_string(), placeholder));
            }
        };
        if let Some(user) = user {
            add(user, "<user>");
        }
        if let Some(host) = host {
            add(host, "<host>");
            // "Annas-MacBook-Pro.local" also appears as "Annas-MacBook-Pro".
            if let Some((short, _)) = host.split_once('.') {
                add(short, "<host>");
            }
        }
        // Longest first, so a host name holding the user name goes whole.
        names.sort_by_key(|(name, _): &(String, _)| std::cmp::Reverse(name.len()));
        let names = names
            .into_iter()
            .filter_map(|(name, placeholder)| word_pattern(&name).map(|re| (re, placeholder)))
            .collect();
        Self { names }
    }

    /// The scrubber for the user running the app and this computer.
    pub fn for_this_machine() -> Self {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok()
            .or_else(|| {
                std::env::home_dir()
                    .and_then(|home| home.file_name().map(|n| n.to_string_lossy().into_owned()))
            });
        let host = gethostname::gethostname().to_string_lossy().into_owned();
        Self::new(user.as_deref(), Some(&host))
    }

    /// The text with everything personal replaced by placeholders. Repeats
    /// until nothing changes any more: a replacement can complete a pattern
    /// with what follows it (a quote closing a quoted path, say).
    pub fn scrub(&self, text: &str) -> String {
        let mut text = self.scrub_once(text);
        for _ in 0..8 {
            let next = self.scrub_once(&text);
            if next == text {
                break;
            }
            text = next;
        }
        text
    }

    fn scrub_once(&self, text: &str) -> String {
        let text = PATH.replace_all(text, |c: &Captures| {
            format!("{}{}", &c[1], path_in_text(&c[2]))
        });
        let text = SERIAL.replace_all(&text, |c: &Captures| {
            if &c[3] == "<serial>" {
                c[0].to_string()
            } else {
                format!("{}{}<serial>", &c[1], &c[2])
            }
        });
        let text = EMAIL.replace_all(&text, "<email>");
        let text = IPV4.replace_all(&text, "<ip>");
        let text = replace_ipv6(&text);
        let mut text = GPS.replace_all(&text, "<gps>").into_owned();
        for (pattern, placeholder) in &self.names {
            text = replace_word(&text, pattern, placeholder);
        }
        text
    }

    /// Scrubs every string in a JSON value, except binary data and digests.
    pub fn scrub_json(&self, value: &mut Value) {
        match value {
            Value::String(s) => *s = self.scrub(s),
            Value::Array(items) => items.iter_mut().for_each(|item| self.scrub_json(item)),
            Value::Object(fields) => {
                for (key, field) in fields.iter_mut() {
                    if !(VERBATIM_KEYS.contains(&key.as_str()) && field.is_string()) {
                        self.scrub_json(field);
                    }
                }
            }
            _ => {}
        }
    }
}

/// `name` as a whole word (case-insensitively), never inside a placeholder.
fn word_pattern(name: &str) -> Option<Regex> {
    Regex::new(&format!(
        r"(?i)(^|[^\p{{L}}\p{{N}}_<]){}($|[^\p{{L}}\p{{N}}_>])",
        regex::escape(name)
    ))
    .ok()
}

/// Replaces IPv6 addresses: runs of hex digits and colons that stand alone
/// (not part of a longer word, such as a Rust path `std::fmt`) and parse as
/// an address with at least one hex digit (not a time, `12:34:56`).
fn replace_ipv6(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in IPV6.find_iter(text) {
        let before = text[..m.start()].chars().next_back();
        let mut rest = text[m.end()..].chars();
        let after = rest.next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        // A dot joins only when a word goes on after it ("abc.def"); a
        // sentence's full stop does not.
        let joined_after = word(after) || (after == Some('.') && word(rest.next()));
        let candidate = m.as_str();
        if !word(before)
            && before != Some('.')
            && !joined_after
            && candidate.chars().any(|c| c.is_ascii_hexdigit())
            && candidate.parse::<std::net::Ipv6Addr>().is_ok()
        {
            out.push_str(&text[last..m.start()]);
            out.push_str("<ip>");
            last = m.end();
        }
    }
    out.push_str(&text[last..]);
    out
}

/// Replaces every match of a `word_pattern` with the placeholder.
fn replace_word(text: &str, re: &Regex, placeholder: &str) -> String {
    // Adjacent matches share a separator; repeat until none is left.
    let mut out = text.to_string();
    loop {
        let next = re
            .replace_all(&out, |c: &Captures| {
                format!("{}{placeholder}{}", &c[1], &c[2])
            })
            .into_owned();
        if next == out {
            return out;
        }
        out = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn anna() -> Scrubber {
        Scrubber::new(Some("anna"), Some("Annas-MacBook-Pro.local"))
    }

    #[test]
    fn paths_keep_only_their_extension() {
        let s = anna();
        assert_eq!(
            s.scrub("open /Users/anna/Pictures/Wedding.CR3 failed"),
            "open <path>.CR3"
        );
        assert_eq!(s.scrub("cache ~/Library/Caches/x"), "cache <path>");
        assert_eq!(
            s.scrub(r"read C:\Users\anna\Bilder\a.NEF: denied"),
            "read <path>.NEF: denied"
        );
        assert_eq!(
            s.scrub(r"share \\nas\photos\2026\b.ARW"),
            "share <path>.ARW"
        );
        assert_eq!(
            s.scrub("url file:///Volumes/Card/DCIM/c.RAF"),
            "url <path>.RAF"
        );
        assert_eq!(s.scrub("folder /Volumes/Anna Card/"), "folder <path>");
    }

    #[test]
    fn unquoted_paths_with_spaces_go_whole() {
        let s = anna();
        assert_eq!(
            s.scrub("open /Volumes/Client Jobs/Jane Wedding.CR3 failed"),
            "open <path>.CR3"
        );
        assert_eq!(
            s.scrub(r"read C:\Users\Jane Doe\Pictures\Jane Wedding.NEF: access denied"),
            "read <path>.NEF: access denied"
        );
        assert_eq!(
            s.scrub("moved /Volumes/Card One/DCIM/a.ARW\nnext line"),
            "moved <path>.ARW\nnext line"
        );
    }

    #[test]
    fn quoted_paths_go_whole_even_with_spaces() {
        let s = anna();
        assert_eq!(
            s.scrub(r#"Failed to open "/Users/anna/My Photos/Anna and Ben.dng": gone"#),
            r#"Failed to open "<path>.dng": gone"#
        );
        assert_eq!(s.scrub("'/home/x/a b/c.tif'"), "'<path>.tif'");
    }

    #[test]
    fn backtraces_keep_source_file_names() {
        let s = Scrubber::new(Some("runner"), None);
        assert_eq!(
            s.scrub("   at /Users/runner/.cargo/registry/src/x/foo-1.0/src/lib.rs:12:5"),
            "   at <path>/lib.rs:12:5"
        );
        assert_eq!(
            s.scrub("panicked at src/develop.rs:40:9"),
            "panicked at src/develop.rs:40:9"
        );
    }

    #[test]
    fn urls_are_not_paths() {
        let s = anna();
        let text = "see https://github.com/rawtonic-app/rawtonic/issues";
        assert_eq!(s.scrub(text), text);
    }

    #[test]
    fn names_serials_addresses_and_positions() {
        let s = anna();
        assert_eq!(
            s.scrub("Anna opened it on Annas-MacBook-Pro.local"),
            "<user> opened it on <host>"
        );
        assert_eq!(s.scrub("annas-macbook-pro said hi"), "<host> said hi");
        assert_eq!(s.scrub("not annabel"), "not annabel");
        assert_eq!(
            s.scrub("BodySerialNumber: 0412345"),
            "BodySerialNumber: <serial>"
        );
        assert_eq!(
            s.scrub(r#"{"SerialNumber":"A1B2C3"}"#),
            r#"{"SerialNumber":"<serial>"}"#
        );
        assert_eq!(
            s.scrub("Lens serial number = 99887"),
            "Lens serial number = <serial>"
        );
        assert_eq!(s.scrub("mail anna.b@example.org now"), "mail <email> now");
        assert_eq!(s.scrub("from 192.168.1.60:8080"), "from <ip>:8080");
        assert_eq!(s.scrub("v6 fe80::1c2:3ff:fe4d:5e6f"), "v6 <ip>");
        assert_eq!(
            s.scrub("ula fdab:cdef:abcd:abcd:abcd:abcd:abcd:abcd up"),
            "ula <ip> up"
        );
        assert_eq!(s.scrub("[::1]:8080"), "[<ip>]:8080");
        assert_eq!(s.scrub("at 52.520008, 13.404954"), "at <gps>");
        assert_eq!(s.scrub(r#"52 deg 31' 12.00" N"#), "<gps>");
    }

    #[test]
    fn leaves_ordinary_log_lines_alone() {
        let s = anna();
        for line in [
            "2026-10-24 09:38:12 [INFO] Develop frame 16.2 ms",
            "Rawtonic 0.1.0-alpha.4 on macos 27.0",
            "core::panicking::panic_fmt",
            "abc::def::fed",
            "Sony ILCE-7M3 firmware 4.01",
        ] {
            assert_eq!(s.scrub(line), line);
        }
    }

    #[test]
    fn json_strings_except_data_and_digests() {
        let s = anna();
        let mut value = serde_json::json!({
            "text": "see /Users/anna/x.CR3",
            "screenshot": { "mime": "image/png", "data": "iVBO/Users/anna+/=" },
            "sha256": "ab",
            "list": ["anna", 3],
        });
        s.scrub_json(&mut value);
        assert_eq!(value["text"], "see <path>.CR3");
        assert_eq!(value["screenshot"]["data"], "iVBO/Users/anna+/=");
        assert_eq!(value["list"][0], "<user>");
    }

    #[test]
    fn quotes_inside_unquoted_names_do_not_end_them() {
        let s = anna();
        assert_eq!(
            s.scrub("open /Volumes/Client's Photos/Jane Wedding.CR3"),
            "open <path>.CR3"
        );
        assert_eq!(
            s.scrub(r#"open C:\Jobs\"Best" <Jane> | Ben's\x.NEF"#),
            "open <path>.NEF"
        );
    }

    #[test]
    fn every_path_spelling_goes_to_the_end_of_its_line() {
        let s = anna();
        assert_eq!(
            s.scrub("copy C:/Users/anna/Pictures/Client Wedding.CR3 done"),
            "copy <path>.CR3"
        );
        assert_eq!(
            s.scrub(r#"open "C:/Jobs/Jane Wedding.NEF": denied"#),
            r#"open "<path>.NEF": denied"#
        );
        assert_eq!(
            s.scrub(r#"Failed "/Volumes/Client \"Jane Wedding\".CR3": gone"#),
            r#"Failed "<path>.CR3": gone"#
        );
        assert_eq!(
            s.scrub("{\"file\":\"/Users/a/b c.CR3\",\"size\":5}"),
            "{\"file\":\"<path>\":5}"
        );
    }

    #[test]
    fn sentence_final_ipv6_goes_too() {
        let s = anna();
        assert_eq!(
            s.scrub("Failed to connect to fdab:cdef:abcd:abcd:abcd:abcd:abcd:abcd."),
            "Failed to connect to <ip>."
        );
        assert_eq!(s.scrub("v fe80::1, then"), "v <ip>, then");
    }

    #[test]
    fn fixed_protocol_values_are_kept_even_when_they_match_a_name() {
        let s = Scrubber::new(Some("apple"), Some("linux"));
        let mut value = serde_json::json!({
            "schema": 1,
            "app": { "version": "0.1.0", "os": "linux", "osVersion": "6.1", "arch": "x86_64" },
            "gpu": { "vendor": "apple", "name": "Apple M5 Pro" },
            "photo": { "make": "Apple", "model": "iPhone", "fileType": "DNG" },
            "text": "linux box of apple",
        });
        s.scrub_json(&mut value);
        assert_eq!(value["app"]["os"], "linux");
        assert_eq!(value["gpu"]["vendor"], "apple");
        assert_eq!(value["gpu"]["name"], "Apple M5 Pro");
        assert_eq!(value["photo"]["make"], "Apple");
        assert_eq!(value["text"], "<host> box of <user>");
    }

    #[test]
    fn names_like_placeholders_go_too_and_placeholders_stay() {
        let s = Scrubber::new(Some("user"), Some("host"));
        assert_eq!(s.scrub("user on host"), "<user> on <host>");
        assert_eq!(s.scrub(&s.scrub("user on host")), "<user> on <host>");
        let s = Scrubber::new(Some("path"), Some("x"));
        assert_eq!(
            s.scrub("path saw x: /a/b.txt"),
            "<user> saw <host>: <path>.txt"
        );
        assert_eq!(
            s.scrub(&s.scrub("path saw x: /a/b.txt")),
            "<user> saw <host>: <path>.txt"
        );
    }

    proptest! {
        #[test]
        fn scrubbing_twice_changes_nothing_more(parts in proptest::collection::vec(prop_oneof![
            Just("/Users/anna/a b/c.CR3".to_string()),
            Just("\"/Users/anna/My Photos/x.dng\"".to_string()),
            Just(r"C:\Users\anna\y.NEF".to_string()),
            Just("anna".to_string()),
            Just("Annas-MacBook-Pro".to_string()),
            Just("SerialNumber=123".to_string()),
            Just("a@b.cd".to_string()),
            Just("10.0.0.1".to_string()),
            Just("fe80::1:2".to_string()),
            Just("1.234567, 2.345678".to_string()),
            Just("src/lib.rs:1:2".to_string()),
            "[ -~]{0,12}",
        ], 0..12), seps in proptest::collection::vec(prop_oneof![Just(" "), Just(""), Just("\n"), Just("/"), Just(":")], 12)) {
            let text: String = parts.iter().zip(seps.iter()).map(|(p, s)| format!("{p}{s}")).collect();
            let s = anna();
            let once = s.scrub(&text);
            prop_assert_eq!(s.scrub(&once), once);
        }
    }
}
