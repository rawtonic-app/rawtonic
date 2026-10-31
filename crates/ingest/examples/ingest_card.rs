//! Times an ingest as the app runs it (`batch::import_batch`: RAW+JPEG
//! pairs, verified copies, optional verified backup) of every file below a
//! card folder into a new session. For A/B measurements of the copy path
//! (G41); the card is only read.
//!
//! `cargo run --release -p rawtonic-ingest --example ingest_card -- CARD SESSION [BACKUP] [TEMPLATE] [METADATA_JSON]`
//! (an empty BACKUP: none)
use rawtonic_catalog::{Library, Mode};
use rawtonic_ingest::batch::{BatchOptions, import_batch};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("card folder")
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        let hidden = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if hidden {
            continue;
        }
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn is_raw(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["nef", "arw", "cr2", "cr3", "raf", "dng", "orf", "rw2"]
            .contains(&e.to_ascii_lowercase().as_str())
    })
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 2,
        "usage: ingest_card CARD SESSION [BACKUP] [TEMPLATE] [METADATA_JSON]"
    );
    let mut sources = Vec::new();
    files(Path::new(&args[0]), &mut sources);
    let session = PathBuf::from(&args[1]);
    let backup = args.get(2).filter(|a| !a.is_empty()).map(PathBuf::from);
    let template = args.get(3).map_or("{orig}", String::as_str);
    let metadata: Option<rawtonic_ingest::metadata::ImportMetadata> = args
        .get(4)
        .map(|path| -> anyhow::Result<_> { Ok(serde_json::from_slice(&std::fs::read(path)?)?) })
        .transpose()?;
    let lib = Library::create(&session, "Bench", Mode::Session)?;
    let options = BatchOptions {
        template,
        sequence_start: 1,
        backup: backup.as_deref(),
        folder: None,
        roots: &[],
        pair_raw: Some(is_raw),
        album: None,
        metadata: metadata.as_ref(),
    };
    let started = Instant::now();
    let mut first = None;
    let mut bytes = 0;
    let results = import_batch(&lib, &sources, &options, &AtomicBool::new(false), |done| {
        first.get_or_insert_with(|| started.elapsed());
        bytes = done.bytes;
    })?;
    let elapsed = started.elapsed();
    let failed: Vec<_> = results.iter().filter(|r| r.error.is_some()).collect();
    for r in &failed {
        eprintln!(
            "{}: {}",
            r.source.display(),
            r.error.as_deref().unwrap_or("")
        );
    }
    println!(
        "{{\"files\":{},\"failed\":{},\"bytes\":{bytes},\"first_s\":{:.3},\"total_s\":{:.3},\"mb_per_s\":{:.1}}}",
        results.len(),
        failed.len(),
        first.unwrap_or_default().as_secs_f64(),
        elapsed.as_secs_f64(),
        bytes as f64 / 1e6 / elapsed.as_secs_f64()
    );
    Ok(())
}
