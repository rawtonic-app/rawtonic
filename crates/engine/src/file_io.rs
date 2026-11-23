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

/// Share of a mapped file read ahead per request, and how many requests
/// run at once. A mapping of a file on a network share is faulted in one
/// read-around window at a time, a round trip each (13 MB/s of a 110 MB/s
/// link, 7 of 40 on Wi-Fi: ING1); several large reads at once run at the
/// link's speed.
const PREFETCH_CHUNK: usize = 8 << 20;
const PREFETCH_THREADS: usize = 4;
/// A first chunk read faster than this (about 320 MB/s) is a local disk or
/// a file already in memory: the mapping needs no help.
const PREFETCH_LOCAL: std::time::Duration = std::time::Duration::from_millis(25);

/// Reads `path` into the system's page cache in the background, in parallel
/// and ahead of a mapping that is about to read all of it (a full decode),
/// unless the first part reads fast (a local disk): the mapping's own reads
/// then hit the cache instead of waiting one round trip each on a network
/// share. Reads only, discards what it reads, never fails the caller.
pub fn prefetch(path: &Path) {
    let path = path.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("source-prefetch".into())
        .spawn(move || prefetch_now(&path));
}

fn prefetch_now(path: &Path) {
    let Ok(file) = fs::File::open(path) else {
        return;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return;
    };
    let chunk = PREFETCH_CHUNK as u64;
    if len <= chunk {
        return;
    }
    let started = std::time::Instant::now();
    if read_chunk(&file, 0, &mut vec![0; PREFETCH_CHUNK]).is_err()
        || started.elapsed() < PREFETCH_LOCAL
    {
        return;
    }
    let next = std::sync::atomic::AtomicU64::new(chunk);
    std::thread::scope(|scope| {
        for _ in 0..PREFETCH_THREADS {
            scope.spawn(|| {
                let mut buf = vec![0; PREFETCH_CHUNK];
                loop {
                    let at = next.fetch_add(chunk, std::sync::atomic::Ordering::Relaxed);
                    if at >= len || read_chunk(&file, at, &mut buf).is_err() {
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

    fn file_of(dir: &Path, name: &str, len: usize) -> std::path::PathBuf {
        let path = dir.join(name);
        let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        fs::File::create(&path).unwrap().write_all(&bytes).unwrap();
        path
    }

    /// The metadata readers of the base look open a RAW the same way on a
    /// disk (mapped) and on a card (read into memory): the same bytes.
    #[test]
    fn a_file_opened_for_metadata_has_its_bytes_on_a_disk_and_on_a_card() {
        let dir = tempfile::tempdir().unwrap();
        let disk = file_of(dir.path(), "A.ARW", 3 << 20);
        let card_dir = dir.path().join("DCIM").join("100MSDCF");
        fs::create_dir_all(&card_dir).unwrap();
        let card = file_of(&card_dir, "A.ARW", 3 << 20);
        for path in [&disk, &card] {
            let source = crate::capture_info::open_for_metadata(path.to_str().unwrap()).unwrap();
            assert_eq!(source.buf(), fs::read(path).unwrap(), "{}", path.display());
        }
        assert!(crate::capture_info::open_for_metadata("/no/such/A.ARW").is_none());
        let empty = file_of(dir.path(), "empty.ARW", 0);
        // An empty file opens empty: no decoder takes it, as before.
        let source = crate::capture_info::open_for_metadata(empty.to_str().unwrap());
        assert!(source.is_none_or(|source| source.buf().is_empty()));
    }

    /// The read-ahead reads a file of several chunks to its end without
    /// touching it, and ignores a missing or small one.
    #[test]
    fn prefetching_leaves_the_file_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let big = file_of(dir.path(), "big.bin", 3 * PREFETCH_CHUNK + 12345);
        let before = fs::read(&big).unwrap();
        prefetch_now(&big);
        prefetch_now(&file_of(dir.path(), "small.bin", 1000));
        prefetch_now(&dir.path().join("missing.bin"));
        assert_eq!(fs::read(&big).unwrap(), before);
    }

    /// With a slow first chunk the parallel part reads every chunk once
    /// (what a share's read does after the first chunk is slow).
    #[test]
    fn every_chunk_of_a_file_is_read_ahead_once() {
        let dir = tempfile::tempdir().unwrap();
        let big = file_of(dir.path(), "big.bin", 5 * PREFETCH_CHUNK + 7);
        let file = fs::File::open(&big).unwrap();
        let mut seen = 0;
        let mut buf = vec![0; PREFETCH_CHUNK];
        for chunk in 0..6u64 {
            read_chunk(&file, chunk * PREFETCH_CHUNK as u64, &mut buf).unwrap();
            let expected = fs::read(&big).unwrap();
            let at = chunk as usize * PREFETCH_CHUNK;
            let end = (at + PREFETCH_CHUNK).min(expected.len());
            assert_eq!(&buf[..end - at], &expected[at..end]);
            seen += end - at;
        }
        assert_eq!(seen, 5 * PREFETCH_CHUNK + 7);
    }
}
