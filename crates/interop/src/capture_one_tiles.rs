//! Capture One's compressed mask tiles (the `Blk` of a `.comask`; notes:
//! benchmark-results/c1l2-2026-11-17/FORMAT.md has the observations this
//! decoder rests on, made by letting Capture One re-encode tiles of our own
//! content).
//!
//! A tile holds 64 × 64 mask values (0–255). Its first byte is the kind:
//! `00` the 4096 values row by row, `02` a coded tile:
//!
//! - byte 1: the value of the top-left pixel;
//! - a residual per remaining pixel: the pixel minus the one above it (row 0:
//!   minus the one to its left), modulo 256, listed in 4 × 4 blocks row by
//!   row, each block's 16 pixels along a Hilbert curve ((0,0), (0,1), (1,1),
//!   (1,0), … as (row, column));
//! - a run-length list of groups of residuals: `00`–`3f` n + 1 zeros;
//!   `40`–`8f` a group of class (high nibble − 3) and n + 1 residuals (low
//!   nibble n); `e8`–`ef` a group of class 8 and n + 1 residuals (low three
//!   bits); `f0`–`f5` and `ff` a group of class 0–5 (8 for `ff`) whose
//!   length − 1 follows as one byte, or two (`0x80 | high`, low);
//! - the values of the groups. A group of class k (1–5) stores an offset P
//!   (minus its smallest residual, at least 0) and each residual + P, all in
//!   k bits; class 8 stores the residuals' bytes. The k bits are split by
//!   width — 1 (k = 1), 2 (k = 2), 1 + 2 (3), 4 (4), 1 + 4 (5), lowest bits
//!   in the narrower part — into a 1-bit, a 2-bit and a 4-bit field stream,
//!   each listing its groups by class and then by position, then the class 8
//!   bytes. The 1-bit stream starts with the four bits of the number 2; each
//!   stream starts on a byte. A stream fills 128-bit words while 32 × 4/w
//!   fields remain, then 32-bit words, then single bytes: in a word of n
//!   bytes field i goes to byte i mod n at bit (i / n)·w; in single bytes
//!   fields follow each other from the lowest bit.
//!
//! Anything else (another kind, unknown group codes, values that do not fill
//! the tile exactly) is refused rather than guessed.

/// Side of a tile.
pub const TILE: usize = 64;
const N: usize = TILE * TILE;

/// Hilbert order of a 4 × 4 block as (row, column).
const HILBERT: [(usize, usize); 16] = [
    (0, 0),
    (0, 1),
    (1, 1),
    (1, 0),
    (2, 0),
    (3, 0),
    (3, 1),
    (2, 1),
    (2, 2),
    (3, 2),
    (3, 3),
    (2, 3),
    (1, 3),
    (1, 2),
    (0, 2),
    (0, 3),
];

/// The tile's pixel (row-major index) at position `p` of the coded order.
fn pixel_at(p: usize) -> usize {
    let (block, j) = (p / 16, p % 16);
    let (by, bx) = (block / 16, block % 16);
    let (dy, dx) = HILBERT[j];
    (by * 4 + dy) * TILE + bx * 4 + dx
}

/// Which bit widths a class's values are split into, narrowest first.
fn widths(class: u8) -> &'static [u8] {
    match class {
        1 => &[1],
        2 => &[2],
        3 => &[1, 2],
        4 => &[4],
        5 => &[1, 4],
        _ => &[],
    }
}

