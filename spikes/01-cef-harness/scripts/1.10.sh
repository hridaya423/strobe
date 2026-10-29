#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
OUT="../../docs/spikes/raw/1.10.txt"
WORK="$PWD/target/spike-out/1.10"
mkdir -p "$(dirname "$OUT")" "$WORK/1080" "$WORK/1080-verify" "$WORK/4k"
: > "$OUT"
./run.sh 1.10 size=1080 n="${N:-300}" out="$WORK/1080" | tee -a "$OUT"
./run.sh 1.10 size=1080 n="${N:-300}" out="$WORK/1080-verify" verify=1 | tee -a "$OUT"
./run.sh 1.10 size=4k n="${N:-300}" out="$WORK/4k" | tee -a "$OUT"
