#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
OUT="../../docs/spikes/raw/1.2.txt"
mkdir -p "$(dirname "$OUT")"
: > "$OUT"
N="${N:-300}"
for variant in layout transform paint; do
  for bf in 1 2 3; do
    ./run.sh 1.2 n="$N" bf="$bf" variant="$variant" | tee -a "$OUT"
  done
  ./run.sh 1.2 n="$N" bf=2 variant="$variant" mode=burst | tee -a "$OUT"
done
