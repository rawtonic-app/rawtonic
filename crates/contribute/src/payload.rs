// SPDX-License-Identifier: AGPL-3.0-or-later
//! The body the user reviews is the body that is sent (ADR 0017): it is
//! scrubbed and serialized once, shown with its SHA-256, and sent only when
//! the digest still matches and scrubbing it again changes nothing. Files
//! (a report's screenshot and photo) are not part of the JSON: the body
//! names their size and SHA-256, and their bytes go to the relay's upload
//! slots unchanged, checked against those values before anything is sent.

use crate::scrub::Scrubber;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Where a submission goes; each is one endpoint of the relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Channel {
    Report,
    Crash,
    CameraTest,
    Stats,
}

impl Channel {
    pub fn path(self) -> &'static str {
        match self {
            Channel::Report => "report",
            Channel::Crash => "crash",
            Channel::CameraTest => "camera-test",
            Channel::Stats => "stats",
        }
    }

    /// The payload version the relay takes on this endpoint: reports are 2
    /// (files announced, not inlined), the others 1.
    pub fn schema(self) -> u64 {
        match self {
            Channel::Report => 2,
            _ => 1,
        }
    }

    /// The relay's body cap for this endpoint (services/relay/src/schema.ts).
    pub fn max_body(self) -> usize {
        match self {
            Channel::Report | Channel::Crash => 96 * 1024,
            Channel::CameraTest => 32 * 1024,
            Channel::Stats => 8 * 1024,
        }
    }
}

/// A file sent with a report, each to its own upload slot of the relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Slot {
    Screenshot,
    Photo,
}

impl Slot {
    /// The relay's size cap for this file (services/relay/src/schema.ts).
    pub fn max_size(self) -> u64 {
        match self {
            Slot::Screenshot => 2 * 1024 * 1024,
            Slot::Photo => 100 * 1024 * 1024,
        }
    }
}

/// A file the body announces: its bytes are sent as they are, after the
/// body, and only if they still have this size and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub slot: Slot,
    pub size: u64,
    /// SHA-256 of the bytes, lower-case hex.
    pub sha256: String,
    /// A screenshot's bytes, base64 (the review dialog shows the image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// The photo's file on this computer; only its bytes are sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl Attachment {
    /// The bytes to send, if they are still the announced ones.
    pub fn bytes(&self) -> Result<Vec<u8>, Unapproved> {
        let bytes = match (&self.data, &self.path) {
            (Some(data), None) => base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| Unapproved::Attachment)?,
            (None, Some(path)) => {
                let mut bytes = Vec::new();
                std::fs::File::open(path)
                    .and_then(|f| f.take(self.slot.max_size() + 1).read_to_end(&mut bytes))
                    .map_err(|_| Unapproved::Attachment)?;
                bytes
            }
            _ => return Err(Unapproved::Attachment),
        };
        if bytes.len() as u64 != self.size || sha256_hex(&bytes) != self.sha256 {
            return Err(Unapproved::Attachment);
        }
        Ok(bytes)
    }
}

