// SPDX-License-Identifier: MIT
// Copyright 2020 Alfred Gutierrez
// Copyright 2021 Daniel Vogelbacher <daniel@chaospixel.com>

use super::{BoxHeader, FourCC, ReadBox, Result, read_box_header_ext};
use byteorder::{BigEndian, ReadBytesExt};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StscBox {
  pub header: BoxHeader,
  pub version: u8,
  pub flags: u32,
  pub entries: Vec<StscEntry>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StscEntry {
  pub first_chunk: u32,
  pub samples_per_chunk: u32,
  pub sample_description_index: u32,
  pub first_sample: u32,
}

impl StscBox {
  pub const TYP: FourCC = FourCC::with(['s', 't', 's', 'c']);

  // Rawtonic patch (RWH1; dnglab 650558fa): `None` for a damaged table
  // instead of the asserts.
  pub fn get_entry_for_sample(&self, sample: u32) -> Option<&StscEntry> {
    if sample == 0 || self.entries.first()?.first_sample != 1 {
      return None;
    }
    match self.entries.binary_search_by(|entry| entry.first_sample.cmp(&sample)) {
      Ok(i) => self.entries.get(i),
      Err(i) => self.entries.get(i.checked_sub(1)?),
    }
  }
}

impl<R: Read + Seek> ReadBox<&mut R> for StscBox {
  fn read_box(reader: &mut R, header: BoxHeader) -> Result<Self> {
    let (version, flags) = read_box_header_ext(reader)?;

    let entry_count = reader.read_u32::<BigEndian>()?;
    let mut entries = Vec::with_capacity(super::claimed_capacity(entry_count));

    // Reader for a single stsc entry
    let mut read_entry = || -> Result<StscEntry> {
      Ok(StscEntry {
        first_chunk: reader.read_u32::<BigEndian>()?,
        samples_per_chunk: reader.read_u32::<BigEndian>()?,
        sample_description_index: reader.read_u32::<BigEndian>()?,
        first_sample: 0,
      })
    };

    if entry_count > 0 {
      // Read first entry and hold it back
      let mut holdback = read_entry()?;
      holdback.first_sample = 1;

      for _ in 1..entry_count {
        let mut entry = read_entry()?;
        // Now we know the chunk count to calc the amount of samples in holdback
        // Rawtonic patch (RWH1): wrapping, as release builds computed it.
        entry.first_sample = holdback
          .first_sample
          .wrapping_add(entry.first_chunk.wrapping_sub(holdback.first_sample).wrapping_mul(holdback.samples_per_chunk));
        entries.push(holdback);
        holdback = entry;
      }
      // Finalize entry list
      entries.push(holdback);
    }

    reader.seek(SeekFrom::Start(header.end_offset()))?;

    Ok(Self {
      header,
      version,
      flags,
      entries,
    })
  }
}
