//! Corrected capture times (Shift Capture Time): a camera clock set wrong
//! is corrected by one signed shift of the selected photographs, so a
//! multi-camera job sorts, finds, names and exports in shooting order.
//!
//! The correction is the library's own record (`capture_overrides`, schema
//! 19), kept apart from what the file says (`assets.capture_time`, which the
//! facts pass may write again): rereading a file never undoes a correction,
//! and removing it (Reset) gives back the file's time. No file is read or
//! written here; camera files and their file times are never changed.
//!
//! Arithmetic is exact: a time is the camera's local wall time with whole
//! seconds, the fraction digits it wrote (EXIF SubSecTimeOriginal) and its
//! UTC offset (EXIF OffsetTimeOriginal). A shift moves the wall time by
//! calendar arithmetic (no time zone or daylight-saving conversion), keeps
//! every fraction digit and the offset as written, and invents none.
use crate::Library;
use anyhow::{Result, bail};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// The effective capture time of asset `a` in SQL: its correction, else
/// what its file says (search, value lists).
pub(crate) const EFFECTIVE: &str =
    "coalesce((SELECT o.captured FROM capture_overrides o WHERE o.asset_id=a.id), a.capture_time)";

/// The form of `local` (and of `assets.capture_time`).
const LOCAL_FORMAT: &str = "%Y-%m-%d %H:%M:%S";
const NANOS: i128 = 1_000_000_000;
/// The most fraction digits kept (nanoseconds).
const MAX_DIGITS: usize = 9;

/// A capture time as a camera writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureTime {
    /// Local wall time, whole seconds, `YYYY-MM-DD HH:MM:SS`.
    pub local: String,
    /// Fraction of the second as written, digits only (`"05"` is 50 ms).
    #[serde(default)]
    pub subsec: Option<String>,
    /// UTC offset as written (`+02:00`); never derived.
    #[serde(default)]
    pub offset: Option<String>,
}

/// Why a time or shift cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TimeError {
    /// Not a date and time (or an impossible one: 30 February).
    Invalid,
    /// Fraction digits other than 0–9, or more than nine.
    Fraction,
    /// The result would leave the years 0001–9999 EXIF can hold.
    OutOfRange,
}

impl std::fmt::Display for TimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TimeError::Invalid => "not a valid date and time",
            TimeError::Fraction => "fraction of a second must be at most nine digits",
            TimeError::OutOfRange => "outside the years 0001 to 9999",
        })
    }
}

impl std::error::Error for TimeError {}

/// Digits of a fraction as written: trimmed, 1–9 ASCII digits; empty is none.
fn fraction(text: &str) -> Result<Option<String>, TimeError> {
    let digits = text.trim().trim_end_matches('\0').trim();
    if digits.is_empty() {
        return Ok(None);
    }
    if digits.len() > MAX_DIGITS || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(TimeError::Fraction);
    }
    Ok(Some(digits.to_string()))
}

/// Nanoseconds of fraction `digits`.
fn fraction_nanos(digits: &str) -> i128 {
    let value: i128 = digits.parse().unwrap_or(0);
    value * 10i128.pow((MAX_DIGITS - digits.len()) as u32)
}

fn parse_local(text: &str) -> Result<NaiveDateTime, TimeError> {
    let text = text.trim();
    for format in [LOCAL_FORMAT, "%Y:%m:%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(time) = NaiveDateTime::parse_from_str(text, format) {
            return Ok(time);
        }
    }
    Err(TimeError::Invalid)
}

fn in_range(time: &NaiveDateTime) -> bool {
    use chrono::Datelike;
    (1..=9999).contains(&time.year())
}

/// Seconds since 0001-01-01 00:00:00 (proleptic Gregorian, no time zone).
fn epoch() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(1, 1, 1)
        .unwrap()
        .and_time(NaiveTime::MIN)
}

impl CaptureTime {
    /// A time from a camera's fields: `local` in EXIF (`2026:10:30 14:05:09`)
    /// or library form, `subsec` and `offset` as written (empty: absent).
    pub fn new(local: &str, subsec: Option<&str>, offset: Option<&str>) -> Result<Self, TimeError> {
        let time = parse_local(local)?;
        ensure_range(&time)?;
        Ok(Self {
            local: time.format(LOCAL_FORMAT).to_string(),
            subsec: subsec.map(fraction).transpose()?.flatten(),
            offset: offset
                .map(|o| o.trim().trim_end_matches('\0').trim().to_string())
                .filter(|o| !o.is_empty()),
        })
    }

