//! Verified copies of one file to one or more places (an import's
//! photograph to the library and to its backup) from a single read of the
//! source.
//!
//! A file of up to [`IN_MEMORY_LIMIT`] bytes is read once into memory: its
//! SHA-256 (recorded with the photograph) is computed from that buffer
//! while the copies are written, and every copy, and then the source read
//! again, is compared with the buffer byte by byte. Larger files stream:
//! each chunk goes to every copy and the hash, and the copies and the
//! source read again are checked against that hash. Nothing is published
//! before every copy and the source passed; each copy is published without
//! replacing a file (`rawtonic_metadata::no_clobber`).
//!
//! Durability: files and folders are synced as they are written; the drive
//! cache is flushed by [`Barrier::flush`], once per volume for everything
//! synced since, before a caller records a copy. See ADR 0019.
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use tempfile::NamedTempFile;

/// Files up to this size are read into memory once and compared byte by
/// byte; larger ones stream and are compared by SHA-256.
pub(crate) const IN_MEMORY_LIMIT: u64 = 256 * 1024 * 1024;

const CHUNK: usize = 1024 * 1024;

/// Reads of a file read into memory.
const READ_CHUNK: usize = 8 * 1024 * 1024;

/// Volumes whose drive cache still holds data synced to them: files and
/// folders are synced to the drive as they are written (`sync_file`,
/// `sync_folder`), and [`flush`](Barrier::flush) asks each volume's drive
/// once to write its cache to permanent storage. On macOS a plain `fsync`
/// leaves data in the drive cache, so the flush is a full sync
/// (`F_FULLFSYNC`) of one folder per volume; elsewhere `fsync` already
/// reaches permanent storage and the flush has nothing to do.
#[derive(Debug, Default)]
pub(crate) struct Barrier {
    /// One folder per volume still to flush.
    #[cfg(target_vendor = "apple")]
    pending: Vec<(u64, PathBuf)>,
}

impl Barrier {
    /// Syncs `file`, a copy already there (an earlier, cut-off run made
    /// it), and its folder to the drive, so the next flush covers it too.
    pub fn sync_existing(&mut self, file: &Path) -> std::io::Result<()> {
        push(&File::open(file)?)?;
        let folder = file.parent().filter(|p| !p.as_os_str().is_empty());
        self.sync_folder(folder.unwrap_or(Path::new(".")))
    }

    /// Syncs `folder`'s entries to the drive (where folders can be synced).
    pub fn sync_folder(&mut self, folder: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            push(&File::open(folder)?)?;
            self.note(folder)
        }
        #[cfg(not(unix))]
        {
            let _ = folder;
            Ok(())
        }
    }

    #[cfg(target_vendor = "apple")]
    fn note(&mut self, folder: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let device = fs::metadata(folder)?.dev();
        if !self.pending.iter().any(|(d, _)| *d == device) {
            self.pending.push((device, folder.to_path_buf()));
        }
        Ok(())
    }

    #[cfg(not(target_vendor = "apple"))]
    fn note(&mut self, _folder: &Path) -> std::io::Result<()> {
        Ok(())
    }

    /// Writes the drive cache of every volume synced to since the last
    /// flush to permanent storage; a volume whose flush failed stays
    /// pending.
    pub fn flush(&mut self) -> std::io::Result<()> {
        #[cfg(test)]
        if FAIL_FLUSH.with(std::cell::Cell::get) {
            return Err(std::io::Error::other("injected flush failure"));
        }
        #[cfg(target_vendor = "apple")]
        while let Some((_, folder)) = self.pending.last() {
            // `sync_all` is `F_FULLFSYNC` on Apple platforms: the whole
            // drive cache, not only this folder's data.
            File::open(folder)?.sync_all()?;
            #[cfg(test)]
            FLUSHES.with(|count| count.set(count.get() + 1));
            self.pending.pop();
        }
        Ok(())
    }
}

/// `fsync` without the drive cache flush (`Barrier::flush` does that).
fn push(file: &File) -> std::io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: `fsync` on a descriptor `file` keeps open.
        if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_vendor = "apple"))]
    file.sync_all()
}

/// Files of up to this size are read ahead (`ReadAhead`).
const READ_AHEAD_LIMIT: u64 = 128 * 1024 * 1024;

/// The bytes `ReadAhead` holds at most (files read but not yet taken).
const READ_AHEAD_BUDGET: u64 = 384 * 1024 * 1024;

