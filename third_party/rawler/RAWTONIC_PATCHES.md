# Rawtonic patches to rawler

Vendored from [CyberTimon/RapidRAW-DngLab](https://github.com/CyberTimon/RapidRAW-DngLab)
(RapidRAW's fork of [dnglab/dnglab](https://github.com/dnglab/dnglab)), commit
`934af4b213a12b95f77d69529627067056066294`, directory `rawler/`, on 2026-10-05.
License: LGPL-2.1 (`LICENSE`), Copyright Daniel Vogelbacher, Pedro Côrte-Real and
contributors (`AUTHORS`). The decision is recorded in
`docs/decisions/0007-vendored-rawler.md`.

Left out of the copy: `data/testdata/` (44 MB of sample checksums and reference
files for the upstream `samplecheck` feature), `benches/`, `fuzz/`. The
integration tests in `tests/` are gated behind `samplecheck` upstream and stay
inert; unit tests in `src/` run with `cargo test -p rawler`.

Every change to upstream code is marked `Rawtonic patch` in the source. Keep
this list current.

## Manifest

- `Cargo.toml`: removed the `[[bench]]` targets, the `criterion` and
  `serde_yaml` dev-dependencies (only used by the left-out benches and the
  `samplecheck` tests); added `[lints]` that allow upstream's clippy and
  unused-code warnings so Rawtonic's `-D warnings` gates only act on these
  patches. `rustfmt.toml` is upstream's (2-space, 160 columns), so
  `cargo fmt --all --check` leaves the vendored style alone.

## Formatting

- `cargo fmt -p rawler` with upstream's `rustfmt.toml` touched six files that
  upstream had left unformatted (`decoders/mod.rs`, `decompressors/packed.rs`,
  `imgop/develop.rs`, `imgop/sensor/bayer/superpixel.rs`,
  `imgop/sensor/xtrans/demosaic.rs`, `pixarray.rs`): whitespace and import
  order only, so `cargo fmt --all --check` passes.

## Decoder fixes

- `src/decompressors/ljpeg/mod.rs`, `src/decompressors/ljpeg/decompressors.rs`,
  `src/pumps.rs`: lossless JPEG restart intervals (DRI/RSTn markers, ITU T.81
  H.1.2.1). The predictors restart at every interval and the bit pump
  byte-aligns past the marker. Samsung Galaxy S22 Ultra / S23 / S23+ Expert
  RAW DNGs (linear, 3 components, one RST per row or per 16 rows) decoded to
  diagonal ramps before. Intervals that are not whole rows are refused with
  an error instead of producing garbage. Unit tests in `ljpeg/mod.rs`.
- `src/decoders/orf.rs`: the colour pattern is built from the selected camera
  mode. High-resolution ORFs of the E-M5 Mark II and PEN-F (`highres` mode,
  GRBG) rendered purple because the normal mode's RGGB was used.
- `src/formats/tiff/entry.rs`: TIFF type 13 (IFD) entries are read as 32-bit
  offsets like LONG. They were read as single unknown bytes, so the Parrot
  Bebop DNGs' SubIFDs (raw image) were lost ("Unsupported DNG compression").
  229 Fujifilm/Olympus/OM files that also carry type-13 entries export
  byte-identical. Unit test `rawtonic_tests` in the same file.
- `src/decoders/mod.rs`: a single uncompressed strip whose StripByteCounts is
  smaller than ImageWidth x ImageLength needs is read at full size when the
  file holds it. DJI/Hasselblad L2D-20c (Mavic 3) DNGs state 5280-sample rows
  but store 5376 (the strip ends exactly at the end of the file); they
  panicked before.

## Levels and white balance against Adobe DNG Converter (2026-10-05)

Checked on all 1,680 non-DNG raw.pixls.us files converted with Adobe DNG
Converter 18.7 (white/black levels from the DNG's raw IFD, AsShotNeutral):

- `src/decoders/arw.rs`: the SR2 white level is a nominal 15360 (3840 for
  12-bit data); Adobe writes 16300-16383 (4095), so every Sony file rendered
  0.09 EV bright. Mapped to full scale (203 of 204 Sony files now within
  0.01 EV). ARW1 bodies (DSLR-A200 to A390) decode to 12 bits but kept the
  14-bit white and rendered two stops dark: white 4095.
- `src/decoders/cr2.rs`, `src/decompressors/ljpeg/mod.rs` (`precision()`):
  ColorData levels are on the 14-bit scale also for the 12-bit EOS 1000D /
  Rebel XS / Kiss F (two stops dark): scaled to the LJPEG precision.
- `src/decoders/crw.rs`, `src/formats/ciff/mod.rs` (`ShotInfo`): the as-shot
  white balance preset comes from ShotInfo word 7 (LibRaw's parse_ciff). The
  camera's fixed `wb_offset` picked the Cloudy preset on the EOS 300D and a
  fixed preset on the PowerShot G3/G5/S45/S50; it is now only the fallback.
- `src/decoders/kdc.rs`: the KDC IFD (0xfe00) is parsed and its as-shot
  preset (0xfa0d) selects the RGB levels (0xfa25-0xfa2a); before, the Shade
  preset or garbage (60.6/15.7) was read on the EasyShare Z981/Z990/Z1015 and
  P712.
- `src/decoders/srw.rs`: black levels from the maker note (0xa028,
  decrypted with the EncryptionKey 0xa020 like ExifTool's Samsung::Crypt)
  instead of 0; equal to Adobe's BlackLevel on all 27 SRW files (NX1/NX500:
  128). 12-bit captures of bodies with a 14-bit white (NX500) scale it.
- `src/decoders/rw2.rs`: 12-bit captures of 14-bit bodies (S1R II) use white
  4095 instead of 16380; the pre-RW2 .RAW files (no RawDataOffset) get no +15
  black offset (Adobe: 0, data start at 1).
- `data/cameras/nikon/coolpix_p330.toml`: blackpoint 3210 (was 0).
- `data/cameras/olympus/e-m1x.toml`: colour pattern RGGB as in the file's
  CFAPattern (BGGR swapped red and blue).

- `src/decoders/dng.rs`: DNG 1.7 enhanced image data (NewSubFileType 16,
  e.g. Lightroom Denoise results stored beside the original CFA data) is
  preferred, as Camera Raw renders it; its missing default crop is taken
  from the main image (relative to the active area the enhanced image
  covers). A 7D Mark II Enhanced-NR DNG: luminance noise 4.47 → 1.12 (Camera
  Raw 0.90), frame 5472 × 3648 like Camera Raw.

- `src/decoders/nef.rs`, `data/cameras/nikon/d1.toml` (hint `prebalanced`):
  the D1 stores white-balanced data (Adobe: AsShotNeutral 1 1 1); its
  WB_RBLevels are no longer applied a second time (dE 10.9 → 0.9 to Camera
  Raw).
- `src/decoders/crw.rs`, `data/cameras/canon/{g5,g6,s45,s60,s70}.toml`
  (hint `nowb`): Adobe ignores the stored white balance of these bodies;
  the decoder reports none so the adapter uses Adobe's default white. The
  PowerShot Pro1 is not among them: Camera Raw uses its stored as-shot
  preset (ColorInfo1, key-mangled, preset slot from ShotInfo; params
  `wb_offset = 48`), dE 3.6 → 1.15 (the other five get 3-8 times worse).

- `src/decoders/tfr.rs`, `data/cameras/hasselblad/{h3d,h4d_50}.toml`
  (hints `signed15`, `file_matrix`): H4D backs with WhiteLevel 32767 store
  signed samples (the 60 MP back black-subtracted); offset by 514 like
  Adobe DNG Converter, black = file black + 514, white 32253 above it.
  H-series 3FR files reporting "Hasselblad H3D" use the IFD0 ColorMatrix1
  of the back. 60 MP H4D dE 16.7 → 1.0 to Camera Raw.

- `src/decoders/cr2.rs`, `data/cameras/canon/{1dm4,1dxm2,40d,50d,5dmk2,5dmk3,5dmk4,5ds,5dsr,60d,6d,6dmk2,70d,7d,7dmk2,80d}.toml`
  (hint `sraw_specular_white`): sRAW/mRAW modes use black 0 and Adobe DNG
  Converter's white in rawler's units (4 x SpecularWhiteLevel on the 5D
  Mark IV, 80D, 1D X Mark II); inverted-coefficient bodies take the sRAW
  balance from the ColorData record before the one with green 1170
  (dcraw); the white is divided by the green gain Adobe's remaining
  as-shot balance implies. 31 files: median dE 3.79 -> 1.01 to Camera Raw.

- `data/cameras/olympus/e-10.toml`: colour pattern RGGB (file CFAPattern),
  black 128 / white 4092 (Adobe DNG Converter: 32 / 1023 on a quarter of
  this scale). dE 10.6 -> 0.8 to Camera Raw.
- `data/cameras/phase_one/p20.toml`: colour pattern GBRG at the active
  area origin (Adobe's RGGB frame starts one row lower). dE 10.9 -> 0.8.

- `data/cameras/canon/d30.toml`: hint `camera_raw_auto_wb` (Camera Raw
  renders D30 files with Auto white balance; the adapter uses a grey world).

- `src/decoders/dcr.rs`: Kodak IFD white balance Auto (0x03fc = 5) uses
  WB_RGBLevelsAuto (0x084d) as Adobe does (DCS Pro 14N/14nx/SLR/c).
- `src/decoders/cr2.rs`: an unknown ColorData layout falls through to the
  other white balance sources; PowerShot WBInfo (0x0029) is read from the
  maker note with the preset of ShotInfo word 7 (PowerShot G9).
- `data/cameras/mamiya/zd.toml`: hint `camera_raw_auto_wb` (Adobe writes an
  auto balance; the adapter uses a grey world).

- `src/decoders/dng.rs`: four-colour DNGs take their plane colours from
  CFAPlaneColor (the demosaic panicked with the default three planes).

## EXIF

- `src/exif.rs`: `Exif` also reads FocalPlaneXResolution / FocalPlaneResolutionUnit
  (0x920e/0x9210 and 0xa20e/0xa210) and ExifImageWidth, so the frame's
  physical width (crop-mode captures) is known for lens corrections.

## Camera names

- `src/decoders/camera.rs`, `src/decoders/mod.rs`: `Camera::canonical_model`
  and `RawMetadata::canonical_model` keep the base entry's clean model for
  regional aliases ("EOS Rebel T3i" -> "EOS 600D") so per-model tables can
  resolve them.

## Camera list

- New bodies (2026-10-05; matrices, levels and crops from Adobe DNG Converter,
  decoder parameters from the sibling model): `nikon/d1h.toml`,
  `nikon/1s1.toml` (1 J1 sensor), `nikon/1j4.toml` (1 V3 sensor),
  `nikon/coolpix_p7700.toml` (P7800), `minolta/dynax_5d.toml` (with Maxxum 5D,
  Alpha Sweet Digital, Alpha-5 Digital), `minolta/5.toml` (DiMAGE 5),
  `leica/dlux6.toml` (Panasonic LX7), `samsung/ek-gn120.toml` (Galaxy NX, NX300
  sensor); aliases: Pentax K2000 (K-m); modes: Panasonic DMC-GM1S 1:1, 3:2,
  16:9. All eleven render within dE 0.4-2.0 of Camera Raw.
- `data/cameras/hasselblad/cfv_50c_fff.toml`: Hasselblad CFV-50c FFF files
  (lossless-compressed, named "Hasselblad CFV-50c"; dE 0.6-1.2 to Camera Raw).
  X1D II FFF and CFV-50c 3FR still fail.
- `data/cameras/sony/fx2.toml`: Sony ILME-FX2 (33 MP sensor of the ILCE-7M4;
  Adobe Standard profile matrices, identical to the ILCE-7M4's).

## Fujifilm 2002-2003 white balance

- `src/decoders/raf.rs` + hint `adobe_old_wb` in `data/cameras/fuji/{f700,s2pro,s5000,s7000}.toml`:
  Adobe reads these bodies' WB_GRGBLevels (0x2ff0) with red and blue divided
  by 1.110 and 1.130 in every scene (Adobe DNG Converter AsShotNeutral of all
  86 corpus RAFs: only these four differ, all by the same factors).

## Epson R-D1 white balance

- `src/decoders/erf.rs`: the EpsonWB levels are scaled by 567/65536 (red) and
  431/65536 (blue) as Adobe DNG Converter does (same ratio on R-D1, R-D1s and
  R-D1x); dcraw's 508 x 1.078 and 382 x 1.173 were 3.4 % / 4.0 % off.

## Olympus SP350/SP500UZ/SP510UZ/SP550UZ white balance

- `src/decoders/orf.rs` + hint `adobe_wb_green_267` in the four tomls: Adobe
  divides these compacts' red/blue levels by 266.67 rather than 256 (patched
  WB_RBLevelsUsed 443 -> 1.6613, 512 -> 1.92 in DNG Converter); the SP570UZ
  and the other Olympus models keep 256.

## Rebranded and renamed bodies

- `src/decoders/mod.rs`: make "HASSELBLAD" → ARW decoder (Lunar = Sony NEX-7),
  "SAMSUNG TECHWIN" → PEF decoder (GX-1L = Pentax K110D).
- New definitions with Adobe DNG Converter's matrices, levels and crops:
  `samsung/gx1l.toml`, `hasselblad/lunar.toml`, `hasselblad/cf132.toml`
  (3FR Model " Flash Sync"), `phase_one/p40.toml`; aliases for the CFV-50c
  3FR (" CFV-50c/Flash Sync") and CFV 100C ("CFV 100C/Electronic Shutter").

## Nikon small NEF levels

- `src/decoders/nef.rs`: the level reset for small (YCbCr) NEFs used a single
  default black value against three white levels; the level correction was
  skipped and the image came out white (D4S). Three zero black values now.

## Phase One backs with diagonal Adobe matrices

- `data/cameras/phase_one/{iq150,iq3_100mp,iq3_100mp_trichr}.toml`,
  `leaf/credo50.toml`: Adobe writes diagonal ColorMatrix tags for these backs
  (the neutral per illuminant only) and puts the colour in ForwardMatrix; the
  tables carried the diagonals, which rendered 34-70 % too saturated. Replaced
  by the equivalent matrices diag(neutral) x inverse(FM) x Bradford(illuminant
  -> D50) from Adobe DNG Converter's tags.

## DNG black level from masked areas

- `src/decoders/dng.rs`: MaskedAreas are read as (top, left, bottom, right)
  (the unused upstream helper swapped x and y) and, for CFA DNGs whose
  BlackLevel is zero, the mean of the masked pixels becomes the black level
  when it is at least 0.5 % of white (Adobe DNG Converter files of the
  EOS-1Ds Mark II: dE 14.4 -> 1.4 against Camera Raw). Already
  black-subtracted files (noise floor only) are left alone.

## DNG DefaultUserCrop

- `src/decoders/dng.rs`: the virtual raw-tag IFD also carries DefaultUserCrop
  (DNG 1.5), which Camera Raw uses as the default crop (the in-camera aspect
  ratio of Adobe DNG Converter files of Panasonic bodies), and
  DefaultCropSize (the frame an embedded lens profile refers to).

## Sony SR2 data inside Adobe DNGs

- `src/decoders/arw.rs`: `ArwDecoder::sr2_subifd_from_adobe_block` decrypts the
  SR2 sub-IFD from the "SR2 " block Adobe DNG Converter keeps in
  DNGPrivateData (original offsets restored; the stored data are shorter than
  SonyLength by the SR2Private directory, the stream decryption covers what
  is there).

## Upstream test fix

- `src/tiles.rs`: the `test_par_bridge*` unit tests built `Vec<i32>` which
  does not implement `SubPixel`; typed as `Vec<u16>` so `cargo test -p rawler`
  compiles.

## EXIF image height

- `src/exif.rs`: `Exif::exif_image_height` (ExifImageHeight, 0xa003) next to
  the existing width; the CR3 decoder exposes no IFDs, and the Canon lens
  data reader needs the recorded image's diagonal.

## Sony ARW 6 (Compressed RAW 2)

- `src/decompressors/sony_arw6.rs`: Rust port of LibRaw's `sony_arw6.cpp`
  (LGPL 2.1 / CDDL 1.0; copyright LibRaw LLC and Jiang Yaogeng, notices in
  the file) for TIFF compression 32766, the ILCE-7M5's Compressed and
  Compressed HQ modes. Output is bit-identical to LibRaw master's on the four
  raw.pixls.us samples and equal to Adobe DNG Converter's raw values.
- `src/decoders/arw.rs`: compression 32766 dispatches to it, with black 1024
  and white 32800 (Adobe DNG Converter's levels for these files).

## Wide-gamut develop

- `src/imgop/develop.rs`, `src/imgop/raw.rs`: `RawDevelop::keep_negative`
  (default false) skips the negative clip after Calibrate, so colours outside
  sRGB survive for wide-gamut exports; Rawtonic limits them to the ProPhoto
  gamut afterwards.

## JPEG XL float tiles (DNG 1.7 merges)

- `src/decompressors/jpegxl.rs`, `src/decoders/mod.rs`: float JPEG XL tiles
  and strips are decoded (Lightroom 15 writes HDR and panorama merges as lossy
  VarDCT float). jxl-oxide gets `force_wide_buffers` (encoders signal 16-bit
  modular buffers that the data overflows; libjxl ignores the hint), and
  XYB-coded tiles are rendered as linear sRGB and converted to the signalled
  primaries here, because jxl-oxide's conversion into e.g. linear BT.2100
  gamut-maps for a display and turned values above 1 grey. Checked against
  libjxl's djxl on a Lightroom HDR tile (equal to 1e-4); unit test with a
  synthetic 0.01–3000 ramp.

## Linear images developed in place

- `src/imgop/develop.rs` (`RawDevelop::develop_linear_in_place`),
  `src/imgop/raw.rs` (`map_3ch_to_rgb_in_place`): LinearRaw three-colour
  images (merges, phones) are rescaled and calibrated in their own buffer
  instead of `develop_intermediate`'s clone, pixel copy and calibrated copy;
  same arithmetic (unit test: bit-identical to `develop_intermediate` for
  integer and float data, with and without the gamut clip). A 50 MP float
  DNG decodes in 0.37 s instead of 2.47 s and peaks at 1.7 GB instead of
  2.5 GB.

## Fewer full-size copies when decoding DNGs

- `src/pixarray.rs` (`Pix2D::into_crop`): crops in place (rows moved
  forward, buffer truncated). Tiles pad the decode buffer, so every tiled DNG
  was copied once at full size to drop the padding.
- `src/rawimage.rs` (`RawImage::apply_scaling`): LinearRaw float data are
  scaled in place (`Cow::to_mut` on the borrowed data copied the image).
- `src/decoders/dng.rs` (`raw_image` with `dummy`): a metadata-only decode
  no longer decompresses the pixels, as rawler's other decoders do (colour
  data of a DNG cost a second full decode).
- 50 MP float DNG through the merge's frame loader (mimalloc): peak
  1889 → 1177 MB; exports byte-identical before/after on a merged float
  DNG, a Lightroom JPEG XL panorama, a PowerShot (CHDK) DNG, a Pixel DNG, a
  NEF and an ARW; unit test `into_crop_equals_crop`.

## Lightroom merges of integer raws; transparency masks

- `src/decompressors/jpegxl.rs`: lossy (XYB) 16-bit integer tiles take the
  float route (linear sRGB → signalled primaries) and get the signalled
  transfer curve (Lightroom's integer tiles are sRGB-encoded, its float tiles
  linear), then are quantised. Equal to libjxl's djxl to ±1 code on a
  Lightroom panorama tile; ignored test `dump_integer_tile`.
- `src/decoders/dng.rs`: the DNG 1.6 transparency mask (NewSubFileType 4)
  is read for three-colour LinearRaw images; transparent pixels (Lightroom's
  panorama edges, padded with smeared edge pixels) become black, ramped
  between mask values 0.1 and 0.9. Masks are no longer candidates for the
  raw IFD.
- The sRGB encoding itself is undone by OpcodeList2 MapPolynomials, applied
  in Rawtonic's `rawdecode` (`dng_opcodes::apply_map_polynomials`).

## Own bit-count table in the lossless JPEG encoder

- `src/ljpeg92.rs`: `build_num_bits_tbl` (bits needed for each value
  0–255) is Rawtonic's own one-line implementation with `leading_zeros`; the
  upstream version said its logic was copied from the Adobe DNG SDK. Same
  table, same output.

## raw.pixls.us fingerprint follow-ups (2026-10-14)

- `src/decoders/tfr.rs`: the raw IFD's Compression tag chooses between
  16-bit samples (1) and Hasselblad lossless JPEG (7); the camera's
  `uncompressed` hint only applies without the tag. The X2D 100C (and the
  X1D II's Phocus FFF) are compressed despite the X-series hint and
  panicked reading past the strip; truncated uncompressed data is an error.
- `src/pumps.rs` (`BitPumpMSB32`): the last partial word is padded with
  zero bytes and reading past the end yields zero bits. `peek_bits` always
  shifts by 32, so a short refill left stale bits and an out-of-bounds
  Huffman lookup at the end of the X1D II stream.
- `data/cameras/hasselblad/x1d2.toml`: alias `Hasselblad X1DM2-50c` (IFD0
  Model of the X1D II).
- `src/formats/tiff/entry.rs`, `src/formats/tiff/reader.rs`
  (`EndianReader::stream_len`): a tag value that would run past the end of
  the file is refused before its buffer is allocated (the reader is left at
  the end, as the failed read left it). Corrupt counts allocated up to
  5.4 GB per tag (Sony DSLR-A100, Xiaomi HM NOTE 1LTE DNG); the corpus
  fingerprint run peaked at 14.8 GB, now 5.2 GB, with identical facts.
- `src/decompressors/ljpeg/mod.rs`: a quantisation table (DCT JPEG) says
  so: Blackmagic's lossy DNGs store 12-bit DCT tiles under Compression 7.
  They stay unsupported.
- `data/cameras/canon/sx710hs.toml` (geometry 5344 x 3950, margins, matrix),
  `sx20is.toml`, `ixus160_chdk.toml` (new CHDK file sizes): raw geometry
  and colour matrices from LibRaw's tables (`identify.cpp` CHDK file-size
  table, `colordata.cpp`). CHDK files are identified by size only; the
  IXUS 160 entry also matches the IXUS 175 / ELPH 180, which writes the
  same size.
- `data/cameras/canon/pro70.toml` (was `.unsupp`): the PowerShot Pro70's
  8 x 2 CMYG mosaic, plane order, levels and crop as Adobe DNG Converter
  18.7 writes them for the sample; rawler's existing Pro70 unpacking. The
  4-colour 8 x 2 demosaic leaves horizontal line artefacts (also on Adobe's
  DNG of the file).
- `src/formats/bmff/mod.rs` (`claimed_capacity`, `read_payload`), `stsz.rs`,
  `stts.rs`, `stsc.rs`, `co64.rs`, `ext_cr3/ctmd.rs`, `ext_cr3/cmt1.rs`–`cmt4.rs`,
  `ext_cr3/cnop.rs`: an ISO BMFF (CR3) box's entry counts and payload sizes
  no longer allocate what they claim up front: vectors grow as entries are
  read, payloads as bytes arrive, and a short file ends in an error. A
  damaged CR3 claiming 2^32 samples or a terabyte CMT box aborted the
  process with a failed allocation (the library's background facts pass
  parses every CR3 it holds). Valid files parse as before.
