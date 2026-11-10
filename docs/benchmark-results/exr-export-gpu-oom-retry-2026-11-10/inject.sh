#!/bin/bash
# inject.sh: injected GPU out-of-memory test of the export retry (task EXR). QA build only
# (identifier cc.rawtonic.qa.exr, feature qa-control), scratch HOME, scratch copies of
# two public corpus RAWs. A: no fault. B: first render fails out of memory (retried).
# C: every render fails (retries exhausted). Outputs to fresh folders.
S=$HOME/rawtonic-scratch/exr/oom; T=$HOME/rawtonic-scratch/exr/target/release
B=$S/bin/target/release; mkdir -p $B $S/home/.cache $S/src
cp $T/rawtonic-app $B/; touch $B/.cargo-lock; [ -e $B/lensfun_db ] || ln -s $T/lensfun_db $B/lensfun_db
[ -d $T/resources ] && [ ! -e $B/resources ] && ln -s $T/resources $B/resources
strings $B/rawtonic-app | grep -q "cc.rawtonic.qa.exr" || { echo "NOT A QA BUILD"; exit 2; }
C=$HOME/rawtonic-scratch/preservation/corpus
for f in pentax-k1.PEF olympus-em1m3.ORF; do [ -e $S/src/$f ] || cp $C/$f $S/src/; done
R=$HOME/rawtonic-scratch/preservation/recipes/raw.new.json
run() { # tag env...
  local tag=$1; shift; local o=$S/out.$tag.$(date +%s); mkdir -p $o
  env HOME=$S/home "$@" $B/rawtonic-app export $S/src --output $o --format tiff --tiff-bit-depth 16 --adjustments $R > $S/$tag.log 2>&1
  echo "$tag rc=$? dir=$o"; (cd $o && find . -type f | sort | xargs -r sha256sum) | sed "s/^/  $tag /"
}
run A
run B RAWTONIC_TEST_GPU_OOM=1
run C RAWTONIC_TEST_GPU_OOM=99
