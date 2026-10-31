#!/usr/bin/env bash
set -uo pipefail

# Gate B, delivered file. Usage: bench/gate-b.sh <fast|reference|theirs> <corpus id>...
# Renders each composition with vrf (or with its own framework for `theirs`), then scores the
# file against bench/refs/<id>/frames. Outputs go to $VRF_GATE_DIR (default /tmp/vrf-gate-b).
# Remotion rejects output paths with a dot in them, so the default is not under the repository.

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
bench=${VRF_BENCH_DIR:-$here}
gate=${VRF_GATE_DIR:-/tmp/vrf-gate-b}
mode=$1
shift

field() { sed -n "s/^$2 *= *\"\{0,1\}\([^\"]*\)\"\{0,1\} *$/\1/p" "$bench/corpus/$1/corpus.toml" | head -1; }

mkdir -p "$gate/$mode" "$gate/scores"
for id in "$@"; do
  framework=$(field "$id" framework)
  source=$bench/corpus/$(field "$id" source)
  out=$gate/$mode/$id.mp4
  rm -f "$out"
  echo "== $mode $id  load before: $(uptime | sed 's/.*load averages*: //')"
  start=$(python3 -c 'import time; print(time.time())')
  if [ "$mode" = theirs ]; then
    if [ "$framework" = hyperframes ]; then
      (cd "$bench/engines" && node_modules/.bin/hyperframes render "$source" -o "$out" --quiet)
    else
      (cd "$source" && node_modules/.bin/remotion render "$(field "$id" entry)" "$(field "$id" composition)" "$out" \
        --browser-executable "$bench/engines/browsers/chrome-headless-shell/mac_arm-156.0.8078.73/chrome-headless-shell-mac-arm64/chrome-headless-shell")
    fi
  elif [ "$framework" = hyperframes ]; then
    "$repo/target/release/strobe" render "$source" --hyperframes "$bench/engines/node_modules/hyperframes" \
      --mode "$mode" -o "$out" --json > "$gate/$mode/$id.json"
  else
    "$repo/target/release/strobe" render "$source" --composition "$(field "$id" composition)" --entry "$(field "$id" entry)" \
      --mode "$mode" -o "$out" --json > "$gate/$mode/$id.json"
  fi
  status=$?
  end=$(python3 -c 'import time; print(time.time())')
  frames=$(field "$id" frames)
  python3 - "$start" "$end" "$frames" "$status" <<'PY'
import sys
start, end, frames, status = float(sys.argv[1]), float(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
print(f"wall {end - start:.2f} s, {frames / (end - start):.1f} fps, exit {status}")
PY
  echo "load after: $(uptime | sed 's/.*load averages*: //')"
  [ "$status" = 0 ] || continue
  "$repo/target/release/vrf-bench" score --a "$out" --b "$bench/refs/$id/frames" --frames every:5 --out "$gate/scores/$mode-$id.json" | grep -v "^heatmap\|lowest SSIM"
done
