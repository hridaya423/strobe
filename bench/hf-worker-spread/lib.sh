root=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
out=${OUT:?set OUT to a scratch directory}

project_dir() {
  echo "$root/bench/corpus/$(sed -n 's/^source = "\(.*\)"/\1/p' "$root/bench/corpus/$1/corpus.toml")"
}

background() {
  case $1 in
    hf-hyperframes-launch) echo 0a0a0a ;;
    synth-*-hf)
      hex=$(grep -o 'html, *body *{[^}]*background: *#[0-9a-fA-F]*' "$root/bench/corpus/$1/index.html" | head -1 | grep -o '#[0-9a-fA-F]*$' | tr -d '#')
      [ ${#hex} = 3 ] && hex=$(echo "$hex" | sed 's/./&&/g')
      echo "$hex"
      ;;
  esac
}

ref_dir() {
  hex=$(background "$1")
  if [ -z "$hex" ]; then
    echo "$root/bench/refs/$1/frames"
    return
  fi
  dst=$out/refs-composited/$1/frames
  if [ "$(ls "$dst" 2>/dev/null | wc -l)" != "$(ls "$root/bench/refs/$1/frames" | wc -l)" ]; then
    "$root/bench/hf-worker-spread/composite.sh" "$1" "$hex" >&2
  fi
  echo "$dst"
}
