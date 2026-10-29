#!/bin/sh
set -eu

runs=${RUNS:-5}
bin=${SPINE_BENCH:-target/release/spine_bench}
out=${OUT:-bench/results/$(date +%Y-%m-%d)-$(hostname -s)-ceilings.md}

wait_idle() {
  waited=0
  while :; do
    load=$(sysctl -n vm.loadavg | awk '{print $2}')
    if awk "BEGIN{exit !($load < 2)}"; then return; fi
    if [ "$waited" -ge 600 ]; then echo "load $load stayed at or over 2 for 10 min" >&2; exit 1; fi
    sleep 10
    waited=$((waited + 10))
  done
}

median() {
  sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'
}

measure() {
  label=$1
  shift
  "$bin" "$@" >/dev/null
  fps=""
  for _ in $(seq "$runs"); do
    wait_idle
    fps="$fps $("$bin" "$@" | sed -n 's/.* = \([0-9.]*\) fps/\1/p')"
  done
  m=$(printf '%s\n' $fps | median)
  echo "| $label | $m | $fps |" | tee -a "$out"
}

mkdir -p "$(dirname "$out")"
{
  echo "# Encoder ceilings, $(hostname -s), $(date '+%Y-%m-%d %H:%M')"
  echo
  echo "$(sysctl -n machdep.cpu.brand_string), macOS $(sw_vers -productVersion). spine_bench: GPU convert, encode and mux, frames already in GPU memory, no page. Median of $runs after one warm-up, load average under 2 before each run."
  echo
  echo "| Case | Median fps | Runs |"
  echo "|---|---|---|"
} >"$out"

measure "H.264 1080p" --frames 1800 --codec h264 --size 1080p
measure "HEVC 1080p" --frames 1800 --codec hevc --size 1080p
measure "H.264 1080p turbo" --frames 1800 --size 1080p --turbo
measure "H.264 4K" --frames 600 --codec h264 --size 4k
measure "HEVC 4K" --frames 600 --codec hevc --size 4k
measure "H.264 4K turbo" --frames 600 --size 4k --turbo
measure "ProRes 422 1080p" --frames 1800 --codec prores --size 1080p
