//! The import dialog's 100 % view of a RAW whose camera JPEG is small: the
//! part the view shows is rendered first, from the same decode, transform
//! and upload as the whole photograph that follows (notes:
//! benchmark-results/impd4-import-zoom-2026-11-14).
//!
//! The view asks for its region (`wait`) while the photograph's full-size
//! render (`session_ingest_full_preview`) runs; that render serves it once
//! the photograph is on the GPU (`serve`), before rendering all of it. One
//! region is wanted at a time: a newer wish, a cancel or the end of the
//! photograph's render answers an older one with nothing.

use std::sync::{Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Pixels rendered around the view on each side, for a small pan.
pub const REGION_MARGIN: u32 = 256;

/// How long a wish waits for its photograph's render to reach it.
const WAIT: Duration = Duration::from_secs(20);

/// A region of the 100 % view: JPEG bytes of `rect` (x, y, width, height)
/// of the whole `full` (width, height) render.
#[derive(Debug, Clone, PartialEq)]
pub struct RegionView {
    pub jpeg: Vec<u8>,
    pub full: (u32, u32),
    pub rect: [u32; 4],
}

impl RegionView {
    /// The view as the frontend reads it: six little-endian u32 (full
    /// width and height, x, y, width, height), then the JPEG.
    pub fn to_bytes(&self) -> Vec<u8> {
        let [x, y, width, height] = self.rect;
        let mut bytes = Vec::with_capacity(24 + self.jpeg.len());
        for value in [self.full.0, self.full.1, x, y, width, height] {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(&self.jpeg);
        bytes
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Wish {
    id: u64,
    path: String,
    /// The view's centre as a share of the photograph's width and height.
    focus: (f64, f64),
    /// The view's size in image pixels (device pixels at 100 %).
    view: (u32, u32),
}

#[derive(Default)]
struct State {
    next: u64,
    wish: Option<Wish>,
    /// The wish a render is rendering the region of.
    serving: Option<u64>,
    /// The answer to wish `id` (`None`: nothing will come for it).
    answer: Option<(u64, Option<RegionView>)>,
}

#[derive(Default)]
pub struct Regions {
    state: Mutex<State>,
    changed: Condvar,
}

static REGIONS: LazyLock<Regions> = LazyLock::new(Regions::default);

/// The app's one set of region wishes.
pub fn regions() -> &'static Regions {
    &REGIONS
}

/// The region of a `full` (width, height) photograph a view of `view`
/// image pixels centred at `focus` (shares) shows, with `margin` pixels
/// around it, kept inside the photograph as the view keeps it (a view
/// larger than the photograph shows all of it). Its corner and size are
/// multiples of 8 where they are not the photograph's edge, so its JPEG
/// blocks are the whole render's.
pub fn region_rect(full: (u32, u32), focus: (f64, f64), view: (u32, u32), margin: u32) -> [u32; 4] {
    let axis = |size: u32, share: f64, room: u32| -> (u32, u32) {
        let room = room.min(size);
        let share = if share.is_finite() {
            share.clamp(0.0, 1.0)
        } else {
            0.5
        };
        let centre = share * f64::from(size);
        let start = (centre - f64::from(room) / 2.0)
            .round()
            .clamp(0.0, f64::from(size - room)) as u32;
        let from = start.saturating_sub(margin) / 8 * 8;
        let to = (start + room).saturating_add(margin).div_ceil(8) * 8;
        (from, to.min(size) - from)
    };
    let (x, width) = axis(full.0, focus.0, view.0);
    let (y, height) = axis(full.1, focus.1, view.1);
    [x, y, width, height]
}

impl Regions {
    /// Wants the region of `path` around `focus` for a view of `view`
    /// pixels and waits for it: `None` when the photograph's render did not
    /// reach it (it was rendered whole first, or it is not a render), when
    /// a newer wish or a cancel replaced it, or after `WAIT`.
    pub fn wait(&self, path: &str, focus: (f64, f64), view: (u32, u32)) -> Option<RegionView> {
        self.wait_for(path, focus, view, WAIT)
    }

    fn wait_for(
        &self,
        path: &str,
        focus: (f64, f64),
        view: (u32, u32),
        wait: Duration,
    ) -> Option<RegionView> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.next += 1;
        let id = state.next;
        state.answer = None;
        state.wish = Some(Wish {
            id,
            path: path.to_string(),
            focus,
            view,
        });
        self.changed.notify_all();
        let until = Instant::now() + wait;
        loop {
            if let Some((answered, _)) = &state.answer
                && *answered == id
            {
                return state.answer.take().and_then(|(_, view)| view);
            }
            // Replaced or cancelled, and no render is at work on it.
            if state.wish.as_ref().is_none_or(|wish| wish.id != id) && state.serving != Some(id) {
                return None;
            }
            let now = Instant::now();
            if now >= until {
                if state.wish.as_ref().is_some_and(|wish| wish.id == id) {
                    state.wish = None;
                }
                return None;
            }
            state = self
                .changed
                .wait_timeout(state, until - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Drops the wish for `path` (the view left it or 100 %, or has the
    /// whole photograph); a wish for another photograph stays.
    pub fn cancel(&self, path: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.wish.take_if(|wish| wish.path == path).is_some() {
            self.changed.notify_all();
        }
    }

    /// Called by `path`'s full-size render once the photograph (`full`
    /// pixels) is ready to render: renders the wanted region of it with
    /// `render` (rect → JPEG) when one is wanted.
    pub fn serve(
        &self,
        path: &str,
        full: (u32, u32),
        render: impl FnOnce([u32; 4]) -> Option<Vec<u8>>,
    ) {
        let wish = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let wish = state.wish.take_if(|wish| wish.path == path);
            state.serving = wish.as_ref().map(|wish| wish.id);
            wish
        };
        let Some(wish) = wish else {
            return;
        };
        let rect = region_rect(full, wish.focus, wish.view, REGION_MARGIN);
        let view = render(rect).map(|jpeg| RegionView { jpeg, full, rect });
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.serving = None;
        state.answer = Some((wish.id, view));
        self.changed.notify_all();
    }

    /// Called when `path`'s full-size render ends: a wish for it that came
    /// too late to be served is answered with nothing (the whole
    /// photograph is on its way).
    pub fn finish(&self, path: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(wish) = state.wish.take_if(|wish| wish.path == path) {
            state.answer = Some((wish.id, None));
            self.changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn the_region_is_the_view_around_the_focus_with_a_margin_on_the_jpeg_grid() {
        // Centred: 1920 × 1200 around the middle of 6000 × 4000, 256 more.
        let [x, y, w, h] = region_rect((6000, 4000), (0.5, 0.5), (1920, 1200), 256);
        assert_eq!([x, y], [1784, 1144]);
        assert_eq!([w, h], [2432, 1712]);
        assert!(x % 8 == 0 && y % 8 == 0 && w % 8 == 0 && h % 8 == 0);
        // Kept inside the photograph at its edges, as the view keeps it.
        assert_eq!(
            region_rect((6000, 4000), (1.0, 0.0), (1920, 1200), 256),
            [3824, 0, 2176, 1456]
        );
        assert_eq!(
            region_rect((6001, 4001), (1.0, 1.0), (1920, 1200), 0),
            [4080, 2800, 1921, 1201]
        );
        // A view larger than the photograph shows all of it.
        assert_eq!(
            region_rect((800, 600), (0.3, f64::NAN), (1920, 1200), 256),
            [0, 0, 800, 600]
        );
    }

    #[test]
    fn a_wish_is_served_by_its_photographs_render_and_others_are_answered_with_nothing() {
        let regions = Arc::new(Regions::default());
        let waiting = {
            let regions = Arc::clone(&regions);
            std::thread::spawn(move || regions.wait("a.RAF", (0.5, 0.5), (100, 100)))
        };
        // Another photograph's render does not take it.
        while regions.state.lock().unwrap().wish.is_none() {
            std::thread::yield_now();
        }
        regions.serve("b.RAF", (1000, 800), |_| panic!("not wanted"));
        regions.serve("a.RAF", (1000, 800), |rect| Some(vec![rect[0] as u8]));
        let view = waiting.join().unwrap().expect("served");
        assert_eq!(view.full, (1000, 800));
        assert_eq!(
            view.rect,
            region_rect((1000, 800), (0.5, 0.5), (100, 100), REGION_MARGIN)
        );
        assert_eq!(view.jpeg, vec![view.rect[0] as u8]);
        let bytes = view.to_bytes();
        assert_eq!(&bytes[..4], &1000u32.to_le_bytes());
        assert_eq!(&bytes[24..], &view.jpeg[..]);

        // A newer wish replaces an older one; a render's end answers its own.
        let older = {
            let regions = Arc::clone(&regions);
            std::thread::spawn(move || regions.wait("a.RAF", (0.5, 0.5), (100, 100)))
        };
        while regions.state.lock().unwrap().wish.is_none() {
            std::thread::yield_now();
        }
        let newer = {
            let regions = Arc::clone(&regions);
            std::thread::spawn(move || regions.wait("c.RAF", (0.5, 0.5), (100, 100)))
        };
        assert_eq!(older.join().unwrap(), None);
        regions.finish("a.RAF");
        regions.finish("c.RAF");
        assert_eq!(newer.join().unwrap(), None);

        // A cancel ends a wait; so does the time limit.
        let cancelled = {
            let regions = Arc::clone(&regions);
            std::thread::spawn(move || regions.wait("d.RAF", (0.5, 0.5), (100, 100)))
        };
        while regions.state.lock().unwrap().wish.is_none() {
            std::thread::yield_now();
        }
        regions.cancel("another.RAF");
        assert!(
            regions.state.lock().unwrap().wish.is_some(),
            "only its own path's"
        );
        regions.cancel("d.RAF");
        assert_eq!(cancelled.join().unwrap(), None);
        assert_eq!(
            regions.wait_for("e.RAF", (0.5, 0.5), (100, 100), Duration::from_millis(10)),
            None
        );
        assert!(regions.state.lock().unwrap().wish.is_none());
    }

    #[test]
    fn a_render_that_found_no_region_answers_with_nothing() {
        let regions = Arc::new(Regions::default());
        let waiting = {
            let regions = Arc::clone(&regions);
            std::thread::spawn(move || regions.wait("a.RAF", (0.5, 0.5), (100, 100)))
        };
        while regions.state.lock().unwrap().wish.is_none() {
            std::thread::yield_now();
        }
        regions.serve("a.RAF", (1000, 800), |_| None);
        assert_eq!(waiting.join().unwrap(), None);
    }
}
