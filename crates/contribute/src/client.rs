// SPDX-License-Identifier: AGPL-3.0-or-later
//! The relay client (ADR 0017). It only sends what a caller hands it after
//! the user's review, one attempt per call: no retries, no queue, no
//! redirects followed, and nothing at all in a build without a relay URL.

use crate::payload::{Prepared, Slot, Unapproved, approve, sha256_hex};
use crate::scrub::Scrubber;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The relay this build sends to, fixed at build time (docs/RELAY.md).
/// Without it the app has no contribution channels.
pub const BUILD_RELAY_URL: Option<&str> = option_env!("RAWTONIC_RELAY_URL");

/// Leading zero bits of the proof of work every send carries; the relay
/// checks it only when its `POW_BITS` is set (at most this many).
pub const POW_BITS: u32 = 20;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How often [`RelayClient::send_if`] asks whether consent still holds
/// while a request is under way.
const STILL_EVERY: Duration = Duration::from_millis(50);
const TIMEOUT: Duration = Duration::from_secs(30);

/// An upload may take 30 s plus 1 s per MiB (ADR 0017).
fn upload_timeout(size: u64) -> Duration {
    TIMEOUT + Duration::from_secs(size.div_ceil(1024 * 1024))
}

/// What the relay answers to a stored submission.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    /// Deletes the submission (`DELETE /report/:id`) with its files; shown
    /// to the user.
    pub id: Option<String>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// The one-time upload URL of each announced file (the client uses
    /// them; they are of no use afterwards).
    #[serde(default, skip_serializing)]
    pub uploads: BTreeMap<Slot, String>,
    /// Files the report announced whose upload was not confirmed (refused,
    /// or no answer: a file may still have arrived). The report itself is
    /// stored and deleted by its id, with whatever of them arrived.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_uploaded: Vec<Slot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// This build has no relay.
    NotConfigured,
    /// The body is not the reviewed one, or not clean.
    Unapproved(Unapproved),
    /// The relay's limits for this network, or for everyone, are reached.
    RateLimited,
    /// The relay refused it (`code` is its error code).
    Refused { status: u16, code: String },
    /// The request never left: no connection to the relay (offline,
    /// unknown host, refused). Nothing can have been stored.
    Unreached(String),
    /// No answer after the request may have arrived (timeout, cut off):
    /// the relay may or may not have stored it.
    Network(String),
    /// The caller withdrew consent before the request was sent.
    Cancelled,
}

impl SendError {
    /// A stable code for the interface's messages.
    pub fn code(&self) -> &'static str {
        match self {
            SendError::NotConfigured => "not-configured",
            SendError::Unapproved(_) => "unapproved",
            SendError::RateLimited => "rate-limited",
            SendError::Refused { status: 404, .. } => "not-found",
            SendError::Refused { .. } => "refused",
            SendError::Unreached(_) | SendError::Network(_) => "network",
            SendError::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Refused { status, code } => write!(f, "relay refused ({status} {code})"),
            SendError::Network(e) => write!(f, "network: {e}"),
            SendError::Unreached(e) => write!(f, "unreached: {e}"),
            SendError::Unapproved(u) => write!(f, "not sent: {u:?}"),
            other => f.write_str(other.code()),
        }
    }
}

impl std::error::Error for SendError {}

/// The proof-of-work stamp (`X-Rawtonic-Pow`) for a body: `seconds.nonce`
/// such that SHA-256("rawtonic-pow-1:" seconds ":" sha256(body) ":" nonce)
/// starts with `bits` zero bits (services/relay/src/limits.ts).
pub fn pow_stamp(body: &[u8], seconds: u64, bits: u32) -> String {
    let body_hash = sha256_hex(body);
    let prefix = format!("rawtonic-pow-1:{seconds}:{body_hash}:");
    // A random start, so two clients sending the same body in the same
    // second do not find the same stamp (the relay refuses a stamp seen
    // before).
    let start = {
        use std::hash::{BuildHasher, Hasher};
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
    };
    for step in 0u64.. {
        let nonce = start.wrapping_add(step);
        let digest = <sha2::Sha256 as sha2::Digest>::digest(format!("{prefix}{nonce}").as_bytes());
        if leading_zero_bits(&digest) >= bits {
            return format!("{seconds}.{nonce}");
        }
    }
    unreachable!("a nonce is found long before u64 runs out")
}

fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in bytes {
        if *byte == 0 {
            bits += 8;
        } else {
            return bits + byte.leading_zeros();
        }
    }
    bits
}

#[derive(Debug, Clone)]
pub struct RelayClient {
    base: reqwest::Url,
    http: reqwest::Client,
    version: String,
    pow_bits: u32,
}