/// A body ready for review: exactly these bytes are sent, then the
/// attachments' bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    pub channel: Channel,
    pub body: String,
    /// SHA-256 of `body`, lower-case hex.
    pub digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareError {
    /// Not a JSON object with the channel's `schema`.
    NotAPayload,
    /// Larger than the relay takes for this channel.
    TooLarge { size: usize, max: usize },
    /// A screenshot that is not base64, or a photo file that cannot be read.
    BadAttachment,
    /// A file larger than the relay takes.
    AttachmentTooLarge { size: u64, max: u64 },
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrepareError::NotAPayload => write!(f, "not a payload of this channel's schema"),
            PrepareError::TooLarge { size, max } => {
                write!(f, "payload of {size} bytes exceeds {max}")
            }
            PrepareError::BadAttachment => write!(f, "attachment unreadable"),
            PrepareError::AttachmentTooLarge { size, max } => {
                write!(f, "attachment of {size} bytes exceeds {max}")
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

fn check_shape(channel: Channel, value: &Value) -> Result<(), PrepareError> {
    match value {
        Value::Object(fields) if fields.get("schema") == Some(&Value::from(channel.schema())) => {
            Ok(())
        }
        _ => Err(PrepareError::NotAPayload),
    }
}

/// SHA-256 and size of a file, read in pieces; refused above `max`.
fn hash_file(path: &Path, max: u64) -> Result<(u64, String), PrepareError> {
    let mut file = std::fs::File::open(path).map_err(|_| PrepareError::BadAttachment)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|_| PrepareError::BadAttachment)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > max {
            return Err(PrepareError::AttachmentTooLarge { size, max });
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((size, digest))
}

/// Takes a report's files out of the payload: `screenshot: {mime, data}`
/// (base64) becomes `{mime, size, sha256}`, `photo.path` (the photo's file
/// on this computer) becomes `photo.file: {size, sha256}`. Done before
/// scrubbing, so neither the image data nor the path reaches the body.
fn take_attachments(fields: &mut Map<String, Value>) -> Result<Vec<Attachment>, PrepareError> {
    let mut attachments = Vec::new();
    if let Some(Value::Object(shot)) = fields.get_mut("screenshot")
        && let Some(Value::String(data)) = shot.remove("data")
    {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&data)
            .map_err(|_| PrepareError::BadAttachment)?;
        let (size, max) = (bytes.len() as u64, Slot::Screenshot.max_size());
        if size > max {
            return Err(PrepareError::AttachmentTooLarge { size, max });
        }
        let sha256 = sha256_hex(&bytes);
        shot.insert("size".into(), json!(size));
        shot.insert("sha256".into(), json!(sha256));
        attachments.push(Attachment {
            slot: Slot::Screenshot,
            size,
            sha256,
            data: Some(data),
            path: None,
        });
    }
    if let Some(Value::Object(photo)) = fields.get_mut("photo")
        && let Some(Value::String(path)) = photo.remove("path")
    {
        let path = PathBuf::from(path);
        let (size, sha256) = hash_file(&path, Slot::Photo.max_size())?;
        photo.insert("file".into(), json!({ "size": size, "sha256": sha256 }));
        attachments.push(Attachment {
            slot: Slot::Photo,
            size,
            sha256,
            data: None,
            path: Some(path),
        });
    }
    Ok(attachments)
}

/// What the body announces for each slot (size and SHA-256).
fn announced(body: &Value) -> Vec<(Slot, Option<u64>, Option<&str>)> {
    fn file(v: &Value) -> (Option<u64>, Option<&str>) {
        (v["size"].as_u64(), v["sha256"].as_str())
    }
    let mut out = Vec::new();
    if let Some(shot) = body.get("screenshot") {
        let (size, sha) = file(shot);
        out.push((Slot::Screenshot, size, sha));
    }
    if let Some(photo_file) = body.get("photo").and_then(|p| p.get("file")) {
        let (size, sha) = file(photo_file);
        out.push((Slot::Photo, size, sha));
    }
    out
}

/// Scrubs a payload and fixes the bytes the user will review. A report's
/// files are taken out first (see `take_attachments`); only reports carry
/// files.
pub fn prepare(
    channel: Channel,
    mut payload: Value,
    scrubber: &Scrubber,
) -> Result<Prepared, PrepareError> {
    check_shape(channel, &payload)?;
    let attachments = match (&mut payload, channel) {
        (Value::Object(fields), Channel::Report) => take_attachments(fields)?,
        _ => Vec::new(),
    };
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
        attachments,
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
    /// The files do not match what the body announces, or their bytes
    /// changed since.
    Attachment,
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
    check_shape(prepared.channel, &parsed).map_err(|_| Unapproved::Unscrubbed)?;
    let mut again = parsed.clone();
    scrubber.scrub_json(&mut again);
    if again != parsed {
        return Err(Unapproved::Unscrubbed);
    }
    // Every announced file is attached, once, and nothing else; whether
    // its bytes are still these is checked when they are read to be sent.
    let mut declared = announced(&parsed);
    let mut attached: Vec<_> = prepared
        .attachments
        .iter()
        .map(|a| (a.slot, Some(a.size), Some(a.sha256.as_str())))
        .collect();
    declared.sort();
    attached.sort();
    if declared != attached
        || prepared
            .attachments
            .iter()
            .any(|a| a.size > a.slot.max_size())
    {
        return Err(Unapproved::Attachment);
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
            json!({"schema": 2, "text": "studio lost /Users/anna/x.CR3"}),
            &scrubber(),
        )
        .unwrap();
        assert!(p.body.contains("<host> lost <path>.CR3"));
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
            attachments: Vec::new(),
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
    fn a_report_takes_one_schema_only() {
        assert_eq!(
            prepare(
                Channel::Report,
                json!({"schema": 1, "text": "x"}),
                &scrubber()
            ),
            Err(PrepareError::NotAPayload)
        );
        assert!(
            prepare(
                Channel::Crash,
                json!({"schema": 1, "message": "x"}),
                &scrubber()
            )
            .is_ok()
        );
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n image bytes /Users/anna";

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn a_screenshot_is_announced_in_the_body_and_sent_as_its_bytes() {
        let p = prepare(
            Channel::Report,
            json!({"schema": 2, "text": "x", "screenshot": {"mime": "image/png", "data": b64(PNG)}}),
            &scrubber(),
        )
        .unwrap();
        let body: Value = serde_json::from_str(&p.body).unwrap();
        assert_eq!(
            body["screenshot"],
            json!({"mime": "image/png", "size": PNG.len(), "sha256": sha256_hex(PNG)})
        );
        assert!(!p.body.contains(&b64(PNG)));
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].slot, Slot::Screenshot);
        assert_eq!(p.attachments[0].bytes().unwrap(), PNG);
        assert_eq!(approve(&p, &scrubber()), Ok(()));

        // Other bytes than the announced ones, or a file left out, are refused.
        let mut swapped = p.clone();
        swapped.attachments[0].data = Some(b64(b"other bytes"));
        assert_eq!(swapped.attachments[0].bytes(), Err(Unapproved::Attachment));
        let mut dropped = p.clone();
        dropped.attachments.clear();
        assert_eq!(approve(&dropped, &scrubber()), Err(Unapproved::Attachment));
        let mut announced_other = p.clone();
        announced_other.attachments[0].sha256 = sha256_hex(b"other bytes");
        assert_eq!(
            approve(&announced_other, &scrubber()),
            Err(Unapproved::Attachment)
        );
    }

    #[test]
    fn a_photo_file_is_hashed_and_its_path_never_reaches_the_body() {
        let dir = std::env::temp_dir().join(format!("rawtonic-contribute-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("anna-wedding.CR3");
        std::fs::write(&path, b"raw bytes").unwrap();
        let p = prepare(
            Channel::Report,
            json!({"schema": 2, "text": "x", "photo": {"make": "Canon", "model": "EOS R5", "fileType": "CR3", "path": path}}),
            &scrubber(),
        )
        .unwrap();
        let body: Value = serde_json::from_str(&p.body).unwrap();
        assert_eq!(
            body["photo"]["file"],
            json!({"size": 9, "sha256": sha256_hex(b"raw bytes")})
        );
        assert!(!p.body.contains("anna-wedding") && body["photo"].get("path").is_none());
        assert_eq!(p.attachments[0].path.as_deref(), Some(path.as_path()));
        assert_eq!(approve(&p, &scrubber()), Ok(()));
        assert_eq!(p.attachments[0].bytes().unwrap(), b"raw bytes");
        // Changed after the review: not sent.
        std::fs::write(&path, b"raw bytez").unwrap();
        assert_eq!(p.attachments[0].bytes(), Err(Unapproved::Attachment));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_above_the_relay_caps_are_refused() {
        let big = vec![0u8; 2 * 1024 * 1024 + 1];
        assert_eq!(
            prepare(
                Channel::Report,
                json!({"schema": 2, "text": "x", "screenshot": {"mime": "image/png", "data": b64(&big)}}),
                &scrubber(),
            ),
            Err(PrepareError::AttachmentTooLarge {
                size: big.len() as u64,
                max: 2 * 1024 * 1024
            })
        );
        assert_eq!(
            prepare(
                Channel::Report,
                json!({"schema": 2, "text": "x", "screenshot": {"mime": "image/png", "data": "not base64!"}}),
                &scrubber(),
            ),
            Err(PrepareError::BadAttachment)
        );
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
