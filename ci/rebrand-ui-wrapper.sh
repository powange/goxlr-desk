#!/usr/bin/env bash
# The desktop app (a Tauri window around the web interface) is built from GoXLR Utility's
# repository at a fixed commit. This turns that checkout into GoXLR Desk's app before it's built:
# its name, window title, identifier, icons, loading page, and the settings file it registers in.
#
#   ci/rebrand-ui-wrapper.sh <checkout of goxlr-utility-ui-wrapper-app>
set -euo pipefail

UI=${1:?usage: $0 <ui wrapper checkout>}
HERE=$(cd "$(dirname "$0")" && pwd)
TAURI="$UI/src-tauri"

# Files can be dropped on the page (sampler), and the app's name and identity
conf="$TAURI/tauri.conf.json"
jq '.productName = "GoXLR Desk"
    | .identifier = "com.github.powange.goxlr-desk-ui"
    | .app.windows[0].title = "GoXLR Desk"
    | .app.windows[0].dragDropEnabled = false' "$conf" > "$conf.tmp" && mv "$conf.tmp" "$conf"

# The program's name (goxlr-desk-ui.exe, as the daemon and the installer expect)
sed -i -e 's/^name = "goxlr-utility-ui"/name = "goxlr-desk-ui"/' \
       -e 's/^description = "GoXLR Utility UI"/description = "GoXLR Desk"/' "$TAURI/Cargo.toml"

# Its messages, and the daemon's settings it registers in (see daemon/src/settings.rs)
sed -i -e 's/ProjectDirs::from("org", "GoXLR-on-Linux", "GoXLR-Utility")/ProjectDirs::from("org", "GoXLR-Desk", "GoXLR-Desk")/' \
       -e 's/GoXLR Utility UI/GoXLR Desk/g' \
       -e 's/GoXLR Utility/GoXLR Desk/g' "$TAURI/src/main.rs"
grep -q 'ProjectDirs::from("org", "GoXLR-Desk", "GoXLR-Desk")' "$TAURI/src/main.rs" \
    || { echo "The desktop app's settings location wasn't found, check $TAURI/src/main.rs" >&2; exit 1; }

# Icons (made by daemon/resources/create-icons.py --tauri)
cp "$HERE"/ui-wrapper/icons/* "$TAURI/icons/"

# The page shown while the app waits for GoXLR Desk (GoXLR Utility's logo otherwise)
cp "$HERE/ui-wrapper/index.html" "$UI/src/index.html"
rm -f "$UI/src/utility.svg"
