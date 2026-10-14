#!/usr/bin/env bash
set -Eeuo pipefail

fail() {
  printf '✗ %s\n' "$*" >&2
  exit 1
}

if [[ "${YUKINAL_ALLOW_DEB_INSTALL_SMOKE:-}" != "1" ]]; then
  fail "refusing to install a Debian package without YUKINAL_ALLOW_DEB_INSTALL_SMOKE=1"
fi

shopt -s nullglob
debs=(target/release/bundle/deb/*.deb)
[[ ${#debs[@]} -eq 1 ]] || fail "expected exactly one .deb under target/release/bundle/deb; found ${#debs[@]}"
deb=${debs[0]}
package=$(dpkg-deb --field "$deb" Package)
[[ "$package" =~ ^[a-z0-9][a-z0-9+.-]*$ ]] || fail "unexpected Debian package name: $package"

if dpkg-query -W -f='${db:Status-Abbrev}' "$package" >/dev/null 2>&1; then
  fail "package $package already has a dpkg database entry; refusing to touch an existing install"
fi

if (( EUID != 0 )); then
  command -v sudo >/dev/null 2>&1 || fail "sudo is required to install and purge the package"
  sudo -n true || fail "sudo must be available without an interactive password"
fi

run_privileged() {
  if (( EUID == 0 )); then
    "$@"
  else
    sudo -n "$@"
  fi
}

install_attempted=0
cleanup() {
  local status=$?
  if (( install_attempted )); then
    if ! run_privileged dpkg --purge "$package"; then
      printf '✗ cleanup could not purge test package %s\n' "$package" >&2
      status=1
    fi
  fi
  exit "$status"
}
trap cleanup EXIT

install_attempted=1
run_privileged dpkg --install "$deb"
dpkg-query -s "$package" | grep -Fxq 'Status: install ok installed' || fail "$package did not reach the installed state"

package_files=$(dpkg-query -L "$package")
mapfile -t agent_paths < <(printf '%s\n' "$package_files" | awk '$0 ~ /\/python-agent\/yukinal-agent\/yukinal-agent$/')
[[ ${#agent_paths[@]} -eq 1 ]] || fail "expected exactly one installed frozen Python Agent in $package"
agent_path=${agent_paths[0]}
[[ -x "$agent_path" ]] || fail "installed Python Agent is not executable: $agent_path"
[[ "$agent_path" == */python-agent/yukinal-agent/yukinal-agent ]] || fail "unexpected installed Python Agent path: $agent_path"
resource_root=${agent_path%/python-agent/yukinal-agent/yukinal-agent}

for required_path in \
  "$agent_path" \
  "$resource_root/NOTICE"; do
  [[ -f "$required_path" ]] || fail "installed package resource is missing: $required_path"
done

node scripts/smoke-installed-agent.mjs "$resource_root"

run_privileged dpkg --purge "$package"
if dpkg-query -W -f='${db:Status-Abbrev}' "$package" >/dev/null 2>&1; then
  fail "$package still has a dpkg database entry after purge"
fi
install_attempted=0
[[ ! -e "$agent_path" ]] || fail "installed Python Agent still exists after purge: $agent_path"
printf '✓ Debian install, frozen Python Agent handshake, and purge smoke: green\n'
