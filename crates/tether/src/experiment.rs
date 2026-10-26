//! Live-view experiments for a session with a camera body, from
//! `RAWTONIC_TETHER_LV_EXPERIMENT` (comma-separated, e.g.
//! `poll-ms=1000,tolerate=5,priority,reopen`). Each one tests one
//! hypothesis about Fujifilm live view (notes:
//! reference/tether-fuji-liveview-plan.md); none is on unless named there,
//! so without the variable the helper behaves exactly as before.
//!
//! - `poll-ms=N`: while live view runs, the camera's events are read only
//!   every N ms instead of after every frame (libgphoto2's Fujifilm event
//!   wait takes up to 300 ms even when asked for 1 ms).
//! - `tolerate=N`: live view goes on through up to N failed frames in a row
//!   (each logged) instead of stopping at the first.
//! - `priority`: the computer takes priority (Fujifilm `prioritymode` USB)
//!   before each live-view frame, without reading it first (libgphoto2
//!   answers it from a 2 s cache while its event wait has set it back).
//! - `reopen`: a shot fired during live view ends the camera's live view
//!   first (TerminateOpenCapture), so the next frame opens it again.
use std::time::Duration;

/// The environment variable that names the experiments.
pub const ENV: &str = "RAWTONIC_TETHER_LV_EXPERIMENT";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveViewExperiment {
    pub poll_every: Option<Duration>,
    pub tolerate: u32,
    pub priority: bool,
    pub reopen: bool,
}

impl LiveViewExperiment {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut experiment = Self::default();
        for item in text.split(',').map(str::trim).filter(|i| !i.is_empty()) {
            let (name, value) = match item.split_once('=') {
                Some((name, value)) => (name.trim(), Some(value.trim())),
                None => (item, None),
            };
            let number = |value: Option<&str>| {
                value
                    .and_then(|v| v.parse::<u32>().ok())
                    .ok_or_else(|| format!("{name} needs a number ({name}=N)"))
            };
            match name {
                "poll-ms" => {
                    experiment.poll_every = Some(Duration::from_millis(u64::from(number(value)?)))
                }
                "tolerate" => experiment.tolerate = number(value)?,
                "priority" if value.is_none() => experiment.priority = true,
                "reopen" if value.is_none() => experiment.reopen = true,
                _ => return Err(format!("unknown live-view experiment {item:?}")),
            }
        }
        Ok(experiment)
    }

    /// The experiments the environment names; none (with a warning) when it
    /// names one that does not exist.
    pub fn from_env() -> Self {
        let Ok(text) = std::env::var(ENV) else {
            return Self::default();
        };
        match Self::parse(&text) {
            Ok(experiment) => {
                if experiment != Self::default() {
                    log::warn!("live-view experiment on: {experiment:?}");
                }
                experiment
            }
            Err(e) => {
                log::warn!("{ENV}: {e}; no experiment");
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experiments_are_named_one_by_one() {
        assert_eq!(
            LiveViewExperiment::parse("").unwrap(),
            LiveViewExperiment::default()
        );
        assert_eq!(
            LiveViewExperiment::parse(" poll-ms=1000, tolerate=5 ,priority,reopen").unwrap(),
            LiveViewExperiment {
                poll_every: Some(Duration::from_secs(1)),
                tolerate: 5,
                priority: true,
                reopen: true,
            }
        );
        for wrong in ["poll-ms", "tolerate=x", "priority=1", "faster"] {
            assert!(LiveViewExperiment::parse(wrong).is_err(), "{wrong}");
        }
    }
}
