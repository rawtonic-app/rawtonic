// SPDX-License-Identifier: AGPL-3.0-or-later
//! Optimized graphs kept between sessions (gap G76, AIW2).
//!
//! A new ONNX Runtime session first optimizes the model's graph (constant
//! folding, fusions, layout): for BiRefNet on the CPU of an M4 Pro 1.0 s of
//! the 1.7 s session (notes: benchmark-results/aiw1-2026-11-18). The first
//! CPU session of a model saves the optimized graph into the app's cache
//! (`Settings::optimized_graphs`); later sessions, also in later launches,
//! load it with optimization off and compute the same bytes.
//!
//! Only CPU sessions are kept. GPU providers (WebGPU, CUDA) partition and
//! fuse the graph for their device; their output from a saved graph has not
//! been measured against today's, so they optimize at every load as before.
//! On macOS the GPU path is Core ML, which keeps its own compiled programs.
//!
//! An entry, for key `K` (a hash of everything the optimized graph depends
//! on, `key_text`), is three files in the cache folder:
//! - `K-ID.onnx`: the graph. ONNX Runtime writes it under a temporary name
//!   (`rawtonic_fs::temp`), renamed without replacing anything when done;
//! - `K-ID.weights`: its initializers as external data (ONNX files cannot
//!   pass 2 GB; BiRefNet's fp16 weights fold to fp32, 0.47 → about 1 GB);
//! - `K.manifest`: written last, published without replacing anything: the
//!   key's text, the source model's SHA-256, and each file's name, size and
//!   BLAKE3 digest. Only a manifest makes an entry.
//!
//! Every use checks the sizes and digests: a missing, short or changed file
//! (eviction by the cache limit, a crash, a bad disk) removes the entry and
//! the model loads as on its first use, saving a new one. A save that fails
//! (read-only or full disk, too little free space to start) leaves the
//! model loaded as before. Files of an entry without a manifest are left
//! over from a crash (or from another process still publishing) and removed
//! when older than `rawtonic_fs::temp::STALE_AGE`.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail, ensure};

use crate::ModelKind;

/// Changes when the entry layout or the session options that shape the
/// optimized graph change.
const FORMAT: &str = "rawtonic-onnx-optimized-1";
const GRAPH: &str = "onnx";
const DATA: &str = "weights";
const MANIFEST: &str = "manifest";

/// The extensions of an entry's files (for the app's cache limit).
pub const EXTENSIONS: [&str; 3] = [GRAPH, DATA, MANIFEST];

/// Free space a save needs before it starts: the graph and its weights
/// (2.1 times the model for BiRefNet; 2.5 times here) and 2 GB to spare.
fn space_needed(source_len: u64) -> u64 {
    source_len.saturating_mul(5) / 2 + 2_000_000_000
}

/// Initializers from this size on go to the weights file.
pub(crate) const EXTERNAL_MIN_BYTES: &str = "1024";

/// A session that took less than this to create (with the save) keeps no
/// graph, only a manifest saying so: on an M4 Pro the saved graph made
/// Sky's session 35 ms and MI-GAN's 16 ms quicker for 226 and 26 MB, and
/// BiRefNet's 1.1 s for 950 MB (3.1 s with the save; AIW2).
pub(crate) const KEEP_FROM: Duration = Duration::from_secs(1);

/// Keys whose save failed in this process.
static FAILED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// What `Entry::lookup` found.
#[derive(Debug)]
pub(crate) enum Lookup {
    Missing,
    /// The session is created from this graph.
    Graph(PathBuf),
    /// The model optimizes quickly; its graph is not kept.
    NotKept,
}

/// Whether `Save::publish` kept the graph.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    Yes,
    No,
}

/// The cache entry of one model for CPU sessions.
pub(crate) struct Entry {
    dir: PathBuf,
    key: String,
    text: String,
    source: PathBuf,
    source_len: u64,
}

/// A save in progress: where ONNX Runtime writes the graph and its weights.
pub(crate) struct Save {
    entry_key: String,
    dir: PathBuf,
    /// The temporary name ONNX Runtime writes the graph to.
    pub(crate) graph_temp: PathBuf,
    graph: PathBuf,
    /// The weights file's name, beside the graph (its reference in the graph).
    pub(crate) data_name: String,
}

