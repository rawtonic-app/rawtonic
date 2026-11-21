use std::{
  fmt::Debug,
  fs::File,
  io::Cursor,
  iter::repeat,
  ops::Deref,
  path::{Path, PathBuf},
  sync::Arc,
};

use md5::Digest;
use memmap2::MmapOptions;

use crate::buffer::PaddedBuf;

pub struct RawSource {
  path: PathBuf,
  inner: RawSourceImpl,
}

enum RawSourceImpl {
  Memmap(memmap2::Mmap),
  Memory(Arc<Vec<u8>>),
  // Rawtonic patch (CRD1): a caller's buffer shared without a copy.
  Shared(Arc<dyn AsRef<[u8]> + Send + Sync + std::panic::RefUnwindSafe>),
}

impl RawSource {
  pub fn new(path: &Path) -> std::io::Result<Self> {
    let file = File::open(path)?;
    let mmap = unsafe { MmapOptions::new().populate().map(&file)? };
    #[cfg(unix)]
    {
      mmap.advise(memmap2::Advice::WillNeed)?;
      mmap.advise(memmap2::Advice::Sequential)?;
    }
    Ok(Self {
      path: path.canonicalize().unwrap_or_else(|_| path.to_owned()),
      inner: RawSourceImpl::Memmap(mmap),
    })
  }

  // Rawtonic patch (RAWC2): new constructor.
  /// Maps the file without reading it ahead (no `populate`, no
  /// read-ahead advice), for callers that only look at a file's headers,
  /// e.g. telling a raw TIFF from an RGB one while listing a folder.
  pub fn new_lazy(path: &Path) -> std::io::Result<Self> {
    let file = File::open(path)?;
    let mmap = unsafe { MmapOptions::new().map(&file)? };
    Ok(Self {
      path: path.to_owned(),
      inner: RawSourceImpl::Memmap(mmap),
    })
  }

  pub fn new_from_shared_vec(buf: Arc<Vec<u8>>) -> Self {
    Self {
      path: PathBuf::default(),
      inner: RawSourceImpl::Memory(buf),
    }
  }

  // Rawtonic patch (CRD1): new constructor.
  /// Shares a buffer the caller already holds (e.g. its own mapping of the
  /// file) without copying it, so reading a file's metadata only touches
  /// the bytes the decoder looks at; `new_from_slice` copies every byte.
  pub fn new_from_shared(buf: Arc<dyn AsRef<[u8]> + Send + Sync + std::panic::RefUnwindSafe>) -> Self {
    Self {
      path: PathBuf::default(),
      inner: RawSourceImpl::Shared(buf),
    }
  }

  pub fn with_path(self, path: impl AsRef<Path>) -> Self {
    Self {
      path: path.as_ref().to_owned(),
      inner: self.inner,
    }
  }

  pub fn new_from_slice(buf: &[u8]) -> Self {
    Self::new_from_shared_vec(Arc::new(Vec::from(buf)))
  }

  /// Calculate digest for file
  pub fn digest(&self) -> Digest {
    md5::compute(self.buf())
  }

  pub fn path(&self) -> &Path {
    &self.path
  }

  pub fn buf(&self) -> &[u8] {
    self.deref()
  }

  pub fn subview(&self, offset: u64, size: u64) -> std::io::Result<&[u8]> {
    self.buf().get(offset as usize..(offset + size) as usize).ok_or(std::io::Error::new(
      std::io::ErrorKind::UnexpectedEof,
      format!("subview(): Offset {}+{} is behind EOF", offset, size),
    ))
  }

