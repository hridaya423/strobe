#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
WORK="$PWD/target/spike-out/1.10-trace"
OUT="../../docs/spikes/raw/1.10-trace.txt"
rm -rf "$WORK"
mkdir -p "$WORK/out" "$(dirname "$OUT")"
xcrun xctrace record --template 'Time Profiler' --time-limit 40s --no-prompt --output "$WORK/trace.trace" \
  --launch -- "$PWD/target/bundle/harness.app/Contents/MacOS/harness" 1.10 n=300 size=1080 out="$WORK/out" >/dev/null
xcrun xctrace export --input "$WORK/trace.trace" \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]' > "$WORK/time-profile.xml"
python3 -I scripts/trace_callback.py "$WORK/time-profile.xml" | tee "$OUT"
