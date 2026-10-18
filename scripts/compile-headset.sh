#!/usr/bin/env bash
set -euo pipefail
build=/work/out/headset
flags=()
for dependency in /work/out/deps/*-src; do
    [ -d "$dependency" ] || continue
    name=$(basename "$dependency" -src)
    flags+=("-DFETCHCONTENT_SOURCE_DIR_${name^^}=$dependency")
done
cmake -S /work/wivrn -B "$build/client" -G Ninja \
    -DCMAKE_TOOLCHAIN_FILE=/work/scripts/aarch64-toolchain.cmake \
    -DCMAKE_PREFIX_PATH=/opt/ktx-arm64/usr \
    -DCMAKE_EXE_LINKER_FLAGS=-L/opt/ktx-arm64/usr/lib \
    -DKTX=/usr/bin/ktx \
    -DGIT_DESC=frame-shell-dev -DGIT_COMMIT="$(git -C /work rev-parse HEAD)" -DCMAKE_BUILD_TYPE=RelWithDebInfo \
    -DWIVRN_BUILD_CLIENT=ON -DWIVRN_BUILD_SERVER=OFF -DWIVRN_BUILD_DASHBOARD=OFF \
    -DWIVRN_BUILD_WIVRNCTL=OFF -DWIVRN_USE_SYSTEM_LIBKTX=ON -DWIVRN_COMPRESS_GLB=OFF \
    -DWIVRN_USE_SYSTEM_BOOST=OFF -DWIVRN_USE_SYSTEM_OPENXR=OFF -DWIVRN_USE_V4L2=ON \
    -DWIVRN_FRAME_EYES=ON -DWIVRN_EXECUTORCH_PREFIX=/opt/executorch-arm64 \
    -DWIVRN_WERROR=OFF "${flags[@]}"
cmake --build "$build/client" --parallel "$(nproc)"
mkdir -p "$build/host-libs"
ln -sf /usr/lib/x86_64-linux-gnu/libshaderc.so.1 "$build/host-libs/libshaderc_shared.so"
export SHADERC_LIB_DIR="$build/host-libs"
cargo build --locked --manifest-path /work/wayvr/Cargo.toml \
    --target aarch64-unknown-linux-gnu -p wayvr --lib --bin wayvr --release \
    --no-default-features --features openxr,wayland,feat-monado-metrics
install -Dm755 "$build/client/bin/wivrn" "$build/stage/frame-shell/frame-shell-client"
aarch64-linux-gnu-strip --strip-debug "$build/stage/frame-shell/frame-shell-client"
install -Dm755 "$CARGO_TARGET_DIR/aarch64-unknown-linux-gnu/release/libwayvr.so" "$build/stage/frame-shell/libwayvr.so"
install -Dm755 "$CARGO_TARGET_DIR/aarch64-unknown-linux-gnu/release/wayvr" "$build/stage/frame-shell/wayvr"
install -Dm755 "$build/client/_deps/openxr_loader-build/src/loader/libopenxr_loader.so" "$build/stage/frame-shell/libopenxr-shell.so"
install -Dm755 "$build/client/_deps/openxr_loader-build/src/loader/libopenxr_loader.so" "$build/stage/frame-shell/libopenxr_loader.so.1"
install -Dm755 /opt/ktx-arm64/usr/lib/libktx.so.4 "$build/stage/frame-shell/libktx.so.4"
install -Dm755 /usr/lib/aarch64-linux-gnu/libcrypto.so.3 "$build/stage/frame-shell/libcrypto.so.3"
find "$build/stage" -type f \( -name '*.so' -o -name '*.so.*' -o -path "$build/stage/home/steamos/.local/opt/monado/bin/*" \) \
    -exec aarch64-linux-gnu-strip --strip-debug {} +
