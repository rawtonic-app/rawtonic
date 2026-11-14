# Empty Modular streams carry no ANS state

Origin: jxl-modular 0.11.3 from crates.io (archive SHA-256
2a2f045b24c738dd91d482be385512b512721ae08a671bd4b27bf1c47f215235), part of
jxl-oxide 0.12.6. Original MIT and Apache-2.0 license texts remain. Rawtonic's
change is AGPL-3.0-or-later (`LICENSE_RAWTONIC_AGPL-3.0`).

Only `src/image.rs::TransformedModularSubimage::decode_inner` changes: a
Modular stream none of whose channels has pixels returns before reading the
32-bit ANS initial state. libjxl's reference decoder does the same
(`lib/jxl/modular/encoding/encoding.cc`, `ModularDecode`:
`if (num_chans == 0) return true;` before the tree, histograms and
`ANSSymbolReader` are read; v0.12.0).

Without it, jxl-oxide failed with `UnexpectedEof` on every multi-group
lossless JPEG XL that jxl-encoder wrote with ANS (efforts 3 and up, any image
larger than 256 × 256 pixels): the global Modular stream of such a frame holds
no channels — they are all in the 256 × 256 groups — and jxl-encoder ends the
LfGlobal section after the tree and histograms, while jxl-oxide read four more
bytes. libjxl's djxl decodes those files. Rawtonic exported lossless JPEG XL
that way from 0.1.0-alpha.1, so the app could not reopen its own lossless
exports.

Checked 2026-11-14 (notes: benchmark-results/jxlm-2026-11-14/REPORT.md):
the formerly failing files (jxl-encoder 0.3.1 efforts 3–7 at 512 × 512, a
36 MP effort-4 frame, three 24–26 MP photo exports) decode to pixels identical
to djxl's; 53 other files (libjxl efforts 1–9 lossless, VarDCT, Modular lossy,
progressive, alpha, JPEG recompression, zune-jpegxl) render bit-identically
with and without the change.

Remove this copy when jxl-oxide ships an equivalent correction.
