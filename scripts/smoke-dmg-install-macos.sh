#!/usr/bin/env bash
set -Eeuo pipefail

fail() {
  printf '✗ %s\n' "$*" >&2
  exit 1
}

shopt -s nullglob
dmgs=(target/release/bundle/dmg/*.dmg)
[[ ${#dmgs[@]} -eq 1 ]] || fail "expected exactly one .dmg under target/release/bundle/dmg; found ${#dmgs[@]}"
dmg=${dmgs[0]}

temp_root=$(mktemp -d "${TMPDIR:-/tmp}/yukinal-dmg-smoke.XXXXXX")
mountpoint="$temp_root/mount"
installed_apps="$temp_root/Applications"
mkdir -p "$mountpoint" "$installed_apps"
attached=0
cleanup() {
  local status=$?
  if (( attached )); then
    hdiutil detach "$mountpoint" >/dev/null || {
      printf '✗ could not detach test DMG from %s\n' "$mountpoint" >&2
      status=1
    }
  fi
  rm -rf -- "$temp_root"
  exit "$status"
}
trap cleanup EXIT

hdiutil attach -readonly -nobrowse -noverify -mountpoint "$mountpoint" "$dmg"
attached=1
app_bundle="$mountpoint/Yukinal.app"
[[ -d "$app_bundle" ]] || fail "mounted DMG does not contain Yukinal.app at its root"
installed_app="$installed_apps/Yukinal.app"
ditto "$app_bundle" "$installed_app"
resource_root="$installed_app/Contents/Resources"
[[ -d "$resource_root" ]] || fail "copied app bundle has no Contents/Resources directory"
node scripts/smoke-installed-agent.mjs "$resource_root"

printf '✓ macOS DMG attach, Applications copy, and bundled Agent handshake: green\n'
