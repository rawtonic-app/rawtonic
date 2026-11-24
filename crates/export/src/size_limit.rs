//! "Limit file size to … KB" for JPEG exports: the highest quality whose
//! whole file (metadata, ICC profile and all) fits the limit, found by
//! binary search over the quality scale.

use serde::{Deserialize, Serialize};

/// One kilobyte of the limit: 1000 bytes, as the upload limits of agencies
/// and websites count it.
pub const BYTES_PER_KB: u64 = 1000;

/// The most encodes a search takes: the 100 qualities halve in 7 steps.
pub const MAX_ENCODES: u32 = 7;

/// A file that stayed above its size limit at the lowest quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeLimitMiss {
    pub limit_bytes: u64,
    /// The size of the smallest file, which was written.
    pub bytes: u64,
}

impl SizeLimitMiss {
    /// The notice the export result shows (English, as the other log
    /// texts; the app words it from the numbers).
    pub fn describe(&self) -> String {
        format!(
            "File size limit {} KB not reachable: the smallest file is {} KB",
            self.limit_bytes.div_ceil(BYTES_PER_KB),
            self.bytes.div_ceil(BYTES_PER_KB)
        )
    }
}

/// The result of a search.
#[derive(Debug)]
pub struct Fit {
    pub quality: u8,
    pub bytes: Vec<u8>,
    /// Whether `bytes` fits the limit; false: the lowest quality's file.
    pub met: bool,
    pub encodes: u32,
}

impl Fit {
    pub fn miss(&self, limit_bytes: u64) -> Option<SizeLimitMiss> {
        (!self.met).then_some(SizeLimitMiss {
            limit_bytes,
            bytes: self.bytes.len() as u64,
        })
    }
}

/// The file limit of `kilobytes` in bytes; 0 means no limit.
pub fn limit_bytes(kilobytes: Option<u32>) -> Option<u64> {
    kilobytes
        .filter(|&kb| kb > 0)
        .map(|kb| u64::from(kb) * BYTES_PER_KB)
}

/// The highest quality in 1..=100 whose file (`encode(quality)`, the whole
/// file) has at most `limit` bytes. Takes at most [`MAX_ENCODES`] encodes.
/// When even quality 1 is above the limit the quality 1 file is returned
/// with `met` false: never an error and never a smaller picture.
pub fn fit_to_limit(
    limit: u64,
    mut encode: impl FnMut(u8) -> Result<Vec<u8>, String>,
) -> Result<Fit, String> {
    let (mut low, mut high) = (1u8, 100u8);
    let mut best: Option<(u8, Vec<u8>)> = None;
    let mut smallest: Option<(u8, Vec<u8>)> = None;
    let mut encodes = 0;
    while low <= high {
        let quality = low + (high - low) / 2;
        let bytes = encode(quality)?;
        encodes += 1;
        if bytes.len() as u64 <= limit {
            best = Some((quality, bytes));
            low = quality + 1;
        } else {
            // A tie goes to the lower quality (the search only descends).
            if smallest.as_ref().is_none_or(|(_, s)| bytes.len() <= s.len()) {
                smallest = Some((quality, bytes));
            }
            let Some(next) = quality.checked_sub(1) else {
                break;
            };
            high = next;
        }
    }
    Ok(match (best, smallest) {
        (Some((quality, bytes)), _) => Fit {
            quality,
            bytes,
            met: true,
            encodes,
        },
        (None, Some((quality, bytes))) => Fit {
            quality,
            bytes,
            met: false,
            encodes,
        },
        (None, None) => unreachable!("the search encodes at least once"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file of 2000 bytes of headers plus 100 bytes per quality step.
    fn fake(quality: u8) -> Result<Vec<u8>, String> {
        Ok(vec![0; 2000 + 100 * quality as usize])
    }

    #[test]
    fn the_highest_quality_that_fits_wins() {
        // 2000 + 100 q <= 6500  ->  q = 45
        let fit = fit_to_limit(6500, fake).unwrap();
        assert!(fit.met);
        assert_eq!(fit.quality, 45);
        assert_eq!(fit.bytes.len(), 6500);
        assert!(fit.encodes <= MAX_ENCODES, "{} encodes", fit.encodes);
    }

    #[test]
    fn every_limit_takes_at_most_seven_encodes_and_is_exact() {
        for limit in (0..13_000).step_by(37) {
            let mut calls = 0;
            let fit = fit_to_limit(limit, |q| {
                calls += 1;
                fake(q)
            })
            .unwrap();
            assert!(calls <= MAX_ENCODES, "limit {limit}: {calls} encodes");
            let expected = (1..=100u8).rev().find(|&q| 2000 + 100 * u64::from(q) <= limit);
            match expected {
                Some(q) => assert!(fit.met && fit.quality == q, "limit {limit}"),
                None => assert!(!fit.met && fit.quality == 1, "limit {limit}"),
            }
        }
    }

    #[test]
    fn a_limit_above_the_best_file_gives_quality_100() {
        let fit = fit_to_limit(1_000_000, fake).unwrap();
        assert!(fit.met);
        assert_eq!(fit.quality, 100);
    }

    #[test]
    fn an_impossible_limit_gives_the_smallest_file_and_a_notice() {
        let fit = fit_to_limit(1000, fake).unwrap();
        assert!(!fit.met);
        assert_eq!(fit.quality, 1);
        assert_eq!(fit.bytes.len(), 2100);
        let miss = fit.miss(1000).unwrap();
        assert_eq!((miss.limit_bytes, miss.bytes), (1000, 2100));
        assert_eq!(
            miss.describe(),
            "File size limit 1 KB not reachable: the smallest file is 3 KB"
        );
        assert!(fit_to_limit(100_000, fake).unwrap().miss(100_000).is_none());
    }

    #[test]
    fn an_encode_error_fails_the_search() {
        assert_eq!(
            fit_to_limit(5000, |_| Err("boom".into())).unwrap_err(),
            "boom"
        );
    }

    #[test]
    fn the_limit_is_in_thousands_of_bytes_and_zero_is_off() {
        assert_eq!(limit_bytes(Some(500)), Some(500_000));
        assert_eq!(limit_bytes(Some(0)), None);
        assert_eq!(limit_bytes(None), None);
    }
}
