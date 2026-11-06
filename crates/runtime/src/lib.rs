//! Process-wide helpers used by every Rawtonic engine layer (ADR 0009).
//! Nothing here knows about Tauri, the catalog or the develop model.

pub mod allocator;
#[cfg(feature = "performance-profiling")]
pub mod performance;

mod cancel;
pub use cancel::CancelToken;
mod cancelled;
pub use cancelled::Cancelled;
mod render_stack;
pub use render_stack::{
    RENDER_FRAME_BUDGET, RENDER_STACK_SIZE, render_thread, spawn_render_thread,
    spawn_scoped_render_thread,
};
