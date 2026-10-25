# Frame shell development: build the PC server and the headset shell, deploy to the headset, run the dev server

set shell := ["bash", "-euo", "pipefail", "-c"]

root := justfile_directory()
host_build := root / "out/host"
headset_build := root / "out/headset"
headset := env("FRAME_HEADSET", "steamos@frame")
monado := env("FRAME_MONADO", root / "../frame-monado")

_default:
    @just --list

# Build frame-monado, the headset shell and the PC server; deploy both to the headset
all:
    just --justfile "{{monado}}/justfile" build
    just build-headset build-host
    just --justfile "{{monado}}/justfile" deploy
    just deploy

# Build the headset shell and deploy it
headset: build-headset deploy

# Build the PC server (host services and WiVRn server) into out/host; FRAME_HOST_MONADO overrides its runtime
build-host:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p "{{host_build}}"
    nix_flags=()
    if [ -n "${FRAME_HOST_MONADO:-}" ]; then
        nix_flags+=(--override-input host-monado "path:$(realpath "$FRAME_HOST_MONADO")" --no-write-lock-file)
    fi
    nix develop "{{root}}" "${nix_flags[@]}" --command cargo build --locked --release \
        --manifest-path "{{root}}/desktop/Cargo.toml" --target-dir "{{root}}/out/desktop"
    nix eval --json "{{root}}#server.cmakeFlags" "${nix_flags[@]}" > "{{host_build}}/cmake-flags.json"
    nix develop "{{root}}" "${nix_flags[@]}" --command just --justfile "{{justfile()}}" _compile-host

