# Rawtonic logo

The sensor's 2×2 Bayer cell (R G / G B) becomes an R: the blue photosite turns into the leg.

| File | Use |
|---|---|
| `rawtonic-symbol.svg` | Symbol, colour, on light backgrounds |
| `rawtonic-symbol-on-dark.svg` | Symbol, colour, on dark backgrounds |
| `rawtonic-symbol-small.svg` | Symbol at 16–32 px (wider gaps) |
| `rawtonic-symbol-black.svg`, `-white.svg` | One colour |
| `rawtonic-horizontal-*.svg`, `rawtonic-stacked-*.svg` | Symbol with the wordmark |
| `rawtonic-wordmark-black.svg`, `-white.svg` | Wordmark alone |
| `app-icon-macos.svg`, `app-icon.svg` | 1024 px app icon sources (macOS icon grid; Windows/Linux) |

Colours: photosites and wordmark `#2b2f33` on light, `#dde4e9` / `#e0e1e3` on dark; the leg is the
only colour, `#4a7394` on light and `#9cb8d0` on dark. Never colour all four photosites.

Keep clear space of half a photosite (about a quarter of the symbol) around the mark. Don't close the
gaps, round the photosites, change the leg's 2:1 slope, rotate, mirror or stretch it, and don't set
the name in another typeface. The wordmark is drawn, not typed.

The rendered app icons live in `src-tauri/icons/` (16–32 px renditions use the small symbol). See
`TRADEMARKS.md` for use of the name and logo by forks.

On macOS 26 and later the app uses a layered Icon Composer icon, `src-tauri/icons/Rawtonic.icon`:
a charcoal fill at the back, the three light photosites in the middle and the blue leg as a Liquid
Glass layer in front, with dark, tinted and clear appearances. The
bundle ships it compiled as `src-tauri/icons/Assets.car` (listed in `tauri.macos.conf.json`), so
builds need no Xcode; after changing `Rawtonic.icon`, run `scripts/compile_macos_icon.sh`
(Xcode 26+). macOS 13–25 use `icon.icns`.
