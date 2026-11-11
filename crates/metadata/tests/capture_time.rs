//! The corrected capture time in XMP sidecars (`Metadata::capture`): a
//! correction writes `exif:DateTimeOriginal` and `photoshop:DateCreated`, a
//! reset removes exactly what it wrote, and without one the writer touches
//! neither. Checked on the real Lightroom Classic 15.6 and Capture One 16.6
//! sidecars of `real_sidecars.rs`.
use rawtonic_metadata::{
    CaptureWrite, Metadata, XmpDateTime, merge_xmp, read_capture_time, read_xmp,
};

const LIGHTROOM: &str = include_str!("fixtures/lightroom-classic-15.6-de.xmp");
const C1_KEYWORDS: &str = include_str!("fixtures/capture-one-16.6-keywords.xmp");
const C1_IPTC: &str = include_str!("fixtures/capture-one-16.6-iptc.xmp");

fn time(text: &str) -> XmpDateTime {
    XmpDateTime::parse(text).unwrap()
}

fn with(text: &str, capture: CaptureWrite) -> Metadata {
    Metadata {
        capture,
        ..read_xmp(text).unwrap().0
    }
}

fn count(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

#[test]
fn xmp_dates_keep_fraction_and_offset_and_invent_none() {
    for text in [
        "2026-10-30T14:05:09",
        "2026-10-30T14:05:09.66",
        "2026-10-30T14:05:09.660+02:00",
        "2026-10-30T14:05:09-05:30",
    ] {
        assert_eq!(time(text).to_xmp(), text);
    }
    // Minutes only: seconds zero. Capture One's `Z` after a local time
    // states no offset.
    assert_eq!(time("2026-10-30T14:05").to_xmp(), "2026-10-30T14:05:00");
    assert_eq!(time("2019-01-24T12:29:49Z").offset, None);
    for bad in [
        "2026-10-30",
        "2026-10-30T14",
        "2026-1-30T14:05:09",
        "2026-10-30T14:05:09.",
        "2026-10-30T14:05:09+2",
        "2026-10-30T14:05.5",
        "yesterday",
    ] {
        assert_eq!(XmpDateTime::parse(bad), None, "{bad}");
    }
    assert!(time("2026-10-30T14:05:09.50").same_moment(&time("2026-10-30T14:05:09.5+01:00")));
    assert!(!time("2026-10-30T14:05:09.5").same_moment(&time("2026-10-30T14:05:09.6")));
    assert!(!time("2026-10-30T14:05:10").same_moment(&time("2026-10-30T14:05:09")));
}

#[test]
fn a_correction_writes_both_dates_and_reads_back_as_written() {
    let corrected = time("2020-10-18T11:34:44.07+02:00");
    for sidecar in [LIGHTROOM, C1_KEYWORDS, C1_IPTC] {
        let written = merge_xmp(
            Some(sidecar),
            &with(sidecar, CaptureWrite::Set(corrected.clone())),
            None,
        )
        .unwrap();
        assert_eq!(
            read_capture_time(&written).unwrap(),
            Some(corrected.clone())
        );
        // One of each, the camera's and Capture One's values replaced.
        assert_eq!(count(&written, "DateTimeOriginal"), 2, "{written}");
        assert_eq!(count(&written, "DateCreated"), 2, "{written}");
        assert_eq!(
            read_xmp(&written).unwrap().0.iptc["dateCreated"],
            "2020-10-18T11:34:44.07+02:00"
        );
        // Written again: the same packet (a sync does not grow it).
        let again = merge_xmp(
            Some(&written),
            &with(&written, CaptureWrite::Set(corrected.clone())),
            None,
        )
        .unwrap();
        assert_eq!(again, written);
    }
    // A sidecar without one gets them, with their namespaces.
    let fresh = merge_xmp(
        None,
        &with(LIGHTROOM, CaptureWrite::Set(corrected.clone())),
        None,
    )
    .unwrap();
    assert_eq!(read_capture_time(&fresh).unwrap(), Some(corrected));
}

#[test]
fn a_reset_removes_exactly_the_dates_it_wrote() {
    let corrected = time("2020-10-18T11:34:44.07+02:00");
    // A sidecar never corrected (no IPTC Date Created in the library).
    let mut plain = with(C1_KEYWORDS, CaptureWrite::Keep);
    plain.iptc.remove("dateCreated");
    let before = merge_xmp(Some(C1_KEYWORDS), &plain, None).unwrap();
    let set = merge_xmp(
        Some(&before),
        &Metadata {
            capture: CaptureWrite::Set(corrected.clone()),
            ..plain.clone()
        },
        None,
    )
    .unwrap();
    let reset = merge_xmp(
        Some(&set),
        &Metadata {
            capture: CaptureWrite::Remove(corrected.clone()),
            ..plain.clone()
        },
        None,
    )
    .unwrap();
    assert_eq!(read_capture_time(&reset).unwrap(), None);
    assert!(!reset.contains("DateCreated"), "{reset}");
    // Then the writer leaves them alone again: the next write is the same.
    assert_eq!(merge_xmp(Some(&reset), &plain, None).unwrap(), reset);

    // A time written since by another editor is not ours to remove.
    let elsewhere = set.replace("2020-10-18T11:34:44.07+02:00", "2020-10-18T09:00:00");
    let kept = merge_xmp(
        Some(&elsewhere),
        &Metadata {
            capture: CaptureWrite::Remove(corrected.clone()),
            ..plain.clone()
        },
        None,
    )
    .unwrap();
    assert_eq!(kept, merge_xmp(Some(&elsewhere), &plain, None).unwrap());
    assert_eq!(
        read_capture_time(&kept).unwrap(),
        Some(time("2020-10-18T09:00:00"))
    );

    // An IPTC Date Created the library holds comes back as any IPTC field.
    let lightroom = with(LIGHTROOM, CaptureWrite::Keep);
    let set = merge_xmp(
        Some(LIGHTROOM),
        &Metadata {
            capture: CaptureWrite::Set(corrected.clone()),
            ..lightroom.clone()
        },
        None,
    )
    .unwrap();
    let reset = merge_xmp(
        Some(&set),
        &Metadata {
            capture: CaptureWrite::Remove(corrected),
            ..lightroom
        },
        None,
    )
    .unwrap();
    assert_eq!(read_capture_time(&reset).unwrap(), None);
    assert_eq!(
        read_xmp(&reset).unwrap().0.iptc["dateCreated"],
        "2026-05-01T10:20:30"
    );
}

#[test]
fn without_a_correction_the_dates_stay_byte_for_byte() {
    for sidecar in [LIGHTROOM, C1_KEYWORDS, C1_IPTC] {
        let plain = with(sidecar, CaptureWrite::Keep);
        let written = merge_xmp(Some(sidecar), &plain, None).unwrap();
        // The camera's attribute or element as the editor wrote it.
        let original = read_capture_time(sidecar).unwrap().unwrap();
        assert_eq!(read_capture_time(&written).unwrap(), Some(original));
        for line in sidecar.lines().filter(|l| l.contains("DateTimeOriginal")) {
            assert!(written.contains(line.trim()), "{line}");
        }
        // A reset of a time the sidecar does not hold changes nothing.
        let other = time("1999-01-01T00:00:00");
        assert_eq!(
            merge_xmp(
                Some(sidecar),
                &with(sidecar, CaptureWrite::Remove(other)),
                None
            )
            .unwrap(),
            written
        );
    }
    assert_eq!(
        read_capture_time(C1_IPTC).unwrap(),
        Some(time("2026-05-01T10:20:30"))
    );
}
