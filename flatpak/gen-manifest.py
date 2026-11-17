#!/usr/bin/env python3
"""Fill in the Flatpak manifest for HEAD and generate the cargo sources of xrizer and frame-host.

Usage: gen-manifest.py <out dir> [repository url]. The url defaults to this checkout (only committed files are built).
"""
import asyncio
import json
import subprocess
import sys
from pathlib import Path

import toml

root = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(root / "wivrn/tools"))
import flatpakcargogenerator  # noqa: E402
from gen_flatpak_manifest import CMakeFile  # noqa: E402

out = Path(sys.argv[1]).resolve()
url = sys.argv[2] if len(sys.argv) > 2 else str(root)
out.mkdir(parents=True, exist_ok=True)
git = lambda *args: subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def cargo_sources(lock: Path, name: str):
    sources = asyncio.run(flatpakcargogenerator.generate_sources(toml.loads(lock.read_text())))
    (out / name).write_text(json.dumps(sources, indent=4))


xrizer = json.loads((root / "nix/xrizer.json").read_text())
# The patched source, so the cargo sources follow the patched Cargo.lock
xrizer_source = Path(subprocess.check_output([sys.executable, str(root / "scripts/fetch-xrizer.py")], text=True).strip())
cargo_sources(xrizer_source / "Cargo.lock", "xrizer-cargo-sources.json")
cargo_sources(root / "desktop/Cargo.lock", "frame-host-cargo-sources.json")

monado = json.loads((root / "flake.lock").read_text())["nodes"]["host-monado"]["locked"]["rev"]
cmake = CMakeFile(str(root / "wivrn/CMakeLists.txt"))
patches = "".join(
    f"      - type: patch\n        path: {root / 'nix' / name}\n        use-git: true\n        options: [--unidiff-zero]\n"
    for name in xrizer["patches"]
)

manifest = (root / "flatpak/io.github.konsti219.FrameShell.yml.in").read_text()
for key, value in {
    "XRIZER_PATCHES\n": patches,
    "XRIZER_COMMIT": xrizer["rev"],
    "XRIZER_VERSION_STRING": f"{xrizer['version']}-frame-shell",
    "FRAMESHELL_URL": url,
    "FRAMESHELL_COMMIT": git("rev-parse", "HEAD"),
    "FRAMESHELL_DESC": git("describe", "--tags", "--always"),
    "MONADO_COMMIT": monado,
    "BOOST_URL": cmake.get("boost", "URL"),
    "BOOST_SHA256": cmake.get("boost", "URL_HASH").split("=")[-1],
    "../../wivrn/": f"{root}/wivrn/",
}.items():
    manifest = manifest.replace(key, value)
(out / "io.github.konsti219.FrameShell.yml").write_text(manifest)
print(out / "io.github.konsti219.FrameShell.yml")
