#!/usr/bin/env bash
# Builds without GitHub Actions, in a container: the Windows installer (default), the ARM one,
# and the Linux packages. The results land in target/local-out.
#
#   ci/local/build.sh                 Windows x64 installer
#   ci/local/build.sh windows-arm     Windows ARM installer
#   ci/local/build.sh linux           .deb and .rpm
#   ci/local/build.sh windows linux   several at once
#
# The first run downloads the toolchains (a few GB, kept in ~/.cache/goxlr-utility-build).
# The web interface is embedded from daemon/web-content: build the UI and copy its dist there
# first if it changed. GOXLR_BUILD_JOBS (default 4) limits parallel compilation, so the build
# doesn't take all the memory Docker has.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
CACHE=${GOXLR_BUILD_CACHE:-$HOME/.cache/goxlr-utility-build}
IMAGE=goxlr-utility-build

mkdir -p "$CACHE"
docker build -t "$IMAGE" "$ROOT/ci/local"
# Run as you, so the files written in the repository stay yours
docker run --rm \
    --user "$(id -u):$(id -g)" \
    -e HOME=/tmp \
    -e CARGO_BUILD_JOBS="${GOXLR_BUILD_JOBS:-4}" \
    -v "$ROOT":/src \
    -v "$CACHE":/cache \
    "$IMAGE" bash /src/ci/local/inside.sh "$@"
