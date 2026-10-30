#!/usr/bin/env bash
set -uo pipefail
. "$(dirname "$0")/lib.sh"
id=$1
cd "$root"
proj=$(project_dir "$id")
mkdir -p "$out/bar/$id" "$out/tmp/$id" "$out/logs"
refdir=$(ref_dir "$id")
for w in ${WORKERS:-1 2 3 4 5 6 7 8 9 10}; do
  base=$out/bar/$id/w$w
  [ -s "$base.json" ] && continue
  ok=0
  for attempt in 1 2 3; do
    rm -rf "$out/tmp/$id"/*
    if TMPDIR=$out/tmp/$id bench/engines/node_modules/.bin/hyperframes render "$proj" -o "$base.mp4" -w "$w" > "$base.render.log" 2>&1 && [ -s "$base.mp4" ]; then
      ok=1
      break
    fi
    echo "$id w$w attempt $attempt failed" >> "$out/logs/failures.log"
  done
  [ $ok = 1 ] || continue
  grep -o '"workerCount":[0-9]*' "$base.render.log" | sort | uniq -c | tr '\n' ' ' > "$base.workers.txt"
  target/release/vrf-bench score --a "$base.mp4" --b "$refdir" --frames every:5 --out "$base.json" > "$base.score.log" 2>&1
  if [ "$id" = hf-hyperframes-launch ]; then
    target/release/vrf-bench score --a "$base.mp4" --b "$root/bench/refs/$id/frames" --frames every:5 --out "$base.raw.json" > "$base.rawscore.log" 2>&1
  fi
  rm -f "$base.mp4"
  echo "$id w$w done" >> "$out/logs/progress.log"
done
rm -rf "$out/tmp/$id"
