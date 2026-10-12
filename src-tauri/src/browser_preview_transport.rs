//! Large interactive JPEGs can go straight to WebKit's image loader. Only
//! opaque, short-lived frame IDs cross the command reply; no files or render
//! commands are exposed by the image protocol.
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

const TAG: &[u8] = b"JPEG_URI___";
const HEADER: usize = 24;
const MIN_JPEG_BYTES: usize = 128 * 1024;
const MAX_FRAMES: usize = 16;
const MAX_BYTES: usize = 32 * 1024 * 1024;
// Always leave room for the four in-flight replies and retained patches.
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Frames {
    entries: VecDeque<(Uuid, Instant, Vec<u8>)>,
    bytes: usize,
}

impl Frames {
    fn prune(&mut self, now: Instant) {
        while self
            .entries
            .front()
            .is_some_and(|(_, inserted, _)| now.duration_since(*inserted) >= TTL)
            || self.entries.len() > MAX_FRAMES
            || self.bytes > MAX_BYTES
        {
            if let Some((_, _, bytes)) = self.entries.pop_front() {
                self.bytes -= bytes.len();
            }
        }
    }

    fn insert(&mut self, jpeg: Vec<u8>, now: Instant) -> Option<Uuid> {
        if jpeg.len() > MAX_BYTES {
            return None;
        }
        let id = Uuid::new_v4();
        self.bytes += jpeg.len();
        self.entries.push_back((id, now, jpeg));
        self.prune(now);
        Some(id)
    }

    fn get(&mut self, id: Uuid, now: Instant) -> Option<Vec<u8>> {
        self.prune(now);
        // The loader may retry or remount the retained patch during refinement.
        self.entries
            .iter()
            .find(|(key, _, _)| *key == id)
            .map(|(_, _, jpeg)| jpeg.clone())
    }
}

fn frames() -> &'static Mutex<Frames> {
    static FRAMES: OnceLock<Mutex<Frames>> = OnceLock::new();
    FRAMES.get_or_init(Default::default)
}

pub(crate) fn interactive_reply(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() < HEADER + MIN_JPEG_BYTES
        || bytes.len() > HEADER + MAX_FRAME_BYTES
        || bytes.get(HEADER..HEADER + 2) != Some(&[0xff, 0xd8])
    {
        return bytes;
    }
    let jpeg = bytes.split_off(HEADER);
    let id = frames()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(jpeg, Instant::now())
        .expect("bounded frame fits cache");
    let mut reply = Vec::with_capacity(TAG.len() + HEADER + 36);
    reply.extend_from_slice(TAG);
    reply.extend_from_slice(&bytes);
    reply.extend_from_slice(id.to_string().as_bytes());
    reply
}

#[cfg(target_os = "linux")]
pub(crate) fn serve(request: tauri::http::Request<Vec<u8>>, responder: tauri::UriSchemeResponder) {
    let jpeg = (request.method() == tauri::http::Method::GET)
        .then(|| Uuid::parse_str(request.uri().path().trim_start_matches('/')).ok())
        .flatten()
        .and_then(|id| {
            frames()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(id, Instant::now())
        });
    let (status, body) = match jpeg {
        Some(jpeg) => (200, jpeg),
        None => (404, Vec::new()),
    };
    responder.respond(
        tauri::http::Response::builder()
            .status(status)
            .header("Content-Type", "image/jpeg")
            .header("Cache-Control", "no-store")
            .header("Access-Control-Allow-Origin", "*")
            .header("X-Content-Type-Options", "nosniff")
            .body(body)
            .expect("static preview response headers"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frames_are_bounded_retryable_and_expire() {
        let mut cache = Frames::default();
        let now = Instant::now();
        let first = cache.insert(vec![1, 2, 3], now).unwrap();
        assert_eq!(cache.get(first, now), Some(vec![1, 2, 3]));
        assert_eq!(cache.get(first, now), Some(vec![1, 2, 3]));
        for _ in 0..MAX_FRAMES {
            cache.insert(vec![0], now);
        }
        assert!(cache.get(first, now).is_none());
        assert_eq!(cache.entries.len(), MAX_FRAMES);
        assert!(cache.insert(vec![0; MAX_BYTES + 1], now).is_none());
        cache.prune(now + TTL);
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn byte_limit_evicts_frames_even_below_the_count_limit() {
        let mut cache = Frames::default();
        let now = Instant::now();
        let first = cache.insert(vec![0; MAX_BYTES / 2 + 1], now).unwrap();
        cache.insert(vec![0; MAX_BYTES / 2 + 1], now);
        assert!(cache.get(first, now).is_none());
        assert_eq!(cache.entries.len(), 1);
    }
    #[test]
    fn small_or_non_jpeg_packets_keep_the_original_transport() {
        for bytes in [vec![0; 40], vec![0; HEADER + MIN_JPEG_BYTES]] {
            assert_eq!(interactive_reply(bytes.clone()), bytes);
        }
        let mut bytes = vec![0; HEADER + MIN_JPEG_BYTES];
        bytes[HEADER..HEADER + 2].copy_from_slice(&[0xff, 0xd8]);
        let reply = interactive_reply(bytes);
        assert_eq!(&reply[..TAG.len()], TAG);
        assert_eq!(reply.len(), TAG.len() + HEADER + 36);
        let id =
            Uuid::parse_str(std::str::from_utf8(&reply[TAG.len() + HEADER..]).unwrap()).unwrap();
        assert!(frames().lock().unwrap().get(id, Instant::now()).is_some());
    }
}
