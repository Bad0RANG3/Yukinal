"""Exercise the native-generated POSIX batch against local fixture executables.

No network or target credentials. Requires a POSIX shell (Git Bash on Windows).
"""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path[:0] = [str(ROOT / "agent/src"), str(ROOT / "agent/tests")]
from yukinal_ssh.client import _parse_system_snapshot_output
from yukinal_agent.system_snapshot import normalize_system_snapshot
from test_system_snapshot import _outputs


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--native-tests", required=True)
    parser.add_argument("--shell", default="sh")
    args = parser.parse_args()
    command = subprocess.check_output([args.native_tests, "--snapshot-toolkit-command"], text=True)
    with tempfile.TemporaryDirectory(prefix="yk-toolkit-") as directory:
        folder = Path(directory)
        fakebin = folder / "bin"
        fakebin.mkdir()
        def script(name, text):
            path = fakebin / name
            path.write_text("#!/bin/sh\n" + text + "\n", encoding="utf-8", newline="\n")
            path.chmod(0o755)
        def quote(text):
            return "'" + text.replace("'", "'\"'\"'") + "'"
        def output(text):
            return "printf '%s\\n' " + quote(text)
        fixtures = _outputs()
        script("uname", 'case "$1" in -s) echo Linux;; -r) echo 6.1;; -m) echo x86_64;; esac')
        script("hostname", "echo fixture-host")
        script("nproc", "echo 4")
        script("head", 'if test -f "$YK_FIXTURE/state"; then echo "cpu 120 0 110 830"; else echo ready > "$YK_FIXTURE/state"; echo "cpu 100 0 100 800"; fi')
        script("grep", 'case "$*" in */proc/cpuinfo*) echo "model name: Fixture CPU";; *) /usr/bin/grep "$@";; esac')
        files = {"/etc/os-release": "os_release", "/proc/meminfo": "memory_info", "/proc/uptime": "uptime_info", "/proc/net/dev": "network_info"}
        script("cat", 'case "$1" in\n' + "\n".join(quote(path) + ") " + output(fixtures[key][1]) + ";;" for path, key in files.items()) + '\n/etc/resolv.conf) echo "nameserver 127.0.0.53";; *) exit 1;; esac')
        script("df", output(fixtures["disk_usage"][1]))
        script("ip", "echo '[]'")
        script("docker", 'case "$1" in info) echo info >> "$YK_FIXTURE/docker-calls"; exit 0;; ps) ' + output(fixtures["docker_ps"][1]) + ';; esac')
        script("nvidia-smi", 'case "$YK_GPU_MODE" in hang) sleep 10;; oversized) /usr/bin/head -c 20000 /dev/zero | /usr/bin/tr "\\0" x;; missing) exit 127;; *) echo "0, Fixture GPU, 73, 2048, 16384, 62";; esac')
        script("id", "echo 1000")
        script("sshd", 'echo "permitrootlogin prohibit-password"; echo "passwordauthentication no"; echo "pubkeyauthentication yes"')
        script("ufw", 'echo "Status: active"')
        script("ss", 'echo "tcp LISTEN 0 128 0.0.0.0:22 0.0.0.0:*"')
        batch = folder / "batch.sh"
        # The batch is exactly what the native SSH operation transmits.
        batch.write_text('export PATH="$YK_FAKEBIN:/usr/bin:/bin"\n' + command, encoding="utf-8", newline="\n")
        def posix(path):
            value = path.as_posix()
            return "/" + value[0].lower() + value[2:] if os.name == "nt" else value
        env = os.environ | {"YK_FIXTURE": posix(folder), "YK_FAKEBIN": posix(fakebin)}
        report = []
        for mode in ("ready", "hang", "oversized", "missing"):
            (folder / "state").unlink(missing_ok=True)
            (folder / "docker-calls").unlink(missing_ok=True)
            started = time.monotonic()
            result = subprocess.run([args.shell, str(batch)], env=env | {"YK_GPU_MODE": mode}, capture_output=True, text=True, timeout=15, check=True)
            elapsed = time.monotonic() - started
            outputs = _parse_system_snapshot_output(result.stdout, "a" * 32)
            snapshot = normalize_system_snapshot(outputs)
            assert len(result.stdout.encode()) < 65536
            assert (folder / "docker-calls").read_text().splitlines() == ["info"], "Docker info must run once"
            assert snapshot["cpu"]["usagePercent"] == 50
            assert snapshot["docker"]["containers"][0]["name"] == "api", "Docker status must survive deadline subshells"
            assert snapshot["security"]["checks"][0]["status"] == "info"
            if mode == "ready":
                assert snapshot["gpu"]["devices"][0]["usagePercent"] == 73
            elif mode == "hang":
                assert outputs["gpu_nvidia"][0] in (124, 137)
                assert snapshot["gpu"]["status"] == "error"
                assert elapsed < 8, "one stuck optional module must not consume the whole SSH deadline"
            elif mode == "oversized":
                assert outputs["gpu_nvidia"] == (125, "")
            else:
                assert snapshot["gpu"]["status"] == "unavailable"
            report.append({"mode": mode, "seconds": round(elapsed, 3), "gpuStatus": snapshot["gpu"]["status"], "dockerInfoCalls": 1})
        print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
