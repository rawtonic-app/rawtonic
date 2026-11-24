# HEIF EXIF items may be larger than 64 KiB

Origin: kamadak-exif 0.6.1 from crates.io (archive SHA-256
1130d80c7374efad55a117d715a3af9368f0fa7a2c54573afc15a188cd984837). The
original BSD-2-Clause license text (`LICENSE`) remains. Rawtonic's change is
AGPL-3.0-or-later.

Only `src/isobmff.rs` changes: `MAX_EXIF_SIZE` is 16 MiB instead of 65535.
65535 is the size of a JPEG APP1 segment; a HEIF/ISOBMFF Exif item has no such
limit. A Xiaomi phone's HEIC (a public test file from the pillow_heif test
set) holds a 192 KB Exif item, and with the 64 KiB cap kamadak-exif failed on
the whole file ("Exif data too large"), so Rawtonic read no capture time at
import and exported no EXIF at all. The cap is kept, raised, so a corrupt item
length still cannot make a reader allocate without bound.

Remove this copy when upstream raises the limit.
