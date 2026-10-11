//! Process-wide helpers used by every Rawtonic engine layer (ADR 0009).
//! Nothing here knows about Tauri, the catalog or the develop model.

pub mod allocator;
#[cfg(feature = "performance-profiling")]
pub mod performance;
