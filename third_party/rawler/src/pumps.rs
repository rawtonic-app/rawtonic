use std::slice::Chunks;

use crate::bits::*;

#[derive(Debug, Clone)]
pub struct BitPumpLSB<'a> {
  buffer: Chunks<'a, u8>,
  bits: u64,
  nbits: u32,
  overrun: bool,
}

impl<'a> BitPumpLSB<'a> {
  pub fn new(src: &'a [u8]) -> Self {
    Self {
      buffer: src.chunks(size_of::<u32>()),
      bits: 0,
      nbits: 0,
      overrun: false,
    }
  }

  /// Refill internal bit buffer - Little-Endian
  ///
  /// For fast refill, we can simply take a whole u32 value out.
  /// For slow refill, there may be 1, 2 or 3 bytes left in buffer. We need
  /// to collect them manually.
  fn refill(&mut self) -> (u32, u32) {
    if let Some(chunk) = self.buffer.next() {
      if chunk.len() == 4 {
        // Fast refill
        let bits: u32 = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        (bits, u32::BITS)
      } else {
        // Slow refill
        chunk
          .into_iter()
          .rev()
          .fold((0, 0), |(bits, bit_cnt), x| ((bits << 8) | *x as u32, bit_cnt + 8))
      }
    } else {
      // Rawtonic patch (RWH1): reading past the end yields zero bits and
      // marks the pump as overrun instead of panicking; decoders turn an
      // overrun into an error (`BitPump::overrun`). A file that decoded
      // before never got here (it panicked), so valid decodes are unchanged.
      self.overrun = true;
      (0, u32::BITS)
    }
  }
}

#[derive(Debug, Clone)]
pub struct BitPumpMSB<'a> {
  buffer: Chunks<'a, u8>,
  bits: u64,
  nbits: u32,
  overrun: bool,
}

impl<'a> BitPumpMSB<'a> {
  /// Rawtonic patch (RWH1): a decoder found an invalid code in the stream;
  /// `overrun()` reports it like data that ended early.
  pub fn mark_damaged(&mut self) {
    self.overrun = true;
  }

  pub fn new(src: &'a [u8]) -> Self {
    Self {
      buffer: src.chunks(size_of::<u32>()),
      bits: 0,
      nbits: 0,
      overrun: false,
    }
  }

  /// Refill internal bit buffer - Big-Endian
  ///
  /// For fast refill, we can simply take a whole u32 value out.
  /// For slow refill, there may be 1, 2 or 3 bytes left in buffer. We need
  /// to collect them manually.
  fn refill(&mut self) -> (u32, u32) {
    if let Some(chunk) = self.buffer.next() {
      if chunk.len() == 4 {
        // Fast refill
        let bits: u32 = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        (bits, u32::BITS)
      } else {
        // Slow refill
        chunk.into_iter().fold((0, 0), |(bits, bit_cnt), x| ((bits << 8) | *x as u32, bit_cnt + 8))
      }
    } else {
      // Rawtonic patch (RWH1): reading past the end yields zero bits and
      // marks the pump as overrun instead of panicking; decoders turn an
      // overrun into an error (`BitPump::overrun`). A file that decoded
      // before never got here (it panicked), so valid decodes are unchanged.
      self.overrun = true;
      (0, u32::BITS)
    }
  }
}

#[derive(Debug, Clone)]
pub struct BitPumpMSB32<'a> {
  buffer: Chunks<'a, u8>,
  pos: usize,
  bits: u64,
  nbits: u32,
  overrun: bool,
}

impl<'a> BitPumpMSB32<'a> {
  pub fn new(src: &'a [u8]) -> Self {
    Self {
      buffer: src.chunks(size_of::<u32>()),
      pos: 0,
      bits: 0,
      nbits: 0,
      overrun: false,
    }
  }

