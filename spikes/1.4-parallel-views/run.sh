#!/bin/sh
set -eu
cd "$(dirname "$0")"
cargo build --release --bin scale >&2
BIN=../../target/release/scale
OUT=${OUT:-../../docs/spikes/raw/1.4/scale.txt}
VRF_BENCH_DIR=${VRF_BENCH_DIR:?set VRF_BENCH_DIR to the bench directory}
export VRF_BENCH_DIR VRF_CACHE_DIR="${VRF_CACHE_DIR:-/tmp/vrf-cache}"
PAGES=${PAGES:-"colour trans github"}
NS=${NS:-"1 2 4 8"}
CONVERT=${CONVERT:-1}
for page in $PAGES; do
  case $page in colour) frames=${FRAMES_COLOUR:-600};; *) frames=${FRAMES_HEAVY:-150};; esac
  for mode in inproc multiproc; do
    for n in $NS; do
      echo "# page=$page mode=$mode n=$n frames/view=$frames convert=$CONVERT load=$(uptime | sed 's/.*load averages*: //')" >> "$OUT"
      if [ $mode = inproc ]; then
        $BIN inproc $n $page $frames $CONVERT 0 nosync >> "$OUT"
      else
        $BIN multiproc $n $page $frames $CONVERT >> "$OUT"
      fi
    done
  done
done