    /// This time checked: what `new` accepts, in its normal form.
    pub fn validated(&self) -> Result<Self, TimeError> {
        Self::new(&self.local, self.subsec.as_deref(), self.offset.as_deref())
    }

    fn wall(&self) -> Result<NaiveDateTime, TimeError> {
        parse_local(&self.local)
    }

    /// Nanoseconds since 0001-01-01 (wall time), for exact differences.
    fn nanos(&self) -> Result<i128, TimeError> {
        let seconds = (self.wall()? - epoch()).num_seconds() as i128;
        Ok(seconds * NANOS + self.subsec.as_deref().map_or(0, fraction_nanos))
    }

    /// This time moved by `shift`: the wall time by calendar arithmetic, the
    /// fraction with as many digits as it or the shift has (none when
    /// neither has one), the offset as it was.
    pub fn shifted(&self, shift: &Shift) -> Result<Self, TimeError> {
        let own = self.subsec.as_deref().map_or(0, str::len);
        let digits = own.max(shift.digits);
        let total = self.nanos()? + shift.nanos;
        let seconds = total.div_euclid(NANOS);
        let nanos = total.rem_euclid(NANOS);
        let seconds = i64::try_from(seconds).map_err(|_| TimeError::OutOfRange)?;
        let wall = epoch()
            .checked_add_signed(
                chrono::TimeDelta::try_seconds(seconds).ok_or(TimeError::OutOfRange)?,
            )
            .ok_or(TimeError::OutOfRange)?;
        ensure_range(&wall)?;
        let subsec = (digits > 0).then(|| {
            let scaled = nanos / 10i128.pow((MAX_DIGITS - digits) as u32);
            format!("{scaled:0digits$}")
        });
        Ok(Self {
            local: wall.format(LOCAL_FORMAT).to_string(),
            subsec,
            offset: self.offset.clone(),
        })
    }

    /// The shift that moves this time to `corrected` (wall time, with an
    /// optional fraction). Without a fraction the corrected time keeps this
    /// one's: setting the clock to the second does not move the shots
    /// within it.
    pub fn shift_to(&self, corrected: &str) -> Result<Shift, TimeError> {
        let (whole, fraction_text) = match corrected.trim().split_once('.') {
            Some((whole, digits)) => (whole, Some(digits)),
            None => (corrected.trim(), None),
        };
        // The date and time first: "30.10.2026" is no date, not a bad fraction.
        CaptureTime::new(whole, None, None)?;
        let target_fraction = match fraction_text {
            Some(digits) => Some(fraction(digits)?.ok_or(TimeError::Fraction)?),
            None => self.subsec.clone(),
        };
        let target = CaptureTime::new(whole, target_fraction.as_deref(), None)?;
        let nanos = target.nanos()? - self.nanos()?;
        let digits = target_fraction.as_deref().map_or(0, str::len);
        Ok(Shift::exact(nanos, digits))
    }

    /// The fields of a photograph's EXIF map (`exif` of its editor state)
    /// this time sets: `DateTimeOriginal` in the map's form, and
    /// `SubSecTimeOriginal`/`OffsetTimeOriginal` (`None`: removed).
    pub fn exif_fields(&self) -> [(&'static str, Option<String>); 3] {
        [
            ("DateTimeOriginal", Some(self.local.clone())),
            ("SubSecTimeOriginal", self.subsec.clone()),
            ("OffsetTimeOriginal", self.offset.clone()),
        ]
    }
}

fn ensure_range(time: &NaiveDateTime) -> Result<(), TimeError> {
    if in_range(time) {
        Ok(())
    } else {
        Err(TimeError::OutOfRange)
    }
}

/// A signed shift, exact to the nanosecond, with the number of fraction
/// digits it was given with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shift {
    /// Signed nanoseconds (as a string in JSON: beyond JavaScript's exact
    /// integers).
    #[serde(with = "nanos_text")]
    pub nanos: i128,
    /// Fraction digits it carries (0–9).
    pub digits: usize,
}

mod nanos_text {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &i128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i128, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// The parts of a shift as a person enters them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ShiftParts {
    /// Earlier instead of later.
    pub negative: bool,
    pub days: String,
    pub hours: String,
    pub minutes: String,
    /// Seconds, with an optional fraction (`.` or `,`).
    pub seconds: String,
}

/// Which part of a shift cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShiftField {
    Days,
    Hours,
    Minutes,
    Seconds,
}

