//! The grid's tiles of a camera JPEG are made without a float copy of the
//! whole image (ADF2, notes: benchmark-results/adf2-2026-11-22): from
//! 2026-11-12 every tile converted the 8-bit JPEG to 12 bytes per pixel
//! first, 290 MB for a 24 MP photo on each of four thumbnail threads, and the
//! Library's peak memory when adding 1,000 RAWs went from 0.97 to 2.2 GB.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(live, Ordering::SeqCst);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn tiles_of_a_24_mp_camera_jpeg_need_no_full_size_float_copy() {
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(6000, 4000, |x, y| {
        image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x ^ y) % 256) as u8])
    }));
    let before = LIVE.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let (medium, small) =
        rawtonic_engine::image_processing::resample_display_pair(&image, 1280, 480);
    let extra = PEAK.load(Ordering::SeqCst) - before;
    assert_eq!((medium.width(), medium.height()), (1280, 853));
    assert_eq!((small.width(), small.height()), (480, 320));
    // The vertical pass's rows (61 MB) and the tiles; a float copy of the
    // source alone would be 288 MB.
    assert!(extra < 120 << 20, "{} MB beside the source", extra >> 20);
}
