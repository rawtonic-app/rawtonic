//! Capture times without a UTC offset are the camera's local wall time; an
//! export turns them into an instant (file times, names) in the computer's
//! time zone. Pinned in one zone with daylight saving before the date
//! library changed (DEPC1), as chrono 0.4 read them: a time inside the
//! spring-forward gap is read as UTC (its first second with the winter
//! offset), a time in the autumn fold with the winter offset (its later
//! instant).
//!
//! One test in its own binary, so setting `TZ` races with nothing.
#![cfg(unix)]

use rawtonic_export::exif_processing::{exif_datetime, parse_raw_creation_date};
use rawtonic_export::export_processing::ExportSettings;

/// Seconds and nanoseconds since 1970 of an instant.
fn unix(time: chrono::DateTime<chrono::Utc>) -> (i64, u32) {
    (time.timestamp(), time.timestamp_subsec_nanos())
}

#[test]
fn local_capture_times_become_instants_in_the_computer_zone() {
    // SAFETY: the only test of this binary, set before any time is read.
    unsafe { std::env::set_var("TZ", "Europe/Zurich") };

    // EXIF's form, written back from every form a date is kept in.
    for (kept, written) in [
        ("2026:07:01 12:00:00", "2026:07:01 12:00:00"),
        ("2026-07-01 12:00:00", "2026:07:01 12:00:00"),
        ("2026-07-01T12:00:00", "2026:07:01 12:00:00"),
        ("2026:07:01 12:00:00.5", "2026:07:01 12:00:00"),
        ("2026-07-01 12:00:00.123456789", "2026:07:01 12:00:00"),
        ("2026-07-01 12:00:00.1234567891", "2026:07:01 12:00:00"),
        ("2026:7:1 9:5:2", "2026:07:01 09:05:02"),
        (" 2026:07:01 12:00:00 ", "2026:07:01 12:00:00"),
        ("2026:02:30 12:00:00", "2026:02:30 12:00:00"),
        ("not a date", "not a date"),
    ] {
        assert_eq!(exif_datetime(kept), written, "{kept:?}");
    }

    for (local, instant) in [
        // Winter (+01:00) and summer (+02:00) time.
        ("2026:01:15 12:00:00", (1_768_474_800, 0)),
        ("2026:07:01 12:00:00", (1_782_900_000, 0)),
        ("2026:07:01 12:00:00.5", (1_782_900_000, 500_000_000)),
        (
            "2026:07:01 12:00:00.1234567891",
            (1_782_900_000, 123_456_789),
        ),
        // 29 March 2026, 02:00 → 03:00: 02:30 never happened there.
        ("2026:03:29 01:59:59", (1_774_745_999, 0)),
        ("2026:03:29 02:00:00", (1_774_746_000, 0)),
        ("2026:03:29 02:30:00", (1_774_751_400, 0)),
        ("2026:03:29 02:59:59", (1_774_753_199, 0)),
        ("2026:03:29 03:00:00", (1_774_746_000, 0)),
        // 25 October 2026, 03:00 → 02:00: 02:30 happened twice.
        ("2026:10:25 01:59:59", (1_792_886_399, 0)),
        ("2026:10:25 02:00:00", (1_792_890_000, 0)),
        ("2026:10:25 02:30:00", (1_792_891_800, 0)),
        ("2026:10:25 03:00:00", (1_792_893_600, 0)),
    ] {
        let read = parse_raw_creation_date(Some(local)).map(unix);
        assert_eq!(read, Some(instant), "{local}");
    }
    assert_eq!(parse_raw_creation_date(Some("0000:00:00 00:00:00")), None);
    assert_eq!(parse_raw_creation_date(Some("")), None);

    // An export names its file from the capture instant in local time.
    let settings: ExportSettings = serde_json::from_value(serde_json::json!({
        "jpegQuality": 90, "keepMetadata": true, "stripGps": false,
        "nameTemplate": "{date}_{time}_{weekday}",
    }))
    .unwrap();
    for (local, name) in [
        ("2026:03:29 02:30:00", "2026-03-29_04-30-00_Sunday"),
        ("2026:10:25 02:30:00", "2026-10-25_02-30-00_Sunday"),
        ("2026:12:31 23:59:59", "2026-12-31_23-59-59_Thursday"),
    ] {
        let date = parse_raw_creation_date(Some(local)).unwrap();
        let naming =
            settings.naming_context(std::path::Path::new("a.raf"), 1, 1, &date, "jpg", None);
        assert_eq!(
            rawtonic_naming::render(&settings.naming_template(), &naming).unwrap(),
            name,
            "{local}"
        );
    }
}
