#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
build="$root/out/headset"
mkdir -p "$build/cargo" "$build/stage"
exec 9>"$build/build.lock"
flock 9
xrizer_source=$(python3 "$root/scripts/fetch-xrizer.py")
bash "$root/scripts/fetch-eye-model.sh" "$build/stage/frame-shell/eye_split96_uncapped_fp16.pte"
podman build -t localhost/frame-shell-cross -f "$root/Containerfile" "$root"
podman run --rm --userns=keep-id \
    -v "$root:/work:Z" -w /work \
    -e CARGO_TARGET_DIR=/work/out/headset/cargo-target \
    -e CARGO_HOME=/work/out/headset/cargo \
    -e CCACHE_DIR=/work/out/headset/ccache \
    localhost/frame-shell-cross bash /work/scripts/compile-headset.sh
podman run --rm --userns=keep-id \
    -v "$root:/work:Z" -w /work \
    -e CARGO_HOME=/work/out/headset/cargo \
    localhost/frame-shell-cross bash /work/scripts/compile-headset-xrizer.sh "/work/${xrizer_source#"$root/"}"
