//! The stack of every thread that renders (export lanes, previews,
//! thumbnails, multi view cells, GPU start-up and pipeline compiles).
//!
//! A render reaches the graphics driver from deep in our code: in the
//! driver, `queue.submit` (a texture clear begins a render pass) needs about
//! 86 KB of stack on AMD's Linux driver (RADV), and the driver's shader compiler
//! on a cold pipeline cache can need more. Rust's default thread stack is
//! 2 MiB. Until 2026-11-06 our own render frames used 2.0 MB of that, so
//! every app export on RADV aborted with a stack overflow, while the CLI,
//! whose command thread has this stack, never failed (notes:
//! benchmark-results/radv-app-vs-cli-2026-11-06/REPORT.md, gap G74).
//!
//! Two guards keep that from returning: the render keeps its large uniform
//! block on the heap, so its frames stay small (`RENDER_FRAME_BUDGET`, tested
//! by a full export on a thread of that size), and every render thread gets
//! this explicit stack, the main-thread default of Linux and macOS, so the
//! margin never depends on a platform default or `RUST_MIN_STACK`. The
//! memory is reserved, not committed: a thread uses only the pages it
//! touches.

/// Stack of every thread that renders or exports. Use it for every pool or
/// thread that calls into the GPU processor or wgpu ([`render_thread`],
/// `rayon::ThreadPoolBuilder::stack_size`, `tokio`'s `thread_stack_size`).
pub const RENDER_STACK_SIZE: usize = 8 * 1024 * 1024;

/// The stack a whole export must fit in, driver included: an eighth of
/// [`RENDER_STACK_SIZE`]. The export regression test renders on a thread of
/// this size, so frame growth fails a test long before it reaches a user's
/// export.
pub const RENDER_FRAME_BUDGET: usize = RENDER_STACK_SIZE / 8;

/// A thread builder named `name` with [`RENDER_STACK_SIZE`].
pub fn render_thread(name: impl Into<String>) -> std::thread::Builder {
    std::thread::Builder::new()
        .name(name.into())
        .stack_size(RENDER_STACK_SIZE)
}

/// Spawns `f` on a thread named `name` with [`RENDER_STACK_SIZE`]; panics
/// when the thread cannot start, as `std::thread::spawn` does.
pub fn spawn_render_thread<F, T>(name: &str, f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    render_thread(name)
        .spawn(f)
        .expect("failed to spawn a render thread")
}

/// [`spawn_render_thread`] for a thread of `scope`.
pub fn spawn_scoped_render_thread<'scope, F, T>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    name: &str,
    f: F,
) -> std::thread::ScopedJoinHandle<'scope, T>
where
    F: FnOnce() -> T + Send + 'scope,
    T: Send + 'scope,
{
    render_thread(name)
        .spawn_scoped(scope, f)
        .expect("failed to spawn a render thread")
}
