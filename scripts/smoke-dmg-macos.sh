#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <dmg-installer>" >&2
  exit 2
fi

installer=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
mkdir -p "$PWD/target/release/installer-smoke"
run_root=$(mktemp -d "$PWD/target/release/installer-smoke/macos.XXXXXX")
mountpoint="$run_root/mount"
applications="$run_root/Applications"
data_dir="$run_root/user-data"
mkdir -p "$mountpoint" "$applications" "$data_dir"
printf 'must survive app removal\n' > "$data_dir/must-survive-uninstall.txt"
marker="$data_dir/must-survive-uninstall.txt"
node_bin=$(command -v node)

hdiutil attach -quiet -readonly -nobrowse -mountpoint "$mountpoint" "$installer"
trap 'hdiutil detach -quiet "$mountpoint" >/dev/null 2>&1 || true' EXIT
app="$mountpoint/Yukinal.app"
test -d "$app/Contents/Resources/yukinal/ui"
test -f "$app/Contents/Resources/yukinal/ui/index.html"
test -x "$app/Contents/MacOS/Yukinal"
cp -R "$app" "$applications/Yukinal.app"
installed="$applications/Yukinal.app"
YUKINAL_DATA_DIR="$data_dir" "$installed/Contents/MacOS/Yukinal" --help
YUKINAL_DATA_DIR="$data_dir" "$installed/Contents/Resources/yukinal/yukinal-desktop-host" --self-check
YUKINAL_DATA_DIR="$data_dir" PATH=/usr/bin:/bin:/usr/sbin:/sbin "$node_bin" scripts/smoke-python-agent.mjs \
  --executable "$installed/Contents/Resources/yukinal/python-agent/yukinal-agent"

hdiutil detach -quiet "$mountpoint"
trap - EXIT
rm -rf -- "$installed"
test -f "$marker"
echo "PASS DMG mount, Applications copy, packaged host and Agent startup, app removal, and user-data retention ($run_root)"
