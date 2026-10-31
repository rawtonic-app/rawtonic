// SPDX-License-Identifier: AGPL-3.0-or-later
//! Eyes open, closed, not visible or uncertain, per face, for culling: the
//! eye aspect ratio of the Face Mesh landmarks and the parse's accessories
//! class at each iris (sunglasses). A suggestion only; the photographer's
//! own call per face wins (catalog `face_eyes.override_state`).
//!
//! The rules and thresholds are stage A's (notes:
//! benchmark-results/fq-face-quality-2026-10-31/REPORT.md): no landmarks →
//! uncertain; accessories within the band → uncertain, above it → not
//! visible; otherwise the mean eye aspect ratio, uncertain within its band.
//! Each threshold and band is the median of the five leave-one-shoot-out
//! folds (each fitted on the other shoots only), not refitted on the whole
//! set. Measured through this code: benchmark-results/fqb-eyes-2026-10-31/.

use serde::{Deserialize, Serialize};

use crate::Face;
use crate::parse::Class;

/// Bump when a rule or threshold changes: stored readings of an older
/// version are read again.
pub const VERSION: u32 = 1;

/// Accessories probability at the iris (the higher of both eyes): above
/// `ACCESSORIES + ACCESSORIES_BAND` the eyes are hidden, within the band
/// uncertain. Folds: 0.175 (band 0.128), 0.367 (0.249, 0.256, 0.241, 0.251).
const ACCESSORIES: f32 = 0.367;
const ACCESSORIES_BAND: f32 = 0.2486;
/// Mean eye aspect ratio: at or below `EAR` closed or partly closed,
/// above open, within `EAR_BAND` of it uncertain. Folds: 0.199 (band
/// 0.0137), 0.153 (0.0219, 0.0151), 0.173 (0.0088, 0.0096).
const EAR: f32 = 0.1731;
const EAR_BAND: f32 = 0.0137;

/// MediaPipe Face Mesh indices per eye (the subject's right eye first):
/// the two corners and two upper/lower lid pairs.
const EYES: [([usize; 2], [[usize; 2]; 2]); 2] = [
    ([33, 133], [[160, 144], [158, 153]]),
    ([263, 362], [[385, 380], [387, 373]]),
];
/// Iris centres (the subject's right, left).
const IRIS: [usize; 2] = [468, 473];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EyeState {
    Open,
    /// Closed or partly closed (blinks, squints).
    Closed,
    /// Hidden (sunglasses).
    NotVisible,
    /// No call: no landmarks, or a score too near its threshold.
    Uncertain,
}

impl EyeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::NotVisible => "notVisible",
            Self::Uncertain => "uncertain",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Self::Open,
            "closed" => Self::Closed,
            "notVisible" => Self::NotVisible,
            "uncertain" => Self::Uncertain,
            _ => return None,
        })
    }
}

/// One face's eyes and the scores behind the call.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EyeReading {
    pub state: EyeState,
    /// Mean eye aspect ratio; `None` without landmarks.
    pub ear: Option<f32>,
    /// Accessories probability at the iris, the higher eye; `None` without
    /// landmarks.
    pub accessories: Option<f32>,
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Mean eye aspect ratio of both eyes: lid opening over eye width.
pub fn eye_aspect_ratio(landmarks: &[[f32; 2]]) -> f32 {
    let ratio = |(corners, pairs): &([usize; 2], [[usize; 2]; 2])| {
        let width = distance(landmarks[corners[0]], landmarks[corners[1]]);
        let height = pairs
            .iter()
            .map(|[a, b]| distance(landmarks[*a], landmarks[*b]))
            .sum::<f32>()
            / 2.0;
        height / width.max(1e-3)
    };
    EYES.iter().map(ratio).sum::<f32>() / 2.0
}

/// The call from the two scores (`None` = no landmarks).
pub fn classify(ear: Option<f32>, accessories: Option<f32>) -> EyeState {
    let (Some(ear), Some(accessories)) = (ear, accessories) else {
        return EyeState::Uncertain;
    };
    if accessories > ACCESSORIES + ACCESSORIES_BAND {
        EyeState::NotVisible
    } else if (accessories - ACCESSORIES).abs() <= ACCESSORIES_BAND || (ear - EAR).abs() <= EAR_BAND
    {
        EyeState::Uncertain
    } else if ear <= EAR {
        EyeState::Closed
    } else {
        EyeState::Open
    }
}

/// Reads one analysed face's eyes.
pub fn read(face: &Face) -> EyeReading {
    let Some(landmarks) = face.landmarks.as_deref() else {
        return EyeReading {
            state: EyeState::Uncertain,
            ear: None,
            accessories: None,
        };
    };
    let ear = eye_aspect_ratio(landmarks);
    let accessories = IRIS
        .iter()
        .map(|&i| {
            let [x, y] = landmarks[i];
            face.parse.sample(Class::Accessories, x, y).0
        })
        .fold(0.0f32, f32::max);
    EyeReading {
        state: classify(Some(ear), Some(accessories)),
        ear: Some(ear),
        accessories: Some(accessories),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Landmarks with both eyes `width` wide and `height` open.
    fn eyes(width: f32, height: f32) -> Vec<[f32; 2]> {
        let mut points = vec![[0.0, 0.0]; 478];
        for (x0, (corners, pairs)) in [(0.0, EYES[0]), (100.0, EYES[1])] {
            points[corners[0]] = [x0, 50.0];
            points[corners[1]] = [x0 + width, 50.0];
            for [a, b] in pairs {
                points[a] = [x0 + width / 2.0, 50.0 - height / 2.0];
                points[b] = [x0 + width / 2.0, 50.0 + height / 2.0];
            }
        }
        points
    }

    #[test]
    fn aspect_ratio_is_lid_opening_over_width() {
        assert!((eye_aspect_ratio(&eyes(40.0, 12.0)) - 0.3).abs() < 1e-6);
        assert_eq!(eye_aspect_ratio(&eyes(40.0, 0.0)), 0.0);
    }

    #[test]
    fn calls_follow_stage_a_rules() {
        let clear = Some(0.02);
        assert_eq!(classify(Some(0.30), clear), EyeState::Open);
        assert_eq!(classify(Some(0.05), clear), EyeState::Closed);
        // Within the eye band: no call either way.
        assert_eq!(classify(Some(EAR + 0.01), clear), EyeState::Uncertain);
        assert_eq!(classify(Some(EAR - 0.01), clear), EyeState::Uncertain);
        // Sunglasses win over any lid reading; doubtful accessories give no call.
        assert_eq!(classify(Some(0.30), Some(0.9)), EyeState::NotVisible);
        assert_eq!(classify(Some(0.05), Some(0.9)), EyeState::NotVisible);
        assert_eq!(classify(Some(0.30), Some(0.4)), EyeState::Uncertain);
        // No landmarks: never a guess.
        assert_eq!(classify(None, None), EyeState::Uncertain);
    }

    #[test]
    fn states_round_trip_as_text() {
        for state in [
            EyeState::Open,
            EyeState::Closed,
            EyeState::NotVisible,
            EyeState::Uncertain,
        ] {
            assert_eq!(EyeState::parse(state.as_str()), Some(state));
        }
        assert_eq!(EyeState::parse("blink"), None);
    }
}