  /// Refill internal bit buffer - Little-Endian
  ///
  /// For fast refill, we can simply take a whole u32 value out.
  /// For slow refill, there may be 1, 2 or 3 bytes left in buffer. We need
  /// to collect them manually.
  fn refill(&mut self) -> (u32, u32) {
    if let Some(chunk) = self.buffer.next() {
      if chunk.len() == 4 {
        // Fast refill
        let bits: u32 = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        (bits, u32::BITS)
      } else {
        // RAWTONIC: the last, partial word is read as a whole word padded
        // with zero bytes. `peek_bits` always shifts by 32 bits, so the old
        // short count left stale bits above the valid ones (garbage codes and
        // an out-of-bounds Huffman lookup at the end of Hasselblad X1D II
        // streams whose length is not a multiple of 4).
        let mut word = [0_u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        (u32::from_le_bytes(word), u32::BITS)
      }
    } else {
      // RAWTONIC: reading past the end yields zero bits instead of a panic
      // (a truncated stream decodes to wrong pixels at its end, not a crash).
      // RWH1: and marks the pump as overrun (`BitPump::overrun`).
      self.overrun = true;
      (0, u32::BITS)
    }
  }

  #[inline(always)]
  pub fn get_pos(&self) -> usize {
    self.pos - ((self.nbits >> 3) as usize)
  }
}

#[derive(Debug, Copy, Clone)]
pub struct BitPumpJPEG<'a> {
  buffer: &'a [u8],
  pos: usize,
  bits: u64,
  nbits: u32,
  finished: bool,
}

impl<'a> BitPumpJPEG<'a> {
  pub fn new(src: &'a [u8]) -> Self {
    Self {
      buffer: src,
      pos: 0,
      bits: 0,
      nbits: 0,
      finished: false,
    }
  }

  /// Continue after a restart marker (RST0..RST7). Rawtonic patch.
  ///
  /// The pump never reads past a marker (it stops on the marker byte after
  /// the 0xFF), so everything still buffered belongs to the finished
  /// interval (its 1-bit padding) and is dropped; the marker itself and any
  /// padding bytes in front of it are skipped. A data byte 0xFF is always
  /// followed by a stuffed 0x00, so 0xFF plus RSTn is unambiguous.
  pub fn restart(&mut self) -> Result<(), String> {
    self.bits = 0;
    self.nbits = 0;
    self.finished = false;
    self.pos = self.pos.saturating_sub(1);
    let start = self.pos;
    while self.pos + 1 < self.buffer.len() {
      if self.pos - start > 8 {
        break;
      }
      if self.buffer[self.pos] == 0xff {
        let marker = self.buffer[self.pos + 1];
        if (0xd0..=0xd7).contains(&marker) {
          self.pos += 2;
          return Ok(());
        } else if marker == 0x00 {
          self.pos += 2; // stuffed data byte in the padding
          continue;
        } else if marker != 0xff {
          return Err(format!("ljpeg: expected a restart marker, found 0xFF{:02X}", marker));
        }
      }
      self.pos += 1;
    }
    Err("ljpeg: restart marker missing".to_string())
  }
}

// Rawtonic patch (RWH1): the shifts and bit counts of the pumps below wrap
// explicitly. That is what release builds computed for the oversized reads
// of damaged files; debug builds (tests) panicked on them.
pub trait BitPump {
  fn peek_bits(&mut self, num: u32) -> u32;
  fn consume_bits(&mut self, num: u32);

  /// Rawtonic patch (RWH1): true once a read needed bits past the end of the
  /// data (the pump supplied zeros). Decoders report it as a damaged file.
  fn overrun(&self) -> bool {
    false
  }

  #[inline(always)]
  fn get_bits(&mut self, num: u32) -> u32 {
    if num == 0 {
      return 0;
    }

    let val = self.peek_bits(num);
    self.consume_bits(num);

    val
  }

  #[inline(always)]
  fn peek_ibits(&mut self, num: u32) -> i32 {
    self.peek_bits(num) as i32
  }

  #[inline(always)]
  fn get_ibits(&mut self, num: u32) -> i32 {
    self.get_bits(num) as i32
  }

  // Sign extend ibits
  #[inline(always)]
  fn get_ibits_sextended(&mut self, num: u32) -> i32 {
    let val = self.get_ibits(num);
    val.wrapping_shl(32_u32.wrapping_sub(num)).wrapping_shr(32_u32.wrapping_sub(num))
  }

