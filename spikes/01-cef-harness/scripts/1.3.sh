#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
OUT="../../docs/spikes/raw/1.3.txt"
mkdir -p "$(dirname "$OUT")"
: > "$OUT"
for size in 1080 4k; do
  for load in idle gpu-light gpu; do
    ./run.sh 1.3 size="$size" load="$load" n="${N:-300}" | tee -a "$OUT"
  done
done
