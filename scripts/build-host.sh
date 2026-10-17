#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
mkdir -p "$root/out/host"
nix_flags=()
if [ -n "${FRAME_HOST_MONADO:-}" ]; then
    nix_flags+=(--override-input host-monado "path:$(realpath "$FRAME_HOST_MONADO")" --no-write-lock-file)
fi
nix develop "$root" "${nix_flags[@]}" --command cargo build --locked --release --manifest-path "$root/desktop/Cargo.toml" --target-dir "$root/out/desktop"
nix eval --json "$root#server.cmakeFlags" "${nix_flags[@]}" > "$root/out/host/cmake-flags.json"
nix develop "$root" "${nix_flags[@]}" --command bash "$root/scripts/compile-host.sh"