impl Entry {
    /// The entry for `source` (a `kind` model) in `dir`, for the loaded
    /// ONNX Runtime.
    pub(crate) fn new(dir: &Path, source: &Path, kind: ModelKind) -> Result<Self> {
        crate::ensure_runtime()?;
        Self::for_runtime(dir, source, kind, ort::info())
    }

    /// `new` for the runtime build `runtime` describes. A cache folder (or
    /// its parent) that is a link is refused: the cache writes and removes
    /// files only in a folder of the app's own.
    fn for_runtime(dir: &Path, source: &Path, kind: ModelKind, runtime: &str) -> Result<Self> {
        ensure!(
            !is_linked(dir),
            "{} is a link or not a folder",
            dir.display()
        );
        let meta = fs::metadata(source).with_context(|| format!("reading {}", source.display()))?;
        let text = key_text(source, &meta, kind, runtime);
        let key = blake3::hash(text.as_bytes()).to_hex()[..32].to_string();
        Ok(Self {
            dir: dir.to_path_buf(),
            key,
            text,
            source: source.to_path_buf(),
            source_len: meta.len(),
        })
    }

    fn manifest_path(&self) -> PathBuf {
        self.dir.join(format!("{}.{MANIFEST}", self.key))
    }

    /// The entry, when it is there and intact (an error: it is there and
    /// damaged). Marks its files used for the cache limit.
    pub(crate) fn lookup(&self) -> Result<Lookup> {
        let text = match fs::read_to_string(self.manifest_path()) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Lookup::Missing),
            Err(e) => return Err(anyhow!(e).context("reading its manifest")),
        };
        let manifest = Manifest::parse(&text).context("its manifest is unreadable")?;
        ensure!(
            manifest.key == self.text,
            "its manifest is of another model"
        );
        let Some(graph) = &manifest.graph else {
            touch(&self.manifest_path(), SystemTime::now());
            return Ok(Lookup::NotKept);
        };
        let mut files = vec![graph];
        files.extend(manifest.data.as_ref());
        for file in &files {
            ensure!(
                self.is_entry_file(&file.name),
                "its manifest names {:?}",
                file.name
            );
            let path = self.dir.join(&file.name);
            let meta =
                fs::symlink_metadata(&path).with_context(|| format!("{} is missing", file.name))?;
            ensure!(meta.is_file(), "{} is not a file", file.name);
            ensure!(
                meta.len() == file.len,
                "{} has {} bytes, not {}",
                file.name,
                meta.len(),
                file.len
            );
            ensure!(
                digest(&path)? == file.digest,
                "{} does not match its digest",
                file.name
            );
        }
        let now = SystemTime::now();
        for path in files
            .iter()
            .map(|f| self.dir.join(&f.name))
            .chain([self.manifest_path()])
        {
            touch(&path, now);
        }
        Ok(Lookup::Graph(self.dir.join(&graph.name)))
    }

    /// An entry file name of this key (`K-ID.onnx` or `K-ID.weights`).
    fn is_entry_file(&self, name: &str) -> bool {
        let Some(rest) = name.strip_prefix(&format!("{}-", self.key)) else {
            return false;
        };
        let Some((id, ext)) = rest.split_once('.') else {
            return false;
        };
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit()) && (ext == GRAPH || ext == DATA)
    }

    /// Removes the entry: its manifest first (so nothing uses it), then the
    /// files it names. Only plain files of this key in the folder go.
    pub(crate) fn discard(&self) {
        let manifest = fs::read_to_string(self.manifest_path())
            .ok()
            .and_then(|text| Manifest::parse(&text).ok());
        remove_file(&self.manifest_path());
        if let Some(manifest) = manifest {
            for file in manifest.graph.iter().chain(manifest.data.as_ref()) {
                if self.is_entry_file(&file.name) {
                    remove_file(&self.dir.join(&file.name));
                }
            }
        }
    }

    /// Starts a save: checks that the folder takes a file and has the free
    /// space, removes old leftovers and picks new names. A save ONNX Runtime
    /// cannot finish fails the session after the optimization, which then
    /// runs again without saving: these checks keep a read-only or full
    /// disk from costing that at every load, and a save that failed in
    /// this process is not tried again (`failed`).
    pub(crate) fn start_save(&self) -> Result<Save> {
        ensure!(
            !FAILED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&self.key),
            "it failed before in this process"
        );
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        drop(
            rawtonic_fs::temp::file_for(&self.manifest_path())
                .with_context(|| format!("{} takes no file", self.dir.display()))?,
        );
        let free = rawtonic_fs::volume::space(&self.dir)?.free;
        let needed = space_needed(self.source_len);
        ensure!(
            free >= needed,
            "{} MB free, {} MB needed",
            free >> 20,
            needed >> 20
        );
        self.remove_leftovers(SystemTime::now());
        let id = unique_id();
        let graph = self.dir.join(format!("{}-{id}.{GRAPH}", self.key));
        let graph_temp = self
            .dir
            .join(format!("{}part", rawtonic_fs::temp::prefix_for(&graph)));
        Ok(Save {
            entry_key: self.text.clone(),
            dir: self.dir.clone(),
            graph_temp,
            graph,
            data_name: format!("{}-{id}.{DATA}", self.key),
        })
    }

    /// Files of this key that no manifest names and that are older than a
    /// save can take, and this key's stale temporary files: only names that
    /// start with the key (32 hex digits) are ever removed.
    fn remove_leftovers(&self, now: SystemTime) {
        // The graph's (`<prefix>K-ID.onnx-part`) and the manifest's
        // (`<prefix>K.manifest-…`) temporary names.
        let temp = format!("{}{}", rawtonic_fs::temp::TEMP_PREFIX, self.key);
        let named: Vec<String> = fs::read_to_string(self.manifest_path())
            .ok()
            .and_then(|text| Manifest::parse(&text).ok())
            .map(|m| m.graph.into_iter().chain(m.data).map(|f| f.name).collect())
            .unwrap_or_default();
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&temp) {
                if !is_linked(&self.dir) {
                    let _ = rawtonic_fs::temp::remove_stale(&entry.path(), now);
                }
                continue;
            }
            if !self.is_entry_file(&name) || named.contains(&name) {
                continue;
            }
            let old = fs::symlink_metadata(entry.path())
                .ok()
                .filter(|m| m.is_file())
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age >= rawtonic_fs::temp::STALE_AGE);
            if old {
                remove_file(&entry.path());
            }
        }
    }

    /// A save of this entry failed: not tried again in this process.
    pub(crate) fn failed(&self) {
        FAILED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(self.key.clone());
    }

    /// The SHA-256 of the source model, recorded in the manifest.
    fn source_sha256(&self) -> Result<String> {
        let mut file = fs::File::open(&self.source)?;
        Ok(rawtonic_fs::sha256_hex_read(&mut file)?)
    }
}