impl RelayClient {
    /// The client for this build's relay, if it has one.
    pub fn from_build(version: &str) -> Option<Self> {
        BUILD_RELAY_URL.and_then(|url| Self::new(url, version).ok())
    }

    /// A client for `base` (https, or http to this computer for tests).
    pub fn new(base: &str, version: &str) -> Result<Self, String> {
        let mut base = reqwest::Url::parse(base).map_err(|e| e.to_string())?;
        let local = matches!(base.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if base.scheme() != "https" && !(base.scheme() == "http" && local) {
            return Err("the relay URL must use https".into());
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("Rawtonic/{version}"))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            base,
            http,
            version: version.to_string(),
            pow_bits: POW_BITS,
        })
    }

    #[cfg(test)]
    fn with_pow_bits(mut self, bits: u32) -> Self {
        self.pow_bits = bits;
        self
    }

    fn url(&self, path: &str) -> reqwest::Url {
        self.base.join(path).expect("relay paths are relative")
    }

    /// Sends a reviewed body, once, then each attached file's bytes to its
    /// upload slot. Refuses a body that is not the reviewed one or that
    /// scrubbing would still change, and files whose bytes are no longer
    /// the announced ones (before anything is sent). A file that does not
    /// arrive is named in the receipt's `not_uploaded`; the report stays.
    pub async fn send(
        &self,
        prepared: &Prepared,
        scrubber: &Scrubber,
    ) -> Result<Receipt, SendError> {
        self.send_if(prepared, scrubber, || true).await
    }

    /// [`send`](Self::send) for as long as `still` holds: asked right before
    /// the request goes out (after the proof of work) — false sends nothing —
    /// and every 50 ms while it is under way, when false drops the request
    /// (`Cancelled`; its connection is closed). Worst case: bytes already
    /// handed to the network before that can still reach the relay.
    pub async fn send_if(
        &self,
        prepared: &Prepared,
        scrubber: &Scrubber,
        still: impl Fn() -> bool,
    ) -> Result<Receipt, SendError> {
        approve(prepared, scrubber).map_err(SendError::Unapproved)?;
        let attachments = prepared.attachments.clone();
        let files = tokio::task::spawn_blocking(move || {
            attachments
                .iter()
                .map(|a| a.bytes().map(|bytes| (a.slot, bytes)))
                .collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|e| SendError::Network(e.to_string()))?
        .map_err(SendError::Unapproved)?;
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| SendError::Network(e.to_string()))?
            .as_secs();
        let body = prepared.body.clone();
        let bits = self.pow_bits;
        let stamp = tokio::task::spawn_blocking(move || pow_stamp(body.as_bytes(), seconds, bits))
            .await
            .map_err(|e| SendError::Network(e.to_string()))?;
        if !still() {
            return Err(SendError::Cancelled);
        }
        let request = self.post(prepared, stamp, files);
        tokio::pin!(request);
        loop {
            tokio::select! {
                answer = &mut request => return answer,
                _ = tokio::time::sleep(STILL_EVERY) => {
                    if !still() {
                        return Err(SendError::Cancelled);
                    }
                }
            }
        }
    }

    async fn post(
        &self,
        prepared: &Prepared,
        stamp: String,
        files: Vec<(crate::payload::Slot, Vec<u8>)>,
    ) -> Result<Receipt, SendError> {
        let response = self
            .http
            .post(self.url(prepared.channel.path()))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("x-rawtonic-client", &self.version)
            .header("x-rawtonic-pow", stamp)
            .body(prepared.body.clone())
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    SendError::Unreached(e.without_url().to_string())
                } else {
                    SendError::Network(e.without_url().to_string())
                }
            })?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| SendError::Network(e.without_url().to_string()))?;
        let mut receipt = match status {
            201 => serde_json::from_slice::<Receipt>(&bytes).map_err(|_| SendError::Refused {
                status,
                code: "bad-answer".into(),
            })?,
            204 => Receipt::default(),
            _ => return Err(refusal(status, &bytes)),
        };
        for (slot, bytes) in files {
            let sent = match receipt.uploads.get(&slot) {
                Some(url) => self.upload(url, bytes).await,
                None => Err(SendError::Refused {
                    status,
                    code: "no-upload-slot".into(),
                }),
            };
            if sent.is_err() {
                receipt.not_uploaded.push(slot);
            }
        }
        Ok(receipt)
    }

    /// Sends a file's bytes to an upload URL the relay answered; only to
    /// the relay itself (same scheme, host and port), never elsewhere.
    async fn upload(&self, url: &str, bytes: Vec<u8>) -> Result<(), SendError> {
        let url = reqwest::Url::parse(url).map_err(|_| SendError::Refused {
            status: 201,
            code: "bad-answer".into(),
        })?;
        if url.origin() != self.base.origin() {
            return Err(SendError::Refused {
                status: 201,
                code: "bad-answer".into(),
            });
        }
        let response = self
            .http
            .put(url)
            .timeout(upload_timeout(bytes.len() as u64))
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .header("x-rawtonic-client", &self.version)
            .body(bytes)
            .send()
            .await
            .map_err(|e| SendError::Network(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        if status == 204 {
            return Ok(());
        }
        let body = response.bytes().await.unwrap_or_default();
        Err(refusal(status, &body))
    }

    /// Deletes a submission by the id its receipt gave.
    pub async fn delete(&self, id: &str) -> Result<(), SendError> {
        if id.len() != 26 || !id.bytes().all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7')) {
            return Err(SendError::Refused {
                status: 404,
                code: "unknown-id".into(),
            });
        }
        let response = self
            .http
            .delete(self.url(&format!("report/{id}")))
            .header("x-rawtonic-client", &self.version)
            .send()
            .await
            .map_err(|e| SendError::Network(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        if status == 204 {
            return Ok(());
        }
        let bytes = response.bytes().await.unwrap_or_default();
        Err(refusal(status, &bytes))
    }
}

fn refusal(status: u16, body: &[u8]) -> SendError {
    if status == 429 {
        return SendError::RateLimited;
    }
    #[derive(Deserialize)]
    struct Answer {
        error: String,
    }
    let code = serde_json::from_slice::<Answer>(body)
        .map(|a| a.error)
        .unwrap_or_else(|_| "unknown".into());
    SendError::Refused { status, code }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{Channel, prepare};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A relay stand-in answering every connection with `answer`; returns its
    /// base URL and the requests it saw (head and body as text).
    async fn relay(answer: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
        relay_answering(&[answer]).await
    }

    /// A relay stand-in answering its connections in turn with `answers`
    /// (the last one from then on); `{base}` in an answer is its base URL
    /// and `{len}` the length of the answer's body.
    async fn relay_answering(answers: &[&'static str]) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let answers: Vec<String> = answers
            .iter()
            .map(|a| {
                let a = a.replace("{base}", &base);
                let body = a.split_once("\r\n\r\n").map_or(0, |(_, b)| b.len());
                a.replace("{len}", &body.to_string())
            })
            .collect();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            for n in 0.. {
                let answer = &answers[n.min(answers.len() - 1)];
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&request).to_string());
                let _ = socket.write_all(answer.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        (base, seen)
    }

    fn scrubber() -> Scrubber {
        Scrubber::new(Some("anna"), None)
    }

    fn report() -> Prepared {
        prepare(
            Channel::Report,
            json!({"schema": 2, "text": "hello"}),
            &scrubber(),
        )
        .unwrap()
    }

    const SHOT: &[u8] = b"screenshot bytes";

    fn report_with_screenshot() -> Prepared {
        use base64::Engine as _;
        prepare(
            Channel::Report,
            json!({"schema": 2, "text": "hello", "screenshot": {
                "mime": "image/png",
                "data": base64::engine::general_purpose::STANDARD.encode(SHOT),
            }}),
            &scrubber(),
        )
        .unwrap()
    }

    const FILED_WITH_SLOT: &str = "HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n{\"id\":\"abcdefghijklmnopqrstuvwxyz\",\"ref\":\"0123abcd\",\"uploads\":{\"screenshot\":\"{base}upload/tok\"},\"expiresAt\":\"2026-10-24T13:00:00.000Z\"}";
    const NO_CONTENT: &str = "HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n";

    #[tokio::test]
    async fn sends_the_report_then_the_screenshot_bytes_to_its_slot() {
        let (base, seen) = relay_answering(&[FILED_WITH_SLOT, NO_CONTENT]).await;
        let client = RelayClient::new(&base, "1").unwrap().with_pow_bits(0);
        let p = report_with_screenshot();
        let receipt = client.send(&p, &scrubber()).await.unwrap();
        assert_eq!(receipt.id.as_deref(), Some("abcdefghijklmnopqrstuvwxyz"));
        assert!(receipt.not_uploaded.is_empty());
        // The upload URLs stay inside the client.
        assert!(
            !serde_json::to_string(&receipt)
                .unwrap()
                .contains("upload/tok")
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].starts_with("POST /report HTTP/1.1"));
        assert!(seen[0].ends_with(&p.body));
        assert!(seen[1].starts_with("PUT /upload/tok HTTP/1.1"));
        let lower = seen[1].to_ascii_lowercase();
        assert!(lower.contains("content-type: application/octet-stream"));
        assert!(lower.contains(&format!("content-length: {}", SHOT.len())));
        assert!(seen[1].ends_with("\r\n\r\nscreenshot bytes"));
    }

    #[tokio::test]
    async fn keeps_the_report_when_its_file_does_not_arrive() {
        let (base, seen) = relay_answering(&[
            FILED_WITH_SLOT,
            "HTTP/1.1 422 Unprocessable Entity\r\ncontent-length: 24\r\nconnection: close\r\n\r\n{\"error\":\"not-stored\"}",
        ])
        .await;
        let client = RelayClient::new(&base, "1").unwrap().with_pow_bits(0);
        let receipt = client
            .send(&report_with_screenshot(), &scrubber())
            .await
            .unwrap();
        assert_eq!(receipt.id.as_deref(), Some("abcdefghijklmnopqrstuvwxyz"));
        assert_eq!(receipt.not_uploaded, vec![Slot::Screenshot]);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn sends_files_only_to_the_relay_itself() {
        let (base, seen) = relay_answering(&[
            "HTTP/1.1 201 Created\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n{\"id\":\"abcdefghijklmnopqrstuvwxyz\",\"uploads\":{\"screenshot\":\"http://127.0.0.2:9/upload/tok\"}}",
        ])
        .await;
        let client = RelayClient::new(&base, "1").unwrap().with_pow_bits(0);
        let receipt = client
            .send(&report_with_screenshot(), &scrubber())
            .await
            .unwrap();
        assert_eq!(receipt.not_uploaded, vec![Slot::Screenshot]);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sends_nothing_when_a_file_changed_after_the_review() {
        use base64::Engine as _;
        let (base, seen) = relay_answering(&[FILED_WITH_SLOT, NO_CONTENT]).await;
        let client = RelayClient::new(&base, "1").unwrap().with_pow_bits(0);
        let mut p = report_with_screenshot();
        p.attachments[0].data = Some(base64::engine::general_purpose::STANDARD.encode(b"other"));
        assert_eq!(
            client.send(&p, &scrubber()).await,
            Err(SendError::Unapproved(Unapproved::Attachment))
        );
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn uploads_get_a_second_more_per_mib() {
        assert_eq!(upload_timeout(0), Duration::from_secs(30));
        assert_eq!(upload_timeout(100 * 1024 * 1024), Duration::from_secs(130));
    }

    #[tokio::test]
    async fn sends_the_reviewed_bytes_once_with_its_headers() {
        let (base, seen) = relay(
            "HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: 52\r\nconnection: close\r\n\r\n{\"id\":\"abcdefghijklmnopqrstuvwxyz\",\"ref\":\"0123abcd\"}",
        )
        .await;
        let client = RelayClient::new(&base, "0.1.0-alpha.4")
            .unwrap()
            .with_pow_bits(4);
        let p = report();
        let receipt = client.send(&p, &scrubber()).await.unwrap();
        assert_eq!(receipt.id.as_deref(), Some("abcdefghijklmnopqrstuvwxyz"));
        assert_eq!(receipt.reference.as_deref(), Some("0123abcd"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let request = &seen[0];
        assert!(request.starts_with("POST /report HTTP/1.1"));
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("x-rawtonic-client: 0.1.0-alpha.4"));
        assert!(lower.contains("content-type: application/json"));
        assert!(lower.contains("user-agent: rawtonic/0.1.0-alpha.4"));
        assert!(request.ends_with(&p.body));
        let stamp = lower
            .lines()
            .find_map(|l| l.strip_prefix("x-rawtonic-pow: "))
            .unwrap()
            .trim()
            .to_string();
        let (seconds, nonce) = stamp.split_once('.').unwrap();
        let digest = <sha2::Sha256 as sha2::Digest>::digest(
            format!(
                "rawtonic-pow-1:{seconds}:{}:{nonce}",
                sha256_hex(p.body.as_bytes())
            )
            .as_bytes(),
        );
        assert!(leading_zero_bits(&digest) >= 4);
    }

    #[tokio::test]
    async fn sends_nothing_once_consent_is_withdrawn_and_tells_unreached_apart() {
        let (base, seen) =
            relay("HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await;
        let client = RelayClient::new(&base, "0.1.0-alpha.4")
            .unwrap()
            .with_pow_bits(4);
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let answer = client
            .send_if(&report(), &scrubber(), || {
                asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                false
            })
            .await;
        assert_eq!(answer, Err(SendError::Cancelled));
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(seen.lock().unwrap().is_empty());
        // Nobody listening: the request never left.
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gone = format!("http://{}/", closed.local_addr().unwrap());
        drop(closed);
        let offline = RelayClient::new(&gone, "0.1.0-alpha.4")
            .unwrap()
            .with_pow_bits(4);
        assert!(matches!(
            offline.send(&report(), &scrubber()).await,
            Err(SendError::Unreached(_))
        ));
    }

    #[tokio::test]
    async fn drops_a_request_under_way_once_consent_is_withdrawn() {
        // A relay that takes the connection and never answers.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });
        let client = RelayClient::new(&base, "0.1.0-alpha.4")
            .unwrap()
            .with_pow_bits(4);
        let on = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let off = on.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            off.store(false, std::sync::atomic::Ordering::SeqCst);
        });
        let started = std::time::Instant::now();
        let answer = client
            .send_if(&report(), &scrubber(), || {
                on.load(std::sync::atomic::Ordering::SeqCst)
            })
            .await;
        assert_eq!(answer, Err(SendError::Cancelled));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn does_not_retry_or_follow_redirects() {
        let (base, seen) = relay(
            "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await;
        let client = RelayClient::new(&base, "1").unwrap().with_pow_bits(0);
        let error = client.send(&report(), &scrubber()).await.unwrap_err();
        assert_eq!(error.code(), "refused");
        assert_eq!(seen.lock().unwrap().len(), 1);

        let (base, seen) = relay(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://127.0.0.1:9/elsewhere\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await;
        let client = RelayClient::new(&base, "1").unwrap().with_pow_bits(0);
        let error = client.send(&report(), &scrubber()).await.unwrap_err();
        assert_eq!(
            error,
            SendError::Refused {
                status: 307,
                code: "unknown".into()
            }
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn sends_nothing_that_was_not_reviewed() {
        let (base, seen) = relay("HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n").await;
        let client = RelayClient::new(&base, "1").unwrap().with_pow_bits(0);
        let mut p = report();
        p.body = p.body.replace("hello", "anna");
        assert_eq!(
            client.send(&p, &scrubber()).await,
            Err(SendError::Unapproved(Unapproved::Changed))
        );
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn maps_limits_and_deletes_by_id() {
        let (base, _) = relay(
            "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 24\r\nconnection: close\r\n\r\n{\"error\":\"rate-limited\"}",
        )
        .await;
        let client = RelayClient::new(&base, "1").unwrap().with_pow_bits(0);
        assert_eq!(
            client.send(&report(), &scrubber()).await,
            Err(SendError::RateLimited)
        );

        let (base, seen) = relay("HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n").await;
        let client = RelayClient::new(&base, "1").unwrap();
        client.delete("abcdefghijklmnopqrstuvwxyz").await.unwrap();
        assert!(
            seen.lock().unwrap()[0]
                .starts_with("DELETE /report/abcdefghijklmnopqrstuvwxyz HTTP/1.1")
        );
        assert_eq!(
            client.delete("../stats").await.unwrap_err().code(),
            "not-found"
        );
    }

    #[test]
    fn needs_https_except_on_this_computer() {
        assert!(RelayClient::new("http://relay.example.org", "1").is_err());
        assert!(RelayClient::new("https://relay.example.org/base", "1").is_ok());
        assert!(RelayClient::new("http://127.0.0.1:8787", "1").is_ok());
        let client = RelayClient::new("https://relay.example.org/base", "1").unwrap();
        assert_eq!(
            client.url("stats").as_str(),
            "https://relay.example.org/base/stats"
        );
    }

    #[test]
    fn this_build_has_no_relay_unless_one_was_given() {
        assert_eq!(
            RelayClient::from_build("1").is_some(),
            BUILD_RELAY_URL.is_some()
        );
    }

    #[test]
    fn the_same_body_in_the_same_second_gets_different_stamps() {
        let stamps: std::collections::HashSet<_> =
            (0..8).map(|_| pow_stamp(b"{}", 1_800_000_000, 4)).collect();
        assert!(stamps.len() > 1);
    }

    #[test]
    fn proof_of_work_meets_its_bits() {
        let stamp = pow_stamp(b"{}", 1_800_000_000, 12);
        let (seconds, nonce) = stamp.split_once('.').unwrap();
        assert_eq!(seconds, "1800000000");
        let digest = <sha2::Sha256 as sha2::Digest>::digest(
            format!("rawtonic-pow-1:{seconds}:{}:{nonce}", sha256_hex(b"{}")).as_bytes(),
        );
        assert!(leading_zero_bits(&digest) >= 12);
    }
}
