#!/usr/bin/env bash
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
build="$root/out/host"
mapfile -t flags < <(python3 - "$build/cmake-flags.json" <<'PY'
import json, sys
for flag in json.load(open(sys.argv[1])):
    if not flag.startswith('-DWIVRN_BUILD_DASHBOARD:'):
        print(flag)
PY
)
cmake -S "$root/wivrn" -B "$build" -G Ninja "${flags[@]}" \
    -DWIVRN_BUILD_DASHBOARD=OFF -DCMAKE_BUILD_TYPE=RelWithDebInfo \
    -DGIT_DESC=frame-shell-dev -DGIT_COMMIT="$(git -C "$root" rev-parse HEAD)" \
    -DCMAKE_INSTALL_PREFIX="$build/install" \
    -DFRAME_HOST_LIBRARY="$root/out/desktop/release/libframe_host.a"
cmake --build "$build" --parallel "${NIX_BUILD_CORES:-4}"
python3 - "$build" <<'PY'
import json, pathlib, sys
build = pathlib.Path(sys.argv[1]).resolve()
runtime = build / '_deps/monado-build/src/xrt/targets'
manifest = {'file_format_version': '1.0.0', 'runtime': {
    'name': 'WiVRn Frame shell',
    'library_path': str((runtime / 'openxr/libopenxr_wivrn.so').resolve()),
    'MND_libmonado_path': str((runtime / 'libmonado/libmonado_wivrn.so').resolve()),
}}
(build / 'openxr_wivrn-dev.json').write_text(json.dumps(manifest, indent=2) + '\n')
PY
