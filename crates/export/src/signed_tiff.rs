//! A TIFF with a provenance manifest, hashed while its pixels are written
//! instead of read back once the file is complete (notes:
//! `benchmark-results/c2p2-2026-11-21/REPORT.md`; a 16-bit 24 MP file is
//! 144 MB, whose SHA-256 alone takes ~48 ms on an M4 Pro).
//!
//! The hash covers the file from its first byte, and the TIFF header's IFD
//! pointer is only final once the metadata and the manifest's IFD are
//! appended. So the file is written in two passes:
//!
//! 1. **Layout**: the encoder writes everything but the strips, which are
//!    left as one run of unwritten bytes ([`LayoutWriter`]); then the
//!    metadata and the reserved manifest ([`crate::c2pa::reserve_tiff`])
//!    are appended. Every byte outside the strips is final now.
//! 2. **Pixels**: the same encoder runs again with the render; its strips
//!    go into their run, in order, and to a hashing thread, the bytes
//!    before and after the run (a few KB) are read back for it
//!    ([`FillWriter`]). Its other writes are the layout pass's and are
//!    dropped.
//!
//! The manifest is then signed with that hash: the binding still covers
//! every byte of the file but the manifest, as [`crate::c2pa::sign_tiff`]'s.

use std::cell::Cell;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::mpsc;

use ring::digest;

/// Bytes the pixel pass hands the hashing thread at a time.
const CHUNK: usize = 4 << 20;

fn position(pos: u64, to: SeekFrom) -> io::Result<u64> {
    match to {
        SeekFrom::Start(at) => Ok(at),
        SeekFrom::Current(by) => pos
            .checked_add_signed(by)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start")),
        SeekFrom::End(_) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a signed TIFF's encoder seeks from its end",
        )),
    }
}

/// The layout pass's writer: what the encoder writes while `in_strip` is
/// set (a strip) is only recorded, the rest goes to the file.
pub(crate) struct LayoutWriter<'a, S> {
    file: &'a mut S,
    pos: u64,
    file_pos: Option<u64>,
    in_strip: &'a Cell<bool>,
    /// The strips' run (start, end).
    strips: Option<(u64, u64)>,
    scattered: bool,
}

impl<S: Write + Seek> Write for LayoutWriter<'_, S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let end = self.pos + buf.len() as u64;
        if self.in_strip.get() {
            self.strips = match self.strips {
                None => Some((self.pos, end)),
                Some((start, last)) if last == self.pos => Some((start, end)),
                strips => {
                    self.scattered = true;
                    strips
                }
            };
        } else {
            if self.file_pos != Some(self.pos) {
                self.file.seek(SeekFrom::Start(self.pos))?;
            }
            self.file.write_all(buf)?;
            self.file_pos = Some(end);
        }
        self.pos = end;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl<S> Seek for LayoutWriter<'_, S> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = position(self.pos, to)?;
        Ok(self.pos)
    }
}

/// The pixel pass's writer: strip bytes go into their run in the file and
/// to the hashing thread; everything else the encoder writes is the layout
/// pass's, already in the file.
pub(crate) struct FillWriter<'a, S> {
    file: &'a mut S,
    pos: u64,
    strips: (u64, u64),
    /// The next strip byte expected.
    next: u64,
    chunk: Vec<u8>,
    chunk_at: u64,
    hasher: &'a mpsc::SyncSender<Vec<u8>>,
    spare: &'a mpsc::Receiver<Vec<u8>>,
}

impl<S: Write + Seek> FillWriter<'_, S> {
    fn pass_on(&mut self) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(self.chunk_at))?;
        self.file.write_all(&self.chunk)?;
        self.chunk_at += self.chunk.len() as u64;
        let empty = match self.spare.try_recv() {
            Ok(mut used) => {
                used.clear();
                used
            }
            Err(_) => Vec::with_capacity(CHUNK),
        };
        let full = std::mem::replace(&mut self.chunk, empty);
        self.hasher
            .send(full)
            .map_err(|_| io::Error::other("the hashing thread stopped"))
    }

    fn finish(mut self) -> io::Result<()> {
        if !self.chunk.is_empty() {
            self.pass_on()?;
        }
        if self.next != self.strips.1 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the TIFF's strips were not all written",
            ));
        }
        Ok(())
    }
}

