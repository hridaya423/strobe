#!/bin/sh
set -eu
cd "$(dirname "$0")"
cargo build --release --bin nodamage >&2
BIN=../../target/release/nodamage
OUT=${OUT:-../../docs/spikes/raw/1.4/nodamage.txt}
LEVELS="file://$(pwd)/pages/levels.html"
run() {
  echo "# $* load=$(uptime | sed 's/.*load averages*: //')" >> "$OUT"
  "$BIN" "$@" 2>&1 | grep -E '^(mode=|frame [0-9]+ changed)' | tail -4 >> "$OUT"
}
: > "$OUT"
run baseline 4 100
run inval-after 4 400
run inval-before 4 400
for q in 2 5 10 20; do run wait-inval 4 400 $q; done
run tick-opacity 4 100 5 /tmp/h-tick.txt
run baseline 4 100 5 /tmp/h-base.txt
cmp /tmp/h-base.txt /tmp/h-tick.txt && echo "# 100 frames: whole-frame hashes of baseline and tick-opacity are equal" >> "$OUT" || true
export NODAMAGE_PAGE="$LEVELS"
for mode in baseline tick-opacity tick-blend; do run $mode 1 512 5 /tmp/l-$mode.txt; done
for mode in tick-opacity tick-blend; do cmp /tmp/l-baseline.txt /tmp/l-$mode.txt && echo "# 512 levels: $mode hashes equal baseline" >> "$OUT"; done
run tick-opacity 1000 2000 5