  pub fn subview_padded(&self, offset: u64, size: u64) -> std::io::Result<PaddedBuf<'_>> {
    if offset + size <= self.len() as u64 {
      if offset + size + 16 <= self.len() as u64 {
        self.subview(offset, size + 16).map(|buf| PaddedBuf::new_ref(buf, size as usize))
      } else {
        let mut buf = Vec::with_capacity((size + 16) as usize);
        buf.extend_from_slice(self.subview(offset, size)?);
        buf.extend(repeat(0).take(16));
        Ok(PaddedBuf::new_owned(buf, size as usize))
      }
    } else {
      Err(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        format!("subview_padded(): Offset {}+{} is behind EOF", offset, size),
      ))
    }
  }

  pub fn subview_until_eof(&self, offset: u64) -> std::io::Result<&[u8]> {
    self.buf().get(offset as usize..).ok_or(std::io::Error::new(
      std::io::ErrorKind::UnexpectedEof,
      format!("subview_until_eof(): Offset {} is behind EOF", offset),
    ))
  }

  pub fn subview_until_eof_padded(&self, offset: u64) -> std::io::Result<PaddedBuf<'_>> {
    if offset < self.len() as u64 {
      let mut buf = Vec::with_capacity((self.len() - offset as usize + 16) as usize);
      buf.extend_from_slice(self.subview_until_eof(offset)?);
      buf.extend(repeat(0).take(16));
      Ok(PaddedBuf::new_owned(buf, self.len() - offset as usize))
    } else {
      Err(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        format!("subview_until_eof_padded(): Offset {} is behind EOF", offset),
      ))
    }
  }

  // Rawtonic patch (CRD2): new methods.
  /// `subview_padded` for a decoder that reads the samples only when
  /// `dummy` is false. A dummy decode (sizes and metadata, no samples) gets
  /// a zeroed buffer of the same length where `subview_padded` would copy
  /// the strip (it ends within 16 bytes of EOF, as the raw data of most
  /// RAWs does): the copy read the whole strip, on a share every byte.
  pub fn subview_padded_for(&self, offset: u64, size: u64, dummy: bool) -> std::io::Result<PaddedBuf<'_>> {
    if dummy && offset + size <= self.len() as u64 && offset + size + 16 > self.len() as u64 {
      return Ok(PaddedBuf::new_owned(vec![0; (size + 16) as usize], size as usize));
    }
    self.subview_padded(offset, size)
  }

  /// `subview_until_eof_padded` as [`Self::subview_padded_for`] does it.
  pub fn subview_until_eof_padded_for(&self, offset: u64, dummy: bool) -> std::io::Result<PaddedBuf<'_>> {
    if dummy && offset < self.len() as u64 {
      let size = self.len() - offset as usize;
      return Ok(PaddedBuf::new_owned(vec![0; size + 16], size));
    }
    self.subview_until_eof_padded(offset)
  }

  pub fn reader(&self) -> Cursor<&[u8]> {
    Cursor::new(self.buf())
  }

  pub fn as_vec(&self) -> std::io::Result<Vec<u8>> {
    Ok(self.buf().to_vec())
  }

  pub fn stream_len(&mut self) -> u64 {
    self.buf().len() as u64
  }
}

impl Deref for RawSource {
  type Target = [u8];

  fn deref(&self) -> &Self::Target {
    match &self.inner {
      RawSourceImpl::Memmap(mmap) => mmap.deref(),
      RawSourceImpl::Memory(mem) => mem.deref(),
      RawSourceImpl::Shared(buf) => buf.as_ref().as_ref(),
    }
  }
}

impl Debug for RawSource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("RawSource").field("path", &self.path).finish()
  }
}

// Rawtonic patch (CRD2): tests of the dummy views.
#[cfg(test)]
mod rawtonic_dummy_view_tests {
  use super::*;

  #[test]
  fn a_dummy_view_near_eof_has_the_padded_length_without_the_strip() {
    let source = RawSource::new_from_shared_vec(Arc::new((1..=100u8).collect()));
    // Ends within 16 bytes of EOF: the real view copies, the dummy one is zeros.
    let real = source.subview_padded(60, 30).unwrap();
    let dummy = source.subview_padded_for(60, 30, true).unwrap();
    assert_eq!((dummy.len(), dummy.size()), (real.len(), real.size()));
    assert!(dummy.iter().all(|b| *b == 0));
    assert_eq!(&source.subview_padded_for(60, 30, false).unwrap()[..], &real[..]);
    // Far from EOF the dummy view is the file's bytes, as before.
    assert_eq!(
      &source.subview_padded_for(10, 30, true).unwrap()[..],
      &source.subview_padded(10, 30).unwrap()[..]
    );
    let real = source.subview_until_eof_padded(40).unwrap();
    let dummy = source.subview_until_eof_padded_for(40, true).unwrap();
    assert_eq!((dummy.len(), dummy.size()), (real.len(), real.size()));
    assert!(dummy.iter().all(|b| *b == 0));
    assert_eq!(&source.subview_until_eof_padded_for(40, false).unwrap()[..], &real[..]);
    // Out of range stays an error.
    assert!(source.subview_padded_for(90, 30, true).is_err());
    assert!(source.subview_until_eof_padded_for(100, true).is_err());
  }
}