impl Save {
    /// After the session was created: syncs what ONNX Runtime wrote, puts
    /// the graph in place and publishes the manifest; or, unless `keep`
    /// (the session was quicker than `KEEP_FROM`), removes the files and
    /// publishes a manifest saying so. Another process that published this
    /// entry first wins; ours is removed.
    pub(crate) fn publish(self, entry: &Entry, keep: bool) -> Result<Kept> {
        if !keep {
            self.abandon_files();
            self.write_manifest(entry, None, None)?;
            return Ok(Kept::No);
        }
        let data = self.dir.join(&self.data_name);
        sync_file(&self.graph_temp).context("the graph was not written")?;
        let has_data = match fs::symlink_metadata(&data) {
            Ok(meta) => meta.is_file(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        if has_data {
            sync_file(&data)?;
        }
        rawtonic_fs::rename_no_clobber(&self.graph_temp, &self.graph)?;
        let result = self.write_manifest(entry, Some(&self.graph), has_data.then_some(&data));
        if result.is_err() {
            remove_file(&self.graph);
            remove_file(&data);
        }
        result.map(|()| Kept::Yes)
    }

    fn write_manifest(
        &self,
        entry: &Entry,
        graph: Option<&PathBuf>,
        data: Option<&PathBuf>,
    ) -> Result<()> {
        let file = |path: &Path| -> Result<File> {
            Ok(File {
                name: path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| anyhow!("no file name"))?
                    .to_string(),
                len: fs::metadata(path)?.len(),
                digest: digest(path)?,
            })
        };
        let manifest = Manifest {
            key: self.entry_key.clone(),
            source_sha256: entry.source_sha256()?,
            graph: graph.map(|g| file(g)).transpose()?,
            data: data.map(|d| file(d)).transpose()?,
        };
        let path = entry.manifest_path();
        let mut temp = rawtonic_fs::temp::file_for(&path)?;
        io::Write::write_all(&mut temp, manifest.text().as_bytes())?;
        temp.as_file().sync_all()?;
        match rawtonic_fs::no_clobber::persist(temp, &path) {
            Ok(_) => {
                rawtonic_fs::sync_dir(&self.dir)?;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                Err(anyhow!("another session saved it first"))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// The session could not be created this way: removes what ONNX Runtime
    /// may have written.
    pub(crate) fn abandon(self) {
        self.abandon_files();
    }

    fn abandon_files(&self) {
        remove_file(&self.graph_temp);
        remove_file(&self.dir.join(&self.data_name));
    }
}

/// Everything the optimized graph depends on: this layout, the source
/// file as it is now (name, size, times, inode: a replaced or rewritten
/// model is another key, as in the app's `verified-files.tsv`), the model,
/// the provider and optimization level, the system, the CPU's features
/// (ONNX Runtime picks layouts and kernels by them) and the runtime's
/// build (version and commit).
fn key_text(source: &Path, meta: &fs::Metadata, kind: ModelKind, runtime: &str) -> String {
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ns = |t: io::Result<SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos())
    };
    #[cfg(unix)]
    let (changed, inode, device) = {
        use std::os::unix::fs::MetadataExt;
        (
            meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128,
            meta.ino(),
            meta.dev(),
        )
    };
    #[cfg(not(unix))]
    let (changed, inode, device) = (ns(meta.created()) as i128, 0u64, 0u64);
    format!(
        "{FORMAT} file={name:?} len={} modified={} changed={changed} inode={inode} device={device} model={} provider=CPUExecutionProvider level=all os={} arch={} cpu={:?} runtime={:?}",
        meta.len(),
        ns(meta.modified()),
        kind.name(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        cpu_features(),
        runtime,
    )
}

/// The CPU features ONNX Runtime's kernels and layouts depend on.
fn cpu_features() -> String {
    let mut found: Vec<&str> = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        macro_rules! probe {
            ($($f:tt),*) => { $(if std::arch::is_x86_feature_detected!($f) { found.push($f); })* };
        }
        probe!(
            "sse4.1",
            "sse4.2",
            "avx",
            "avx2",
            "fma",
            "f16c",
            "avx512f",
            "avx512bw",
            "avx512vl",
            "avx512vnni",
            "avx512bf16",
            "avx512fp16",
            "avxvnni"
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        macro_rules! probe {
            ($($f:tt),*) => { $(if std::arch::is_aarch64_feature_detected!($f) { found.push($f); })* };
        }
        probe!("neon", "fp16", "dotprod", "i8mm", "bf16", "sve", "sve2");
    }
    found.join(",")
}

/// A name part no other save of this or another process picks.
fn unique_id() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{nanos:x}{:x}{:x}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    )
}

/// BLAKE3 of a file, read through a memory map on several threads (1 GB
/// in well under a second).
fn digest(path: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher
        .update_mmap_rayon(path)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Syncs a file to disk (through a handle with write access: Windows'
/// `FlushFileBuffers` needs one).
fn sync_file(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new().write(true).open(path)?.sync_all()
}

/// Marks a file used (its modification time) for the cache limit's
/// least-recently-used order.
fn touch(path: &Path, now: SystemTime) {
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(now);
    }
}

/// Whether `dir` or its parent is a link (or not a folder) now. Checked
/// when the entry is made and again right before every removal, so a cache
/// folder replaced by a link meanwhile loses nothing (and only names that
/// start with an entry's key are ever removed).
fn is_linked(dir: &Path) -> bool {
    [Some(dir), dir.parent()]
        .into_iter()
        .flatten()
        .any(|folder| {
            fs::symlink_metadata(folder).is_ok_and(|m| !m.is_dir() || m.file_type().is_symlink())
        })
}

/// Removes a plain file of ours; a missing one is fine, a link stays, and
/// nothing goes when its folder became a link (`is_linked`).
fn remove_file(path: &Path) {
    if path.parent().is_some_and(is_linked) {
        return;
    }
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        let _ = fs::remove_file(path);
    }
}

struct File {
    name: String,
    len: u64,
    digest: String,
}

struct Manifest {
    key: String,
    source_sha256: String,
    /// `None`: not kept (the session was quick to create).
    graph: Option<File>,
    data: Option<File>,
}

impl Manifest {
    fn text(&self) -> String {
        let mut lines = vec![
            format!("format\t{FORMAT}"),
            format!("key\t{}", self.key),
            format!("source-sha256\t{}", self.source_sha256),
        ];
        match &self.graph {
            Some(graph) => lines.push(format!(
                "graph\t{}\t{}\t{}",
                graph.name, graph.len, graph.digest
            )),
            None => lines.push("graph\tnot kept".to_string()),
        }
        if let Some(data) = &self.data {
            lines.push(format!(
                "weights\t{}\t{}\t{}",
                data.name, data.len, data.digest
            ));
        }
        lines.join("\n") + "\n"
    }