/// The classes a field stream of width `w` holds, in its order.
fn stream_classes(w: u8) -> &'static [u8] {
    match w {
        1 => &[1, 3, 5],
        2 => &[2, 3],
        _ => &[4, 5],
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Group {
    class: u8,
    /// First position (1–4095).
    start: usize,
    len: usize,
}

fn read_len(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let x = *b.get(i)?;
    if x & 0x80 != 0 {
        Some((
            (((x & 0x7f) as usize) << 8) | *b.get(i + 1)? as usize,
            i + 2,
        ))
    } else {
        Some((x as usize, i + 1))
    }
}

/// The groups of a coded tile and where its values start.
fn groups(b: &[u8]) -> Option<(Vec<Group>, usize)> {
    let (mut i, mut p) = (2, 1);
    let mut out = Vec::new();
    while p < N {
        let x = *b.get(i)?;
        let (class, len) = match x {
            0x00..=0x3f => {
                i += 1;
                (0, x as usize + 1)
            }
            0x40..=0x8f => {
                i += 1;
                ((x >> 4) - 3, (x & 15) as usize + 1)
            }
            0xe8..=0xef => {
                i += 1;
                (8, (x & 7) as usize + 1)
            }
            0xf0..=0xf5 | 0xff => {
                let (n, next) = read_len(b, i + 1)?;
                i = next;
                (if x == 0xff { 8 } else { x & 15 }, n + 1)
            }
            _ => return None,
        };
        out.push(Group {
            class,
            start: p,
            len,
        });
        p += len;
    }
    (p == N).then_some((out, i))
}

/// Reads `count` fields of `w` bits starting at byte `at`; returns them and
/// the byte after the stream.
fn read_stream(d: &[u8], at: usize, w: usize, count: usize) -> Option<(Vec<u8>, usize)> {
    let mask = ((1u16 << w) - 1) as u8;
    let mut out = Vec::with_capacity(count);
    let (mut byte, mut left) = (at, count);
    for word in [16usize, 4] {
        let per = word * 8 / w;
        while left >= per {
            let bytes = d.get(byte..byte + word)?;
            for s in 0..8 / w {
                for &v in bytes {
                    out.push((v >> (s * w)) & mask);
                }
            }
            byte += word;
            left -= per;
        }
    }
    for k in 0..left {
        let bit = k * w;
        let v = *d.get(byte + bit / 8)?;
        out.push((v >> (bit % 8)) & mask);
    }
    Some((out, byte + (left * w).div_ceil(8)))
}

#[cfg(any(test, feature = "test-support"))]
/// Writes `fields` of `w` bits as `read_stream` reads them.
fn write_stream(out: &mut Vec<u8>, fields: &[u8], w: usize) {
    let mut rest = fields;
    for word in [16usize, 4] {
        let per = word * 8 / w;
        while rest.len() >= per {
            let mut bytes = vec![0u8; word];
            for (i, &f) in rest[..per].iter().enumerate() {
                bytes[i % word] |= f << ((i / word) * w);
            }
            out.extend(bytes);
            rest = &rest[per..];
        }
    }
    let mut bytes = vec![0u8; (rest.len() * w).div_ceil(8)];
    for (k, &f) in rest.iter().enumerate() {
        bytes[k * w / 8] |= f << ((k * w) % 8);
    }
    out.extend(bytes);
}

/// The 64 × 64 values (row by row) of a tile, or None when it is not one
/// this decoder knows.
pub fn decode(b: &[u8]) -> Option<Vec<u8>> {
    match b.first()? {
        0 => (b.len() == N + 1).then(|| b[1..].to_vec()),
        2 => decode_coded(b),
        _ => None,
    }
}

fn decode_coded(b: &[u8]) -> Option<Vec<u8>> {
    let first = *b.get(1)?;
    let (groups, values) = groups(b)?;
    let tail = b.get(values..)?;
    let fields = |w: u8| -> usize {
        groups
            .iter()
            .filter(|g| stream_classes(w).contains(&g.class))
            .map(|g| g.len + 1)
            .sum()
    };
    let (ones, at) = read_stream(tail, 0, 1, 4 + fields(1))?;
    if ones[..4] != [0, 1, 0, 0] {
        return None;
    }
    let (twos, at) = read_stream(tail, at, 2, fields(2))?;
    let (fours, at) = read_stream(tail, at, 4, fields(4))?;
    let bytes: usize = groups.iter().filter(|g| g.class == 8).map(|g| g.len).sum();
    let raw = tail.get(at..at + bytes)?;
    if at + bytes != tail.len() {
        return None;
    }
    // Offsets and values, assembled from the streams' parts.
    let mut offset = vec![0u16; N];
    let mut value = vec![0u16; N];
    for (w, stream) in [(1u8, &ones[4..]), (2, &twos), (4, &fours)] {
        let mut it = stream.iter();
        for &class in stream_classes(w) {
            let shift: u32 = widths(class)
                .iter()
                .take_while(|&&x| x != w)
                .map(|&x| x as u32)
                .sum();
            for g in groups.iter().filter(|g| g.class == class) {
                offset[g.start] |= (*it.next()? as u16) << shift;
                for v in &mut value[g.start..g.start + g.len] {
                    *v |= (*it.next()? as u16) << shift;
                }
            }
        }
    }
    let mut residual = [0u8; N];
    let mut raw = raw.iter();
    for g in &groups {
        for p in g.start..g.start + g.len {
            residual[pixel_at(p)] = match g.class {
                0 => 0,
                8 => *raw.next()?,
                _ => value[p].wrapping_sub(offset[g.start]) as u8,
            };
        }
    }
    let mut out = vec![0u8; N];
    out[0] = first;
    for i in 1..N {
        let prediction = if i >= TILE { out[i - TILE] } else { out[i - 1] };
        out[i] = prediction.wrapping_add(residual[i]);
    }
    Some(out)
}

#[cfg(any(test, feature = "test-support"))]
/// The smallest class (1–5, else 8) that holds `values` with their offset.
fn class_of(values: &[i16]) -> u8 {
    let offset = -values.iter().copied().min().unwrap_or(0).min(0);
    let top = values
        .iter()
        .map(|v| v + offset)
        .max()
        .unwrap_or(0)
        .max(offset);
    (1..=5).find(|k| top < 1 << k).unwrap_or(8)
}

#[cfg(any(test, feature = "test-support"))]
/// A coded tile of `pixels` (row by row), as `decode` reads it; the
/// row-by-row kind when coding would not be smaller. Groups hold at most
/// `max_group` residuals (more than 16 uses the long group codes).
pub fn encode(pixels: &[u8], max_group: usize) -> Vec<u8> {
    assert_eq!(pixels.len(), N);
    let mut residual = [0i16; N];
    for (p, r) in residual.iter_mut().enumerate().skip(1) {
        let i = pixel_at(p);
        let prediction = if i >= TILE {
            pixels[i - TILE]
        } else {
            pixels[i - 1]
        };
        *r = pixels[i].wrapping_sub(prediction) as i8 as i16;
    }
    // Groups: zero runs, and runs of non-zero residuals cut into pieces.
    let mut groups = Vec::new();
    let mut p = 1;
    while p < N {
        let zero = residual[p] == 0;
        let mut end = p;
        while end < N && (residual[end] == 0) == zero && (zero || end - p < max_group) {
            end += 1;
        }
        let class = if zero { 0 } else { class_of(&residual[p..end]) };
        groups.push(Group {
            class,
            start: p,
            len: end - p,
        });
        p = end;
    }
    let mut out = vec![2, pixels[0]];
    for g in &groups {
        let n = g.len - 1;
        match (g.class, g.len) {
            (0, 1..=64) => out.push(n as u8),
            (1..=5, 1..=16) => out.push(((g.class + 3) << 4) | n as u8),
            (8, 1..=8) => out.push(0xe8 | n as u8),
            (class, _) => {
                out.push(if class == 8 { 0xff } else { 0xf0 | class });
                if n < 0x80 {
                    out.push(n as u8);
                } else {
                    out.extend([0x80 | (n >> 8) as u8, n as u8]);
                }
            }
        }
    }
    let offset = |g: &Group| {
        -residual[g.start..g.start + g.len]
            .iter()
            .min()
            .unwrap()
            .min(&0)
    };
    let mut streams: [Vec<u8>; 3] = [vec![0, 1, 0, 0], Vec::new(), Vec::new()];
    for (k, w) in [1u8, 2, 4].into_iter().enumerate() {
        for &class in stream_classes(w) {
            let shift: u32 = widths(class)
                .iter()
                .take_while(|&&x| x != w)
                .map(|&x| x as u32)
                .sum();
            let part = |v: i16| (((v as u16) >> shift) & ((1 << w) - 1)) as u8;
            for g in groups.iter().filter(|g| g.class == class) {
                let o = offset(g);
                streams[k].push(part(o));
                for &r in &residual[g.start..g.start + g.len] {
                    streams[k].push(part(r + o));
                }
            }
        }
    }
    write_stream(&mut out, &streams[0], 1);
    write_stream(&mut out, &streams[1], 2);
    write_stream(&mut out, &streams[2], 4);
    for g in groups.iter().filter(|g| g.class == 8) {
        out.extend(residual[g.start..g.start + g.len].iter().map(|&r| r as u8));
    }
    if out.len() > N {
        let mut raw = vec![0];
        raw.extend(pixels);
        return raw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|h| u8::from_str_radix(h, 16).unwrap())
            .collect()
    }

    /// A tile whose residual (minus the pixel above) is `v` at each
    /// (row, column) and zero elsewhere.
    fn columns(at: &[((usize, usize), i16)]) -> Vec<u8> {
        let mut t = vec![0u8; N];
        for &((y, x), v) in at {
            for yy in y..TILE {
                t[yy * TILE + x] = t[yy * TILE + x].wrapping_add(v as u8);
            }
        }
        t
    }

    /// Tiles as Capture One 16.6 coded them, from content we chose
    /// (benchmark-results/c1l2-2026-11-17/FORMAT.md, probes).
    #[test]
    fn reads_tiles_capture_one_coded() {
        let cases: Vec<(&str, Vec<u8>)> = vec![
            // A dot of 0 at the top-left of a tile of 255.
            ("02 00 42 f0 8f fb 52", {
                let mut t = vec![255; N];
                t[0] = 0;
                t
            }),
            // Class 1 with an offset: −1 at block (8, 8)'s first pixel.
            ("02 00 f0 88 7e 41 f0 87 7d 52", columns(&[((32, 32), -1)])),
            // Class 3: one 1-bit and one 2-bit part.
            (
                "02 00 f0 88 7e 63 f0 87 7b a2 00 25 00",
                columns(&[((32, 32), 1), ((32, 33), 2), ((33, 33), -1), ((33, 32), -2)]),
            ),
            // Class 4, a 32-bit word: residuals j − 8 along the Hilbert curve.
            (
                "02 00 f0 88 7e 7f f0 87 6f 02 38 40 51 62 b7 c8 d9 ea 0f",
                columns(
                    &HILBERT
                        .iter()
                        .enumerate()
                        .map(|(j, &(dy, dx))| ((32 + dy, 32 + dx), j as i16 - 8))
                        .collect::<Vec<_>>(),
                ),
            ),
            // Class 8: a residual of 64.
            (
                "02 00 f0 88 7e e9 f0 87 7d 02 40 00",
                columns(&[((32, 32), 64)]),
            ),
        ];
        for (bytes, want) in cases {
            assert_eq!(decode(&hex(bytes)).as_deref(), Some(&want[..]), "{bytes}");
        }
        // 2 along the first row from x = 1 on: a 128-bit and a 32-bit word
        // of the 2-bit stream.
        let ramp: Vec<u8> = (0..N).map(|i| 2 * (i % TILE) as u8).collect();
        let bytes = format!(
            "02 00 50 {} f0 8e fd 02 88 a2 a8 aa 2a 8a a2 a8 aa 2a 8a a2 a8 aa 2a 8a 8a 2a aa a8 00",
            "0b 53 ".repeat(16)
        );
        assert_eq!(decode(&hex(&bytes)).unwrap(), ramp);
        // 2 at the first four pixels of 17 blocks from block (8, 0) on.
        let at: Vec<_> = (128..145)
            .flat_map(|b: usize| {
                HILBERT[..4]
                    .iter()
                    .map(move |&(dy, dx)| ((b / 16 * 4 + dy, b % 16 * 4 + dx), 2))
            })
            .collect();
        let bytes = format!(
            "02 00 f0 87 fe 53 {}f0 86 fb 02 a8 aa 2a 8a a2 a8 aa 2a 8a a2 a8 aa 2a 8a a2 a8 aa a8 a2 8a a8 02",
            "0b 53 ".repeat(16)
        );
        assert_eq!(decode(&hex(&bytes)).unwrap(), columns(&at));
    }

    #[test]
    fn our_tiles_read_back() {
        let mut seed = 7u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 24) as u8
        };
        let smooth: Vec<u8> = (0..N).map(|i| ((i % 64) * 3 + (i / 64)) as u8).collect();
        let dither: Vec<u8> = (0..N).map(|i| 120 + (i / 700) as u8 + next() % 2).collect();
        let noise: Vec<u8> = (0..N).map(|_| next()).collect();
        let mut dot = vec![0u8; N];
        dot[2080] = 200;
        for tile in [smooth, dither, noise, dot, vec![0; N], vec![255; N]] {
            for max_group in [1, 4, 16, 64, 4095] {
                let coded = encode(&tile, max_group);
                assert_eq!(decode(&coded).as_ref(), Some(&tile), "group {max_group}");
            }
        }
    }

    #[test]
    fn refuses_what_it_does_not_know() {
        let good = hex("02 00 f0 88 7e 41 f0 87 7d 52");
        assert!(decode(&good).is_some());
        // Unknown kind, unknown group code, a value too many or too few,
        // another stream header.
        for bad in [
            "03 00 f0 88 7e 41 f0 87 7d 52",
            "02 00 f0 88 7e 91 f0 87 7d 52",
            "02 00 f0 88 7e 41 f0 87 7d 52 00",
            "02 00 f0 88 7e 41 f0 87 7d",
            "02 00 f0 88 7e 41 f0 87 7e 52",
            "02 00 f0 88 7e 41 f0 87 7d 53",
        ] {
            assert!(decode(&hex(bad)).is_none(), "{bad}");
        }
        assert!(decode(&[0; 10]).is_none());
    }
}
