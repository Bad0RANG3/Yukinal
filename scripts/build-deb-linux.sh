#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <verified-payload-directory> <output-directory>" >&2
  exit 2
fi

payload=$(realpath "$1")
output=$(realpath -m "$2")
manifest=$(dirname "$payload")/release-manifest.json
[[ -f "$manifest" ]] || { echo "release manifest is missing: $manifest" >&2; exit 1; }
command -v dpkg-deb >/dev/null || { echo "dpkg-deb is required" >&2; exit 1; }

identity=$(node -e 'const m=require(process.argv[1]); process.stdout.write([m.version,m.platform,m.arch].join("\n"))' "$manifest")
version=$(printf '%s\n' "$identity" | sed -n '1p')
platform=$(printf '%s\n' "$identity" | sed -n '2p')
arch=$(printf '%s\n' "$identity" | sed -n '3p')
[[ "$platform" == "linux" ]] || { echo "expected linux payload, got $platform" >&2; exit 1; }
case "$arch" in
  x64) deb_arch=amd64 ;;
  arm64) deb_arch=arm64 ;;
  *) echo "unsupported Linux architecture: $arch" >&2; exit 1 ;;
esac

for required in yukinal-desktop-host yukinal ui/index.html python-agent/yukinal-agent ssh-service/yukinal-ssh-service assets/yukinal.png; do
  [[ -f "$payload/$required" ]] || { echo "required payload file is missing: $payload/$required" >&2; exit 1; }
done

stage=$(mktemp -d "${TMPDIR:-/tmp}/yukinal-deb.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/DEBIAN" "$stage/usr/lib/yukinal" "$stage/usr/bin" \
  "$stage/usr/share/applications" "$stage/usr/share/icons/hicolor/128x128/apps" \
  "$output"
cp -a "$payload/." "$stage/usr/lib/yukinal/"
cat > "$stage/DEBIAN/control" <<EOF
Package: yukinal
Version: $version
Section: utils
Priority: optional
Architecture: $deb_arch
Maintainer: Yukinal Project <yukinal@users.noreply.github.com>
Depends: libwebkit2gtk-4.1-0, libsecret-1-0, libssl3, libstdc++6
Description: AI-native remote development and infrastructure workspace
 Desktop SSH operations workspace with a structured C++ SSH service and a
 controlled Python Agent runtime.
EOF
cat > "$stage/usr/bin/yukinal" <<'EOF'
#!/bin/sh
exec /usr/lib/yukinal/yukinal "$@"
EOF
chmod 755 "$stage/usr/bin/yukinal" "$stage/usr/lib/yukinal/yukinal"
cat > "$stage/usr/share/applications/yukinal.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Yukinal
Comment=AI-native remote development and infrastructure workspace
Exec=/usr/bin/yukinal
Icon=yukinal
Terminal=false
Categories=Development;System;RemoteAccess;
StartupNotify=true
EOF
install -m 644 "$payload/assets/yukinal.png" "$stage/usr/share/icons/hicolor/128x128/apps/yukinal.png"

installer="$output/yukinal-${version}-linux-${deb_arch}.deb"
dpkg-deb --root-owner-group --build "$stage" "$installer"
dpkg-deb --info "$installer" >/dev/null
echo "Linux installer: $installer"
