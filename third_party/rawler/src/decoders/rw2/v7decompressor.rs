/// Ported from Libraw
use crate::{
  decoders::*,
  pumps::{BitPump, BitPumpLSB},
};

/// This works for 12 and 14 bit depth images
pub(crate) fn decode_panasonic_v7(buf: &[u8], width: usize, height: usize, bps: u32, dummy: bool) -> Result<PixU16> {
  const V7_BYTES_PER_BLOCK: usize = 16;

  let pixels_per_block = match bps {
    14 => 9,
    12 => 10,
    _ => return Err(RawlerError::DecoderFailed(format!("RW2 v7: unsupported bit depth {}", bps))),
  };
  let blocks_per_row = width / pixels_per_block;

  // Rawtonic patch (RWH1; dnglab 5ab840c0, b4630649): unsupported
  // parameters and data shorter than the image are errors.
  crate::packed::check_image("RW2 v7", width, height)?;
  if width % pixels_per_block != 0 {
    return Err(RawlerError::DecoderFailed(format!(
      "RW2 v7: width {} is no multiple of {}",
      width, pixels_per_block
    )));
  }

  let bytes_per_row = V7_BYTES_PER_BLOCK * blocks_per_row;

  if !dummy && buf.len() < height * bytes_per_row {
    return Err(RawlerError::DecoderFailed(format!("RW2 v7: image data too short")));
  }
  Ok(decode_threaded(
    width,
    height,
    dummy,
    &(|out, row| {
      let src = &buf[row * bytes_per_row..row * bytes_per_row + bytes_per_row];
      for (block_id, block) in src.chunks_exact(V7_BYTES_PER_BLOCK).enumerate() {
        let start = block_id * pixels_per_block;
        let out = &mut out[start..start + pixels_per_block];
        let mut pump = BitPumpLSB::new(block);
        out.iter_mut().for_each(|pixel| *pixel = pump.get_bits(14) as u16);
      }
    }),
  ))
}
