#!/usr/bin/env bash
set -uo pipefail

host=${VRF_REMOTE:-mac-tailscale}
root=${VRF_REMOTE_ROOT:-honey/videorenderingframework}
corpus=${CORPUS:-all}
runs=${RUNS:-5}
here=$(cd "$(dirname "$0")" && pwd)

cd "$here/.." && cargo build --release -p vrf-bench -p vrf-cli -q
rsync -a target/release/vrf-bench "$host:$root/bin/"
ssh "$host" "mkdir -p \$HOME/$root/target/release/bundle"
rsync -a target/release/vrf "$host:$root/target/release/"
rsync -a --delete target/release/bundle/vrf.app "$host:$root/target/release/bundle/"

status=0
for engine in "$@"; do
  ssh "$host" "R=\$HOME/$root; export VRF_BENCH_DIR=\$R/bench PATH=\$R/tools:\$R/tools/node-v26.7.0-darwin-arm64/bin:\$R/bin:\$PATH; cd \$R && vrf-bench run --engine $engine --corpus $corpus --runs $runs --out-dir \$R/out/$engine" || status=1
  mkdir -p "$here/out/$engine"
  rsync -a "$host:$root/out/$engine/" "$here/out/$engine/"
  rsync -a "$host:$root/bench/results/" "$here/results/"
  ssh "$host" "rm -rf \$HOME/$root/out/$engine"
done
exit $status
