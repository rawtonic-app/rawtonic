//! Brush strokes the editor sent once and refers to since.
//!
//! A layer stack of brush layers holds thousands of points. A preview request
//! carries the whole recipe, so with many layers parsing and copying the
//! unchanged strokes of every layer cost more than the frame (notes: WP6 of
//! plans/local-adjustments-g29-g31.md). The editor therefore sends a brush
//! component's `parameters` once with a `paramsToken`, and afterwards
//! `{"$paramsRef": token}` in their place. The recipe stays unchanged: tokens
//! exist only in preview requests and in this store.
//!
//! A reference to a token the store does not hold (after a restart or an
//! eviction) fails the request with [`MISSING_PARAMETERS`]; the editor then
//! sends the full parameters again.

use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// The key of a reference in place of a component's `parameters`.
pub const REFERENCE_KEY: &str = "$paramsRef";
/// The component key naming full parameters for later references.
pub const TOKEN_KEY: &str = "paramsToken";
/// The start of the error of a request with an unknown reference.
pub const MISSING_PARAMETERS: &str = "MASK_PARAMETERS_MISSING";

const STORE_BYTE_BUDGET: usize = 256 * 1024 * 1024;
const STORE_ENTRY_LIMIT: usize = 4096;

struct Stored {
    token: String,
    hash: u64,
    parameters: Arc<Value>,
    bytes: usize,
}

#[derive(Default)]
struct Store {
    entries: VecDeque<Stored>,
    bytes: usize,
}

impl Store {
    fn get(&mut self, token: &str) -> Option<(u64, Arc<Value>)> {
        let index = self.entries.iter().rposition(|e| e.token == token)?;
        let entry = self.entries.remove(index)?;
        let found = (entry.hash, Arc::clone(&entry.parameters));
        self.entries.push_back(entry);
        Some(found)
    }

