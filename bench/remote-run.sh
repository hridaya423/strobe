#!/usr/bin/env bash
set -uo pipefail

if [ $# -eq 0 ]; then
  echo "usage: bench/remote-run.sh <hyperframes|remotion|vrf>... (env: CORPUS, RUNS, VRF_REMOTE, VRF_REMOTE_ROOT)" >&2
  exit 2
fi

host=${VRF_REMOTE:-mac-tailscale}
root=${VRF_REMOTE_ROOT:-honey/videorenderingframework}
corpus=${CORPUS:-all}
runs=${RUNS:-5}
here=$(cd "$(dirname "$0")" && pwd)

cd "$here/.." || exit 1
cargo build --release -p vrf-bench -p vrf-cli -q || exit 1
rsync -a target/release/vrf-bench "$host:$root/bin/" || exit 1
ssh "$host" "mkdir -p \$HOME/$root/target/release/bundle" || exit 1
rsync -a target/release/strobe "$host:$root/target/release/" || exit 1
rsync -a --delete target/release/bundle/strobe.app "$host:$root/target/release/bundle/"

case $corpus in
  all | synth | synth-*) rsync -a --exclude external --exclude .cache "$here/corpus/" "$host:$root/bench/corpus/" || exit 1 ;;
esac

status=0
for engine in "$@"; do
  ssh "$host" "R=\$HOME/$root; export VRF_BENCH_DIR=\$R/bench PATH=\$R/tools:\$R/tools/node-v26.7.0-darwin-arm64/bin:\$R/bin:\$PATH; cd \$R && vrf-bench run --engine $engine --corpus $corpus --runs $runs --out-dir \$R/out/$engine" || status=1
  mkdir -p "$here/out/$engine"
  rsync -a "$host:$root/out/$engine/" "$here/out/$engine/"
  rsync -a "$host:$root/bench/results/" "$here/results/"
  ssh "$host" "rm -rf \$HOME/$root/out/$engine"
done
exit $status
