// Rawtonic patch (DEPC1): pins `RawMetadata::last_modified` before rawler's
// date library changed from chrono to jiff. One test in its own binary, so
// setting `TZ` races with nothing.
#![cfg(unix)]

use rawler::decoders::RawMetadata;
use std::time::UNIX_EPOCH;

fn modified(date: &str, hours: Option<i16>, offset: Option<&str>) -> Option<Result<u64, ()>> {
  let mut metadata = RawMetadata::default();
  metadata.exif.modify_date = Some(date.to_string());
  metadata.exif.timezone_offset = hours.map(|h| vec![0, h]);
  metadata.exif.offset_time = offset.map(str::to_string);
  match metadata.last_modified() {
    Ok(Some(time)) => Some(Ok(time.duration_since(UNIX_EPOCH).unwrap().as_secs())),
    Ok(None) => None,
    Err(_) => Some(Err(())),
  }
}

#[test]
fn modification_dates_become_instants() {
  // SAFETY: the only test of this binary, set before any time is read.
  unsafe { std::env::set_var("TZ", "Europe/Zurich") };
  let summer = 1_782_900_000; // 2026-07-01 10:00:00 UTC
  // The TimeZoneOffset tag (hours), then OffsetTime, then the computer's zone.
  assert_eq!(modified("2026:07:01 12:00:00", Some(2), Some("-05:00")), Some(Ok(summer)));
  assert_eq!(modified("2026:07:01 12:00:00", Some(-3), None), Some(Ok(summer + 5 * 3600)));
  for (offset, seconds) in [
    ("+02:00", summer),
    ("+0200", summer),
    ("+02 00", summer),
    ("+02:00 ", summer),
    ("+02:00:00", summer),
    ("-03:30", summer + 5 * 3600 + 1800),
    ("+14:00", summer - 12 * 3600),
    ("\u{2212}02:00", summer + 4 * 3600),
  ] {
    assert_eq!(modified("2026:07:01 12:00:00", None, Some(offset)), Some(Ok(seconds)), "{offset:?}");
  }
  // An offset that is none: the computer's zone (summer time, +02:00).
  for offset in ["+05", "05:00", "+5:00", "Z", "+24:00", " +05:00", ""] {
    assert_eq!(modified("2026:07:01 12:00:00", None, Some(offset)), Some(Ok(summer)), "{offset:?}");
  }
  assert_eq!(modified("2026:07:01 12:00:00", None, None), Some(Ok(summer)));
  assert_eq!(modified("2026:01:15 12:00:00", None, None), Some(Ok(1_768_474_800)));
  // Daylight saving: the fold reads as winter time, the gap is no time.
  assert_eq!(modified("2026:10:25 02:30:00", None, None), Some(Ok(1_792_891_800)));
  assert_eq!(modified("2026:03:29 02:00:00", None, None), Some(Ok(1_774_746_000)));
  assert_eq!(modified("2026:03:29 02:30:00", None, None), None);
  // Not a date: an error; the form is EXIF's only.
  assert_eq!(modified("2026-07-01 12:00:00", None, None), Some(Err(())));
  assert_eq!(modified("2026:02:30 12:00:00", None, None), Some(Err(())));
  assert_eq!(modified("2026:7:1 9:5:2", None, Some("+00:00")), Some(Ok(1_782_896_702)));
}
