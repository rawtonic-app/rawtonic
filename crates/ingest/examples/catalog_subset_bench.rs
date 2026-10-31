//! Times Export as Catalog (G39) of N photographs by reference from a
//! catalog of 2·N, each with a three-step edit history, metadata and an
//! album. The originals are small placeholder files: by reference they are
//! not read, so this measures the database work (snapshot, pruning,
//! re-pathing, compaction) and publishing, not copying originals.
//!
//! `cargo run --release -p rawtonic-ingest --example catalog_subset_bench -- 1000 <scratch folder>`
use rawtonic_catalog::{Library, Mode};
use rawtonic_ingest::catalog_export::{Options, export_catalog};
use serde_json::json;
use std::{fs, path::PathBuf, sync::atomic::AtomicBool, time::Instant};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let count: usize = args.next().unwrap_or_else(|| "1000".into()).parse()?;
    let root = PathBuf::from(args.next().expect("scratch folder"));
    anyhow::ensure!(
        !root.exists(),
        "{} exists; give a new folder",
        root.display()
    );
    fs::create_dir_all(root.join("Shoot"))?;
    let lib = Library::create(&root.join("Main"), "Main", Mode::Catalog)?;
    let setup = Instant::now();
    let mut files = Vec::new();
    for n in 0..count * 2 {
        let file = root.join("Shoot").join(format!("IMG_{n:05}.ARW"));
        fs::write(&file, format!("placeholder {n}"))?;
        files.push(file);
    }
    let report = lib.add_files(&files)?;
    let ids: Vec<String> = lib.assets(None)?.into_iter().map(|a| a.id).collect();
    anyhow::ensure!(
        ids.len() == count * 2,
        "added {} of {}: {report:?}",
        ids.len(),
        count * 2
    );
    for (n, id) in ids.iter().enumerate() {
        for step in 0..3 {
            lib.save_editor_state(
                id,
                &json!({"rating": n % 6, "adjustments": {"exposure": step as f64 * 0.3, "contrast": n % 50, "temperature": 5200 + step}}),
            )?;
        }
    }
    let album = lib.create_collection("Selects", None)?;
    lib.add_to_collection(&album.id, &ids[..count])?;
    lib.checkpoint()?;
    println!(
        "setup: {} photos in {:.1} s",
        ids.len(),
        setup.elapsed().as_secs_f64()
    );
    let chosen = &ids[..count];
    let started = Instant::now();
    let receipt = export_catalog(
        &lib.directory,
        chosen,
        &root.join("Job"),
        &Options {
            name: "Job".into(),
            include_originals: false,
            leave_out_owned_elsewhere: false,
        },
        &AtomicBool::new(false),
        |_| {},
        |_, _, _| Ok(0),
    )?;
    let took = started.elapsed();
    println!(
        "export by reference: {} photos in {:.3} s ({:.2} ms/photo); catalog.sqlite {} KiB",
        receipt.photographs,
        took.as_secs_f64(),
        took.as_secs_f64() * 1000.0 / receipt.photographs as f64,
        fs::metadata(root.join("Job/catalog.sqlite"))?.len() / 1024
    );
    Ok(())
}
