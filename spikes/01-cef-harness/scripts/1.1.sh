#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
OUT="../../docs/spikes/raw/1.1.txt"
mkdir -p "$(dirname "$OUT")"
: > "$OUT"
for damage in full small; do
  for pacing in 1 50; do
    ./run.sh 1.1 n="${N:-2000}" pacing_ms="$pacing" damage="$damage" | tee -a "$OUT"
  done
done
./run.sh 1.1 n=50 nodamage=1 watchdog_ms=100 | tee -a "$OUT"
