#!/bin/bash
# sweep.sh BIN TAG JOBS ROUNDS PHOTO... : like run.sh app (same HOME, args) but keeps stderr of every failure
BIN=$1; T=$2; J=$3; N=$4; shift 4
D=$HOME/rawtonic-scratch/preservation; A=$HOME/rawtonic-scratch/axf/$T; mkdir -p $A/home/.cache $A/err
export BIN A HANG
for r in $(seq 1 $N); do
  for f in "$@"; do for rc in $D/recipes/raw.*.json; do echo "$f $rc $r"; done; done
done | xargs -P $J -n 3 bash -c '
  n=$(basename "$0"); k=$(basename "$1" .json); o="$A/out.$n.$k.$2.tif"
  s=$(date +%s.%N)
  HOME="$A/home" timeout -s INT ${HANG:-400} gdb -q -batch -ex run -ex "thread apply all bt 25" --args "$BIN" export "$0" --output "$o" --tiff-bit-depth 16 --adjustments "$1" >"$A/err/$n.$k.$2" 2>&1; rc=$?
  e=$(date +%s.%N)
  if [ -s "$o" ]; then echo "$(sha256sum "$o" | cut -c1-16) $n.$k rc=$rc" >> $A/hashes; rm -f "$o"; [ $rc = 0 ] && rm -f "$A/err/$n.$k.$2"
  else echo "FAIL $n.$k round=$2 rc=$rc secs=$(python3 -c "print(round($e-$s,1))")" >> $A/fails; fi'
echo "SWEEP_DONE $T ok=$(wc -l < $A/hashes) fail=$(wc -l < $A/fails 2>/dev/null || echo 0)"
