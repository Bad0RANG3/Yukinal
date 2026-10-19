#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <deb-installer>" >&2
  exit 2
fi

installer=$(realpath "$1")
mkdir -p "$PWD/target/release/installer-smoke"
run_root=$(mktemp -d "$PWD/target/release/installer-smoke/linux.XXXXXX")
data_dir="$run_root/user-data"
mkdir -p "$data_dir"
printf 'must survive uninstall\n' > "$data_dir/must-survive-uninstall.txt"
marker="$data_dir/must-survive-uninstall.txt"
node_bin=$(command -v node)

if dpkg-query -W -f='${Status}' yukinal 2>/dev/null | grep -q 'install ok installed'; then
  echo 'refusing to replace a pre-existing Yukinal package on this runner' >&2
  exit 1
fi
sudo apt-get install -y "$installer"
test -x /usr/bin/yukinal
test -f /usr/lib/yukinal/ui/index.html
test -x /usr/lib/yukinal/python-agent/yukinal-agent
YUKINAL_DATA_DIR="$data_dir" /usr/lib/yukinal/yukinal-desktop-host --self-check
YUKINAL_DATA_DIR="$data_dir" /usr/bin/yukinal --help

# The harness uses its absolute Node path, while the frozen Agent child sees only
# OS directories and cannot fall back to a system Node or Python installation.
YUKINAL_DATA_DIR="$data_dir" PATH=/usr/sbin:/usr/bin:/sbin:/bin "$node_bin" scripts/smoke-python-agent.mjs \
  --executable /usr/lib/yukinal/python-agent/yukinal-agent

sudo apt-get remove -y yukinal
if dpkg-query -W -f='${Status}' yukinal 2>/dev/null | grep -q 'install ok installed'; then
  echo 'Debian package remained installed after removal' >&2
  exit 1
fi
test -f "$marker"
echo "PASS Debian install, packaged host and Agent startup, remove, and user-data retention ($run_root)"
