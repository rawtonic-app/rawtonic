#!/bin/bash
# real.sh MODE LEAVE HOLD: a real GPU out-of-memory (task EXR). vram_hog (AXF) leaves LEAVE MiB
# free; one pentax-k1.PEF raw.new export starts (MODE app: the QA app binary of inject.sh;
# cli: the rawtonic CLI); HOLD s after the export's first out-of-memory warning the hog is
# killed (a co-tenant that frees its memory). Prints rc and the output's SHA-256 (compare with
# an export without the hog). Run only with the GPU otherwise idle.
S=$HOME/rawtonic-scratch/exr/oom; T=$HOME/rawtonic-scratch/exr/target/release
MODE=${1:-app}; LEAVE=${2:-1200}; HOLD=${3:-1}
R=$HOME/rawtonic-scratch/preservation/recipes/raw.new.json; P=$S/src/pentax-k1.PEF
o=$S/out.real.$MODE.$LEAVE.$(date +%s); mkdir -p $o; L=$o.log
if [ "$LEAVE" -gt 0 ]; then $S/hog $LEAVE > $S/hog.log & H=$!; sleep 2; cat $S/hog.log; fi
if [ $MODE = cli ]; then
  RAWTONIC_LENSFUN_DB=$T/lensfun_db HOME=$S/home $T/rawtonic export $P --output $o/pentax-k1.tif --format tiff --tiff-bit-depth 16 --adjustments $R > $L 2>&1 & E=$!
else
  mkdir -p $S/src1; [ -e $S/src1/pentax-k1.PEF ] || cp $P $S/src1/
  HOME=$S/home $S/bin/target/release/rawtonic-app export $S/src1 --output $o --format tiff --tiff-bit-depth 16 --adjustments $R > $L 2>&1 & E=$!
fi
if [ -n "$H" ]; then
  until grep -q "ran out of memory for" $L || ! kill -0 $E 2>/dev/null; do sleep 0.2; done
  sleep $HOLD; kill $H 2>/dev/null
fi
wait $E; echo "real $MODE rc=$? leave=$LEAVE hold=$HOLD"
(cd $o && sha256sum *); grep -E "ran out of memory for|freed GPU|Export error|exports failed" $L | head
