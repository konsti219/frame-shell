#!/usr/bin/env bash
# Downloads Babble Frame Runtime's eye model (GPL-3.0-or-later) for the headset client: fetch-eye-model.sh <file>
set -euo pipefail
dest=$1
url=https://media.githubusercontent.com/media/Project-Babble/Babble-Frame-Runtime/511bf3772568224b95b0aa21875e163dd10f4ec5/runtime/resources/models/eye_split96_uncapped_fp16.pte
sha256=b09a6359dc267f0be41f14c29635254341f4f4674d885b21c3672619b09c4871
if [ -f "$dest" ] && echo "$sha256  $dest" | sha256sum -c --quiet - 2>/dev/null; then
    exit 0
fi
mkdir -p "$(dirname "$dest")"
curl -fL "$url" -o "$dest.tmp"
echo "$sha256  $dest.tmp" | sha256sum -c --quiet -
mv "$dest.tmp" "$dest"
