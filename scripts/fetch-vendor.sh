#!/usr/bin/env bash
# Fetch the vendored gpui (from Zed) at the pinned revision.
set -euo pipefail

ZED_REV="c7bea343838f42b4c14151393e41f0bbe033c95e"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/vendor/zed"

if [ -d "$DEST/.git" ]; then
    git -C "$DEST" fetch --depth 1 origin "$ZED_REV"
    git -C "$DEST" checkout --quiet "$ZED_REV"
else
    mkdir -p "$ROOT/vendor"
    git init --quiet "$DEST"
    git -C "$DEST" remote add origin https://github.com/zed-industries/zed.git
    git -C "$DEST" fetch --depth 1 origin "$ZED_REV"
    git -C "$DEST" checkout --quiet FETCH_HEAD
fi

# Apply local patches (idempotent: reset tracked files first).
git -C "$DEST" checkout --quiet -- .
for patch in "$ROOT"/patches/*.patch; do
    [ -e "$patch" ] || continue
    git -C "$DEST" apply "$patch"
    echo "applied $(basename "$patch")"
done
echo "vendor/zed at $(git -C "$DEST" rev-parse HEAD)"
