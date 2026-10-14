from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "agent" / "src"))

from yukinal_ssh import SshServiceClient


def run(command: list[str], *, capture: bool = False) -> str:
    result = subprocess.run(command, check=True, text=True, capture_output=capture)
    return result.stdout.strip() if capture else ""


def unused_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def wait_healthy(client: SshServiceClient, timeout: float = 10) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if client.health():
                return
        except Exception:
            time.sleep(0.1)
    raise RuntimeError("C++ service did not start")


def main() -> int:
    parser = argparse.ArgumentParser(description="Exercise a real SSH success against an isolated OpenSSH container")
    parser.add_argument("--service-binary", default=os.environ.get("YUKINAL_SSH_SERVICE_BINARY"))
    args = parser.parse_args()
    if not args.service_binary or not Path(args.service_binary).is_file():
        parser.error("pass --service-binary or set YUKINAL_SSH_SERVICE_BINARY to the built executable")
    missing = [name for name in ("docker", "ssh-keygen", "ssh-keyscan") if not shutil.which(name)]
    if missing:
        parser.error("missing required command(s): " + ", ".join(missing))

    docker_context = REPO_ROOT / "native" / "tests" / "ssh-container"
    suffix = uuid.uuid4().hex[:10]
    image = f"yukinal-ssh-test:{suffix}"
    container = f"yukinal-ssh-test-{suffix}"
    process: subprocess.Popen[bytes] | None = None
    token = "ssh-container-test-token-long-enough-for-local-service"
    try:
        with tempfile.TemporaryDirectory(prefix="yukinal-ssh-container-") as temporary:
            root = Path(temporary)
            private_key = root / "id_ed25519"
            public_key = root / "id_ed25519.pub"
            known_hosts = root / "known_hosts"
            config_path = root / "service.json"
            run(["ssh-keygen", "-q", "-t", "rsa", "-b", "3072", "-m", "PEM", "-N", "", "-f", str(private_key)])
            wrong_private_key = root / "wrong_ed25519"
            wrong_public_key = root / "wrong_ed25519.pub"
            run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(wrong_private_key)])
            run(["docker", "build", "-t", image, "-f", str(docker_context / "Dockerfile"), str(docker_context)])
            run(["docker", "run", "-d", "--rm", "--name", container, "-p", "127.0.0.1::22", image])
            mapped = run(["docker", "port", container, "22/tcp"], capture=True)
            match = re.search(r"127\.0\.0\.1:(\d+)", mapped)
            if not match:
                raise RuntimeError("Docker did not report the mapped loopback SSH port")
            ssh_port = int(match.group(1))
            run(["docker", "cp", str(public_key), f"{container}:/tmp/yukinal_authorized_keys"])
            run([
                "docker", "exec", container, "sh", "-c",
                "install -o yukinal -g yukinal -m 0700 -d /home/yukinal/.ssh && "
                "install -o yukinal -g yukinal -m 0600 /tmp/yukinal_authorized_keys "
                "/home/yukinal/.ssh/authorized_keys",
            ])
            scan_output = ""
            scan_deadline = time.monotonic() + 20
            while time.monotonic() < scan_deadline and not scan_output:
                scan = subprocess.run(
                    ["ssh-keyscan", "-T", "5", "-p", str(ssh_port), "127.0.0.1"],
                    check=False,
                    text=True,
                    capture_output=True,
                )
                scan_output = scan.stdout.strip()
                if not scan_output:
                    time.sleep(0.2)
            if not scan_output:
                raise RuntimeError("ssh-keyscan returned no host key for the test container")
            known_hosts.write_text(scan_output + "\n", encoding="utf-8")

            service_port = unused_port()
            while service_port == ssh_port:
                service_port = unused_port()
            config_path.write_text(json.dumps({
                "listen_port": service_port,
                "api_token_env": "YUKINAL_TEST_API_TOKEN",
                "targets": [{
                    "target_id": "test-openssh",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "yukinal",
                    "known_hosts_file": str(known_hosts),
                    "auth": {"type": "private_key", "private_key_file": str(private_key)},
                }, {
                    "target_id": "test-bad-auth",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "yukinal",
                    "known_hosts_file": str(known_hosts),
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_BAD_PASSWORD"},
                }],
            }), encoding="utf-8")
            environment = os.environ.copy()
            environment["YUKINAL_TEST_API_TOKEN"] = token
            environment["YUKINAL_TEST_BAD_PASSWORD"] = "intentionally-wrong-password"
            process = subprocess.Popen(
                [args.service_binary, "--config", str(config_path)],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                env=environment,
            )
            client = SshServiceClient(token, f"http://127.0.0.1:{service_port}")
            wait_healthy(client)
            result = client.service_status("test-openssh", "nginx.service", timeout_seconds=10)
            if result.exit_code != 0 or result.stdout != "active\n":
                raise RuntimeError(f"unexpected service status result: exit={result.exit_code}, stdout={result.stdout!r}")
            try:
                client.service_status("test-bad-auth", "nginx.service", timeout_seconds=5)
            except Exception as error:
                if getattr(error, "code", None) != "authentication_failed":
                    raise RuntimeError(f"wrong SSH auth error classification: {getattr(error, 'code', type(error).__name__)}") from error
                if environment["YUKINAL_TEST_BAD_PASSWORD"] in str(error):
                    raise RuntimeError("SSH failure response exposed a credential") from error
            else:
                raise RuntimeError("invalid SSH credentials unexpectedly succeeded")

            try:
                client.service_status("test-openssh", "slow.service", timeout_seconds=1)
            except Exception as error:
                if getattr(error, "code", None) != "timeout":
                    raise RuntimeError(f"wrong timeout error classification: {getattr(error, 'code', type(error).__name__)}") from error
            else:
                raise RuntimeError("slow SSH operation unexpectedly succeeded within its deadline")

            try:
                client.service_status("test-openssh", "large.service", timeout_seconds=10)
            except Exception as error:
                if getattr(error, "code", None) != "output_limit":
                    raise RuntimeError(f"wrong output-limit classification: {getattr(error, 'code', type(error).__name__)}") from error
            else:
                raise RuntimeError("oversized SSH output unexpectedly succeeded")

            known_hosts.write_text("", encoding="utf-8")
            try:
                client.service_status("test-openssh", "nginx.service", timeout_seconds=5)
            except Exception as error:
                if getattr(error, "code", None) != "host_key_unverified":
                    raise RuntimeError(f"wrong unknown-host-key classification: {getattr(error, 'code', type(error).__name__)}") from error
            else:
                raise RuntimeError("an unknown SSH host key unexpectedly succeeded")

            wrong_key_parts = wrong_public_key.read_text(encoding="utf-8").split()
            known_hosts.write_text(
                f"[127.0.0.1]:{ssh_port} {wrong_key_parts[0]} {wrong_key_parts[1]}\n",
                encoding="utf-8",
            )
            try:
                client.service_status("test-openssh", "nginx.service", timeout_seconds=5)
            except Exception as error:
                if getattr(error, "code", None) != "host_key_unverified":
                    raise RuntimeError(f"wrong changed-host-key classification: {getattr(error, 'code', type(error).__name__)}") from error
            else:
                raise RuntimeError("a changed SSH host key unexpectedly succeeded")
            print("PASS SSH success, authentication failure, timeout, output cap, and unknown/changed host-key rejection")
    finally:
        if process and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        subprocess.run(["docker", "rm", "-f", container], check=False, capture_output=True)
        subprocess.run(["docker", "image", "rm", image], check=False, capture_output=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
