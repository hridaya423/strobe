#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
OUT="../../docs/spikes/raw/1.7.txt"
mkdir -p "$(dirname "$OUT")"
: > "$OUT"
for only in all css raf now; do
  ./run.sh 1.7 n="${N:-200}" only="$only" | tee -a "$OUT"
done