    fn insert(&mut self, token: String, hash: u64, parameters: Value, bytes: usize) {
        if let Some(index) = self.entries.iter().position(|e| e.token == token) {
            let old = self.entries.remove(index).unwrap();
            self.bytes -= old.bytes;
        }
        while !self.entries.is_empty()
            && (self.entries.len() >= STORE_ENTRY_LIMIT || self.bytes + bytes > STORE_BYTE_BUDGET)
        {
            let old = self.entries.pop_front().unwrap();
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        self.entries.push_back(Stored {
            token,
            hash,
            parameters: Arc::new(parameters),
            bytes,
        });
    }
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

fn with_store<T>(f: impl FnOnce(&mut Store) -> T) -> T {
    let mut store = STORE.lock().unwrap_or_else(|e| e.into_inner());
    f(store.get_or_insert_with(Store::default))
}

/// The token of a reference in place of parameters.
fn reference(parameters: &Value) -> Option<&str> {
    let object = parameters.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object.get(REFERENCE_KEY)?.as_str()
}

/// Stores the parameters a request names with a token and checks that
/// every reference is known. Only brush components of `masks` take part.
pub fn hydrate(adjustments: &mut Value) -> Result<(), String> {
    let Some(masks) = adjustments.get_mut("masks").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let mut missing = Vec::new();
    for sub in masks
        .iter_mut()
        .filter_map(|mask| mask.get_mut("subMasks").and_then(Value::as_array_mut))
        .flatten()
        .filter_map(Value::as_object_mut)
    {
        let token = sub.remove(TOKEN_KEY);
        if sub.get("type").and_then(Value::as_str) != Some("brush") {
            continue;
        }
        let Some(parameters) = sub.get("parameters") else {
            continue;
        };
        if let Some(token) = reference(parameters) {
            if with_store(|store| store.get(token)).is_none() {
                missing.push(token.to_string());
            }
        } else if let Some(Value::String(token)) = token {
            let (hash, bytes) = measured_hash(parameters);
            let parameters = parameters.clone();
            with_store(|store| store.insert(token, hash, parameters, bytes));
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("{MISSING_PARAMETERS}: {}", missing.join(",")))
    }
}

/// The parameters a reference stands for, or `parameters` themselves.
pub fn resolve(parameters: &Value) -> Option<Arc<Value>> {
    let token = reference(parameters)?;
    with_store(|store| store.get(token)).map(|(_, parameters)| parameters)
}

/// A hash of a component's parameters by content: the same for the full
/// parameters and a reference to them.
pub fn content_hash(parameters: &Value) -> u64 {
    if let Some(token) = reference(parameters)
        && let Some((hash, _)) = with_store(|store| store.get(token))
    {
        return hash;
    }
    measured_hash(parameters).0
}

/// Calls `f` with the parameters a reference stands for, else `parameters`.
pub fn with_resolved<T>(parameters: &Value, f: impl FnOnce(&Value) -> T) -> T {
    match resolve(parameters) {
        Some(resolved) => f(&resolved),
        None => f(parameters),
    }
}

/// A content hash and an estimate of the value's heap size.
fn measured_hash(value: &Value) -> (u64, usize) {
    let mut hasher = DefaultHasher::new();
    let mut bytes = 0;
    hash_value(value, &mut hasher, &mut bytes);
    (hasher.finish(), bytes)
}

/// Equal values (as parsed) hash equally; a number's integer or float
/// representation is part of the hash.
fn hash_value(value: &Value, hasher: &mut DefaultHasher, bytes: &mut usize) {
    *bytes += std::mem::size_of::<Value>();
    match value {
        Value::Null => 0u8.hash(hasher),
        Value::Bool(b) => (1u8, *b).hash(hasher),
        Value::Number(n) => {
            if let Some(v) = n.as_u64() {
                (2u8, v).hash(hasher)
            } else if let Some(v) = n.as_i64() {
                (3u8, v).hash(hasher)
            } else {
                (4u8, n.as_f64().unwrap_or(f64::NAN).to_bits()).hash(hasher)
            }
        }
        Value::String(text) => {
            *bytes += text.len();
            (5u8, text).hash(hasher)
        }
        Value::Array(items) => {
            (6u8, items.len()).hash(hasher);
            for item in items {
                hash_value(item, hasher, bytes);
            }
        }
        Value::Object(map) => {
            (7u8, map.len()).hash(hasher);
            for (key, item) in map {
                *bytes += key.len() + 3 * std::mem::size_of::<usize>();
                key.hash(hasher);
                hash_value(item, hasher, bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn brush(id: &str, parameters: Value, token: Option<&str>) -> Value {
        let mut sub = json!({"id": id, "type": "brush", "visible": true, "mode": "additive",
            "parameters": parameters});
        if let Some(token) = token {
            sub[TOKEN_KEY] = json!(token);
        }
        sub
    }

    #[test]
    fn references_stand_for_the_parameters_sent_with_their_token() {
        let lines = json!({"lines": [{"brushSize": 40, "points": [{"x": 1.5, "y": 2}]}]});
        let token = "test-ref-1#1";
        let mut first = json!({"masks": [{"subMasks": [brush("b", lines.clone(), Some(token))]}]});
        hydrate(&mut first).unwrap();
        // The token is not part of the recipe the renderer reads.
        assert_eq!(
            first["masks"][0]["subMasks"][0],
            brush("b", lines.clone(), None)
        );

        let reference = json!({REFERENCE_KEY: token});
        let mut later = json!({"masks": [{"subMasks": [brush("b", reference.clone(), None)]}]});
        hydrate(&mut later).unwrap();
        assert_eq!(*resolve(&reference).unwrap(), lines);
        assert_eq!(content_hash(&reference), content_hash(&lines));
        with_resolved(&reference, |p| assert_eq!(*p, lines));
        assert!(resolve(&lines).is_none());

        // Different points, a different hash.
        let moved = json!({"lines": [{"brushSize": 40, "points": [{"x": 1.5, "y": 3}]}]});
        assert_ne!(content_hash(&moved), content_hash(&lines));
    }

    #[test]
    fn unknown_references_fail_the_request() {
        let mut request = json!({"masks": [{"subMasks": [
            brush("b", json!({REFERENCE_KEY: "test-ref-unknown"}), None)]}]});
        let error = hydrate(&mut request).unwrap_err();
        assert!(error.starts_with(MISSING_PARAMETERS), "{error}");
        assert!(error.contains("test-ref-unknown"));
    }

    #[test]
    fn a_reference_renders_and_caches_like_its_strokes() {
        use crate::mask_generation::{MaskBitmapCache, PhotoGeometry, cached_or_generated_mask};
        use crate::masks::MaskDefinition;
        let lines = |auto: bool| {
            json!({"lines": [{"tool": "brush", "brushSize": 20, "feather": 0.5, "opacity": 100,
                "flow": 100, "autoMask": auto,
                "points": [{"x": 10, "y": 10}, {"x": 50, "y": 40}]}]})
        };
        let layer = |parameters: Value| -> MaskDefinition {
            serde_json::from_value(json!({"id": "l", "name": "l", "visible": true,
                "invert": false, "opacity": 100, "adjustments": {},
                "subMasks": [brush("b", parameters, None)]}))
            .unwrap()
        };
        let token = "test-ref-render#1";
        let mut request = json!({"masks": [{"subMasks": [brush("b", lines(false), Some(token))]}]});
        hydrate(&mut request).unwrap();
        let referenced = layer(json!({REFERENCE_KEY: token}));
        let full = layer(lines(false));
        let geometry = PhotoGeometry::default();
        let mask = |cache: &Mutex<MaskBitmapCache>, def: &MaskDefinition| {
            cached_or_generated_mask(cache, def, 64, 48, 1.0, (0.0, 0.0), &geometry, None)
                .unwrap()
                .unwrap()
        };
        let cache = Mutex::new(MaskBitmapCache::new());
        let from_reference = mask(&cache, &referenced);
        // One cache entry for both forms: a slider move after a stroke
        // finds the stroke's mask.
        assert!(Arc::ptr_eq(&from_reference, &mask(&cache, &full)));
        let fresh = mask(&Mutex::new(MaskBitmapCache::new()), &full);
        assert_eq!(from_reference.as_raw(), fresh.as_raw());
        assert!(fresh.as_raw().iter().any(|v| *v > 0));

        let auto = "test-ref-render#2";
        let mut request = json!({"masks": [{"subMasks": [brush("b", lines(true), Some(auto))]}]});
        hydrate(&mut request).unwrap();
        assert!(layer(json!({REFERENCE_KEY: auto})).requires_warped_image());
        assert!(!referenced.requires_warped_image());
    }

    #[test]
    fn integer_and_float_numbers_hash_apart() {
        assert_ne!(
            content_hash(&json!({"x": 1})),
            content_hash(&json!({"x": 1.0}))
        );
        assert_eq!(
            content_hash(&json!({"x": 1.0})),
            content_hash(&json!({"x": 1.0}))
        );
    }
}
