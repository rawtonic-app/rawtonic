#!/bin/sh
# A/B run of the hot-path benchmarks: the working tree (head) against a base
# commit, interleaved, with a table of medians, change and noise band
# (scripts/bench/ab_table.py). Run it before shipping a performance change.
# Usage: scripts/bench/ab.sh [--base REF] [--rounds N] [--bench LIST]
#                            [--filter REGEX] [--min-band PCT] [--fail-on-slower] [--keep]
#   --base REF        commit to compare with (default origin/main)
#   --rounds N        runs per side, alternating base/head (default 2: A B B A)
#   --bench LIST      comma list of decode,develop,encode (default all)
#   --filter REGEX    criterion filter, e.g. 'raw_decode|demosaic'
#   --min-band PCT    smallest noise band (default 3; scripts/bench/ab_table.py)
#   --fail-on-slower  exit 3 when a benchmark is slower beyond its noise band
#   --keep            keep the base worktree and bench binaries
# Inputs: $RAWTONIC_BENCH_FIXTURES (scripts/bench/fetch_fixtures.sh), else
# synthetic stand-ins. Work files: $RAWTONIC_BENCH_WORK (default
# target/bench-ab; the table is kept as ab.md and ab.json there); both
# sides build into the head's target dir.
# Cost: two release builds of the benches (one if the base is already
# built), then about 3 minutes per round and side for all three benches.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
BASE=origin/main; ROUNDS=2; BENCHES=decode,develop,encode; FILTER=''; FAIL=''; KEEP=0; MIN_BAND=3
while [ $# -gt 0 ]; do
  case "$1" in
    --base) BASE=$2; shift ;;
    --rounds) ROUNDS=$2; shift ;;
    --bench) BENCHES=$2; shift ;;
    --filter) FILTER=$2; shift ;;
    --min-band) MIN_BAND=$2; shift ;;
    --fail-on-slower) FAIL=--fail-on-slower ;;
    --keep) KEEP=1 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

for bench in $(echo "$BENCHES" | tr ',' ' '); do
  case "$bench" in decode|develop|encode) ;; *) echo "unknown bench: $bench" >&2; exit 2 ;; esac
done
case "$ROUNDS" in ''|*[!0-9]*|0) echo "--rounds needs a positive number" >&2; exit 2 ;; esac

TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
WORK=${RAWTONIC_BENCH_WORK:-$TARGET/bench-ab}
BASE_SHA=$(git rev-parse --verify "$BASE^{commit}")
HEAD_SHA=$(git rev-parse HEAD)
DIRTY=''; [ -z "$(git status --porcelain)" ] || DIRTY=' + uncommitted changes'
rm -rf "$WORK/criterion" "$WORK/bin" "$WORK/status"
mkdir -p "$WORK/bin/base" "$WORK/bin/head"
BASE_SRC=$WORK/base-src

cleanup() {
  if [ "$KEEP" = 0 ] && [ -d "$BASE_SRC" ]; then
    git worktree remove --force "$BASE_SRC" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

# Package of each bench target.
package() {
  case "$1" in
    decode) echo rawtonic-rawdecode ;;
    develop) echo rawtonic-engine ;;
    encode) echo rawtonic-export ;;
  esac
}

# build SIDE SRC: release-builds the bench targets in SRC and copies the
# executables to $WORK/bin/SIDE.
build() {
  side=$1; src=$2
  set --
  for bench in $(echo "$BENCHES" | tr ',' ' '); do
    set -- "$@" -p "$(package "$bench")" --bench "$bench"
  done
  echo "ab: building $side benches ($src)" >&2
  (cd "$src" && CARGO_TARGET_DIR=$TARGET cargo bench --locked "$@" \
    --features rawtonic-rawdecode/test-support,rawtonic-engine/test-support \
    --no-run --message-format=json-render-diagnostics) |
    python3 -I -c '
import json, shutil, sys
out = sys.argv[1]
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    if msg.get("reason") == "compiler-artifact" and msg.get("executable") and "bench" in msg["target"]["kind"]:
        shutil.copy2(msg["executable"], out + "/" + msg["target"]["name"])
' "$WORK/bin/$side"
  for bench in $(echo "$BENCHES" | tr ',' ' '); do
    [ -x "$WORK/bin/$side/$bench" ] || { echo "ab: $side has no bench '$bench' (added after $BASE?)" >&2; exit 1; }
  done
}

if [ "$BASE_SHA" = "$HEAD_SHA" ] && [ -z "$DIRTY" ]; then
  build head "$ROOT"
  cp "$WORK/bin/head/"* "$WORK/bin/base/"
else
  [ -d "$BASE_SRC" ] && git worktree remove --force "$BASE_SRC" >/dev/null 2>&1
  git worktree add --detach "$BASE_SRC" "$BASE_SHA" >/dev/null
  build base "$BASE_SRC"
  build head "$ROOT"
fi

echo "ab: base $BASE ($BASE_SHA), head $HEAD_SHA$DIRTY" >&2
if [ -n "${RAWTONIC_BENCH_FIXTURES:-}" ]; then
  echo "ab: fixtures $RAWTONIC_BENCH_FIXTURES" >&2
else
  echo "ab: RAWTONIC_BENCH_FIXTURES unset: synthetic inputs only" >&2
fi
echo "ab: load $(uptime | sed 's/.*load average[s]*: //')  (an idle machine gives the narrowest bands)" >&2

# Alternate the order each round (A B, B A, ...) so drift (heat, other
# load) falls on both sides alike.
round=1
while [ "$round" -le "$ROUNDS" ]; do
  if [ $((round % 2)) = 1 ]; then order='base head'; else order='head base'; fi
  for side in $order; do
    for bench in $(echo "$BENCHES" | tr ',' ' '); do
      echo "ab: round $round/$ROUNDS $side $bench" >&2
      # shellcheck disable=SC2086  # FILTER is one optional word
      CRITERION_HOME=$WORK/criterion/$side "$WORK/bin/$side/$bench" --bench \
        --save-baseline "r$round" --noplot $FILTER >"$WORK/last-run.log" 2>&1 ||
        { tail -n 20 "$WORK/last-run.log" >&2; exit 1; }
    done
  done
  round=$((round + 1))
done

# The table also goes to $WORK/ab.md (check.sh --brief shows no output of
# a passing step).
status=0
{
  echo
  echo "base $BASE ($(git rev-parse --short "$BASE_SHA")) vs head $(git rev-parse --short HEAD)$DIRTY, $ROUNDS rounds, $(uname -sm)"
  echo
  # shellcheck disable=SC2086  # FAIL is one optional word
  python3 -I "$ROOT/scripts/bench/ab_table.py" "$WORK" --json "$WORK/ab.json" --min-band "$MIN_BAND" $FAIL ||
    echo "exit $?" >"$WORK/status"
} | tee "$WORK/ab.md"
[ ! -f "$WORK/status" ] || { status=$(cut -d' ' -f2 "$WORK/status"); rm -f "$WORK/status"; }
exit "$status"
