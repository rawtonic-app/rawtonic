//! Local time as used for dates in the editor and CLI.

use jiff::{Timestamp, ZonedDateTime, tz::TimeZone};

#[cfg(any(windows, test))]
#[path = "windows_zones.rs"]
mod windows_zones;

pub fn now() -> ZonedDateTime {
    Timestamp::now().to_zoned_datetime(zone())
}

#[cfg(not(windows))]
pub fn zone() -> TimeZone {
    TimeZone::system()
}

/// Windows local time follows the system setting even when the process has a TZ override.
#[cfg(windows)]
pub fn zone() -> TimeZone {
    windows_zone().unwrap_or_else(TimeZone::unknown)
}

#[cfg(windows)]
fn windows_zone() -> Option<TimeZone> {
    use windows_sys::Win32::System::Time::{
        DYNAMIC_TIME_ZONE_INFORMATION, GetDynamicTimeZoneInformation, TIME_ZONE_ID_INVALID,
    };

    let mut info = DYNAMIC_TIME_ZONE_INFORMATION::default();
    // SAFETY: info is a writable DYNAMIC_TIME_ZONE_INFORMATION for the API to fill.
    if unsafe { GetDynamicTimeZoneInformation(&mut info) } == TIME_ZONE_ID_INVALID {
        return None;
    }
    let end = info.TimeZoneKeyName.iter().position(|&unit| unit == 0)?;
    let key = String::from_utf16(&info.TimeZoneKeyName[..end]).ok()?;
    TimeZone::get(windows_to_iana(&key)?).ok()
}

#[cfg(any(windows, test))]
fn windows_to_iana(key: &str) -> Option<&'static str> {
    windows_zones::WINDOWS_TO_IANA
        .iter()
        .find_map(|&(windows, iana)| windows.eq_ignore_ascii_case(key).then_some(iana))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_windows_zone_keys_to_iana() {
        assert_eq!(windows_to_iana("UTC"), Some("Etc/UTC"));
        assert_eq!(
            windows_to_iana("pacific standard time"),
            Some("America/Los_Angeles"),
        );
        assert_eq!(
            windows_to_iana("Nepal Standard Time"),
            Some("Asia/Katmandu"),
        );
        assert_eq!(windows_to_iana(""), None);
        assert_eq!(windows_to_iana("Asia/Katmandu"), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_local_zone_ignores_tz() {
        use std::{env, process::Command};

        const EXPECTED_ZONE: &str = "RAWTONIC_TEST_EXPECTED_WINDOWS_ZONE";
        let actual = zone();
        let actual = actual.iana_name().expect("recognized Windows time zone");

        if let Ok(expected) = env::var(EXPECTED_ZONE) {
            assert_eq!(actual, expected);
            let jiff_zone = TimeZone::try_system().unwrap();
            if let Ok(tz) = env::var("TZ") {
                assert_eq!(jiff_zone.iana_name(), Some(tz.as_str()));
                assert_ne!(jiff_zone.iana_name(), Some(actual));
            } else {
                assert_eq!(jiff_zone.iana_name(), Some(actual));
            }
            return;
        }

        // Give each probe a fresh Jiff cache and its own environment so other
        // tests can run concurrently without seeing the TZ override.
        let mut probe = Command::new(env::current_exe().unwrap());
        probe
            .args([
                "--exact",
                "local_time::tests::windows_local_zone_ignores_tz",
            ])
            .env(EXPECTED_ZONE, actual)
            .env_remove("TZ");

        let override_zone = if actual == "Pacific/Kiritimati" {
            "Pacific/Honolulu"
        } else {
            "Pacific/Kiritimati"
        };
        for tz in [None, Some(override_zone)] {
            if let Some(tz) = tz {
                probe.env("TZ", tz);
            }
            let output = probe.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "TZ={tz:?}: {stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
