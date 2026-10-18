//! Frames on the helper's pipes: a little-endian `u32` length (of kind and
//! payload), one kind byte, the payload. JSON frames carry requests and
//! messages; live-view frames carry one camera JPEG each.
use std::io::{self, Read, Write};

pub const JSON: u8 = 0;
pub const LIVE_VIEW: u8 = 1;

/// Larger frames are refused: a corrupt length must not allocate gigabytes.
/// Live-view JPEGs are well under a megabyte; JSON is small.
pub const MAX_FRAME: usize = 32 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub payload: Vec<u8>,
}

pub fn write(out: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len() + 1)
        .ok()
        .filter(|&len| len as usize <= MAX_FRAME)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(&[kind])?;
    out.write_all(payload)?;
    out.flush()
}

pub fn write_json(out: &mut impl Write, value: &impl serde::Serialize) -> io::Result<()> {
    let payload = serde_json::to_vec(value).map_err(io::Error::other)?;
    write(out, JSON, &payload)
}

/// The next frame, or `None` at a clean end of the stream.
pub fn read(input: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut len = [0u8; 4];
    match input.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bad frame length {len}"),
        ));
    }
    let mut kind = [0u8; 1];
    input.read_exact(&mut kind)?;
    let mut payload = vec![0u8; len - 1];
    input.read_exact(&mut payload)?;
    Ok(Some(Frame {
        kind: kind[0],
        payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_end_cleanly() {
        let mut buffer = Vec::new();
        write(&mut buffer, JSON, b"{}").unwrap();
        write(&mut buffer, LIVE_VIEW, &[0xff, 0xd8, 0xff]).unwrap();
        let mut input = buffer.as_slice();
        assert_eq!(
            read(&mut input).unwrap(),
            Some(Frame {
                kind: JSON,
                payload: b"{}".to_vec()
            })
        );
        assert_eq!(
            read(&mut input).unwrap().unwrap().payload,
            [0xff, 0xd8, 0xff]
        );
        assert_eq!(read(&mut input).unwrap(), None);
    }

    #[test]
    fn a_corrupt_length_is_refused() {
        let mut input: &[u8] = &[0xff, 0xff, 0xff, 0xff, 0];
        assert!(read(&mut input).is_err());
        let mut input: &[u8] = &[0, 0, 0, 0];
        assert!(read(&mut input).is_err());
    }
}
