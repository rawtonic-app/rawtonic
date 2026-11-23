//! Read-only access to source files.
use memmap2::{Mmap, MmapOptions};
use std::fmt;
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub enum ReadFileError {
    Io(std::io::Error),
    Locked,
    Empty,
    NotFound,
    Invalid,
}

impl fmt::Display for ReadFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadFileError::Io(err) => write!(f, "IO error: {}", err),
            ReadFileError::Locked => write!(f, "File is locked"),
            ReadFileError::Empty => write!(f, "File is empty"),
            ReadFileError::NotFound => write!(f, "File not found"),
            ReadFileError::Invalid => write!(f, "Invalid file"),
        }
    }
}

/// Maps a source file read-only, refusing empty, missing or locked files.
pub fn read_file_mapped(path: &Path) -> Result<Mmap, ReadFileError> {
    read_file_mapped_with_metadata(path).map(|(mmap, _)| mmap)
}

/// [`read_file_mapped`] and the metadata of the opened file the mapping
/// is of (not of whatever the path names later).
pub fn read_file_mapped_with_metadata(path: &Path) -> Result<(Mmap, fs::Metadata), ReadFileError> {
    if !path.is_file() {
        return Err(ReadFileError::Invalid);
    }
    if !path.exists() {
        return Err(ReadFileError::NotFound);
    }
    if path.metadata().map_err(ReadFileError::Io)?.len() == 0 {
        return Err(ReadFileError::Empty);
    }
    let file = fs::File::open(path).map_err(ReadFileError::Io)?;
    if file.try_lock_shared().is_err() {
        return Err(ReadFileError::Locked);
    }
    let metadata = file.metadata().map_err(ReadFileError::Io)?;
    let mmap = unsafe {
        MmapOptions::new()
            .len(metadata.len() as usize)
            .map(&file)
            .map_err(ReadFileError::Io)?
    };
    Ok((mmap, metadata))
}

/// Size of a read-ahead request, and how many run at once. A mapping of a
/// file on a network share is faulted in one read-around window at a time,
/// a round trip each (13 MB/s of a 110 MB/s link, 7 of 40 on Wi-Fi: ING1);
/// several large reads at once run at the link's speed.
const PREFETCH_CHUNK: usize = 8 << 20;
const PREFETCH_THREADS: usize = 4;

/// Reads `path` into the system's page cache in the background, in parallel
/// and ahead of a mapping that is about to read all of it (a full decode):
/// the mapping's own reads then hit the cache instead of waiting one round
/// trip each on a network share. Reads only, discards what it reads, never
/// fails the caller. The decode reads every byte anyway; on a local disk
/// this only reorders reads of a file that is cached or read at once.
pub fn prefetch(path: &Path) {
    let path = path.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("source-prefetch".into())
        .spawn(move || {
            let Ok(file) = fs::File::open(&path) else {
                return;
            };
            let Ok(len) = file.metadata().map(|m| m.len()) else {
                return;
            };
            prefetch_chunks(len, &|at, buf| read_chunk(&file, at, buf).is_ok());
        });
}

/// Calls `read(offset, buffer)` for each chunk of a file of `len` bytes, from
/// `PREFETCH_THREADS` threads, each chunk once, each thread with one buffer
/// of a chunk's size for all its reads; a thread stops when its read reports
/// failure.
fn prefetch_chunks(len: u64, read: &(dyn Fn(u64, &mut [u8]) -> bool + Sync)) {
    let chunk = PREFETCH_CHUNK as u64;
    let next = std::sync::atomic::AtomicU64::new(0);
    std::thread::scope(|scope| {
        for _ in 0..PREFETCH_THREADS {
            scope.spawn(|| {
                let mut buf = vec![0; PREFETCH_CHUNK];
                loop {
                    let at = next.fetch_add(chunk, std::sync::atomic::Ordering::Relaxed);
                    if at >= len || !read(at, &mut buf) {
                        return;
                    }
                }
            });
        }
    });
}