/// A file `ReadAhead` read: open, as it was when opened, its bytes and
/// their SHA-256.
pub(crate) struct ReadFile {
    path: PathBuf,
    file: File,
    before: fs::Metadata,
    buffer: Vec<u8>,
    sha256: String,
}

/// Bytes read ahead and not yet taken.
#[derive(Default)]
struct Budget {
    held: std::sync::Mutex<u64>,
    freed: std::sync::Condvar,
}

impl Budget {
    /// Waits until `len` more bytes fit (always for the first), unless
    /// `stop` is set; whether they were taken.
    fn take(&self, len: u64, stop: &AtomicBool) -> bool {
        let mut held = self.held.lock().expect("budget");
        while *held > 0 && *held + len > READ_AHEAD_BUDGET {
            if stop.load(Ordering::Acquire) {
                return false;
            }
            held = self.freed.wait(held).expect("budget");
        }
        *held += len;
        !stop.load(Ordering::Acquire)
    }

    fn give(&self, len: u64) {
        *self.held.lock().expect("budget") -= len;
        self.freed.notify_all();
    }
}

/// Reads the files of an import ahead, in the order they are copied, while
/// those before are written and checked: one thread reads files into
/// memory (up to `READ_AHEAD_BUDGET` bytes not yet taken), another hashes
/// them. Only reads; a file it skipped (too large, unreadable) or that is
/// copied out of turn is read by its copy as usual (`copy_verified`).
pub(crate) struct ReadAhead {
    order: std::collections::HashMap<PathBuf, usize>,
    head: std::cell::RefCell<Option<ReadFile>>,
    queue: Option<std::sync::mpsc::Receiver<ReadFile>>,
    budget: std::sync::Arc<Budget>,
    stop: std::sync::Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl ReadAhead {
    /// Starts reading `paths`, in the order their copies are made.
    pub fn start(paths: Vec<PathBuf>) -> Self {
        let mut order = std::collections::HashMap::new();
        for (i, path) in paths.iter().enumerate() {
            order.entry(path.clone()).or_insert(i);
        }
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let budget = std::sync::Arc::new(Budget::default());
        let (read_tx, read_rx) = std::sync::mpsc::channel::<ReadFile>();
        let (hash_tx, hash_rx) = std::sync::mpsc::channel::<ReadFile>();
        let (reading, reader_budget) = (stop.clone(), budget.clone());
        let reader = std::thread::spawn(move || {
            for path in paths {
                if reading.load(Ordering::Acquire) {
                    break;
                }
                let Ok(mut file) = File::open(&path) else {
                    continue;
                };
                let Ok(before) = file.metadata() else {
                    continue;
                };
                if !before.is_file() || before.len() > READ_AHEAD_LIMIT {
                    continue;
                }
                if !reader_budget.take(before.len(), &reading) {
                    break;
                }
                let read = read_all(&mut file, before.len(), &reading, &mut |_| {});
                let Ok(buffer) = read else {
                    reader_budget.give(before.len());
                    continue;
                };
                let read = ReadFile {
                    path,
                    file,
                    before,
                    buffer,
                    sha256: String::new(),
                };
                if read_tx.send(read).is_err() {
                    break;
                }
            }
        });
        let hasher = std::thread::spawn(move || {
            for mut read in read_rx {
                read.sha256 = hex(Sha256::digest(&read.buffer));
                if hash_tx.send(read).is_err() {
                    break;
                }
            }
        });
        Self {
            order,
            head: Default::default(),
            queue: Some(hash_rx),
            budget,
            stop,
            threads: vec![reader, hasher],
        }
    }

    /// `source` as read ahead, when it was (files before it that were not
    /// taken are dropped).
    pub fn take(&self, source: &Path) -> Option<ReadFile> {
        let wanted = *self.order.get(source)?;
        let mut head = self.head.borrow_mut();
        loop {
            let next = match head.take() {
                Some(next) => next,
                None => self.queue.as_ref()?.recv().ok()?,
            };
            let at = self.order[&next.path];
            if at > wanted {
                *head = Some(next);
                return None;
            }
            self.budget.give(next.before.len());
            if at == wanted {
                return Some(next);
            }
        }
    }
}

impl Drop for ReadAhead {
    fn drop(&mut self) {
        {
            // Under the budget's lock, so a reader about to wait for budget
            // sees the stop (or is waiting and woken below).
            let _held = self.budget.held.lock();
            self.stop.store(true, Ordering::Release);
        }
        self.budget.freed.notify_all();
        // Dropping the queue ends the hasher, and with it the reader.
        self.head.take();
        self.queue.take();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// What every copy must equal.
enum Content {
    /// The source's bytes (read once).
    Bytes(Vec<u8>),
    /// The source's SHA-256 (streamed).
    Hash,
}

/// Copies of a source, written, synced and checked, not yet published.
pub(crate) struct Verified {
    pub sha256: String,
    pub bytes: u64,
    content: Content,
    /// Each destination's temporary file (taken when published).
    temps: Vec<Option<(NamedTempFile, PathBuf)>>,
}

impl Verified {
    /// Publishes the copy for `destinations[index]` of [`copy_verified`]
    /// there, never replacing a file, and syncs its folder (`barrier`).
    pub fn publish(&mut self, index: usize, barrier: &mut Barrier) -> Result<PathBuf> {
        let (temp, destination) = self.temps[index]
            .take()
            .context("This copy was published already")?;
        let parent = destination
            .parent()
            .context("Destination has no parent")?
            .to_path_buf();
        if let rawtonic_metadata::no_clobber::Persisted::Copied(mut copy) =
            rawtonic_metadata::no_clobber::persist(temp, &destination)?
        {
            // The volume had no exclusive rename: check the copy through its
            // handle (the name may already be another file's, so a bad copy
            // is kept, never removed by name).
            copy.rewind()?;
            ensure!(
                self.matches(&mut copy)?,
                "Destination verification failed (the bad copy is kept at {})",
                destination.display()
            );
        }
        barrier.sync_folder(&parent)?;
        Ok(destination)
    }

    fn matches(&self, reader: &mut impl Read) -> Result<bool> {
        Ok(match &self.content {
            Content::Bytes(bytes) => same_bytes(reader, bytes)?,
            Content::Hash => sha256_of(reader)? == self.sha256,
        })
    }
}

/// Copies `source` to temporary files beside each of `destinations`
/// (whose folders exist and which must not exist) from one read, syncs
/// them to the drive and checks each, and the source read again, against
/// what was read (see the module notes). Fails, publishing nothing, when
/// a check fails, the source changed meanwhile or `cancel` is set;
/// `progress` gets the bytes read so far.
pub(crate) fn copy_verified(
    source: &Path,
    destinations: &[&Path],
    cancel: &AtomicBool,
    progress: impl FnMut(u64),
    ahead: Option<&ReadAhead>,
) -> Result<Verified> {
    let read = ahead.and_then(|ahead| ahead.take(source));
    copy_verified_from(
        source,
        destinations,
        cancel,
        progress,
        read,
        IN_MEMORY_LIMIT,
    )
}

/// `copy_verified` with `source` as `read` ahead, if it was, reading files
/// of up to `in_memory` bytes into memory.
fn copy_verified_from(
    source: &Path,
    destinations: &[&Path],
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
    read: Option<ReadFile>,
    in_memory: u64,
) -> Result<Verified> {
    for destination in destinations {
        ensure!(
            !destination.exists(),
            "Destination already exists: {}",
            destination.display()
        );
    }
    let (mut input, before, read) = match read {
        Some(read) => (read.file, read.before, Some((read.buffer, read.sha256))),
        None => {
            let input = File::open(source).context("Cannot read source")?;
            let before = input.metadata()?;
            (input, before, None)
        }
    };
    ensure!(before.is_file(), "Source is not a file");
    let mut temps = Vec::with_capacity(destinations.len());
    for destination in destinations {
        let parent = destination.parent().context("Destination has no parent")?;
        temps.push((NamedTempFile::new_in(parent)?, destination.to_path_buf()));
    }
    ensure!(!cancel.load(Ordering::Acquire), "Ingest cancelled");
    let (sha256, bytes, content) = match read {
        Some((buffer, sha256)) => {
            progress(buffer.len() as u64);
            copy_in_memory(source, buffer, Some(sha256), &mut temps)?
        }
        None if before.len() <= in_memory => {
            let buffer = read_all(&mut input, before.len(), cancel, &mut progress)?;
            copy_in_memory(source, buffer, None, &mut temps)?
        }
        None => copy_streaming(source, &mut input, &mut temps, cancel, &mut progress)?,
    };
    let after = input.metadata()?;
    ensure!(
        after.len() == before.len()
            && bytes == before.len()
            && after.modified()? == before.modified()?,
        "Source changed during ingest"
    );
    ensure!(!cancel.load(Ordering::Acquire), "Ingest cancelled");
    Ok(Verified {
        sha256,
        bytes,
        content,
        temps: temps.into_iter().map(Some).collect(),
    })
}

/// Reads `input` (`len` bytes expected) in chunks, checking `cancel`
/// between them.
fn read_all(
    input: &mut File,
    len: u64,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(u64),
) -> Result<Vec<u8>> {
    let mut buffer = vec![0; usize::try_from(len)?];
    let mut filled = 0;
    while filled < buffer.len() {
        ensure!(!cancel.load(Ordering::Acquire), "Ingest cancelled");
        let end = buffer.len().min(filled + READ_CHUNK);
        let n = input.read(&mut buffer[filled..end])?;
        if n == 0 {
            break;
        }
        filled += n;
        progress(filled as u64);
    }
    buffer.truncate(filled);
    // Grown since it was opened: not the file to copy.
    ensure!(input.read(&mut [0])? == 0, "Source changed during ingest");
    Ok(buffer)
}

#[cfg(test)]
type AfterWrite = Box<dyn Fn(&Path)>;

#[cfg(test)]
thread_local! {
    /// Called with each copy's temporary file after it is written and
    /// synced, before it is checked (a test corrupts one).
    static AFTER_WRITE: std::cell::RefCell<Option<AfterWrite>> =
        const { std::cell::RefCell::new(None) };
    /// Makes `Barrier::flush` fail (a test of what is recorded then).
    pub(crate) static FAIL_FLUSH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Volumes flushed by `Barrier::flush` (macOS).
    pub(crate) static FLUSHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Sets (or clears) the `AFTER_WRITE` hook for this thread.
#[cfg(test)]
pub(crate) fn set_after_write(hook: Option<AfterWrite>) {
    AFTER_WRITE.with(|h| *h.borrow_mut() = hook);
}

#[cfg(test)]
fn after_write(paths: &[PathBuf]) {
    AFTER_WRITE.with(|hook| {
        if let Some(hook) = hook.borrow().as_ref() {
            paths.iter().for_each(|path| hook(path));
        }
    });
}

/// Writes `buffer`, the source's bytes, to each of `temps` and checks
/// them (`sha256`: the buffer's, when known).
fn copy_in_memory(
    source: &Path,
    buffer: Vec<u8>,
    sha256: Option<String>,
    temps: &mut [(NamedTempFile, PathBuf)],
) -> Result<(String, u64, Content)> {
    let bytes = buffer.len() as u64;
    let data = buffer.as_slice();
    // Written and synced in parallel while the hash is computed.
    let (sha256, written) = std::thread::scope(|scope| {
        let hash = scope.spawn(|| sha256.unwrap_or_else(|| hex(Sha256::digest(data))));
        let written: Vec<_> = temps
            .iter_mut()
            .map(|(temp, _)| {
                scope.spawn(move || -> std::io::Result<()> {
                    let file = temp.as_file_mut();
                    file.write_all(data)?;
                    push(file)
                })
            })
            .collect();
        let written: std::io::Result<Vec<()>> = written
            .into_iter()
            .map(|w| w.join().expect("copy thread"))
            .collect();
        (hash.join().expect("hash thread"), written)
    });
    written?;
    #[cfg(test)]
    after_write(
        &temps
            .iter()
            .map(|(t, _)| t.path().to_path_buf())
            .collect::<Vec<_>>(),
    );
    // Each copy read back, and the source read again (a change while it
    // was copied, even with its date kept), compared with what was read.
    let checks = std::thread::scope(|scope| {
        let copies: Vec<_> = temps
            .iter_mut()
            .map(|(temp, _)| {
                scope.spawn(move || -> Result<bool> {
                    let file = temp.as_file_mut();
                    file.rewind()?;
                    Ok(same_bytes(file, data)?)
                })
            })
            .collect();
        let source_again = same_bytes(&mut File::open(source)?, data);
        let copies: Result<Vec<bool>> = copies
            .into_iter()
            .map(|c| c.join().expect("check thread"))
            .collect();
        Ok::<_, anyhow::Error>((copies?, source_again?))
    })?;
    ensure!(
        checks.0.iter().all(|same| *same),
        "Destination verification failed"
    );
    ensure!(checks.1, "Source changed during verification");
    Ok((sha256, bytes, Content::Bytes(buffer)))
}

fn copy_streaming(
    source: &Path,
    input: &mut File,
    temps: &mut [(NamedTempFile, PathBuf)],
    cancel: &AtomicBool,
    progress: &mut impl FnMut(u64),
) -> Result<(String, u64, Content)> {
    let mut buffer = vec![0; CHUNK];
    let mut h = Sha256::new();
    let mut bytes = 0;
    loop {
        ensure!(!cancel.load(Ordering::Acquire), "Ingest cancelled");
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        for (temp, _) in temps.iter_mut() {
            temp.write_all(&buffer[..n])?;
        }
        h.update(&buffer[..n]);
        bytes += n as u64;
        progress(bytes);
    }
    let sha256 = hex(h.finalize());
    #[cfg(test)]
    after_write(
        &temps
            .iter()
            .map(|(t, _)| t.path().to_path_buf())
            .collect::<Vec<_>>(),
    );
    let expected = sha256.as_str();
    let checks = std::thread::scope(|scope| {
        let copies: Vec<_> = temps
            .iter_mut()
            .map(|(temp, _)| {
                scope.spawn(move || -> Result<bool> {
                    let file = temp.as_file_mut();
                    push(file)?;
                    file.rewind()?;
                    Ok(sha256_of(file)? == expected)
                })
            })
            .collect();
        let source_again = File::open(source)
            .map_err(anyhow::Error::from)
            .and_then(|mut f| sha256_of(&mut f));
        let copies: Result<Vec<bool>> = copies
            .into_iter()
            .map(|c| c.join().expect("check thread"))
            .collect();
        Ok::<_, anyhow::Error>((copies?, source_again?))
    })?;
    ensure!(
        checks.0.iter().all(|same| *same),
        "Destination SHA-256 verification failed"
    );
    ensure!(checks.1 == sha256, "Source changed during verification");
    Ok((sha256, bytes, Content::Hash))
}

/// Whether `reader` holds exactly `expected`.
fn same_bytes(reader: &mut impl Read, expected: &[u8]) -> std::io::Result<bool> {
    let mut buffer = vec![0; CHUNK];
    let mut at = 0;
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            return Ok(at == expected.len());
        }
        if expected.len() - at < n || buffer[..n] != expected[at..at + n] {
            return Ok(false);
        }
        at += n;
    }
}

pub(crate) fn sha256_of(f: &mut impl Read) -> Result<String> {
    let mut h = Sha256::new();
    let mut buffer = vec![0; CHUNK];
    loop {
        let n = f.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        h.update(&buffer[..n]);
    }
    Ok(hex(h.finalize()))
}

fn hex(digest: impl AsRef<[u8]>) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corrupt(path: &Path) {
        let mut bytes = fs::read(path).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 1;
        fs::write(path, bytes).unwrap();
    }