# In nix develop: the WiVRn server and the development runtime manifest
_compile-host:
    #!/usr/bin/env bash
    set -euo pipefail
    build="{{host_build}}"
    mapfile -t flags < <(python3 -c '
    import json, sys
    for flag in json.load(open(sys.argv[1])):
        if not flag.startswith("-DWIVRN_BUILD_DASHBOARD:"):
            print(flag)
    ' "$build/cmake-flags.json")
    cmake -S "{{root}}/wivrn" -B "$build" -G Ninja "${flags[@]}" \
        -DWIVRN_BUILD_DASHBOARD=OFF -DCMAKE_BUILD_TYPE=RelWithDebInfo \
        -DGIT_DESC=frame-shell-dev -DGIT_COMMIT="$(git -C "{{root}}" rev-parse HEAD)" \
        -DCMAKE_INSTALL_PREFIX="$build/install" \
        -DFRAME_HOST_LIBRARY="{{root}}/out/desktop/release/libframe_host.a"
    cmake --build "$build" --parallel "${NIX_BUILD_CORES:-4}"
    python3 -c '
    import json, pathlib, sys
    build = pathlib.Path(sys.argv[1]).resolve()
    runtime = build / "_deps/monado-build/src/xrt/targets"
    manifest = {"file_format_version": "1.0.0", "runtime": {
        "name": "WiVRn Frame shell",
        "library_path": str((runtime / "openxr/libopenxr_wivrn.so").resolve()),
        "MND_libmonado_path": str((runtime / "libmonado/libmonado_wivrn.so").resolve()),
    }}
    (build / "openxr_wivrn-dev.json").write_text(json.dumps(manifest, indent=2) + "\n")
    ' "$build"

# Cross-build the headset shell (WiVRn client, WayVR, xrizer) in the Podman SDK into out/headset/stage
build-headset: _fetch-eye-model
    #!/usr/bin/env bash
    set -euo pipefail
    build="{{headset_build}}"
    mkdir -p "$build/cargo" "$build/stage"
    exec 9>"$build/build.lock"
    flock 9
    xrizer_source=$(python3 "{{root}}/scripts/fetch-xrizer.py")
    podman build -t localhost/frame-shell-cross -f "{{root}}/Containerfile" "{{root}}"
    podman run --rm -i --userns=keep-id -v "{{root}}:/work:Z" -w /work \
        -e CARGO_HOME=/work/out/headset/cargo -e CCACHE_DIR=/work/out/headset/ccache \
        -e XRIZER_SOURCE="${xrizer_source#"{{root}}/"}" localhost/frame-shell-cross bash -euo pipefail -s <<'EOF'
    build=/work/out/headset
    export CARGO_TARGET_DIR=$build/cargo-target
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
    # X11 apps in WayVR (Steam's client among them): SteamOS doesn't ship xwayland-satellite
    cargo install --locked --git https://github.com/Supreeeme/xwayland-satellite --tag v0.8.3 \
        --target aarch64-unknown-linux-gnu --root "$build/xwayland-satellite" xwayland-satellite
    stage=$build/stage/frame-shell
    install -Dm755 "$build/xwayland-satellite/bin/xwayland-satellite" "$stage/xwayland-satellite"
    install -Dm755 "$build/client/bin/wivrn" "$stage/frame-shell-client"
    aarch64-linux-gnu-strip --strip-debug "$stage/frame-shell-client"
    install -Dm755 "$CARGO_TARGET_DIR/aarch64-unknown-linux-gnu/release/libwayvr.so" "$stage/libwayvr.so"
    install -Dm755 "$CARGO_TARGET_DIR/aarch64-unknown-linux-gnu/release/wayvr" "$stage/wayvr"
    loader=$build/client/_deps/openxr_loader-build/src/loader/libopenxr_loader.so
    install -Dm755 "$loader" "$stage/libopenxr-shell.so"
    install -Dm755 "$loader" "$stage/libopenxr_loader.so.1"
    install -Dm755 /opt/ktx-arm64/usr/lib/libktx.so.4 "$stage/libktx.so.4"
    install -Dm755 /usr/lib/aarch64-linux-gnu/libcrypto.so.3 "$stage/libcrypto.so.3"
    find "$stage" -type f \( -name '*.so' -o -name '*.so.*' \) ! -path '*/xrizer/bin/linux64/*' \
        -exec aarch64-linux-gnu-strip --strip-debug {} +

    # xrizer for OpenVR games, ARM64 and x86-64 (FEX)
    export XRIZER_VERSION="$(python3 -c 'import json; print(json.load(open("/work/nix/xrizer.json"))["version"])')-frame-shell"
    export CARGO_TARGET_DIR=$build/xrizer-target
    for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
        if [[ $target == x86_64* ]]; then
            platform=linux64
            export PKG_CONFIG_LIBDIR=/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig
            target_loader=/usr/lib/x86_64-linux-gnu/libopenxr_loader.so.1
            strip=strip
        else
            platform=linuxarm64
            export PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig
            target_loader=$loader
            strip=aarch64-linux-gnu-strip
        fi
        cargo rustc --locked --manifest-path "/work/$XRIZER_SOURCE/Cargo.toml" --release --target "$target" --lib \
            -- -C 'link-arg=-Wl,-rpath,$ORIGIN'
        dest=$stage/xrizer/bin/$platform
        install -Dm755 "$CARGO_TARGET_DIR/$target/release/libxrizer.so" "$dest/vrclient.so"
        install -Dm755 "$target_loader" "$dest/libopenxr_loader.so.1"
        "$strip" --strip-debug "$dest/vrclient.so" "$dest/libopenxr_loader.so.1"
    done
    EOF

# Babble Frame Runtime's eye kit (GPL-3.0-or-later) for the headset client
_fetch-eye-model:
    #!/usr/bin/env bash
    set -euo pipefail
    dir="{{headset_build}}/stage/frame-shell"
    base=https://media.githubusercontent.com/media/Project-Babble/Babble-Frame-Runtime/e96a3d31d945ae6d7b17cecd5455a1f1052d6c5e/runtime/resources/models
    rm -f "$dir/eye_split96_uncapped_fp16.pte"
    for model in eye_kit_stage1_fp16.pte:82ceeeabe6d73991882529dacadebc7836d9d5f0be3bd20147bcd91051e8a47e \
        eye_kit_stage2_fp16.pte:caeaaf14dc6a45af37ebf30ce20c215c4bf374b0271817ad1e506a7c17797afb; do
        name=${model%%:*} sha256=${model#*:} dest="$dir/${model%%:*}"
        if [ -f "$dest" ] && echo "$sha256  $dest" | sha256sum -c --quiet - 2>/dev/null; then
            continue
        fi
        mkdir -p "$dir"
        curl -fL "$base/$name" -o "$dest.tmp"
        echo "$sha256  $dest.tmp" | sha256sum -c --quiet -
        mv "$dest.tmp" "$dest"
    done

# Install the headset shell and restart it if Monado runs; FRAME_HEADSET overrides steamos@frame
deploy:
    #!/usr/bin/env bash
    set -euo pipefail
    stage="{{headset_build}}/stage/frame-shell"
    [[ -x "$stage/frame-shell-client" && -f "$stage/xrizer/bin/linux64/vrclient.so" && -f "$stage/xrizer/bin/linuxarm64/vrclient.so" ]] || {
        echo "Run just build-headset first" >&2; exit 1;
    }
    headset="{{headset}}"
    ssh "$headset" 'mkdir -p ~/.local/state/frame-shell/deploy ~/.config/systemd/user'
    tar -C "$stage" -cf - . | ssh "$headset" 'tar -C ~/.local/state/frame-shell/deploy -xf -'
    scp "{{root}}/headset/not-with-shell.conf" "{{root}}/headset/launcher" "{{root}}/headset/frame-shell.desktop" \
        "{{root}}/headset/steamvr" "{{root}}/headset/podman" "$headset":.local/state/frame-shell/deploy/
    scp "{{root}}/wayvr/dash-frontend/assets/dashboard/frame_shell_logo.png" "$headset":.local/state/frame-shell/deploy/frame-shell.png
    scp "{{root}}/headset/frame-shell.service" "$headset":.config/systemd/user/frame-shell.service
    ssh "$headset" 'bash -euo pipefail -s' <<'EOF'
    systemctl --user daemon-reload
    stage="$HOME/.local/state/frame-shell/deploy"
    for binary in "$stage/frame-shell-client" "$stage/libwayvr.so" "$stage/wayvr"; do
        if report=$(LD_LIBRARY_PATH="$stage" ldd "$binary" 2>&1) && [[ "$report" != *"not found"* ]]; then :; else
            printf '%s\n' "$report" >&2
            exit 1
        fi
    done
    systemctl --user stop frame-shell-dev.service 2>/dev/null || true
    systemctl --user stop frame-shell.service monado-wivrn.service monado-wayvr.service
    mkdir -p "$HOME/.local/bin" "$HOME/.local/lib/frame-shell" "$HOME/.local/share/applications" "$HOME/.config/systemd/user/monado-wivrn.service.d" "$HOME/.config/systemd/user/monado-wayvr.service.d"
    install -m755 "$stage/launcher" "$HOME/.local/bin/frame-shell"
    # Listed in SteamVR's non-Steam apps: switches to the desktop session, which is the frame shell
    install -m644 "$stage/frame-shell.desktop" "$HOME/.local/share/applications/frame-shell.desktop"
    install -Dm644 "$stage/frame-shell.png" "$HOME/.local/share/icons/hicolor/256x256/apps/frame-shell.png"
    rm -f "$HOME/.local/share/wayvr/openxr-layer-control"
    # Remove the retired Plasma helper files; Desktop is built into the shell now
    rm -f "$HOME/.local/bin/frame-shell-desktop" "$HOME/.local/share/applications/deckard-nested-desktop.desktop" \
        "$HOME/.local/share/frame-shell/desktop-config/autostart/steam.desktop" "$stage/desktop" "$stage/deckard-nested-desktop.desktop"
    rmdir "$HOME/.local/share/frame-shell/desktop-config/autostart" "$HOME/.local/share/frame-shell/desktop-config" 2>/dev/null || true
    install -m755 "$stage/frame-shell-client" "$HOME/.local/bin/frame-shell-client"
    install -Dm755 -t "$HOME/.local/lib/frame-shell/bin" "$stage/steamvr" "$stage/podman" "$stage/xwayland-satellite"
    install -m755 "$stage/wayvr" "$HOME/.local/bin/wayvr"
    install -m755 "$stage/"*.so* "$HOME/.local/lib/frame-shell/"
    cp -a "$stage/xrizer" "$HOME/.local/lib/frame-shell/"
    install -Dm644 -t "$HOME/.local/share/frame-shell" "$stage/eye_kit_stage1_fp16.pte" "$stage/eye_kit_stage2_fp16.pte"
    rm -f "$HOME/.local/share/frame-shell/eye_split96_uncapped_fp16.pte" "$stage/eye_split96_uncapped_fp16.pte"
    install -m644 "$stage/not-with-shell.conf" "$HOME/.config/systemd/user/monado-wivrn.service.d/not-with-shell.conf"
    install -m644 "$stage/not-with-shell.conf" "$HOME/.config/systemd/user/monado-wayvr.service.d/not-with-shell.conf"
    touch "$HOME/.config/steamframe-enable-shell"
    systemctl --user daemon-reload
    if systemctl --user is-active --quiet monado.service; then
        systemctl --user start frame-shell.service
    else
        echo 'Installed. Switch the headset to desktop mode to start the shell.'
    fi
    EOF

# Serve the development PC server instead of the installed one until stopped
run:
    #!/usr/bin/env bash
    set -euo pipefail
    build="{{host_build}}"
    server="$build/server/wivrn-server"
    [ -x "$server" ] || { echo "Run just build-host first" >&2; exit 1; }
    config="${XDG_CONFIG_HOME:-$HOME/.config}"
    mkdir -p "$build/config/wivrn"
    if { [ ! -f "$build/config/wivrn/cookie" ] || [ ! -f "$build/config/wivrn/known_keys.json" ]; } && [ -f "$config/wivrn/cookie" ]; then
        for state in cookie known_keys.json; do
            if [ -f "$config/wivrn/$state" ]; then
                cp -- "$config/wivrn/$state" "$build/config/wivrn/$state"
                chmod 600 "$build/config/wivrn/$state"
            fi
        done
    fi
    was_active=false
    if systemctl --user is-active --quiet frame-shell.service; then
        was_active=true
        systemctl --user stop frame-shell.service
    fi
    # Games launched normally (xrizer, OpenXR apps) use the development runtime while this runs
    runtime_dir="$config/openxr/1"
    mkdir -p "$runtime_dir"
    saved_runtime=''
    # (left over from a run that was killed: nothing to restore)
    if cmp -s -- "$build/openxr_wivrn-dev.json" "$runtime_dir/active_runtime.json"; then
        rm -f -- "$runtime_dir/active_runtime.json"
    fi
    if [ -e "$runtime_dir/active_runtime.json" ] || [ -L "$runtime_dir/active_runtime.json" ]; then
        saved_runtime="$build/active_runtime.json.saved"
        mv -fT -- "$runtime_dir/active_runtime.json" "$saved_runtime"
    fi
    cp -fT -- "$build/openxr_wivrn-dev.json" "$runtime_dir/active_runtime.json"
    server_pid=''
    cleanup() {
        if [ -n "$server_pid" ]; then
            kill "$server_pid" 2>/dev/null || true
            wait "$server_pid" 2>/dev/null || true
        fi
        rm -f -- "$runtime_dir/active_runtime.json"
        if [ -n "$saved_runtime" ]; then mv -fT -- "$saved_runtime" "$runtime_dir/active_runtime.json"; fi
        if "$was_active"; then systemctl --user start frame-shell.service; fi
    }
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    export FRAME_SHELL_RUNTIME_JSON="$build/openxr_wivrn-dev.json"
    export FRAME_SHELL_CAPTURE_TOKEN="${FRAME_SHELL_CAPTURE_TOKEN:-${XDG_STATE_HOME:-$HOME/.local/state}/frame-desktop/capture-token}"
    mkdir -p -- "$(dirname -- "$FRAME_SHELL_CAPTURE_TOKEN")"
    if [ ! -e "$FRAME_SHELL_CAPTURE_TOKEN" ] && [ -f "$build/capture-token" ]; then
        install -m600 -- "$build/capture-token" "$FRAME_SHELL_CAPTURE_TOKEN"
    fi
    export GST_REGISTRY="$build/gstreamer-registry.bin"
    # The portal keys restore tokens by app, taken from the systemd scope: in a terminal's app-*.scope the tokens
    # shared with the installed frame-shell.service don't match and every monitor asks again
    XDG_CONFIG_HOME="$build/config" systemd-run --user --scope --quiet --collect --unit="frame-shell-dev-$$" -- \
        nix develop "{{root}}" --command "$server" -f "{{root}}/server.json" --no-manage-active-runtime &
    server_pid=$!
    wait "$server_pid"

# Clean up after a killed `just run`: its server, the runtime override, the installed WiVRn
stop:
    #!/usr/bin/env bash
    set -euo pipefail
    build="{{host_build}}"
    pkill -INT -f "^$build/server/wivrn-server " || true
    for _ in $(seq 50); do pgrep -f "^$build/server/wivrn-server " >/dev/null || break; sleep 0.2; done
    runtime_dir="${XDG_CONFIG_HOME:-$HOME/.config}/openxr/1"
    if cmp -s -- "$build/openxr_wivrn-dev.json" "$runtime_dir/active_runtime.json"; then
        rm -f -- "$runtime_dir/active_runtime.json"
    fi
    if [ -e "$build/active_runtime.json.saved" ]; then
        mv -fT -- "$build/active_runtime.json.saved" "$runtime_dir/active_runtime.json"
    fi
    systemctl --user start frame-shell.service

# Launch a command against the development runtime
[positional-arguments]
app +command:
    @[ -f "{{host_build}}/openxr_wivrn-dev.json" ] || { echo "Run just build-host first" >&2; exit 1; }
    @XR_RUNTIME_JSON="{{host_build}}/openxr_wivrn-dev.json" exec "$@"

# wivrnctl of the development server
[positional-arguments]
ctl +args:
    @exec "{{host_build}}/server/wivrnctl" "$@"