/// Fills `buf` from `file` at `offset` (less at the end of the file).
fn read_chunk(file: &fs::File, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        #[cfg(unix)]
        let read =
            std::os::unix::fs::FileExt::read_at(file, &mut buf[done..], offset + done as u64);
        #[cfg(windows)]
        let read =
            std::os::windows::fs::FileExt::seek_read(file, &mut buf[done..], offset + done as u64);
        match read? {
            0 => break,
            n => done += n,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    fn file_of(dir: &Path, name: &str, len: usize) -> std::path::PathBuf {
        let path = dir.join(name);
        let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        fs::File::create(&path).unwrap().write_all(&bytes).unwrap();
        path
    }

    /// The by-path facts of the base look read the buffer the caller holds
    /// for that file (the import dialog's mapping) instead of the file, and
    /// the file itself (read into memory) otherwise or after the call.
    #[test]
    fn the_facts_of_a_file_read_the_buffer_held_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_of(dir.path(), "A.ARW", 3 << 20);
        let other = file_of(dir.path(), "B.ARW", 1 << 20);
        let (path, other) = (path.to_str().unwrap(), other.to_str().unwrap());
        let from_disk = |p: &str| {
            crate::capture_info::open_for_metadata(p)
                .unwrap()
                .buf()
                .to_vec()
        };
        let on_disk = from_disk(path);
        assert_eq!(on_disk, fs::read(path).unwrap());
        let held: crate::capture_info::SharedBuf = std::sync::Arc::new(vec![7u8; 100]);
        crate::capture_info::with_held_source(path, held, || {
            assert_eq!(from_disk(path), vec![7u8; 100]);
            assert_eq!(from_disk(other), fs::read(other).unwrap());
        });
        assert_eq!(from_disk(path), on_disk);
        assert!(crate::capture_info::open_for_metadata("/no/such/A.ARW").is_none());
    }

    /// A panic inside the call does not leave the buffer held.
    #[test]
    fn a_held_buffer_is_released_when_the_call_panics() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_of(dir.path(), "A.ARW", 1000);
        let path = path.to_str().unwrap();
        let held: crate::capture_info::SharedBuf = std::sync::Arc::new(vec![1u8; 5]);
        let _ = std::panic::catch_unwind(|| {
            crate::capture_info::with_held_source(path, held, || panic!("decode failed"))
        });
        let source = crate::capture_info::open_for_metadata(path).unwrap();
        assert_eq!(source.buf().len(), 1000);
    }

    /// The read-ahead visits every chunk of a file exactly once, also when
    /// the start of the file is already in memory (every chunk is asked).
    #[test]
    fn every_chunk_is_read_ahead_once() {
        let chunk = PREFETCH_CHUNK as u64;
        for len in [chunk - 1, chunk, 5 * chunk + 7, 40 * chunk] {
            let seen = Mutex::new(Vec::new());
            prefetch_chunks(len, &|at, _| {
                seen.lock().unwrap().push(at);
                true
            });
            let mut seen = seen.into_inner().unwrap();
            seen.sort_unstable();
            let expected: Vec<u64> = (0..len.div_ceil(chunk)).map(|i| i * chunk).collect();
            assert_eq!(seen, expected, "{len}");
        }
    }

    /// A failing read stops its thread; the others finish and none repeats a
    /// chunk.
    #[test]
    fn a_failing_read_stops_only_its_thread() {
        let chunk = PREFETCH_CHUNK as u64;
        let seen = Mutex::new(Vec::new());
        prefetch_chunks(30 * chunk, &|at, _| {
            seen.lock().unwrap().push(at);
            at != chunk
        });
        let mut seen = seen.into_inner().unwrap();
        let count = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), count);
        assert!(seen.contains(&chunk) && count >= 4);
    }

    /// `read_chunk` fills a chunk from its offset and stops at the end of
    /// the file; reading ahead changes nothing in the file.
    #[test]
    fn chunks_are_read_from_their_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let big = file_of(dir.path(), "big.bin", 2 * PREFETCH_CHUNK + 12345);
        let expected = fs::read(&big).unwrap();
        let file = fs::File::open(&big).unwrap();
        let mut buf = vec![0; PREFETCH_CHUNK];
        for chunk in 0..3 {
            let at = chunk * PREFETCH_CHUNK;
            read_chunk(&file, at as u64, &mut buf).unwrap();
            let end = (at + PREFETCH_CHUNK).min(expected.len());
            assert_eq!(&buf[..end - at], &expected[at..end]);
        }
        prefetch(&big);
        prefetch(&dir.path().join("missing.bin"));
        assert_eq!(fs::read(&big).unwrap(), expected);
    }
}