  /// Count the leading zeroes block-wise in 31 bits
  /// per block and returns the count.
  /// All zero bits are consumed.
  #[inline(always)]
  fn consume_zerobits(&mut self) -> u32 {
    // Take one bit less because leading_zeros() is undefined
    // when all bits in register are zero.
    const BITS_PER_LOOP: u32 = u32::BITS - 1;
    let mut count = 0;
    // Count-and-skip all the leading `0`s.
    loop {
      let batch: u32 = (self.peek_bits(BITS_PER_LOOP) << 1) | 0x1;
      let n = batch.leading_zeros();
      self.consume_bits(n);
      count += n;
      // Rawtonic patch (RWH1): past the end the pump supplies zeros forever.
      if n != BITS_PER_LOOP || self.overrun() {
        break;
      }
    }
    count
  }
}

impl<'a> BitPump for BitPumpLSB<'a> {
  fn overrun(&self) -> bool {
    self.overrun
  }

  #[inline(always)]
  fn peek_bits(&mut self, num: u32) -> u32 {
    if num > self.nbits {
      let (inbits, bit_cnt) = self.refill();
      self.bits = (((inbits as u64) << 32) | self.bits.wrapping_shl(32_u32.wrapping_sub(self.nbits))).wrapping_shr(32_u32.wrapping_sub(self.nbits));
      self.nbits += bit_cnt;
    }
    (self.bits & 0x0ffffffffu64.wrapping_shr(32_u32.wrapping_sub(num))) as u32
  }

  #[inline(always)]
  fn consume_bits(&mut self, num: u32) {
    self.nbits = self.nbits.wrapping_sub(num);
    self.bits = self.bits.wrapping_shr(num);
  }
}

impl<'a> BitPump for BitPumpMSB<'a> {
  fn overrun(&self) -> bool {
    self.overrun
  }

  #[inline(always)]
  fn peek_bits(&mut self, num: u32) -> u32 {
    if num > self.nbits {
      let (inbits, bit_cnt) = self.refill();
      self.bits = (self.bits << bit_cnt) | inbits as u64;
      self.nbits += bit_cnt;
    }
    self.bits.wrapping_shr(self.nbits.wrapping_sub(num)) as u32
  }

  #[inline(always)]
  fn consume_bits(&mut self, num: u32) {
    self.nbits = self.nbits.wrapping_sub(num);
    self.bits &= 1_u64.wrapping_shl(self.nbits).wrapping_sub(1);
  }
}

impl<'a> BitPump for BitPumpMSB32<'a> {
  fn overrun(&self) -> bool {
    self.overrun
  }

  #[inline(always)]
  fn peek_bits(&mut self, num: u32) -> u32 {
    if num > self.nbits {
      let (inbits, bit_cnt) = self.refill();
      self.bits = (self.bits << 32) | inbits as u64;
      self.nbits += bit_cnt;
      self.pos += bit_cnt as usize / 8;
    }
    self.bits.wrapping_shr(self.nbits.wrapping_sub(num)) as u32
  }

  #[inline(always)]
  fn consume_bits(&mut self, num: u32) {
    self.nbits = self.nbits.wrapping_sub(num);
    self.bits &= 1_u64.wrapping_shl(self.nbits).wrapping_sub(1);
  }
}

