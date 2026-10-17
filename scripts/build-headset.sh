#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
build="$root/out/headset"
mkdir -p "$build/cargo" "$build/stage"
exec 9>"$build/build.lock"
flock 9
podman build -t localhost/frame-shell-cross -f "$root/Containerfile" "$root"
podman run --rm --userns=keep-id \
    -v "$root:/work:Z" -w /work \
    -e CARGO_TARGET_DIR=/work/out/headset/cargo-target \
    -e CARGO_HOME=/work/out/headset/cargo \
    -e CCACHE_DIR=/work/out/headset/ccache \
    localhost/frame-shell-cross bash /work/scripts/compile-headset.sh