    fn parse(text: &str) -> Result<Self> {
        let mut format = None;
        let mut key = None;
        let mut source_sha256 = None;
        let mut graph = None;
        let mut data = None;
        let file = |fields: &[&str]| -> Result<File> {
            let [name, len, digest] = fields else {
                bail!("a file line has {} fields", fields.len());
            };
            Ok(File {
                name: name.to_string(),
                len: len.parse()?,
                digest: digest.to_string(),
            })
        };
        for line in text.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["format", value] => format = Some(value.to_string()),
                ["key", value] => key = Some(value.to_string()),
                ["source-sha256", value] => source_sha256 = Some(value.to_string()),
                ["graph", "not kept"] => graph = Some(None),
                ["graph", rest @ ..] => graph = Some(Some(file(rest)?)),
                ["weights", rest @ ..] => data = Some(file(rest)?),
                _ => bail!("unknown line {line:?}"),
            }
        }
        ensure!(format.as_deref() == Some(FORMAT), "another format");
        Ok(Self {
            key: key.ok_or_else(|| anyhow!("no key"))?,
            source_sha256: source_sha256.ok_or_else(|| anyhow!("no source"))?,
            graph: graph.ok_or_else(|| anyhow!("no graph"))?,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder for one test (`OPTIMIZED_SCRATCH`, else beside the
    /// test binary's target folder), empty.
    fn scratch(name: &str) -> PathBuf {
        let base = std::env::var_os("OPTIMIZED_SCRATCH")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("optimized-tests")
            });
        let dir = base.join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_entry(dir: &Path, source: &Path, kind: ModelKind) -> Result<Entry> {
        Entry::for_runtime(dir, source, kind, "test runtime")
    }

    /// An entry as a save leaves it, without ONNX Runtime: a source file,
    /// a graph and weights written where the save says, then published.
    fn saved_entry(dir: &Path) -> (Entry, PathBuf) {
        let source = dir.join("model.onnx");
        fs::write(&source, b"source model").unwrap();
        let cache = dir.join("cache");
        let entry = test_entry(&cache, &source, ModelKind::Subject).unwrap();
        let save = entry.start_save().unwrap();
        fs::write(&save.graph_temp, b"graph").unwrap();
        fs::write(cache.join(&save.data_name), vec![7u8; 4096]).unwrap();
        assert_eq!(save.publish(&entry, true).unwrap(), Kept::Yes);
        (entry, cache)
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_published_entry_is_found_and_names_only_its_own_files() {
        let dir = scratch("published");
        let (entry, cache) = saved_entry(&dir);
        let Lookup::Graph(graph) = entry.lookup().unwrap() else {
            panic!("no graph");
        };
        assert_eq!(fs::read(&graph).unwrap(), b"graph");
        let names = files_in(&cache);
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.iter().all(|n| !n.starts_with('.')), "{names:?}");
        let manifest = fs::read_to_string(entry.manifest_path()).unwrap();
        assert!(manifest.contains(&rawtonic_fs::sha256_hex(b"source model")));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_changed_or_missing_file_is_an_error_and_discard_removes_the_entry() {
        let dir = scratch("damaged");
        let (entry, cache) = saved_entry(&dir);
        let weights = files_in(&cache)
            .into_iter()
            .find(|n| n.ends_with(".weights"))
            .unwrap();
        // Same size, one byte changed: only the digest can tell.
        let mut bytes = fs::read(cache.join(&weights)).unwrap();
        bytes[100] ^= 1;
        fs::write(cache.join(&weights), &bytes).unwrap();
        let error = entry.lookup().unwrap_err();
        assert!(format!("{error:#}").contains("digest"), "{error:#}");
        entry.discard();
        assert!(files_in(&cache).is_empty());
        assert!(matches!(entry.lookup().unwrap(), Lookup::Missing));

        let (entry, cache) = saved_entry(&dir);
        let weights = files_in(&cache)
            .into_iter()
            .find(|n| n.ends_with(".weights"))
            .unwrap();
        fs::remove_file(cache.join(weights)).unwrap();
        let error = entry.lookup().unwrap_err();
        assert!(format!("{error:#}").contains("missing"), "{error:#}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn another_source_file_is_another_key() {
        let dir = scratch("key");
        let (entry, _) = saved_entry(&dir);
        // The model replaced by a new file (new inode and times).
        let source = dir.join("model.onnx");
        let next = dir.join("next.onnx");
        fs::write(&next, b"source model").unwrap();
        fs::rename(&next, &source).unwrap();
        let replaced = test_entry(&dir.join("cache"), &source, ModelKind::Subject).unwrap();
        assert_ne!(replaced.key, entry.key);
        assert!(matches!(replaced.lookup().unwrap(), Lookup::Missing));
        let other_kind = test_entry(&dir.join("cache"), &source, ModelKind::Sky).unwrap();
        assert_ne!(other_kind.key, replaced.key);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_second_publish_of_the_same_entry_leaves_the_first() {
        let dir = scratch("race");
        let (entry, cache) = saved_entry(&dir);
        let before = files_in(&cache);
        let save = entry.start_save().unwrap();
        fs::write(&save.graph_temp, b"graph 2").unwrap();
        fs::write(cache.join(&save.data_name), b"weights 2").unwrap();
        assert!(save.publish(&entry, true).is_err());
        assert_eq!(files_in(&cache), before);
        let Lookup::Graph(graph) = entry.lookup().unwrap() else {
            panic!("no graph");
        };
        assert_eq!(fs::read(graph).unwrap(), b"graph");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_abandoned_save_leaves_nothing_and_old_leftovers_go() {
        let dir = scratch("leftovers");
        let source = dir.join("model.onnx");
        fs::write(&source, b"source model").unwrap();
        let cache = dir.join("cache");
        let entry = test_entry(&cache, &source, ModelKind::Subject).unwrap();
        let save = entry.start_save().unwrap();
        fs::write(&save.graph_temp, b"partial").unwrap();
        fs::write(cache.join(&save.data_name), b"partial").unwrap();
        save.abandon();
        assert!(files_in(&cache).is_empty());

        // A crash after the graph was renamed, before the manifest: an
        // unnamed file of this key, removed once it is old, not before.
        let orphan = cache.join(format!("{}-abc.{DATA}", entry.key));
        let unrelated = cache.join("notes.txt");
        fs::write(&orphan, b"orphan").unwrap();
        fs::write(&unrelated, b"not ours").unwrap();
        entry.remove_leftovers(SystemTime::now());
        assert!(orphan.exists());
        entry.remove_leftovers(SystemTime::now() + rawtonic_fs::temp::STALE_AGE * 2);
        assert!(!orphan.exists());
        assert!(unrelated.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_quick_session_keeps_only_a_manifest() {
        let dir = scratch("quick");
        let source = dir.join("model.onnx");
        fs::write(&source, b"source model").unwrap();
        let cache = dir.join("cache");
        let entry = test_entry(&cache, &source, ModelKind::Sky).unwrap();
        let save = entry.start_save().unwrap();
        fs::write(&save.graph_temp, b"graph").unwrap();
        fs::write(cache.join(&save.data_name), b"weights").unwrap();
        assert_eq!(save.publish(&entry, false).unwrap(), Kept::No);
        assert_eq!(files_in(&cache), vec![format!("{}.{MANIFEST}", entry.key)]);
        assert!(matches!(entry.lookup().unwrap(), Lookup::NotKept));
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_folder_or_an_earlier_failure_starts_no_save() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("read-only");
        let source = dir.join("model.onnx");
        fs::write(&source, b"source model").unwrap();
        let cache = dir.join("cache");
        fs::create_dir(&cache).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o555)).unwrap();
        let entry = test_entry(&cache, &source, ModelKind::Subject).unwrap();
        let error = entry.start_save().err().expect("no save");
        assert!(format!("{error:#}").contains("takes no file"), "{error:#}");
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(entry.start_save().is_ok());
        entry.failed();
        let error = entry.start_save().err().expect("no save");
        assert!(format!("{error:#}").contains("failed before"), "{error:#}");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_cache_folder_is_refused_and_other_temporary_files_stay() {
        let dir = scratch("links");
        let source = dir.join("model.onnx");
        fs::write(&source, b"source model").unwrap();
        let photos = dir.join("photos");
        fs::create_dir(&photos).unwrap();
        std::os::unix::fs::symlink(&photos, dir.join("linked")).unwrap();
        let error = test_entry(&dir.join("linked"), &source, ModelKind::Subject)
            .err()
            .expect("refused");
        assert!(format!("{error:#}").contains("link"), "{error:#}");
        let error = test_entry(
            &dir.join("linked").join("graphs"),
            &source,
            ModelKind::Subject,
        )
        .err()
        .expect("refused");
        assert!(format!("{error:#}").contains("link"), "{error:#}");

        // The cache folder replaced by a link after the entry was made: no
        // removal reaches the folder it points to.
        let moved = dir.join("moved");
        fs::create_dir(&moved).unwrap();
        let swapped = moved.join("cache");
        fs::create_dir(&swapped).unwrap();
        let entry = test_entry(&swapped, &source, ModelKind::Subject).unwrap();
        fs::rename(&swapped, dir.join("old-cache")).unwrap();
        std::os::unix::fs::symlink(&photos, &swapped).unwrap();
        let named = photos.join(format!("{}-1.{DATA}", entry.key));
        let temp = photos.join(format!(
            "{}{}.manifest-x",
            rawtonic_fs::temp::TEMP_PREFIX,
            entry.key
        ));
        fs::write(&named, b"photo").unwrap();
        fs::write(&temp, b"photo").unwrap();
        fs::write(
            photos.join(format!("{}.{MANIFEST}", entry.key)),
            b"not ours",
        )
        .unwrap();
        entry.remove_leftovers(SystemTime::now() + rawtonic_fs::temp::STALE_AGE * 2);
        entry.discard();
        assert!(named.exists() && temp.exists());
        assert!(photos.join(format!("{}.{MANIFEST}", entry.key)).exists());

        // Another job's old temporary file in the cache folder stays.
        let cache = dir.join("cache");
        fs::create_dir(&cache).unwrap();
        let other = cache.join(format!("{}photo.jpg-x", rawtonic_fs::temp::TEMP_PREFIX));
        fs::write(&other, b"other").unwrap();
        let entry = test_entry(&cache, &source, ModelKind::Subject).unwrap();
        let ours = cache.join(format!(
            "{}{}-1.onnx-part",
            rawtonic_fs::temp::TEMP_PREFIX,
            entry.key
        ));
        let manifest_temp = rawtonic_fs::temp::file_for(&entry.manifest_path())
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        fs::write(&ours, b"ours").unwrap();
        entry.remove_leftovers(SystemTime::now() + rawtonic_fs::temp::STALE_AGE * 2);
        assert!(other.exists());
        assert!(!ours.exists());
        assert!(!manifest_temp.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn manifest_file_names_are_checked() {
        let dir = scratch("names");
        let source = dir.join("model.onnx");
        fs::write(&source, b"m").unwrap();
        let entry = test_entry(&dir, &source, ModelKind::Subject).unwrap();
        let k = &entry.key;
        assert!(entry.is_entry_file(&format!("{k}-0a1b.onnx")));
        assert!(entry.is_entry_file(&format!("{k}-0a1b.weights")));
        assert!(!entry.is_entry_file(&format!("{k}-0a1b.manifest")));
        assert!(!entry.is_entry_file(&format!("{k}-../x.onnx")));
        assert!(!entry.is_entry_file(&format!("{k}-.onnx")));
        assert!(!entry.is_entry_file("model.onnx"));
        let _ = fs::remove_dir_all(dir);
    }

    /// The real models: outputs of a session from the saved graph, also
    /// after a damaged entry was rebuilt, bit-identical to a session that
    /// optimizes the pack's file, as today. Needs `ORT_DYLIB_PATH` and the
    /// pack in `AI_MODELS` (every model that keeps its graph); `AI_ONLY`
    /// limits the files (comma-separated prefixes).
    ///
    /// cargo test -p rawtonic-onnx --lib optimized::tests::real -- --ignored --nocapture
    #[test]
    #[ignore = "needs ORT_DYLIB_PATH and AI_MODELS"]
    fn real_models_compute_the_same_bits_from_the_saved_graph() {
        use crate::bench::{Picture, inputs_for, model_files, run_once};
        use std::time::Instant;
        let models = PathBuf::from(std::env::var("AI_MODELS").expect("AI_MODELS"));
        let only: Vec<String> = std::env::var("AI_ONLY")
            .unwrap_or_default()
            .split(',')
            .map(str::to_string)
            .collect();
        let picture = Picture::synthetic(1536, 1024);
        let bits = |outputs: &[crate::bench::Output]| -> Vec<(String, Vec<u32>)> {
            outputs
                .iter()
                .map(|o| {
                    (
                        o.name.clone(),
                        o.values.iter().map(|v| v.to_bits()).collect(),
                    )
                })
                .collect()
        };
        let mut tested = 0;
        for path in model_files(&models, false).unwrap() {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let kind = ModelKind::from_file_name(&name).unwrap();
            if !kind.keeps_optimized_graph()
                || !only.is_empty() && !only.iter().any(|o| name.starts_with(o.as_str()))
            {
                continue;
            }
            let cache = scratch(&format!("real-{name}"));
            let entry = Entry::new(&cache, &path, kind).unwrap();
            let timed = |what: &str, f: &dyn Fn() -> ort::session::Session| {
                let started = Instant::now();
                let session = f();
                println!("{name}: {what} {} ms", started.elapsed().as_millis());
                session
            };
            let mut today = timed("session as today", &|| {
                crate::builder(kind, None, None)
                    .unwrap()
                    .commit_from_file(&path)
                    .unwrap()
            });
            let inputs = inputs_for(&today, kind, &picture).unwrap();
            let expected = bits(&run_once(&mut today, &inputs).unwrap());
            drop(today);
            assert!(matches!(entry.lookup().unwrap(), Lookup::Missing));
            for (what, damage) in [
                ("first session (saves)", false),
                ("from the saved graph", false),
                ("after damage (rebuilds)", true),
                ("from the rebuilt graph", false),
            ] {
                if damage {
                    let weights = files_in(&cache)
                        .into_iter()
                        .find(|n| n.ends_with(".weights"))
                        .expect("a weights file");
                    let path = cache.join(weights);
                    let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
                    use std::io::{Seek, Write};
                    file.seek(io::SeekFrom::Start(64)).unwrap();
                    file.write_all(&[0x55; 8]).unwrap();
                    drop(file);
                    assert!(entry.lookup().is_err());
                }
                let mut session = timed(what, &|| {
                    crate::cpu_session_cached(&path, kind, None, &entry, Duration::ZERO).unwrap()
                });
                assert_eq!(
                    bits(&run_once(&mut session, &inputs).unwrap()),
                    expected,
                    "{name}: {what}"
                );
                assert!(
                    matches!(entry.lookup().unwrap(), Lookup::Graph(_)),
                    "{name}: {what}"
                );
            }
            let bytes: u64 = files_in(&cache)
                .iter()
                .map(|n| fs::metadata(cache.join(n)).unwrap().len())
                .sum();
            println!("{name}: entry {} MB, {:?}", bytes >> 20, files_in(&cache));
            let _ = fs::remove_dir_all(&cache);
            tested += 1;
        }
        assert!(tested > 0, "no model of {only:?} in {}", models.display());
    }
}
