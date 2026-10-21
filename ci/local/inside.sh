#!/usr/bin/env bash
# Runs inside the build container (see build.sh): builds what's asked into /src/target/local-out.
set -euo pipefail

UI_REPO=https://github.com/goxlr-on-linux/goxlr-utility-ui-wrapper-app
UI_COMMIT=e71bb2e33439ca832ee7f2cbd5b29e500e2a8596
OUT=/src/target/local-out
mkdir -p "$OUT" /cache/downloads

VERSION=$(cargo metadata --no-deps --format-version 1 --manifest-path /src/Cargo.toml \
    | jq -r '.packages[] | select(.name == "goxlr-daemon") | .version')

# The desktop window (Tauri), at the commit the release workflow uses
ui_source() {
    if [ ! -d /cache/ui/.git ]; then
        git clone --quiet "$UI_REPO" /cache/ui
    fi
    git -C /cache/ui fetch --quiet origin
    git -C /cache/ui checkout --quiet --force "$UI_COMMIT"
    # As in the release workflow: files can be dropped on the page
    local conf=/cache/ui/src-tauri/tauri.conf.json
    jq '.app.windows[0].dragDropEnabled = false' "$conf" > "$conf.tmp" && mv "$conf.tmp" "$conf"
}

download() { # url file
    [ -s "/cache/downloads/$2" ] || curl --location --fail --silent --show-error --retry 3 -o "/cache/downloads/$2" "$1"
}

# Windows names its libraries in any case (User32.lib, ole32.lib); Linux doesn't ignore case, so
# the names crates ask for are linked to the files the SDK has.
fix_library_case() { # target
    cargo xwin env --target "$1" >/dev/null
    local dir
    for dir in /cache/xwin/xwin/sdk/lib/um/* /cache/xwin/xwin/sdk/lib/ucrt/* /cache/xwin/xwin/crt/lib/*; do
        [ -d "$dir" ] || continue
        (
            cd "$dir"
            for file in *.lib *.Lib; do
                [ -e "$file" ] || continue
                local base=${file%.*}
                for name in "$base.lib" "${base^}.lib" "${base,,}.lib"; do
                    [ -e "$name" ] || ln -s "$file" "$name"
                done
            done
        )
    done
}

build_windows() { # target suffix vc_redist_url
    local target=$1 suffix=$2 redist=$3
    echo "== Windows $target"
    fix_library_case "$target"
    cargo xwin build --manifest-path /src/Cargo.toml --release --all-features --target "$target"
    ui_source
    cargo xwin build --manifest-path /cache/ui/src-tauri/Cargo.toml --target-dir /src/target --release --target "$target"

    # NSIS reads the files where a native build puts them: a staging copy with that layout
    local stage=/tmp/stage-$target
    rm -rf "$stage" && mkdir -p "$stage/target/release" "$stage/daemon/resources" "$stage/ci"
    cp -r /src/ci/nsis "$stage/ci/"
    cp /src/LICENSE /src/LICENSE-3RD-PARTY "$stage/"
    cp /src/daemon/resources/goxlr-utility.ico "$stage/daemon/resources/"
    local bin=/src/target/$target/release
    for file in goxlr-daemon.exe goxlr-client.exe goxlr-client-quiet.exe goxlr-defaults.exe \
                goxlr-launcher.exe goxlr-utility-ui.exe; do
        cp "$bin/$file" "$stage/target/release/"
    done
    # Screen reader DLLs (tolk), next to the binaries. They only exist for x64: the ARM installer
    # carries them too (the installer expects them), the ARM daemon can't load them and speaks
    # with Windows' own voice (see the README).
    for dll in SAAPI64.dll nvdaControllerClient64.dll; do
        found=$(find "$bin" /cache/cargo/registry/src -name "$dll" -path "*x64*" 2>/dev/null | head -1 || true)
        [ -n "$found" ] || found=$(find "$bin" -name "$dll" | head -1)
        cp "$found" "$stage/target/release/$dll"
    done

    download "https://go.microsoft.com/fwlink/p/?LinkId=2124703" MicrosoftEdgeWebview2Setup.exe
    download "$redist" "vc_redist$suffix.exe"
    cp /cache/downloads/MicrosoftEdgeWebview2Setup.exe "$stage/ci/nsis/"
    cp "/cache/downloads/vc_redist$suffix.exe" "$stage/ci/nsis/vc_redist.exe"

    makensis -V2 "-DPRODUCT_VERSION=$VERSION$suffix" "$stage/ci/nsis/goxlr-utility.nsi"
    cp "$stage/ci/Output/"*.exe "$OUT/"
}

build_linux() {
    echo "== Linux"
    cd /src
    ./ci/build-packages
    cp target/debian/*.deb target/generate-rpm/*.rpm "$OUT/" 2>/dev/null || true
}

for what in "${@:-windows}"; do
    case "$what" in
        windows) build_windows x86_64-pc-windows-msvc "" https://aka.ms/vs/17/release/vc_redist.x64.exe ;;
        windows-arm) build_windows aarch64-pc-windows-msvc -arm64 https://aka.ms/vs/17/release/vc_redist.arm64.exe ;;
        linux) build_linux ;;
        *) echo "Unknown target: $what (windows, windows-arm, linux)" >&2; exit 1 ;;
    esac
done
echo "Done, in target/local-out:"
ls -la "$OUT"