impl<'a> BitPump for BitPumpJPEG<'a> {
  #[inline(always)]
  fn peek_bits(&mut self, num: u32) -> u32 {
    if num > self.nbits && !self.finished {
      if (self.buffer.len() >= 4)
        && self.pos < self.buffer.len() - 4
        && self.buffer[self.pos + 0] != 0xff
        && self.buffer[self.pos + 1] != 0xff
        && self.buffer[self.pos + 2] != 0xff
        && self.buffer[self.pos + 3] != 0xff
      {
        let inbits: u64 = BEu32(self.buffer, self.pos) as u64;
        self.bits = (self.bits << 32) | inbits;
        self.pos += 4;
        self.nbits += 32;
      } else {
        // Read 32 bits the hard way
        let mut read_bytes = 0;
        while read_bytes < 4 && !self.finished {
          let byte = {
            if self.pos >= self.buffer.len() {
              self.finished = true;
              0
            } else {
              let nextbyte = self.buffer[self.pos];
              if nextbyte != 0xff {
                nextbyte
              } else if self.buffer.get(self.pos + 1) == Some(&0x00) {
                // Rawtonic patch (RWH1): `get`, a final 0xFF panicked.
                self.pos += 1; // Skip the extra byte used to mark 255
                nextbyte
              } else {
                self.finished = true;
                0
              }
            }
          };
          self.bits = (self.bits << 8) | (byte as u64);
          self.pos += 1;
          self.nbits += 8;
          read_bytes += 1;
        }
      }
    }
    if num > self.nbits && self.finished {
      // Stuff with zeroes to not fail to read
      self.bits <<= 32;
      self.nbits += 32;
    }

    self.bits.wrapping_shr(self.nbits.wrapping_sub(num)) as u32
  }

  #[inline(always)]
  fn consume_bits(&mut self, num: u32) {
    // Rawtonic patch (RWH1): no debug assert; damaged data consumes more
    // than was peeked, which wraps as in release builds.
    self.nbits = self.nbits.wrapping_sub(num);
    self.bits &= 1_u64.wrapping_shl(self.nbits).wrapping_sub(1);
  }
}

#[derive(Debug, Copy, Clone)]
pub struct ByteStream<'a> {
  buffer: &'a [u8],
  pos: usize,
  endian: Endian,
}

// Rawtonic patch (RWH1, notes: reference/dnglab-2026-11-16-review.md; dnglab
// 3bc2e446 returns errors from ByteStream): reads past the end return zeros
// (and `get_bytes` a shorter vector) instead of panicking, and
// `remaining_bytes` is 0 there. Parsers check `remaining_bytes`/`is_exhausted`
// where a short read matters; in-range reads are unchanged.
impl<'a> ByteStream<'a> {
  pub fn new(src: &'a [u8], endian: Endian) -> Self {
    Self { buffer: src, pos: 0, endian }
  }

  #[inline(always)]
  pub fn remaining_bytes(&self) -> usize {
    self.buffer.len().saturating_sub(self.pos)
  }

  /// The position is at or past the end of the data.
  #[inline(always)]
  pub fn is_exhausted(&self) -> bool {
    self.pos >= self.buffer.len()
  }

  #[inline(always)]
  pub fn get_pos(&self) -> usize {
    self.pos
  }

  #[inline(always)]
  fn bytes(&self, n: usize) -> Option<&'a [u8]> {
    self.buffer.get(self.pos..self.pos.checked_add(n)?)
  }

  #[inline(always)]
  pub fn peek_u8(&self) -> u8 {
    self.buffer.get(self.pos).copied().unwrap_or(0)
  }
  #[inline(always)]
  pub fn get_u8(&mut self) -> u8 {
    let val = self.peek_u8();
    self.pos = self.pos.saturating_add(1);
    val
  }

  #[inline(always)]
  pub fn peek_i8(&self) -> i8 {
    self.peek_u8() as i8
  }
  #[inline(always)]
  pub fn get_i8(&mut self) -> i8 {
    let val = self.peek_i8();
    self.pos = self.pos.saturating_add(1);
    val
  }

  #[inline(always)]
  pub fn peek_u16(&self) -> u16 {
    self.bytes(2).map(|b| self.endian.read_u16(b, 0)).unwrap_or(0)
  }
  #[inline(always)]
  pub fn get_u16(&mut self) -> u16 {
    let val = self.peek_u16();
    self.pos = self.pos.saturating_add(2);
    val
  }

  #[inline(always)]
  pub fn peek_i16(&self) -> i16 {
    self.bytes(2).map(|b| self.endian.read_i16(b, 0)).unwrap_or(0)
  }
  #[inline(always)]
  pub fn get_i16(&mut self) -> i16 {
    let val = self.peek_i16();
    self.pos = self.pos.saturating_add(2);
    val
  }

  #[inline(always)]
  pub fn peek_u32(&self) -> u32 {
    self.bytes(4).map(|b| self.endian.read_u32(b, 0)).unwrap_or(0)
  }

  #[inline(always)]
  pub fn get_u32(&mut self) -> u32 {
    let val = self.peek_u32();
    self.pos = self.pos.saturating_add(4);
    val
  }

  #[inline(always)]
  pub fn get_bytes(&mut self, n: usize) -> Vec<u8> {
    let start = self.pos.min(self.buffer.len());
    let end = self.pos.saturating_add(n).min(self.buffer.len());
    let val = self.buffer[start..end].to_vec();
    self.pos = self.pos.saturating_add(n);
    val
  }

  #[inline(always)]
  pub fn consume_bytes(&mut self, num: usize) {
    self.pos = self.pos.saturating_add(num)
  }

  #[inline(always)]
  pub fn skip_to_marker(&mut self) -> Result<usize, String> {
    let mut skip_count = 0;
    loop {
      if self.pos + 1 >= self.buffer.len() {
        return Err("No marker found inside rest of buffer".to_string());
      }
      if self.buffer[self.pos] == 0xFF && self.buffer[self.pos + 1] != 0 && self.buffer[self.pos + 1] != 0xFF {
        break;
      }
      self.pos += 1;
      skip_count += 1;
    }
    self.pos += 1; // Make the next byte the marker
    Ok(skip_count + 1)
  }
}

