#!/usr/bin/env python3
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
config_bytes = (root / "nix/xrizer.json").read_bytes()
config = json.loads(config_bytes)
patches = [root / "nix" / name for name in config["patches"]]
digest = hashlib.sha256(config_bytes + b"".join(p.read_bytes() for p in patches)).hexdigest()[:16]
cache = root / "out/deps"
cache.mkdir(parents=True, exist_ok=True)
dest = cache / f"xrizer-{digest}"
if not dest.exists():
    with tempfile.TemporaryDirectory(dir=cache) as tmp:
        source = Path(tmp) / "source"
        subprocess.run(["git", "clone", "--quiet", "--no-checkout",
                        f"https://github.com/{config['owner']}/{config['repo']}.git", str(source)], check=True)
        subprocess.run(["git", "-C", str(source), "checkout", "--quiet", config["rev"]], check=True)
        subprocess.run(["git", "-C", str(source), "apply", "--unidiff-zero", *map(str, patches)], check=True)
        source.rename(dest)
print(dest)
