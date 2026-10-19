#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <verified-payload-directory> <output-directory>" >&2
  exit 2
fi

payload=$(cd "$1" && pwd)
output=$(mkdir -p "$2" && cd "$2" && pwd)
manifest=$(dirname "$payload")/release-manifest.json
[[ -f "$manifest" ]] || { echo "release manifest is missing: $manifest" >&2; exit 1; }
command -v hdiutil >/dev/null || { echo "hdiutil is required" >&2; exit 1; }
command -v sips >/dev/null || { echo "sips is required" >&2; exit 1; }
command -v iconutil >/dev/null || { echo "iconutil is required" >&2; exit 1; }

identity=$(node -e 'const m=require(process.argv[1]); process.stdout.write([m.version,m.platform,m.arch].join("\n"))' "$manifest")
version=$(printf '%s\n' "$identity" | sed -n '1p')
platform=$(printf '%s\n' "$identity" | sed -n '2p')
arch=$(printf '%s\n' "$identity" | sed -n '3p')
[[ "$platform" == "darwin" ]] || { echo "expected macOS payload, got $platform" >&2; exit 1; }
case "$arch" in
  x64) asset_arch=x64 ;;
  arm64) asset_arch=arm64 ;;
  *) echo "unsupported macOS architecture: $arch" >&2; exit 1 ;;
esac

for required in yukinal-desktop-host yukinal ui/index.html python-agent/yukinal-agent ssh-service/yukinal-ssh-service assets/app-icon.png; do
  [[ -f "$payload/$required" ]] || { echo "required payload file is missing: $payload/$required" >&2; exit 1; }
done

work=$(mktemp -d "${TMPDIR:-/tmp}/yukinal-dmg.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
layout="$work/layout"
app="$layout/Yukinal.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$work/icon.iconset"
cp -a "$payload" "$app/Contents/Resources/yukinal"

icon_source="$payload/assets/app-icon.png"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$icon_source" --out "$work/icon.iconset/icon_${size}x${size}.png" >/dev/null
  doubled=$((size * 2))
  sips -z "$doubled" "$doubled" "$icon_source" --out "$work/icon.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$work/icon.iconset" -o "$app/Contents/Resources/Yukinal.icns"

short_version=${version%%-*}
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>Yukinal</string>
  <key>CFBundleIconFile</key><string>Yukinal.icns</string>
  <key>CFBundleIdentifier</key><string>dev.yukinal.desktop</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Yukinal</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$short_version</string>
  <key>CFBundleVersion</key><string>$short_version</string>
  <key>YukinalBuildVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
cat > "$app/Contents/MacOS/Yukinal" <<'EOF'
#!/bin/sh
set -eu
CONTENTS=$(CDPATH= cd -P "$(dirname "$0")/.." && pwd)
PAYLOAD="$CONTENTS/Resources/yukinal"
export YUKINAL_AGENT_ENTRY="$PAYLOAD/python-agent/yukinal-agent"
export YUKINAL_SSH_SERVICE_BINARY="$PAYLOAD/ssh-service/yukinal-ssh-service"
exec "$PAYLOAD/yukinal-desktop-host" --ui-dir "$PAYLOAD/ui" "$@"
EOF
chmod 755 "$app/Contents/MacOS/Yukinal"
ln -s /Applications "$layout/Applications"

installer="$output/yukinal-${version}-macos-${asset_arch}.dmg"
hdiutil create -quiet -volname "Yukinal $version" -srcfolder "$layout" -ov -format UDZO "$installer"
echo "macOS installer: $installer"
