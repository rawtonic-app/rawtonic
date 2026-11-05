//! Typed recipe keys.

use serde_json::Value;

/// A value a recipe key holds, read and written as JSON.
pub trait KeyValue: Sized {
    /// The value `json` holds, or None when it holds another type (a
    /// missing key, `null`, a string where a number belongs, …).
    fn from_json(json: &Value) -> Option<Self>;
    /// The JSON the value is saved as.
    fn to_json(&self) -> Value;
}

impl KeyValue for f64 {
    /// Any JSON number (an integer too), as the renderer has always read
    /// slider values (`as_f64`).
    fn from_json(json: &Value) -> Option<Self> {
        json.as_f64()
    }

    /// A finite number; a non-finite one is saved as `null`, as serde_json
    /// writes it.
    fn to_json(&self) -> Value {
        Value::from(*self)
    }
}

impl KeyValue for bool {
    fn from_json(json: &Value) -> Option<Self> {
        json.as_bool()
    }

    fn to_json(&self) -> Value {
        Value::Bool(*self)
    }
}

impl KeyValue for u64 {
    fn from_json(json: &Value) -> Option<Self> {
        json.as_u64()
    }
    fn to_json(&self) -> Value {
        (*self).into()
    }
}

/// One key of the recipe: its name in the saved JSON, the value it means
/// when absent (or of another type), and the version of its section's
/// version key (`toneVersion` for [`crate::tone`], …) from which the
/// renderer reads it (1: always).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key<T> {
    name: &'static str,
    default: T,
    since_version: u64,
}

impl<T> Key<T> {
    pub const fn new(name: &'static str, default: T, since_version: u64) -> Self {
        Self {
            name,
            default,
            since_version,
        }
    }

    /// The key's name in the saved JSON.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The first version of the section's version key that reads the key.
    pub const fn since_version(&self) -> u64 {
        self.since_version
    }
}

impl<T: Clone> Key<T> {
    /// What the key means when the recipe does not hold it.
    pub fn default(&self) -> T {
        self.default.clone()
    }
}

impl KeyValue for String {
    fn from_json(json: &Value) -> Option<Self> {
        json.as_str().map(str::to_owned)
    }
    fn to_json(&self) -> Value {
        Value::String(self.clone())
    }
}

impl KeyValue for Value {
    fn from_json(json: &Value) -> Option<Self> {
        Some(json.clone())
    }
    fn to_json(&self) -> Value {
        self.clone()
    }
}