/// More than any EXIF year range: keeps `nanos` far from overflow.
const MOST_DAYS: i128 = 3_660_000;

impl Shift {
    fn exact(nanos: i128, digits: usize) -> Self {
        // Fewest digits that still hold the shift exactly.
        let mut digits = digits.min(MAX_DIGITS);
        while digits > 0 && nanos % 10i128.pow((MAX_DIGITS - digits + 1) as u32) == 0 {
            digits -= 1;
        }
        Self { nanos, digits }
    }

    /// A shift from its parts: whole non-negative days, hours, minutes and
    /// seconds (any size: 90 minutes is fine), the seconds with up to nine
    /// fraction digits; empty parts are zero.
    pub fn from_parts(parts: &ShiftParts) -> Result<Self, ShiftField> {
        let whole = |text: &str, field| -> Result<i128, ShiftField> {
            let text = text.trim();
            if text.is_empty() {
                return Ok(0);
            }
            if text.len() > 9 || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(field);
            }
            text.parse().map_err(|_| field)
        };
        let days = whole(&parts.days, ShiftField::Days)?;
        let hours = whole(&parts.hours, ShiftField::Hours)?;
        let minutes = whole(&parts.minutes, ShiftField::Minutes)?;
        let seconds_text = parts.seconds.trim().replace(',', ".");
        let (seconds, digits) = match seconds_text.split_once('.') {
            Some((whole_part, fraction_part)) => {
                let digits = fraction(fraction_part)
                    .map_err(|_| ShiftField::Seconds)?
                    .ok_or(ShiftField::Seconds)?;
                (
                    whole(whole_part, ShiftField::Seconds)? * NANOS + fraction_nanos(&digits),
                    digits.len(),
                )
            }
            None => (whole(&seconds_text, ShiftField::Seconds)? * NANOS, 0),
        };
        let nanos = ((days * 24 + hours) * 60 + minutes) * 60 * NANOS + seconds;
        if nanos > MOST_DAYS * 86_400 * NANOS {
            return Err(if days > 0 {
                ShiftField::Days
            } else {
                ShiftField::Hours
            });
        }
        Ok(Self::exact(
            if parts.negative { -nanos } else { nanos },
            digits,
        ))
    }

    /// The parts of this shift (largest units first, days unbounded).
    pub fn parts(&self) -> ShiftParts {
        let magnitude = self.nanos.abs();
        let seconds = magnitude / NANOS;
        let fraction = magnitude % NANOS;
        let mut seconds_text = (seconds % 60).to_string();
        if self.digits > 0 {
            let scaled = fraction / 10i128.pow((MAX_DIGITS - self.digits) as u32);
            seconds_text = format!("{seconds_text}.{scaled:0width$}", width = self.digits);
        }
        ShiftParts {
            negative: self.nanos < 0,
            days: (seconds / 86_400).to_string(),
            hours: (seconds / 3600 % 24).to_string(),
            minutes: (seconds / 60 % 60).to_string(),
            seconds: seconds_text,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.nanos == 0
    }
}

/// One photograph's correction to write: from `expected` (its correction
/// when the change was planned; `None`: it had none) to `new` (`None`:
/// removed, its file's time again).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureChange {
    pub asset_id: String,
    pub expected: Option<CaptureTime>,
    pub new: Option<CaptureTime>,
}

/// Sets `time`'s fields in an editor state's EXIF map, if it has one.
pub fn overlay(state: &mut Value, time: &CaptureTime) {
    let Some(exif) = state.get_mut("exif").and_then(Value::as_object_mut) else {
        return;
    };
    for (key, value) in time.exif_fields() {
        match value {
            Some(value) => {
                exif.insert(key.into(), Value::String(value));
            }
            None => {
                exif.remove(key);
            }
        }
    }
}

