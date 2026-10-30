#!/usr/bin/env bash
set -uo pipefail
. "$(dirname "$0")/lib.sh"
mode=$1
shift
cd "$root"
mkdir -p "$out/ours/$mode" "$out/logs"
for id in "$@"; do
  base=$out/ours/$mode/$id
  refdir=$(ref_dir "$id")
  ok=0
  for attempt in 1 2 3; do
    if target/release/vrf render "$(project_dir "$id")" --hyperframes "$root/bench/engines/node_modules/hyperframes" --mode "$mode" --json -o "$base.mp4" > "$base.report.json" 2> "$base.render.log" && [ -s "$base.mp4" ]; then
      ok=1
      break
    fi
    echo "$mode $id attempt $attempt failed" >> "$out/logs/ours-failures.log"
  done
  [ $ok = 1 ] || continue
  target/release/vrf-bench score --a "$base.mp4" --b "$refdir" --frames every:5 --out "$base.json" > "$base.score.log" 2>&1
  if [ "$id" = hf-hyperframes-launch ]; then
    target/release/vrf-bench score --a "$base.mp4" --b "$root/bench/refs/$id/frames" --frames every:5 --out "$base.raw.json" > "$base.rawscore.log" 2>&1
  fi
  echo "$mode $id done" >> "$out/logs/ours-progress.log"
done
