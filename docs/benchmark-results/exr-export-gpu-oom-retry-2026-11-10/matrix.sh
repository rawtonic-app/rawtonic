#!/bin/bash
# matrix.sh: inject.sh, then real.sh at several free-memory levels (GPU otherwise idle)
cd $HOME/rawtonic-scratch/exr/oom; rm -f bin/target/release/rawtonic-app
./run.sh > m.inject 2>&1
for a in "app2 0 1" "app2 1200 1" "app2 800 1" "app 800 1" "app 1800 1" "cli 0 1" "cli 1800 1" "cli 1200 1" "cli 800 1"; do
  set -- $a; ./real.sh $1 $2 $3 > m.$1.$2 2>&1
done
cat m.inject; for f in m.app2.0 m.app2.1200 m.app2.800 m.app.800 m.app.1800 m.cli.0 m.cli.1800 m.cli.1200 m.cli.800; do echo "== $f"; grep -v "^held\|freed GPU" $f; done