impl Library {
    /// Asset `id`'s corrected capture time, if it has one.
    pub fn capture_override(&self, id: &str) -> Result<Option<CaptureTime>> {
        Ok(self
            .conn
            .query_row(
                "SELECT captured,subsec,offset FROM capture_overrides WHERE asset_id=?",
                [id],
                |r| {
                    Ok(CaptureTime {
                        local: r.get(0)?,
                        subsec: r.get(1)?,
                        offset: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// The corrected capture times of those of `ids` that have one.
    pub fn capture_overrides(&self, ids: &[String]) -> Result<HashMap<String, CaptureTime>> {
        let mut query = self
            .conn
            .prepare_cached("SELECT captured,subsec,offset FROM capture_overrides WHERE asset_id=?")?;
        let mut found = HashMap::new();
        for id in ids {
            let time = query
                .query_row([id], |r| {
                    Ok(CaptureTime {
                        local: r.get(0)?,
                        subsec: r.get(1)?,
                        offset: r.get(2)?,
                    })
                })
                .optional()?;
            if let Some(time) = time {
                found.insert(id.clone(), time);
            }
        }
        Ok(found)
    }

    /// Writes `changes` in one transaction, all or none: when any
    /// photograph is gone or its correction is no longer the one expected
    /// (changed since the change was planned), nothing is written and those
    /// photographs are returned. Returns no conflicts once written.
    pub fn set_capture_times(&self, changes: &[CaptureChange]) -> Result<Vec<String>> {
        let mut checked = Vec::with_capacity(changes.len());
        for change in changes {
            let new = match &change.new {
                Some(time) => Some(time.validated()?),
                None => None,
            };
            checked.push((change, new));
        }
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut conflicts = Vec::new();
        for (change, _) in &checked {
            let exists: bool = tx
                .query_row("SELECT 1 FROM assets WHERE id=?", [&change.asset_id], |_| {
                    Ok(true)
                })
                .optional()?
                .unwrap_or(false);
            let current = self.capture_override(&change.asset_id)?;
            if !exists || current != change.expected {
                conflicts.push(change.asset_id.clone());
            }
        }
        if !conflicts.is_empty() {
            return Ok(conflicts);
        }
        for (change, new) in checked {
            match new {
                Some(time) => tx.execute(
                    "INSERT INTO capture_overrides(asset_id,captured,subsec,offset) VALUES(?,?,?,?)
                     ON CONFLICT(asset_id) DO UPDATE SET captured=excluded.captured,
                        subsec=excluded.subsec,offset=excluded.offset",
                    params![change.asset_id, time.local, time.subsec, time.offset],
                )?,
                None => tx.execute(
                    "DELETE FROM capture_overrides WHERE asset_id=?",
                    [&change.asset_id],
                )?,
            };
        }
        tx.commit()?;
        Ok(Vec::new())
    }
}

/// A `CaptureTime` from its parts (tests and callers that build one).
pub fn time(local: &str, subsec: Option<&str>, offset: Option<&str>) -> Result<CaptureTime> {
    match CaptureTime::new(local, subsec, offset) {
        Ok(time) => Ok(time),
        Err(e) => bail!("{local:?}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(local: &str, subsec: Option<&str>) -> CaptureTime {
        CaptureTime::new(local, subsec, Some("+02:00")).unwrap()
    }

    fn parts(negative: bool, d: &str, h: &str, m: &str, s: &str) -> ShiftParts {
        ShiftParts {
            negative,
            days: d.into(),
            hours: h.into(),
            minutes: m.into(),
            seconds: s.into(),
        }
    }

    fn shift(negative: bool, d: &str, h: &str, m: &str, s: &str) -> Shift {
        Shift::from_parts(&parts(negative, d, h, m, s)).unwrap()
    }

    #[test]
    fn a_shift_keeps_fraction_digits_and_offset() {
        let t = at("2026:10:30 23:59:59", Some("050"));
        let moved = t.shifted(&shift(false, "", "", "", "1")).unwrap();
        assert_eq!(moved.local, "2026-10-31 00:00:00");
        assert_eq!(moved.subsec.as_deref(), Some("050"));
        assert_eq!(moved.offset.as_deref(), Some("+02:00"));
        // And back exactly.
        assert_eq!(moved.shifted(&shift(true, "", "", "", "1")).unwrap(), t);
    }

    #[test]
    fn a_fractional_shift_adds_only_the_digits_it_needs() {
        let t = at("2026-10-30 12:00:00", Some("5"));
        let moved = t.shifted(&shift(false, "", "", "", "0.75")).unwrap();
        assert_eq!(moved.local, "2026-10-30 12:00:01");
        assert_eq!(moved.subsec.as_deref(), Some("25"));
        // A time without a fraction gains none from a whole shift...
        let plain = CaptureTime::new("2026-10-30 12:00:00", None, None).unwrap();
        let moved = plain.shifted(&shift(false, "", "1", "", "")).unwrap();
        assert_eq!(moved.subsec, None);
        assert_eq!(moved.offset, None);
        // ...and only the shift's own digits from a fractional one.
        let moved = plain.shifted(&shift(true, "", "", "", "0.5")).unwrap();
        assert_eq!(moved.local, "2026-10-30 11:59:59");
        assert_eq!(moved.subsec.as_deref(), Some("5"));
    }

    #[test]
    fn calendar_boundaries_are_plain_arithmetic() {
        let leap = at("2024-02-28 23:30:00", None);
        assert_eq!(
            leap.shifted(&shift(false, "", "1", "", "")).unwrap().local,
            "2024-02-29 00:30:00"
        );
        assert_eq!(
            leap.shifted(&shift(false, "1", "1", "", "")).unwrap().local,
            "2024-03-01 00:30:00"
        );
        let new_year = at("2025-12-31 23:00:00", Some("999"));
        let moved = new_year.shifted(&shift(false, "", "", "60", "")).unwrap();
        assert_eq!(
            (moved.local.as_str(), moved.subsec.as_deref()),
            ("2026-01-01 00:00:00", Some("999"))
        );
        // A daylight-saving night is not special: wall time moves by the shift.
        let dst = at("2026-03-29 01:59:59", None);
        assert_eq!(
            dst.shifted(&shift(false, "", "", "", "1")).unwrap().local,
            "2026-03-29 02:00:00"
        );
        assert_eq!(
            at("2026-03-01 00:00:00", None)
                .shifted(&shift(true, "1", "", "", ""))
                .unwrap()
                .local,
            "2026-02-28 00:00:00"
        );
    }

    #[test]
    fn relative_intervals_survive_any_shift() {
        let burst = [
            at("2026-10-30 12:00:00", Some("01")),
            at("2026-10-30 12:00:00", Some("34")),
            at("2026-10-30 12:00:01", Some("02")),
        ];
        for s in [
            shift(false, "", "", "", "0.99"),
            shift(true, "400", "23", "59", "59.999999999"),
            shift(false, "", "", "", "3601.5"),
        ] {
            let moved: Vec<_> = burst.iter().map(|t| t.shifted(&s).unwrap()).collect();
            for i in 1..burst.len() {
                assert_eq!(
                    moved[i].nanos().unwrap() - moved[i - 1].nanos().unwrap(),
                    burst[i].nanos().unwrap() - burst[i - 1].nanos().unwrap()
                );
            }
        }
    }

    proptest::proptest! {
        /// Any shift and its opposite give back the exact time, and any two
        /// shots keep their exact interval.
        #[test]
        fn shifts_are_exact_and_reversible(
            seconds_a in 0i64..(3000 * 365 * 86_400),
            seconds_b in 0i64..(3000 * 365 * 86_400),
            fraction_a in proptest::option::of("[0-9]{1,6}"),
            fraction_b in proptest::option::of("[0-9]{1,3}"),
            shift_nanos in -(500i128 * 365 * 86_400 * NANOS)..(500i128 * 365 * 86_400 * NANOS),
            digits in 0usize..=9,
        ) {
            let base = NaiveDate::from_ymd_opt(1000, 1, 1).unwrap().and_time(NaiveTime::MIN);
            let at = |seconds: i64, fraction: &Option<String>| CaptureTime::new(
                &(base + chrono::TimeDelta::seconds(seconds)).format(LOCAL_FORMAT).to_string(),
                fraction.as_deref(),
                Some("-03:30"),
            ).unwrap();
            let (a, b) = (at(seconds_a, &fraction_a), at(seconds_b, &fraction_b));
            let unit = 10i128.pow((MAX_DIGITS - digits) as u32);
            let shift = Shift::exact(shift_nanos / unit * unit, digits);
            let back = Shift { nanos: -shift.nanos, digits: shift.digits };
            let (moved_a, moved_b) = (a.shifted(&shift).unwrap(), b.shifted(&shift).unwrap());
            proptest::prop_assert_eq!(moved_b.nanos().unwrap() - moved_a.nanos().unwrap(),
                b.nanos().unwrap() - a.nanos().unwrap());
            proptest::prop_assert_eq!(moved_a.nanos().unwrap() - a.nanos().unwrap(), shift.nanos);
            proptest::prop_assert_eq!(moved_a.offset.as_deref(), Some("-03:30"));
            let returned = moved_a.shifted(&back).unwrap();
            proptest::prop_assert_eq!(returned.nanos().unwrap(), a.nanos().unwrap());
            proptest::prop_assert_eq!(returned.local, a.local);
            // Every fraction digit written is kept (padded to the shift's digits at most).
            if let Some(fraction) = &a.subsec {
                let kept = returned.subsec.clone().unwrap();
                proptest::prop_assert!(kept.starts_with(fraction.as_str()));
                proptest::prop_assert!(kept[fraction.len()..].bytes().all(|d| d == b'0'));
            }
        }
    }

    #[test]
    fn results_outside_exif_years_are_refused() {
        let early = at("0001-01-01 00:00:00", None);
        assert_eq!(
            early.shifted(&shift(true, "", "", "", "1")),
            Err(TimeError::OutOfRange)
        );
        let late = at("9999-12-31 23:59:59", Some("9"));
        assert_eq!(
            late.shifted(&shift(false, "", "", "", "0.1")),
            Err(TimeError::OutOfRange)
        );
    }

    #[test]
    fn malformed_parts_name_their_field() {
        for (p, field) in [
            (parts(false, "-1", "", "", ""), ShiftField::Days),
            (parts(false, "", "1.5", "", ""), ShiftField::Hours),
            (parts(false, "", "", "x", ""), ShiftField::Minutes),
            (parts(false, "", "", "", "1.1234567891"), ShiftField::Seconds),
            (parts(false, "", "", "", "1."), ShiftField::Seconds),
            (parts(false, "999999999", "", "", ""), ShiftField::Days),
        ] {
            assert_eq!(Shift::from_parts(&p), Err(field), "{p:?}");
        }
        assert!(shift(false, "", "", "", "").is_zero());
        assert_eq!(shift(false, "", "", "", "1,5").nanos, 1_500_000_000);
    }

    #[test]
    fn parts_round_trip() {
        let s = shift(true, "2", "25", "61", "7.250");
        assert_eq!(s.digits, 2);
        assert_eq!(s.parts(), parts(true, "3", "2", "1", "7.25"));
        assert_eq!(Shift::from_parts(&s.parts()).unwrap(), s);
    }

    #[test]
    fn a_reference_correction_gives_the_shift_between() {
        let reference = at("2026-10-30 12:00:00", Some("345"));
        // To the second: the fraction stays.
        let s = reference.shift_to("2026-10-30 13:02:03").unwrap();
        assert_eq!(s.nanos, 3723 * NANOS);
        assert_eq!(s.digits, 0);
        assert_eq!(reference.shifted(&s).unwrap().subsec.as_deref(), Some("345"));
        // With a fraction: exactly that.
        let s = reference.shift_to("2026-10-30 11:59:59.5").unwrap();
        assert_eq!(s.nanos, -845_000_000);
        let moved = reference.shifted(&s).unwrap();
        assert_eq!(
            (moved.local.as_str(), moved.subsec.as_deref()),
            ("2026-10-30 11:59:59", Some("500"))
        );
        assert_eq!(reference.shift_to("2026-02-30 00:00:00"), Err(TimeError::Invalid));
    }

    #[test]
    fn camera_fields_are_read_as_written() {
        let t = CaptureTime::new("2026:10:30 14:05:09", Some(" 07\0"), Some("-05:00 ")).unwrap();
        assert_eq!(t.local, "2026-10-30 14:05:09");
        assert_eq!(t.subsec.as_deref(), Some("07"));
        assert_eq!(t.offset.as_deref(), Some("-05:00"));
        assert_eq!(
            CaptureTime::new("2026:10:30 14:05:09", Some("1x"), None),
            Err(TimeError::Fraction)
        );
        assert_eq!(
            CaptureTime::new("0000:00:00 00:00:00", None, None),
            Err(TimeError::Invalid)
        );
        assert_eq!(CaptureTime::new("2026:10:30 14:05:09", Some(""), Some("")).unwrap().subsec, None);
    }

    #[test]
    fn overlay_sets_only_a_map_that_is_there() {
        let t = CaptureTime::new("2026-10-30 14:05:09", None, Some("+01:00")).unwrap();
        let mut state = serde_json::json!({"exif": {"Make": "X", "SubSecTimeOriginal": "12"}});
        overlay(&mut state, &t);
        assert_eq!(
            state["exif"],
            serde_json::json!({"Make": "X", "DateTimeOriginal": "2026-10-30 14:05:09", "OffsetTimeOriginal": "+01:00"})
        );
        let mut bare = serde_json::json!({"rating": 1});
        overlay(&mut bare, &t);
        assert_eq!(bare, serde_json::json!({"rating": 1}));
    }
}
