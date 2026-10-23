//! `rawtonic search-bench`: the library search on a synthetic library
//! (G37's 50k-photograph budget). Builds a session of tiny files in a
//! temporary folder with varied capture facts, ratings, labels, flags and
//! keywords, registers them as the app does (planned, then inserted in
//! batches), then times typical searches, value lists and smart album
//! counts. Measures the catalog only: no IPC, no grid rendering.
use anyhow::{Context, Result};
use rawtonic_catalog::{CaptureFacts, Contents, Library, Mode, Rule, query::FACETS};
use rawtonic_metadata::Metadata;
use serde_json::{Value, json};
use std::time::Instant;

/// Deterministic numbers without a dependency (xorshift64).
struct Numbers(u64);

impl Numbers {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const CAMERAS: &[(&str, &str)] = &[
    ("SONY", "ILCE-7RM4"),
    ("SONY", "ILCE-7M3"),
    ("SONY", "ILCE-1"),
    ("Canon", "Canon EOS R5"),
    ("Canon", "Canon EOS R6"),
    ("Canon", "Canon EOS 5D Mark IV"),
    ("NIKON CORPORATION", "NIKON Z 7"),
    ("NIKON CORPORATION", "NIKON Z 8"),
    ("NIKON CORPORATION", "NIKON D850"),
    ("FUJIFILM", "X-T4"),
    ("FUJIFILM", "GFX 100S"),
    ("OM Digital Solutions", "OM-1"),
    ("Panasonic", "DC-S5"),
    ("LEICA CAMERA AG", "LEICA Q2"),
    ("Apple", "iPhone 15 Pro"),
];
const LENSES: &[&str] = &[
    "FE 24-70mm F2.8 GM II",
    "FE 85mm F1.4 GM",
    "FE 70-200mm F2.8 GM OSS II",
    "FE 16-35mm F2.8 GM",
    "RF24-70mm F2.8 L IS USM",
    "RF85mm F1.2 L USM",
    "RF100-500mm F4.5-7.1 L IS USM",
    "NIKKOR Z 24-70mm f/2.8 S",
    "NIKKOR Z 50mm f/1.8 S",
    "XF16-55mmF2.8 R LM WR",
    "GF110mmF2 R LM WR",
    "M.12-40mm F2.8 PRO",
    "LUMIX S 20-60/F3.5-5.6",
    "SUMMILUX 1:1.7/28 ASPH.",
];
const ISOS: &[u32] = &[64, 100, 160, 200, 400, 800, 1600, 3200, 6400, 12800, 25600];
const APERTURES: &[f64] = &[1.2, 1.4, 1.8, 2.0, 2.8, 4.0, 5.6, 8.0, 11.0, 16.0];
const EXPOSURES: &[(u32, u32)] = &[
    (1, 8000),
    (1, 2000),
    (1, 500),
    (1, 250),
    (1, 125),
    (1, 60),
    (1, 15),
    (1, 2),
    (2, 1),
    (30, 1),
];
const LABELS: &[&str] = &["", "", "", "Red", "Yellow", "Green", "Blue", "Purple"];

fn vocabulary() -> Vec<String> {
    let base = [
        "wedding",
        "bride",
        "groom",
        "portrait",
        "landscape",
        "mountain",
        "beach",
        "city",
        "night",
        "family",
        "children",
        "dog",
        "cat",
        "street",
        "architecture",
        "food",
        "travel",
        "concert",
        "sport",
        "football",
        "studio",
        "product",
        "fashion",
        "macro",
        "flower",
        "bird",
        "wildlife",
        "sunset",
        "snow",
        "forest",
    ];
    // 300 keywords: the base words and numbered client/place names.
    let mut words: Vec<String> = base.iter().map(|w| w.to_string()).collect();
    words.extend((0..270).map(|i| format!("client {i:03}")));
    words
}

/// A synthetic photograph `i`: its folder and file name, facts, metadata.
fn photograph(i: u32, numbers: &mut Numbers, words: &[String]) -> (String, CaptureFacts, Metadata) {
    let day = i / 100; // 100 photographs a day, one folder a day
    let date =
        chrono::NaiveDate::from_ymd_opt(2023, 1, 1).unwrap() + chrono::Days::new(u64::from(day));
    let (make, model) = CAMERAS[numbers.below(CAMERAS.len())];
    let jpeg = numbers.below(10) == 0;
    let name = format!(
        "{}/DSC{:05}.{}",
        date.format("%Y-%m-%d"),
        i % 100_000,
        if jpeg { "JPG" } else { "ARW" }
    );
    let (n, d) = EXPOSURES[numbers.below(EXPOSURES.len())];
    let facts = CaptureFacts {
        captured: Some(format!(
            "{} {:02}:{:02}:{:02}",
            date.format("%Y-%m-%d"),
            8 + i % 12,
            i % 60,
            (i * 7) % 60
        )),
        make: Some(make.into()),
        model: Some(model.into()),
        lens: Some(LENSES[numbers.below(LENSES.len())].into()),
        focal_length: Some(
            [14.0, 24.0, 35.0, 50.0, 85.0, 105.0, 135.0, 200.0, 400.0][numbers.below(9)],
        ),
        aperture: Some(APERTURES[numbers.below(APERTURES.len())]),
        iso: Some(ISOS[numbers.below(ISOS.len())]),
        exposure: Some(f64::from(n) / f64::from(d)),
        camera_rating: None,
    };
    let metadata = Metadata {
        rating: [0, 0, 0, 1, 2, 3, 3, 4, 5][numbers.below(9)],
        label: LABELS[numbers.below(LABELS.len())].into(),
        flag: [0, 0, 0, 1, -1][numbers.below(5)],
        keywords: (0..numbers.below(7))
            .map(|_| words[numbers.below(words.len())].clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        ..Metadata::default()
    };
    (name, facts, metadata)
}

/// Median, 95th percentile and maximum of `iterations` runs of `work`, in
/// milliseconds, and what the last run returned.
fn time<T>(iterations: u32, mut work: impl FnMut() -> Result<T>) -> Result<(Value, T)> {
    let mut timings = Vec::new();
    let mut last = None;
    for _ in 0..iterations {
        let now = Instant::now();
        last = Some(work()?);
        timings.push(now.elapsed().as_secs_f64() * 1000.0);
    }
    timings.sort_by(f64::total_cmp);
    let p95 = timings[((timings.len() - 1) as f64 * 0.95).ceil() as usize];
    let round = |v: f64| (v * 100.0).round() / 100.0;
    Ok((
        json!({"median": round(timings[timings.len() / 2]), "p95": round(p95), "max": round(*timings.last().unwrap())}),
        last.context("No runs")?,
    ))
}

fn rule(value: Value) -> Rule {
    serde_json::from_value(value).expect("benchmark rules are valid")
}

pub fn run(photos: u32, iterations: u32) -> Result<()> {
    anyhow::ensure!(
        (1_000..=500_000).contains(&photos),
        "Use 1000–500000 photos"
    );
    anyhow::ensure!((3..=1000).contains(&iterations), "Use 3–1000 iterations");
    let temp = tempfile::tempdir()?;
    let directory = temp.path().join("Bench");
    let lib = Library::create(&directory, "Search benchmark", Mode::Session)?;
    let capture = directory.join("Capture");
    let words = vocabulary();
    let mut numbers = Numbers(0x9E37_79B9_7F4A_7C15);

    let started = Instant::now();
    let mut batch = Vec::new();
    for i in 0..photos {
        let (name, facts, metadata) = photograph(i, &mut numbers, &words);
        let path = capture.join(&name);
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        std::fs::write(&path, i.to_le_bytes())?;
        let contents = Contents {
            metadata: Some(metadata),
            ..Contents::default()
        };
        batch.push(lib.plan_add(&path, contents)?.with_facts(facts));
        if batch.len() == 5_000 {
            lib.insert_planned(std::mem::take(&mut batch))?;
        }
    }
    lib.insert_planned(batch)?;
    let build_s = started.elapsed().as_secs_f64();

    // Ten smart albums of the kinds photographers keep.
    let smart_rules = [
        json!({"field":"rating","op":"ge","value":4}),
        json!({"field":"flag","op":"is","value":"picked"}),
        json!({"field":"keyword","op":"is","value":"wedding"}),
        json!({"field":"captureDate","op":"inLast","value":{"amount":30,"unit":"days"}}),
        json!({"combine":"all","rules":[{"field":"camera","op":"is","value":"ILCE-7RM4"},{"field":"iso","op":"ge","value":3200}]}),
        json!({"combine":"all","rules":[{"field":"lens","op":"contains","value":"85mm"},{"field":"aperture","op":"le","value":1.8}]}),
        json!({"field":"label","op":"is","value":["red","yellow"]}),
        json!({"combine":"any","rules":[{"field":"keyword","op":"is","value":"bride"},{"field":"keyword","op":"is","value":"groom"}]}),
        json!({"field":"fileType","op":"is","value":"jpeg"}),
        json!({"combine":"all","rules":[{"field":"captureDate","op":"between","value":["2023-06-01","2023-08-31"]},{"combine":"none","rules":[{"field":"flag","op":"is","value":"rejected"}]}]}),
    ];
    for (i, rules) in smart_rules.iter().enumerate() {
        lib.create_smart_collection(&format!("Smart {i}"), None, &rule(rules.clone()))?;
    }
    drop(lib);

    let mut results = serde_json::Map::new();
    let (open, lib) = time(iterations, || Library::open(&directory))?;
    results.insert("open_library".into(), open);
    let folder = capture.join("2023-03-15");
    let searches = [
        ("all_paths", json!({"combine":"all","rules":[]})),
        (
            "text_keyword",
            json!({"field":"text","op":"contains","value":"wedding"}),
        ),
        (
            "text_filename_part",
            json!({"field":"text","op":"contains","value":"0123"}),
        ),
        (
            "text_short_word",
            json!({"field":"text","op":"contains","value":"Z 8"}),
        ),
        (
            "text_two_words",
            json!({"field":"text","op":"contains","value":"bride 85mm"}),
        ),
        (
            "camera_and_iso",
            json!({"combine":"all","rules":[{"field":"camera","op":"is","value":"ILCE-7RM4"},{"field":"iso","op":"ge","value":1600}]}),
        ),
        (
            "rating_flag_label",
            json!({"combine":"all","rules":[{"field":"rating","op":"ge","value":3},{"field":"flag","op":"is","value":"picked"},{"field":"label","op":"is","value":"red"}]}),
        ),
        (
            "nested_any_none",
            json!({"combine":"all","rules":[
            {"combine":"any","rules":[{"field":"make","op":"is","value":"Canon"},{"field":"lens","op":"startsWith","value":"FE"}]},
            {"combine":"none","rules":[{"field":"keyword","op":"is","value":"client 007"},{"field":"rating","op":"is","value":0}]},
            {"field":"captureDate","op":"between","value":["2023-01-01","2023-12-31"]}]}),
        ),
        (
            "folder_scope_text",
            json!({"combine":"all","rules":[{"field":"folder","op":"contains","value":folder.to_string_lossy()},{"field":"text","op":"contains","value":"DSC"}]}),
        ),
    ];
    let mut matched = serde_json::Map::new();
    for (name, value) in searches {
        let search = rule(value);
        let (timing, paths) = time(iterations, || lib.search(&search))?;
        results.insert(name.into(), timing);
        matched.insert(name.into(), json!(paths.len()));
    }
    let everything = Rule::everything();
    let (facets, values) = time(iterations, || lib.facets(&everything, FACETS))?;
    results.insert("facets_all_fields".into(), facets);
    let one_camera = rule(json!({"field":"camera","op":"is","value":"ILCE-7RM4"}));
    let (scoped, _) = time(iterations, || lib.facets(&one_camera, FACETS))?;
    results.insert("facets_within_camera".into(), scoped);
    let (albums, listed) = time(iterations, || lib.collections())?;
    results.insert("albums_with_10_smart_counts".into(), albums);
    // One metadata write with the keyword index kept by its triggers.
    let first = lib
        .search_ids(&everything)?
        .into_iter()
        .next()
        .context("No photographs")?;
    let mut meta = lib.metadata(&first)?;
    let (write, _) = time(iterations, || {
        meta.keywords.push("benchmark".into());
        lib.set_metadata(&first, &meta)
    })?;
    results.insert("metadata_write_with_keywords".into(), write);

    crate::print(json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "fixture": format!("synthetic session, {photos} photographs ({} cameras, {} lenses, {} keywords, 100 per folder), SQLite WAL FULL, warm page cache", CAMERAS.len(), LENSES.len(), words.len()),
        "build_seconds": (build_s * 10.0).round() / 10.0,
        "iterations": iterations,
        "ms": results,
        "matches": matched,
        "facet_values": values.iter().map(|(field, v)| (format!("{field:?}"), v.len())).collect::<std::collections::BTreeMap<_, _>>(),
        "smart_album_counts": listed.iter().map(|c| c.count).collect::<Vec<_>>(),
        "covers": "catalog queries in one process, returning paths",
        "not_covered": "IPC to the webview, grid filtering and rendering (measured separately in the app)",
    }))
}
