#!/usr/bin/env bash
set -euo pipefail
source=$1
build=/work/out/headset
version=$(python3 -c 'import json; print(json.load(open("/work/nix/xrizer.json"))["version"])')
export XRIZER_VERSION="$version-frame-shell"
export CARGO_TARGET_DIR="$build/xrizer-target"
for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
    if [[ $target == x86_64* ]]; then
        platform=linux64
        export PKG_CONFIG_LIBDIR=/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig
        loader=/usr/lib/x86_64-linux-gnu/libopenxr_loader.so.1
        strip=strip
    else
        platform=linuxarm64
        export PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig
        loader="$build/client/_deps/openxr_loader-build/src/loader/libopenxr_loader.so"
        strip=aarch64-linux-gnu-strip
    fi
    cargo rustc --locked --manifest-path "$source/Cargo.toml" --release --target "$target" --lib \
        -- -C 'link-arg=-Wl,-rpath,$ORIGIN'
    dest="$build/stage/frame-shell/xrizer/bin/$platform"
    install -Dm755 "$CARGO_TARGET_DIR/$target/release/libxrizer.so" "$dest/vrclient.so"
    install -Dm755 "$loader" "$dest/libopenxr_loader.so.1"
    "$strip" --strip-debug "$dest/vrclient.so" "$dest/libopenxr_loader.so.1"
done