    fn with_after_write<T>(hook: impl Fn(&Path) + 'static, f: impl FnOnce() -> T) -> T {
        AFTER_WRITE.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        let result = f();
        AFTER_WRITE.with(|h| *h.borrow_mut() = None);
        result
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A copy that differs from what was read (a bad write, a flipped bit
    /// on the way) fails the copy: neither destination is published and no
    /// temporary file stays, read into memory or streamed.
    #[test]
    fn a_copy_that_differs_is_never_published() {
        for in_memory in [u64::MAX, 0] {
            let d = tempfile::tempdir().unwrap();
            let source = d.path().join("card/DSC.ARW");
            fs::create_dir_all(source.parent().unwrap()).unwrap();
            fs::write(&source, vec![7u8; 3_000_000]).unwrap();
            let (a, b) = (d.path().join("a"), d.path().join("b"));
            fs::create_dir_all(&a).unwrap();
            fs::create_dir_all(&b).unwrap();
            let (to_a, to_b) = (a.join("DSC.ARW"), b.join("DSC.ARW"));
            let corrupt_b = b.clone();
            let error = with_after_write(
                move |temp| {
                    if temp.starts_with(&corrupt_b) {
                        corrupt(temp)
                    }
                },
                || {
                    copy_verified_from(
                        &source,
                        &[&to_a, &to_b],
                        &AtomicBool::new(false),
                        |_| {},
                        None,
                        in_memory,
                    )
                    .err()
                    .expect("a bad copy fails")
                },
            );
            assert!(error.to_string().contains("verification failed"), "{error}");
            assert!(names(&a).is_empty() && names(&b).is_empty(), "{in_memory}");
            assert_eq!(fs::read(&source).unwrap(), vec![7u8; 3_000_000]);
        }
    }

    /// Both copies come from one read, equal the source and carry its
    /// SHA-256, whether read into memory or streamed.
    #[test]
    fn one_read_makes_every_copy() {
        for in_memory in [u64::MAX, 0] {
            let d = tempfile::tempdir().unwrap();
            let source = d.path().join("DSC.ARW");
            let bytes: Vec<u8> = (0..2_500_000u32).map(|i| (i * 31 % 251) as u8).collect();
            fs::write(&source, &bytes).unwrap();
            let (to_a, to_b) = (d.path().join("a.ARW"), d.path().join("b.ARW"));
            let mut copy = copy_verified_from(
                &source,
                &[&to_a, &to_b],
                &AtomicBool::new(false),
                |_| {},
                None,
                in_memory,
            )
            .unwrap();
            let mut barrier = Barrier::default();
            assert_eq!(copy.publish(1, &mut barrier).unwrap(), to_b);
            assert_eq!(copy.publish(0, &mut barrier).unwrap(), to_a);
            barrier.flush().unwrap();
            assert_eq!(fs::read(&to_a).unwrap(), bytes);
            assert_eq!(fs::read(&to_b).unwrap(), bytes);
            assert_eq!(copy.sha256, hex(Sha256::digest(&bytes)));
            assert!(copy.publish(0, &mut barrier).is_err());
        }
    }

    /// A source changed after it was read ahead (same size, a byte
    /// different) is not copied: the read again finds it changed.
    #[test]
    fn a_source_changed_after_it_was_read_ahead_is_not_copied() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("DSC.ARW");
        fs::write(&source, vec![1u8; 1_000_000]).unwrap();
        let ahead = ReadAhead::start(vec![source.clone()]);
        let read = ahead.take(&source).expect("read ahead");
        corrupt(&source);
        let target = d.path().join("copy.ARW");
        let error = copy_verified_from(
            &source,
            &[&target],
            &AtomicBool::new(false),
            |_| {},
            Some(read),
            IN_MEMORY_LIMIT,
        )
        .err()
        .expect("changed source");
        assert!(error.to_string().contains("Source changed"), "{error}");
        assert!(!target.exists());
    }

    /// Files are handed over in order; one asked out of turn is not
    /// waited for, files passed over are dropped, and stopping early ends
    /// the threads.
    #[test]
    fn read_ahead_hands_files_over_in_order() {
        let d = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = (0..5)
            .map(|i| {
                let path = d.path().join(format!("{i}.ARW"));
                fs::write(&path, vec![i as u8; 10_000 + i]).unwrap();
                path
            })
            .collect();
        let ahead = ReadAhead::start(paths.clone());
        let first = ahead.take(&paths[0]).unwrap();
        assert_eq!(first.buffer, vec![0u8; 10_000]);
        assert_eq!(first.sha256, hex(Sha256::digest(&first.buffer)));
        // 1 is passed over; 0 again or a file it never read: none.
        let third = ahead.take(&paths[2]).unwrap();
        assert_eq!(third.buffer, vec![2u8; 10_002]);
        assert!(ahead.take(&paths[1]).is_none());
        assert!(ahead.take(&paths[0]).is_none());
        assert!(ahead.take(&d.path().join("other.ARW")).is_none());
        drop(ahead);
        // A file gone before it was read is skipped, not waited for.
        fs::remove_file(&paths[3]).unwrap();
        let ahead = ReadAhead::start(paths.clone());
        assert!(ahead.take(&paths[3]).is_none());
        assert_eq!(ahead.take(&paths[4]).unwrap().buffer.len(), 10_004);
    }

    /// The drive cache is flushed once per volume for everything synced to
    /// it; a failed flush keeps it pending.
    #[test]
    #[cfg(target_vendor = "apple")]
    fn one_flush_per_volume() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let mut barrier = Barrier::default();
        barrier.sync_folder(&a).unwrap();
        barrier.sync_folder(&b).unwrap();
        assert_eq!(barrier.pending.len(), 1);
        FAIL_FLUSH.with(|f| f.set(true));
        assert!(barrier.flush().is_err());
        FAIL_FLUSH.with(|f| f.set(false));
        assert_eq!(barrier.pending.len(), 1);
        barrier.flush().unwrap();
        assert!(barrier.pending.is_empty());
    }
}
