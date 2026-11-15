//! A display frame that waits for its variant gives way to a newer request
//! (G76/SUPS). Under a compile limit a frame can wait for a compile for
//! most of a minute: the first Halation frame on a cold Radeon 680M waited
//! ~53 s for its variant in a helper, a Contrast edit 18.6 s for the
//! planned one (G76/COLD2, WCOLD2C). The processor renders one frame at a
//! time, so turning Halation off again or opening another photo showed only
//! after that compile ended. Lightroom and Capture One always show the
//! latest state: here a caller that can drop a frame (the Develop view's
//! preview worker) renders it inside [`supersedable`], and while the frame
//! waits for a compile it ends as soon as the caller's check says a newer
//! request is waiting, with [`FRAME_SUPERSEDED`]. Only the wait ends; the
//! compile goes on (a helper's job moves to idle time, `idle_compiles`).
//! Renders outside [`supersedable`] (exports, thumbnails) never skip.

use std::cell::RefCell;

/// The error of a frame a newer request replaced while it waited
/// ([`supersedable`]); [`is_superseded`] tells it apart from failures.
pub const FRAME_SUPERSEDED: &str =
    "Frame superseded by a newer request while it waited for graphics";

/// Whether a render's `error` is [`FRAME_SUPERSEDED`] (also wrapped in
/// another message).
pub fn is_superseded(error: &str) -> bool {
    error.contains(FRAME_SUPERSEDED)
}

type Check = Box<dyn Fn() -> bool>;

thread_local! {
    static CHECK: RefCell<Option<Check>> = const { RefCell::new(None) };
}

/// Runs `render` on this thread so that a frame of it that waits for a
/// compile ends with [`FRAME_SUPERSEDED`] once `superseded` holds (asked
/// every few milliseconds while it waits, never otherwise). Nested calls
/// keep the outer check afterwards.
pub fn supersedable<T>(superseded: impl Fn() -> bool + 'static, render: impl FnOnce() -> T) -> T {
    struct Restore(Option<Check>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let outer = self.0.take();
            CHECK.with(|check| *check.borrow_mut() = outer);
        }
    }
    let outer = CHECK.with(|check| check.borrow_mut().replace(Box::new(superseded)));
    let _restore = Restore(outer);
    render()
}

/// Whether the render running on this thread was replaced by a newer
/// request (`supersedable`); false outside one.
pub(super) fn superseded() -> bool {
    CHECK.with(|check| check.borrow().as_ref().is_some_and(|check| check()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn only_renders_inside_supersedable_can_be_superseded() {
        assert!(!superseded(), "an export or thumbnail never skips");
        let newer = Rc::new(Cell::new(false));
        // The check stays on this thread: it need not be `Send`.
        let flag = Rc::clone(&newer);
        let outcome = supersedable(
            move || flag.get(),
            || {
                assert!(!superseded());
                newer.set(true);
                superseded()
            },
        );
        assert!(outcome);
        assert!(!superseded(), "the check ends with the render");
    }

    #[test]
    fn a_nested_render_restores_the_outer_check() {
        supersedable(
            || true,
            || {
                supersedable(|| false, || assert!(!superseded()));
                assert!(superseded());
            },
        );
        assert!(!superseded());
    }

    #[test]
    fn the_error_is_recognised_also_when_wrapped() {
        assert!(is_superseded(FRAME_SUPERSEDED));
        assert!(is_superseded(&format!("Render failed: {FRAME_SUPERSEDED}")));
        assert!(!is_superseded("Preview rendering failed: out of memory"));
    }
}
