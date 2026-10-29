#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export CEF_PATH="${CEF_PATH:-$HOME/.local/share/cef}"
cargo build --release >&2
OUT="$PWD/target/spike-out/1.5"
RAW="../../docs/spikes/raw/1.5.txt"
PAGE="file://$PWD/pages/s15.html"
mkdir -p "$(dirname "$RAW")"
rm -rf "$OUT"
mkdir -p "$OUT"
: > "$RAW"
REF=target/release/headless_ref
for mode in gpu cpu swcomp; do
  for run in 0 1 2 3 4; do
    if [ "$run" = 0 ]; then extra="keep=all"; else extra="against=$OUT/$mode-r0.hashes"; fi
    HARNESS_RASTER="$mode" ./run.sh 1.5 out="$OUT" label="$mode-r$run" $extra | tee -a "$RAW"
  done
done
HARNESS_RASTER=gpu ./run.sh 1.5 out="$OUT" label=gpu-bf2 bf=2 against="$OUT/gpu-r0.hashes" | tee -a "$RAW"
HARNESS_RASTER=cpu ./run.sh 1.5 out="$OUT" label=cpu-bf2 bf=2 against="$OUT/cpu-r0.hashes" | tee -a "$RAW"
$REF capture out="$OUT" label=hs-cpu-r0 raster=cpu mode=screenshot page="$PAGE" | tee -a "$RAW"
$REF capture out="$OUT" label=hs-cpu-r1 raster=cpu mode=screenshot page="$PAGE" | tee -a "$RAW"
$REF capture out="$OUT" label=hs-gpu-r0 raster=gpu mode=screenshot page="$PAGE" | tee -a "$RAW"
for mode in gpu cpu swcomp; do
  for run in 1 2 3 4; do
    $REF compare dir_a="$OUT" dir_b="$OUT" label_a="$mode-r0" label_b="$mode-r$run" | tee -a "$RAW"
  done
done
$REF compare dir_a="$OUT" dir_b="$OUT" label_a=gpu-r0 label_b=gpu-bf2 | tee -a "$RAW"
$REF compare dir_a="$OUT" dir_b="$OUT" label_a=cpu-r0 label_b=cpu-bf2 | tee -a "$RAW"
$REF compare dir_a="$OUT" dir_b="$OUT" label_a=hs-cpu-r0 label_b=hs-cpu-r1 | tee -a "$RAW"
$REF compare dir_a="$OUT" dir_b="$OUT" label_a=cpu-r0 label_b=hs-cpu-r0 | tee -a "$RAW"
$REF compare dir_a="$OUT" dir_b="$OUT" label_a=gpu-r0 label_b=hs-gpu-r0 | tee -a "$RAW"
$REF compare dir_a="$OUT" dir_b="$OUT" label_a=gpu-r0 label_b=cpu-r0 | tee -a "$RAW"
$REF compare dir_a="$OUT" dir_b="$OUT" label_a=swcomp-r0 label_b=cpu-r0 | tee -a "$RAW"
