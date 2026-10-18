#!/bin/sh
# Fetches the public RAW fixtures of the hot-path benchmarks (raw.pixls.us,
# CC0) into DIR and checks their SHA-256. Never commit them.
# Usage: scripts/bench/fetch_fixtures.sh [DIR]
#   DIR defaults to $RAWTONIC_BENCH_FIXTURES, else
#   ${XDG_CACHE_HOME:-~/.cache}/rawtonic-bench-fixtures.
# Then: export RAWTONIC_BENCH_FIXTURES=DIR (docs/DEVELOPMENT.md "Benchmarks").
set -eu
DIR=${1:-${RAWTONIC_BENCH_FIXTURES:-${XDG_CACHE_HOME:-$HOME/.cache}/rawtonic-bench-fixtures}}
mkdir -p "$DIR"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# name  sha256  url (names as crates/rawdecode/benches/support/mod.rs expects)
while read -r name sum url; do
  [ -n "$name" ] || continue
  file="$DIR/$name"
  if [ -f "$file" ] && [ "$(sha256 "$file")" = "$sum" ]; then
    echo "ok      $name"
    continue
  fi
  curl -fsSL --retry 2 -o "$file.part" "$url"
  got=$(sha256 "$file.part")
  if [ "$got" != "$sum" ]; then
    rm -f "$file.part"
    echo "FAILED  $name: SHA-256 $got, expected $sum ($url)" >&2
    exit 1
  fi
  mv "$file.part" "$file"
  echo "fetched $name"
done <<'EOF'
bayer-nef.NEF 5dea8ff6f95f372e4eb0852f75e728f182d7aa4057b2b76631f0fd3066c6a4b5 https://raw.pixls.us/data/Nikon/D7200/DSC_0977.NEF
bayer-cr3.CR3 5d2e076d38e7835d4c4f13dc2030a913cc573a31d7199b60777d23a25ccc3ef9 https://raw.pixls.us/data/Canon/EOS%20R10/IMG_4276.CR3
xtrans-raf.RAF 24abd27b4a200f3170ea1579672944b807d0eb35bf21422a3937bb644f27f2eb https://raw.pixls.us/data/Fujifilm/X-T3/AFXT2720.RAF
EOF
echo "export RAWTONIC_BENCH_FIXTURES='$DIR'"
