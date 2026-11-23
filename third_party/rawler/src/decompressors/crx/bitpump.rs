// SPDX-License-Identifier: LGPL-2.1
// Copyright 2021 Daniel Vogelbacher <daniel@chaospixel.com>

//! Rawtonic patch (RWL1): the big-endian bit pump of upstream rawler's
//! `pumps.rs` (dnglab `163c08f3`), which the CRX decoder reads its bit
//! streams through. Upstream's pumps return `Result` since rawler 0.8.0; the
//! rest of this vendored copy still uses the older infallible pumps, so the
//! CRX decoder keeps its own copy instead of the shared module changing.

use std::fmt;

/// Maximum count of bits the pump returns at once
const MAX_PEEK_BITS: u32 = u32::BITS;

/// Error of a bit pump read
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitPumpError {
  /// The buffer has fewer than the requested bits left
  Eof,
  /// More bits requested at once than the pump can return
  ExcessiveBits { num: u32, max: u32 },
}

impl fmt::Display for BitPumpError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Eof => write!(f, "unexpected end of bit stream"),
      Self::ExcessiveBits { num, max } => write!(f, "can't read {num} bits at once from bit stream, max is {max}"),
    }
  }
}

impl std::error::Error for BitPumpError {}

// Errors are built in cold functions, so the compiler keeps the error
// branches out of the hot decoder loops.
#[cold]
fn excessive_bits(num: u32, max: u32) -> BitPumpError {
  BitPumpError::ExcessiveBits { num, max }
}

#[cold]
fn eof() -> BitPumpError {
  BitPumpError::Eof
}

#[derive(Debug, Clone)]
pub struct BitPumpMSB<'a> {
  /// Bytes not yet moved into `bits`
  buffer: &'a [u8],
  /// Bit register, the next bit of the stream is the most significant bit
  bits: u64,
  /// Count of valid bits in `bits`, always below 64
  nbits: u32,
}

impl<'a> BitPumpMSB<'a> {
  pub fn new(src: &'a [u8]) -> Self {
    Self {
      buffer: src,
      bits: 0,
      nbits: 0,
    }
  }

  /// Refill internal bit buffer - Big-Endian
  ///
  /// Afterwards the register holds at least 56 bits, or all
  /// remaining bits at the end of the buffer.
  #[inline(always)]
  fn refill(&mut self) {
    if let Some((word, _)) = self.buffer.split_first_chunk::<8>() {
      // Fast refill: load 8 bytes behind the valid bits, but only
      // account for the whole bytes which fit. The bits below `nbits`
      // are the same bits the next refill loads, so OR-ing is safe.
      self.bits |= u64::from_be_bytes(*word) >> self.nbits;
      let bytes = (63 - self.nbits) >> 3;
      self.buffer = &self.buffer[bytes as usize..];
      self.nbits += bytes * 8;
    } else {
      self.refill_tail();
    }
  }

  /// Slow refill, there are less than 8 bytes left in buffer.
  /// We need to collect them manually.
  #[cold]
  fn refill_tail(&mut self) {
    while let Some((byte, rest)) = self.buffer.split_first()
      && self.nbits <= 55
    {
      self.bits |= (*byte as u64) << (56 - self.nbits);
      self.buffer = rest;
      self.nbits += 8;
    }
  }

  /// Count and consume a run of zero bits which
  /// reaches the end of the bit register.
  #[cold]
  fn consume_zerobits_refill(&mut self) -> Result<u32, BitPumpError> {
    let mut count = 0;
    loop {
      // All valid bits are zero
      count += self.nbits;
      self.consume_bits(self.nbits);
      self.refill();
      if self.nbits == 0 {
        return Err(eof());
      }
      let zeros = self.bits.leading_zeros();
      if zeros < self.nbits {
        self.consume_bits(zeros);
        return Ok(count + zeros);
      }
    }
  }

  /// Return the next `num` bits without consuming them.
  ///
  /// Fails if the buffer has fewer than `num` bits left or if `num`
  /// is larger than the pump can return at once.
  #[inline(always)]
  pub fn peek_bits(&mut self, num: u32) -> Result<u32, BitPumpError> {
    if num > MAX_PEEK_BITS {
      return Err(excessive_bits(num, MAX_PEEK_BITS));
    }
    if num > self.nbits {
      self.refill();
      if num > self.nbits {
        return Err(eof());
      }
    }
    // Two shifts because num may be zero
    Ok(((self.bits >> 1) >> (63 - num)) as u32)
  }

  /// Consume `num` bits. Only valid after a successful
  /// [`Self::peek_bits`] of at least `num` bits.
  #[inline(always)]
  pub fn consume_bits(&mut self, num: u32) {
    debug_assert!(num <= self.nbits);
    self.bits <<= num;
    self.nbits -= num;
  }

  #[inline(always)]
  pub fn get_bits(&mut self, num: u32) -> Result<u32, BitPumpError> {
    if num == 0 {
      return Ok(0);
    }
    let val = self.peek_bits(num)?;
    self.consume_bits(num);
    Ok(val)
  }

  /// Count and consume the zero bits before the next 1-bit, which is not
  /// consumed. The run is counted in the bit register, up to 63 bits at once.
  ///
  /// Fails if the buffer ends before a 1-bit is found.
  #[inline(always)]
  pub fn consume_zerobits(&mut self) -> Result<u32, BitPumpError> {
    let zeros = self.bits.leading_zeros();
    if zeros < self.nbits {
      self.consume_bits(zeros);
      Ok(zeros)
    } else {
      self.consume_zerobits_refill()
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reads_bits_and_zero_runs_and_stops_at_the_end() {
    // 0b0001_0110, 0b1000_0000, then 70 zero bits and a 1-bit
    let mut data = vec![0b0001_0110, 0b1000_0000];
    data.extend_from_slice(&[0; 8]);
    data.push(0b0100_0000);
    let mut pump = BitPumpMSB::new(&data);
    assert_eq!(pump.consume_zerobits(), Ok(3));
    assert_eq!(pump.get_bits(1), Ok(1));
    assert_eq!(pump.get_bits(4), Ok(0b0110));
    assert_eq!(pump.get_bits(1), Ok(1));
    assert_eq!(pump.consume_zerobits(), Ok(7 + 64 + 1));
    assert_eq!(pump.get_bits(1), Ok(1));
    assert_eq!(pump.get_bits(6), Ok(0));
    assert_eq!(pump.get_bits(1), Err(BitPumpError::Eof));
    assert_eq!(pump.consume_zerobits(), Err(BitPumpError::Eof));
    assert_eq!(pump.peek_bits(33), Err(BitPumpError::ExcessiveBits { num: 33, max: 32 }));
  }
}
