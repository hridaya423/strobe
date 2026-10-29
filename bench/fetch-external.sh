#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"
ext=corpus/external
mkdir -p "$ext"

(cd engines && npm ci --no-fund --no-audit --loglevel=error)

HF_COMMIT=d7ac35069d74a3a437780579b3b7fe2f3eeace5f
HF_DIRS=(hyperframes-launch HF-heygen-stripe spacex-launch figma-launch)

clone_at() {
  local url=$1 dir=$2 commit=$3
  if [ ! -d "$dir/.git" ]; then
    GIT_LFS_SKIP_SMUDGE=1 git clone -q --filter=blob:none --no-checkout "$url" "$dir"
  fi
  git -C "$dir" fetch -q origin "$commit"
  GIT_LFS_SKIP_SMUDGE=1 git -C "$dir" checkout -q --detach "$commit"
}

clone_at https://github.com/heygen-com/hyperframes-launches.git "$ext/hyperframes-launches" "$HF_COMMIT"
include=$(printf '%s/**,' "${HF_DIRS[@]}")
git -C "$ext/hyperframes-launches" lfs pull --include="${include%,}"

remotion_project() {
  local repo=$1 commit=$2
  local dir="$ext/$repo"
  clone_at "https://github.com/remotion-dev/$repo.git" "$dir" "$commit"
  (
    cd "$dir"
    if git ls-files --error-unmatch package-lock.json >/dev/null 2>&1; then npm ci --no-fund --no-audit --loglevel=error --legacy-peer-deps
    else npm install --no-fund --no-audit --loglevel=error --legacy-peer-deps; fi
    ./node_modules/.bin/remotion browser ensure >/dev/null
    local shell=node_modules/.remotion/chrome-headless-shell
    [ -d "$shell" ] || return 0
    local bin=$(find "$shell" -name chrome-headless-shell -type f | head -1)
    if ! "$bin" --version >/dev/null 2>&1; then
      local zip=$(find "$shell" -name '*.zip' | head -1)
      local out="$shell/$(basename "$zip" .zip | sed 's/chrome-headless-shell-//')"
      rm -rf "$out" && mkdir -p "$out" && ditto -x -k "$zip" "$out"
      "$bin" --version >/dev/null
    fi
  )
}

remotion_project 4-0-trailer a79f7784219fe7461bb5e5e1e18bbf4a3d5be8ae
remotion_project transitions-video 8d4b188b42976f7dbdf536c681ce2b3e5e5e1669
remotion_project github-unwrapped ece8397df97eb8e0578f8825058da2bf650b1f7d

npx -y @puppeteer/browsers install chrome-headless-shell@156.0.8078.73 --path "$PWD/engines/browsers" >/dev/null
