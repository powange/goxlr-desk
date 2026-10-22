#!/usr/bin/env bash
# Builds without GitHub Actions, in a container: the Windows installer (default), the ARM one,
# and the Linux packages. The results land in target/local-out.
#
#   ci/local/build.sh                 Windows x64 installer
#   ci/local/build.sh windows-arm     Windows ARM installer
#   ci/local/build.sh linux           .deb and .rpm (built on Debian bookworm, see Dockerfile.linux)
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

# Linux packages are built in their own image (older glibc), with their own target folder so
# nothing built in one image is linked in the other.
windows=()
linux=0
for what in "${@:-windows}"; do
    if [ "$what" = linux ]; then linux=1; else windows+=("$what"); fi
done

mkdir -p "$CACHE/target-linux" "$ROOT/target/local-out"
run() { # image extra docker arguments.. -- inside.sh arguments..
    local image=$1; shift
    local extra=()
    while [ "$1" != -- ]; do extra+=("$1"); shift; done
    shift
    # Run as you, so the files written in the repository stay yours
    docker run --rm \
        --user "$(id -u):$(id -g)" \
        -e HOME=/tmp \
        -e CARGO_BUILD_JOBS="${GOXLR_BUILD_JOBS:-4}" \
        -v "$ROOT":/src \
        -v "$CACHE":/cache \
        "${extra[@]}" \
        "$image" bash /src/ci/local/inside.sh "$@"
}

if [ ${#windows[@]} -gt 0 ]; then
    docker build -t "$IMAGE" -f "$ROOT/ci/local/Dockerfile" "$ROOT/ci/local"
    run "$IMAGE" -- "${windows[@]}"
fi
if [ $linux = 1 ]; then
    docker build -t "$IMAGE-linux" -f "$ROOT/ci/local/Dockerfile.linux" "$ROOT/ci/local"
    run "$IMAGE-linux" \
        -v "$CACHE/target-linux":/src/target \
        -v "$ROOT/target/local-out":/src/target/local-out \
        -- linux
fi
