// SPDX-License-Identifier: AGPL-3.0-or-later
//! The body the user reviews is the body that is sent (ADR 0017): it is
//! scrubbed and serialized once, shown with its SHA-256, and sent only when
//! the digest still matches and scrubbing it again changes nothing.

use crate::scrub::Scrubber;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Where a submission goes; each is one endpoint of the relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Channel {
    Report,
    Crash,
    CameraTest,
    RawSample,
    Stats,
}

impl Channel {
    pub fn path(self) -> &'static str {
        match self {
            Channel::Report => "report",
            Channel::Crash => "crash",
            Channel::CameraTest => "camera-test",
            Channel::RawSample => "raw-sample",
            Channel::Stats => "stats",
        }
    }

    /// The relay's body cap for this endpoint (services/relay/src/schema.ts).
    pub fn max_body(self) -> usize {
        match self {
            Channel::Report => 3 * 1024 * 1024,
            Channel::Crash => 96 * 1024,
            Channel::CameraTest => 32 * 1024,
            Channel::RawSample | Channel::Stats => 8 * 1024,
        }
    }
}

/// A body ready for review: exactly these bytes are sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    pub channel: Channel,
    pub body: String,
    /// SHA-256 of `body`, lower-case hex.
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareError {
    /// Not a JSON object with `"schema": 1`.
    NotAPayload,
    /// Larger than the relay takes for this channel.
    TooLarge { size: usize, max: usize },
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrepareError::NotAPayload => write!(f, "not a schema-1 payload"),
            PrepareError::TooLarge { size, max } => {
                write!(f, "payload of {size} bytes exceeds {max}")
            }
        }
    }
}

impl std::error::Error for PrepareError {}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn check_shape(value: &Value) -> Result<(), PrepareError> {
    match value {
        Value::Object(fields) if fields.get("schema") == Some(&Value::from(1)) => Ok(()),
        _ => Err(PrepareError::NotAPayload),
    }
}

/// Scrubs a payload and fixes the bytes the user will review.
pub fn prepare(
    channel: Channel,
    mut payload: Value,
    scrubber: &Scrubber,
) -> Result<Prepared, PrepareError> {
    check_shape(&payload)?;
    scrubber.scrub_json(&mut payload);
    // Pretty-printed: the review dialog shows these very bytes.
    let body = serde_json::to_string_pretty(&payload).map_err(|_| PrepareError::NotAPayload)?;
    if body.len() > channel.max_body() {
        return Err(PrepareError::TooLarge {
            size: body.len(),
            max: channel.max_body(),
        });
    }
    let digest = sha256_hex(body.as_bytes());
    Ok(Prepared {
        channel,
        body,
        digest,
    })
}

/// Why a reviewed body may not be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unapproved {
    /// The body is not the one reviewed (digest differs).
    Changed,
    /// Scrubbing would still change it, or it is not a payload.
    Unscrubbed,
    TooLarge,
}

/// Checks a reviewed body before it is sent.
pub fn approve(prepared: &Prepared, scrubber: &Scrubber) -> Result<(), Unapproved> {
    if sha256_hex(prepared.body.as_bytes()) != prepared.digest {
        return Err(Unapproved::Changed);
    }
    if prepared.body.len() > prepared.channel.max_body() {
        return Err(Unapproved::TooLarge);
    }
    let parsed: Value = serde_json::from_str(&prepared.body).map_err(|_| Unapproved::Unscrubbed)?;
    check_shape(&parsed).map_err(|_| Unapproved::Unscrubbed)?;
    let mut again = parsed.clone();
    scrubber.scrub_json(&mut again);
    if again != parsed {
        return Err(Unapproved::Unscrubbed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scrubber() -> Scrubber {
        Scrubber::new(Some("anna"), Some("studio"))
    }

    #[test]
    fn prepares_scrubbed_bytes_with_their_digest() {
        let p = prepare(
            Channel::Report,
            json!({"schema": 1, "text": "lost /Users/anna/x.CR3 on studio"}),
            &scrubber(),
        )
        .unwrap();
        assert!(p.body.contains("lost <path>.CR3 on <host>"));
        assert_eq!(p.digest, sha256_hex(p.body.as_bytes()));
        assert_eq!(approve(&p, &scrubber()), Ok(()));
    }

    #[test]
    fn refuses_a_body_changed_after_review_or_still_personal() {
        let p = prepare(
            Channel::Crash,
            json!({"schema": 1, "message": "boom"}),
            &scrubber(),
        )
        .unwrap();
        let changed = Prepared {
            body: p.body.replace("boom", "bang"),
            ..p.clone()
        };
        assert_eq!(approve(&changed, &scrubber()), Err(Unapproved::Changed));
        let body = r#"{"schema":1,"message":"anna was here"}"#.to_string();
        let personal = Prepared {
            channel: Channel::Crash,
            digest: sha256_hex(body.as_bytes()),
            body,
        };
        assert_eq!(approve(&personal, &scrubber()), Err(Unapproved::Unscrubbed));
    }

    #[test]
    fn refuses_what_is_not_a_payload_or_too_large() {
        assert_eq!(
            prepare(Channel::Stats, json!([1]), &scrubber()),
            Err(PrepareError::NotAPayload)
        );
        assert_eq!(
            prepare(Channel::Stats, json!({"schema": 2}), &scrubber()),
            Err(PrepareError::NotAPayload)
        );
        let big = "x".repeat(9 * 1024);
        assert!(matches!(
            prepare(Channel::Stats, json!({"schema": 1, "t": big}), &scrubber()),
            Err(PrepareError::TooLarge { .. })
        ));
    }

    #[test]
    fn channels_name_the_relay_endpoints() {
        assert_eq!(
            serde_json::to_value(Channel::CameraTest).unwrap(),
            json!("cameraTest")
        );
        assert_eq!(Channel::CameraTest.path(), "camera-test");
    }
}