/// This pump is for bitstreams where values are stored in LSB bit order.
/// During refill, bits are converted from LSB to MSB so peaking
/// is done by reading in MSB mode.
///
/// Input bitstream is:     1011 0101 0010 1110...
/// Output for peek(10) is: 1010 1101 01
#[derive(Debug, Copy, Clone)]
pub struct BitPumpReverseBitsMSB<'a> {
  buffer: &'a [u8],
  pos: usize,
  bits: u64,
  nbits: u32,
  overrun: bool,
}

impl<'a> BitPumpReverseBitsMSB<'a> {
  pub fn new(src: &'a [u8]) -> Self {
    Self {
      buffer: src,
      pos: 0,
      bits: 0,
      nbits: 0,
      overrun: false,
    }
  }
}

impl<'a> BitPump for BitPumpReverseBitsMSB<'a> {
  fn overrun(&self) -> bool {
    self.overrun
  }

  #[inline(always)]
  fn peek_bits(&mut self, num: u32) -> u32 {
    debug_assert!(num <= 32);
    if num > self.nbits {
      // Rawtonic patch (RWH1): zeros past the end instead of a panic.
      let mut raw: [u8; 4] = match self.buffer.get(self.pos..self.pos + 4) {
        Some(word) => BEu32(word, 0).to_ne_bytes(),
        None => {
          self.overrun = true;
          [0; 4]
        }
      };
      raw[0] = raw[0].reverse_bits();
      raw[1] = raw[1].reverse_bits();
      raw[2] = raw[2].reverse_bits();
      raw[3] = raw[3].reverse_bits();
      let inbits: u64 = u32::from_ne_bytes(raw) as u64;
      self.bits = (self.bits << 32) | inbits;
      self.pos += 4;
      self.nbits += 32;
    }
    self.bits.wrapping_shr(self.nbits.wrapping_sub(num)) as u32
  }

  #[inline(always)]
  fn consume_bits(&mut self, num: u32) {
    self.nbits = self.nbits.wrapping_sub(num);
    self.bits &= 1_u64.wrapping_shl(self.nbits).wrapping_sub(1);
  }
}

#[cfg(test)]
mod rawtonic_tests {
  use super::*;

  /// A stream whose length is not a multiple of 4: the last two bytes are
  /// read as a word padded with zeros, then zero bits follow.
  #[test]
  fn msb32_partial_last_word_and_end() {
    let data = [0x78, 0x56, 0x34, 0x12, 0xcd, 0xab];
    let mut pump = BitPumpMSB32::new(&data);
    assert_eq!(pump.get_bits(32), 0x1234_5678);
    assert_eq!(pump.get_bits(16), 0x0000);
    assert_eq!(pump.get_bits(16), 0xabcd);
    assert_eq!(pump.get_bits(16), 0);
  }
}