impl<S: Write + Seek> Write for FillWriter<'_, S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let end = self.pos + buf.len() as u64;
        let (start, stop) = self.strips;
        if end > start && self.pos < stop {
            if self.pos != self.next || end > stop {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the TIFF's strips differ from its layout",
                ));
            }
            if !self.chunk.is_empty() && self.chunk.len() + buf.len() > CHUNK {
                self.pass_on()?;
            }
            self.chunk.extend_from_slice(buf);
            self.next = end;
        }
        self.pos = end;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<S> Seek for FillWriter<'_, S> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = position(self.pos, to)?;
        Ok(self.pos)
    }
}

/// `len` bytes of `file` from `at`.
fn read_range<S: Read + Seek>(file: &mut S, at: u64, len: u64) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; usize::try_from(len).map_err(io::Error::other)?];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Writes a TIFF signed with the manifest of `plan` into `file` (empty, at
/// position 0). `encode` writes the image into its writer: called twice,
/// it must write the same bytes but the strips, which it writes while the
/// flag it gets is set (the layout pass, with any strip data) and as the
/// render gives them (the pixel pass, where the flag is not used).
/// `metadata` appends the metadata to the file between the passes.
pub(crate) fn write<S, L, P>(
    file: &mut S,
    layout: L,
    metadata: impl FnOnce(&mut S) -> Result<(), String>,
    pixels: P,
    plan: &crate::c2pa::ManifestPlan,
    identity: &crate::c2pa::SigningIdentity,
) -> Result<(), String>
where
    S: Read + Write + Seek,
    L: FnOnce(&mut LayoutWriter<'_, S>, &Cell<bool>) -> Result<(), String>,
    P: FnOnce(&mut FillWriter<'_, S>) -> Result<(), String>,
{
    let failed = |e: io::Error| format!("The signed TIFF could not be written: {e}");
    let in_strip = Cell::new(false);
    let strips = {
        let mut writer = LayoutWriter {
            file: &mut *file,
            pos: 0,
            file_pos: None,
            in_strip: &in_strip,
            strips: None,
            scattered: false,
        };
        layout(&mut writer, &in_strip)?;
        writer.flush().map_err(failed)?;
        match writer.strips {
            Some(strips) if !writer.scattered => strips,
            _ => return Err("The TIFF's strips are not one run of bytes.".to_string()),
        }
    };
    metadata(file)?;
    let manifest = crate::c2pa::reserve_tiff(file, plan, identity)?;
    let at = manifest.start();
    if strips.1 > at {
        return Err("The TIFF's strips run into its manifest.".to_string());
    }

    let hash = std::thread::scope(|scope| {
        let (to_hasher, chunks) = mpsc::sync_channel::<Vec<u8>>(2);
        let (give_back, spare) = mpsc::channel();
        let hasher = scope.spawn(move || {
            let mut context = digest::Context::new(&digest::SHA256);
            for chunk in chunks {
                context.update(&chunk);
                // Reused by the writer; gone once it has finished.
                let _ = give_back.send(chunk);
            }
            context
        });
        let filled = (|| -> Result<(), String> {
            let send = |bytes| {
                to_hasher
                    .send(bytes)
                    .map_err(|_| "The hashing thread stopped.".to_string())
            };
            send(read_range(file, 0, strips.0).map_err(failed)?)?;
            let mut writer = FillWriter {
                file: &mut *file,
                pos: 0,
                strips,
                next: strips.0,
                chunk: Vec::with_capacity(CHUNK),
                chunk_at: strips.0,
                hasher: &to_hasher,
                spare: &spare,
            };
            pixels(&mut writer)?;
            writer.finish().map_err(failed)?;
            let mut from = strips.1;
            while from < at {
                let len = (at - from).min(CHUNK as u64);
                send(read_range(file, from, len).map_err(failed)?)?;
                from += len;
            }
            Ok(())
        })();
        drop(to_hasher);
        let context = hasher
            .join()
            .map_err(|_| "The hashing thread failed.".to_string())?;
        filled.map(|()| context.finish())
    })?;
    manifest.sign(file, hash.as_ref())
}
