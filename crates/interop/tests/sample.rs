//! Reads a real library given by the environment, read-only, and prints a
//! summary: `RAWTONIC_INTEROP_SAMPLE=/path/x.lrcat cargo test -p rawtonic-interop --test sample -- --ignored --nocapture`.
use std::path::Path;

#[test]
#[ignore]
fn summarize_a_real_library() {
    let path = std::env::var("RAWTONIC_INTEROP_SAMPLE").expect("RAWTONIC_INTEROP_SAMPLE");
    let path = Path::new(&path);
    let before = std::fs::read(path).unwrap();
    let library = if path.extension().is_some_and(|e| e == "lrcat") {
        rawtonic_interop::lightroom::read(path).unwrap()
    } else {
        rawtonic_interop::capture_one::read(path).unwrap()
    };
    assert_eq!(
        std::fs::read(path).unwrap(),
        before,
        "the original is untouched"
    );
    let copies = library
        .photos
        .iter()
        .filter(|p| p.copy_of.is_some())
        .count();
    let edited = library
        .photos
        .iter()
        .filter(|p| !matches!(p.develop, rawtonic_interop::Develop::None))
        .count();
    let rated = library
        .photos
        .iter()
        .filter(|p| p.metadata.rating > 0)
        .count();
    println!(
        "{}: {} photos ({copies} copies, {edited} with settings, {rated} rated), {} albums, not read {:?}",
        library.name,
        library.photos.len(),
        library.albums.len(),
        library.not_read
    );
    for root in library.roots() {
        println!("  {} photos, {} found", root.photos, root.found);
    }
}
