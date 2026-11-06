//! The develop recipe (plan 2.2, notes: plans/architecture-cleanup-2026-11.md).
//!
//! Two representations:
//! - [`StoredRecipe`] (and its borrowed view [`StoredRef`]) is the recipe
//!   as saved: the JSON object exactly as read, with key presence, explicit
//!   `null`s, unknown and nested fields untouched. It is read and written
//!   only through typed keys ([`Key`]), one registry per section
//!   ([`tone`], …). Nothing is filled in on load or save, so presence rules
//!   such as `is_new_recipe` keep working and a recipe saved without an
//!   edit is the value it was read as.
//! - [`ResolvedRecipe`] is what renders: typed sections of value objects,
//!   produced only by [`normalize`] from a stored recipe and never written
//!   back.
//!
//! The crate sits below the engine (the engine depends on it, never the
//! reverse) and depends on serde and serde_json only
//! (`scripts/check_layering.sh`). Sections move here one at a time; the
//! keys of a section not moved yet are still read by the engine itself.

pub mod color;
pub mod detail;
mod key;
mod resolved;
mod stored;
pub mod tone;
pub mod ts;

pub use key::{Key, KeyValue};
pub use resolved::{ResolvedRecipe, normalize};
pub use stored::{Section, StoredRecipe, StoredRef};
