/**
 * Small launch shims for the frozen desktop payload. The host intentionally
 * does not discover sibling files implicitly: the install layout is explicit
 * and can be verified from the release manifest.
 */

export function renderWindowsLauncher() {
  return [
    "@echo off",
    "setlocal",
    'set "YUKINAL_AGENT_ENTRY=%~dp0python-agent\\yukinal-agent.exe"',
    'set "YUKINAL_SSH_SERVICE_BINARY=%~dp0ssh-service\\yukinal-ssh-service.exe"',
    '"%~dp0yukinal-desktop-host.exe" --ui-dir "%~dp0ui" %*',
    "",
  ].join("\r\n");
}

export function renderUnixLauncher() {
  return [
    "#!/bin/sh",
    "set -eu",
    'APP_DIR=$(CDPATH= cd -P "$(dirname "$0")" && pwd)',
    'export YUKINAL_AGENT_ENTRY="$APP_DIR/python-agent/yukinal-agent"',
    'export YUKINAL_SSH_SERVICE_BINARY="$APP_DIR/ssh-service/yukinal-ssh-service"',
    'exec "$APP_DIR/yukinal-desktop-host" --ui-dir "$APP_DIR/ui" "$@"',
    "",
  ].join("\n");
}
