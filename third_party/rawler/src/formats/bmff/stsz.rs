// SPDX-License-Identifier: MIT
// Copyright 2020 Alfred Gutierrez
// Copyright 2021 Daniel Vogelbacher <daniel@chaospixel.com>

use super::{BoxHeader, FourCC, ReadBox, Result, read_box_header_ext};
use byteorder::{BigEndian, ReadBytesExt};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StszBox {
  pub header: BoxHeader,
  pub version: u8,
  pub flags: u32,
  pub sample_size: u32,
  pub sample_count: u32,
  pub sample_sizes: Vec<u32>,
}

impl StszBox {
  pub const TYP: FourCC = FourCC::with(['s', 't', 's', 'z']);

  // Rawtonic patch (RWH1): `None` for a sample the table doesn't hold.
  pub fn sample_size(&self, sample: u32) -> Option<u32> {
    if self.sample_size > 0 {
      Some(self.sample_size)
    } else {
      self.sample_sizes.get((sample as usize).checked_sub(1)?).copied()
    }
  }
}

impl<R: Read + Seek> ReadBox<&mut R> for StszBox {
  fn read_box(reader: &mut R, header: BoxHeader) -> Result<Self> {
    let (version, flags) = read_box_header_ext(reader)?;

    let sample_size = reader.read_u32::<BigEndian>()?;
    let sample_count = reader.read_u32::<BigEndian>()?;
    let mut sample_sizes = Vec::with_capacity(super::claimed_capacity(sample_count));
    if sample_size == 0 {
      for _ in 0..sample_count {
        let sample_number = reader.read_u32::<BigEndian>()?;
        sample_sizes.push(sample_number);
      }
    } else {
      // If the sample_size is non-zero, it is the only sample_size
      sample_sizes.push(sample_size);
    }

    reader.seek(SeekFrom::Start(header.end_offset()))?;

    Ok(Self {
      header,
      version,
      flags,
      sample_size,
      sample_count,
      sample_sizes,
    })
  }
}
