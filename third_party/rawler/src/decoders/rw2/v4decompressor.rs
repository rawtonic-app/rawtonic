use crate::{decoders::*, pumps::BitPump};

pub(crate) fn decode_panasonic_v4(buf: &[u8], width: usize, height: usize, split: bool, dummy: bool) -> PixU16 {
  decode_threaded_multiline(
    width,
    height,
    5,
    dummy,
    &(|out: &mut [u16], row| {
      let skip = ((width * row * 9) + (width / 14 * 2 * row)) / 8;
      let blocks = skip / 0x4000;
      let src = &buf[blocks * 0x4000..];
      let mut pump = BitPumpPanasonic::new(src, split);
      for _ in 0..(skip % 0x4000) {
        pump.get_bits(8);
      }

      let mut sh: i32 = 0;
      for out in out.chunks_exact_mut(14) {
        let mut pred: [i32; 2] = [0, 0];
        let mut nonz: [i32; 2] = [0, 0];

        for i in 0..14 {
          if (i % 3) == 2 {
            sh = 4 >> (3 - pump.get_bits(2));
          }
          if nonz[i & 1] != 0 {
            let j = pump.get_bits(8) as i32;
            if j != 0 {
              pred[i & 1] -= 0x80 << sh;
              if pred[i & 1] < 0 || sh == 4 {
                pred[i & 1] &= !(-1 << sh);
              }
              pred[i & 1] += j << sh;
            }
          } else {
            nonz[i & 1] = pump.get_bits(8) as i32;
            if nonz[i & 1] != 0 || i > 11 {
              pred[i & 1] = (nonz[i & 1] << 4) | (pump.get_bits(4) as i32);
            }
          }
          out[i] = pred[i & 1] as u16;
        }
      }
      Ok(())
    }),
  )
  .expect("Failed to decode") // Decoder should never fail
}

pub struct BitPumpPanasonic<'a> {
  buffer: &'a [u8],
  pos: usize,
  nbits: u32,
  split: bool,
}

impl<'a> BitPumpPanasonic<'a> {
  pub fn new(src: &'a [u8], split: bool) -> BitPumpPanasonic<'a> {
    BitPumpPanasonic {
      buffer: src,
      pos: 0,
      nbits: 0,
      split,
    }
  }
}

impl BitPumpPanasonic<'_> {
  // Rawtonic patch: byte `i` of the current block as LibRaw's block buffer holds it.
  fn block_byte(&self, i: usize) -> u8 {
    if i >= 0x4000 {
      return 0;
    }
    let file_offset = if self.split { (i + 0x4000 - 0x2008) % 0x4000 } else { i };
    self.buffer[self.pos - 0x4000 + file_offset]
  }
}

impl<'a> BitPump for BitPumpPanasonic<'a> {
  fn peek_bits(&mut self, num: u32) -> u32 {
    if num > self.nbits {
      self.nbits += 0x4000 * 8;
      self.pos += 0x4000;
    }
    // Rawtonic patch: both bytes are addressed in the rotated block buffer
    // (as LibRaw's `pana_data`), not as two neighbouring file bytes. With the
    // split layout, block byte 0x2007 is the file's last byte of the block and
    // block byte 0x2008 its first; reading the file byte after 0x3fff took the
    // next block's first byte and corrupted the 14-pixel group crossing there.
    // Past the block end (0x4000) the buffer holds zeros.
    let byte = (((self.nbits - num) >> 3) ^ 0x3ff0) as usize;
    let bits = self.block_byte(byte) as u32 | (self.block_byte(byte + 1) as u32) << 8;
    (bits >> ((self.nbits - num) & 7)) & (0x0ffffffffu32 >> (32 - num))
  }

  fn consume_bits(&mut self, num: u32) {
    self.nbits -= num;
  }
}

// Rawtonic patch: the split layout's wrap inside a block.
#[cfg(test)]
mod rawtonic_tests {
  use super::*;

  /// LibRaw's `pana_data` bit reader: one rotated 0x4000-byte block buffer
  /// followed by two zero bytes, both bytes read in buffer space.
  struct Reference<'a> {
    file: &'a [u8],
    load_flags: usize,
    block: usize,
    buf: [u8; 0x4002],
    vpos: u32,
  }

  impl Reference<'_> {
    fn bits(&mut self, nb: u32) -> u32 {
      if self.vpos == 0 {
        let src = &self.file[self.block * 0x4000..(self.block + 1) * 0x4000];
        self.buf[self.load_flags..0x4000].copy_from_slice(&src[..0x4000 - self.load_flags]);
        self.buf[..self.load_flags].copy_from_slice(&src[0x4000 - self.load_flags..]);
        self.block += 1;
      }
      self.vpos = self.vpos.wrapping_sub(nb) & 0x1ffff;
      let byte = ((self.vpos >> 3) ^ 0x3ff0) as usize;
      ((self.buf[byte] as u32 | (self.buf[byte + 1] as u32) << 8) >> (self.vpos & 7)) & !(!0u32 << nb)
    }
  }

  fn pseudo_random_blocks(blocks: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_u32;
    (0..blocks * 0x4000)
      .map(|_| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as u8
      })
      .collect()
  }

  #[test]
  fn split_read_across_the_wrap_takes_the_same_blocks_first_byte() {
    let mut file = vec![0u8; 2 * 0x4000];
    // Block buffer bytes 0x2007 and 0x2008 are file bytes 0x3fff and 0x0000 of block 0.
    file[0x3fff] = 0xa0;
    file[0x0000] = 0x0b;
    file[0x4000] = 0xff; // next block's first byte must not leak in
    let mut pump = BitPumpPanasonic::new(&file, true);
    let v = (0x2007 ^ 0x3ff0) * 8 + 4;
    let mut left = 0x20000 - (v + 8);
    while left > 0 {
      let n = left.min(16);
      pump.get_bits(n);
      left -= n;
    }
    assert_eq!(pump.get_bits(8), 0xba);
  }

  #[test]
  fn bit_pump_matches_libraw_on_split_and_plain_blocks() {
    let file = pseudo_random_blocks(3);
    for (split, load_flags) in [(true, 0x2008), (false, 0)] {
      let mut pump = BitPumpPanasonic::new(&file, split);
      let mut reference = Reference {
        file: &file,
        load_flags,
        block: 0,
        buf: [0; 0x4002],
        vpos: 0,
      };
      // The v4 decoder's read widths, 128 bits per group as a 14-pixel group, so no read crosses a block end.
      let widths = [2u32, 8, 4, 8, 8, 4, 2, 8, 8, 8, 4, 8, 2, 8, 8, 4, 8, 8, 2, 8, 8];
      let mut consumed = 0usize;
      let mut i = 0;
      while consumed < 3 * 0x20000 {
        let nb = widths[i % widths.len()];
        assert_eq!(pump.get_bits(nb), reference.bits(nb), "split {split}, bit {consumed}");
        consumed += nb as usize;
        i += 1;
      }
    }
  }
}
