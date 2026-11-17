from __future__ import annotations

import argparse
import base64
import json
import os
import queue
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import uuid
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "agent" / "src"))

from yukinal_ssh import SshServiceClient


_DISK_USAGE_HEADER = [
    "Filesystem", "1024-blocks", "Used", "Available", "Capacity", "Mounted", "on"
]


def is_root_disk_usage_output(output: str) -> bool:
    lines = output.splitlines()
    if len(lines) != 2 or lines[0].split() != _DISK_USAGE_HEADER:
        return False
    fields = lines[1].split()
    return (
        len(fields) == 6
        and bool(fields[0])
        and all(re.fullmatch(r"[0-9]+", value) for value in fields[1:4])
        and re.fullmatch(r"[0-9]{1,3}%", fields[4]) is not None
        and fields[5] == "/"
    )


class OpenSshContainerHarnessTests(unittest.TestCase):
    def test_root_disk_usage_accepts_overlay_mounts(self) -> None:
        output = (
            "Filesystem     1024-blocks     Used Available Capacity Mounted on\n"
            "overlay          151263856 63287860  87959612      42% /\n"
        )
        self.assertTrue(is_root_disk_usage_output(output))

    def test_root_disk_usage_rejects_bad_columns_and_non_root_mounts(self) -> None:
        header = "Filesystem 1024-blocks Used Available Capacity Mounted on\n"
        for output in (
            header + "/dev/root x 250 750 25% /\n",
            header + "/dev/root 1000 250 750 25% /home\n",
            header + "/dev/root 1000 250 750 25%\n",
            "Filesystem Size Used Available Use% Mounted on\n/dev/root 1G 250M 750M 25% /\n",
        ):
            with self.subTest(output=output):
                self.assertFalse(is_root_disk_usage_output(output))


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


def api_post(base_url: str, token: str, path: str, payload: dict[str, object]) -> dict[str, object]:
    request = Request(base_url + path, data=json.dumps(payload).encode("utf-8"), method="POST",
                      headers={"Content-Type": "application/json", "Authorization": "Bearer " + token})
    try:
        response = urlopen(request, timeout=15)
    except HTTPError as error:
        response = error
    with response:
        return json.load(response)


def exercise_execution_leases(base_url: str, token: str) -> None:
    payload: dict[str, object] = {"target_id": "test-openssh", "operation": "service_status",
                                 "parameters": {"service": "nginx.service"}, "timeout_seconds": 10}
    missing = api_post(base_url, token, "/v1/ssh/exec", payload)
    if missing.get("error", {}).get("code") != "execution_required":
        raise RuntimeError("the real SSH service accepted an operation without an execution lease")
    execution_id = uuid.uuid4().hex
    api_post(base_url, token, "/v1/ssh/cancel", {"execution_id": execution_id})
    payload["execution"] = {"id": execution_id, "expires_at_ms": time.time_ns() // 1_000_000 + 10_000}
    cancelled = api_post(base_url, token, "/v1/ssh/exec", payload)
    if cancelled.get("error", {}).get("code") != "execution_cancelled":
        raise RuntimeError("a cancelled execution lease was accepted")
    payload["execution"] = {"id": uuid.uuid4().hex, "expires_at_ms": time.time_ns() // 1_000_000 - 1}
    expired = api_post(base_url, token, "/v1/ssh/exec", payload)
    if expired.get("error", {}).get("code") != "execution_expired":
        raise RuntimeError("an expired execution lease was accepted")
    payload["execution"] = {"id": uuid.uuid4().hex, "expires_at_ms": time.time_ns() // 1_000_000 + 10_000}
    if api_post(base_url, token, "/v1/ssh/exec", payload).get("status") != "succeeded":
        raise RuntimeError("a fresh execution lease failed against real OpenSSH")
    if api_post(base_url, token, "/v1/ssh/exec", payload).get("error", {}).get("code") != "execution_cancelled":
        raise RuntimeError("a consumed execution lease was replayed")


def exercise_real_terminal(service_binary: str, root: Path, target: dict[str, object],
                           environment: dict[str, str]) -> None:
    config = root / "terminal.json"
    config.write_text(json.dumps({"listen_port": 8765, "api_token_env": "YUKINAL_TEST_API_TOKEN",
                                  "targets": [target]}), encoding="utf-8")
    process = subprocess.Popen([service_binary, "--config", str(config), "--terminal", "--cols", "80", "--rows", "24"],
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                               env=environment, text=True, encoding="utf-8")
    assert process.stdin is not None and process.stdout is not None
    events: queue.Queue[dict[str, object]] = queue.Queue()

    def read_events() -> None:
        assert process.stdout is not None
        for line in process.stdout:
            try:
                events.put(json.loads(line))
            except json.JSONDecodeError:
                events.put({"type": "error", "message": "invalid worker JSON"})

    reader = threading.Thread(target=read_events, daemon=True)
    reader.start()
    output = bytearray()

    def next_event() -> dict[str, object]:
        try:
            return events.get(timeout=10)
        except queue.Empty as error:
            raise RuntimeError("the real OpenSSH terminal did not respond") from error

    def send(payload: dict[str, object]) -> None:
        assert process.stdin is not None
        process.stdin.write(json.dumps(payload) + "\n")
        process.stdin.flush()

    def wait_output(marker: bytes) -> None:
        while marker not in output:
            event = next_event()
            if event.get("type") != "output":
                raise RuntimeError(f"unexpected real terminal event: {event.get('type')}")
            output.extend(base64.b64decode(str(event["data"]), validate=True))

    try:
        if next_event().get("type") != "ready":
            raise RuntimeError("the real OpenSSH terminal did not start")
        send({"type": "write", "source": "user_pty", "data": "stty -echo; printf '__ENV__%s|%s__\\n' \"$TERM\" \"$COLORTERM\"\n"})
        wait_output(b"__ENV__xterm-256color|truecolor__")
        send({"type": "resize", "columns": 121, "rows": 41})
        send({"type": "write", "source": "user_pty", "data": "printf '__SIZE__'; stty size\n"})
        wait_output(b"__SIZE__41 121")
        send({"type": "write", "source": "user_pty", "data":
              "printf '\\033[31mred\\033[0m|\\033[38;5;202m256\\033[0m|\\033[38;2;1;2;3mrgb\\033[0m|\\033[2J\\033]8;;https://evil.invalid\\007link\\033]8;;\\007__COLOUR_DONE__\\n'\n"})
        wait_output(b"__COLOUR_DONE__")
        for expected in (b"\x1b[31mred\x1b[0m", b"\x1b[38;5;202m256\x1b[0m", b"\x1b[38;2;1;2;3mrgb\x1b[0m"):
            if expected not in output:
                raise RuntimeError("the real OpenSSH terminal lost a supported SGR colour")
        if b"\x1b[2J" in output or b"\x1b]" in output or b"https://evil.invalid" in output:
            raise RuntimeError("the real OpenSSH terminal exposed a non-SGR control sequence")
        for command, marker in (
            ("ls --color=always -- /home/yukinal", "__LS_DONE__"),
            ("git -C /home/yukinal/colour-repo -c color.status=always status --short", "__STATUS_DONE__"),
            ("git -C /home/yukinal/colour-repo --no-pager diff --color=always", "__DIFF_DONE__"),
        ):
            start = len(output)
            send({"type": "write", "source": "user_pty", "data": command + "; printf '" + marker + "\\n'\n"})
            wait_output(marker.encode("ascii"))
            if b"\x1b[" not in output[start:]:
                raise RuntimeError("the real OpenSSH terminal lost colour from " + command.split()[0])
        send({"type": "write", "source": "agent_tool", "data": "touch /home/yukinal/unauthorized-ai-pty\n"})
        while True:
            event = next_event()
            if event.get("type") == "output":
                continue
            if event.get("type") != "error":
                raise RuntimeError("the real OpenSSH terminal accepted a second AI stdin write")
            break
        if next_event().get("type") != "closed" or process.wait(timeout=5) == 0:
            raise RuntimeError("the real OpenSSH terminal did not close after forbidden AI stdin")
    finally:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)
        process.stdin.close()
        process.stdout.close()
        reader.join(timeout=2)


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
            audit_path = root / "ssh-stream.jsonl"
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
            run(["docker", "exec", container, "ln", "-s", "/etc", "/home/yukinal/outside-root"])
            quoted_workdir = "/home/yukinal/literal ; `touch INJECTION` ' directory"
            run(["docker", "exec", "-u", "yukinal", container, "mkdir", "--", quoted_workdir])
            run(["docker", "exec", "-u", "yukinal", container, "sh", "-c",
                 "mkdir /home/yukinal/colour-repo && cd /home/yukinal/colour-repo && git init -q && "
                 "printf 'before\\n' > sample.txt && git add sample.txt && "
                 "git -c user.name=Yukinal -c user.email=test@example.invalid commit -qm baseline && "
                 "printf 'after\\n' > sample.txt"])
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
            target = {
                "target_id": "test-openssh", "host": "127.0.0.1", "port": ssh_port,
                "username": "yukinal", "known_hosts_file": str(known_hosts),
                "auth": {"type": "private_key", "private_key_file": str(private_key)},
                "stream_audit_file": str(audit_path),
            }
            config_path.write_text(json.dumps({
                "listen_port": service_port,
                "api_token_env": "YUKINAL_TEST_API_TOKEN",
                "targets": [target, {**target, "target_id": "test-literal-exec", "allow_server_exec": True}, {
                    "target_id": "test-bad-auth",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "yukinal",
                    "known_hosts_file": str(known_hosts),
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_BAD_PASSWORD"},
                    "stream_audit_file": str(audit_path),
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
            exercise_execution_leases(f"http://127.0.0.1:{service_port}", token)
            exercise_real_terminal(args.service_binary, root, target, environment)
            run(["docker", "exec", container, "test", "!", "-e", "/home/yukinal/unauthorized-ai-pty"])
            literal_env = "literal ; `touch INJECTION` ' # value"
            literal_result = client.server_exec(
                "test-literal-exec", 'printf "%s\\n%s\\n" "$PWD" "$LANG"',
                timeout_ms=10_000, max_output_bytes=4096, workdir=quoted_workdir,
                env={"LANG": literal_env},
            )
            if literal_result.stdout != quoted_workdir + "\n" + literal_env + "\n":
                raise RuntimeError("OpenSSH command construction changed literal workdir/environment arguments")
            run(["docker", "exec", container, "test", "!", "-e", "/home/yukinal/INJECTION"])
            run(["docker", "exec", container, "test", "!", "-e", quoted_workdir + "/INJECTION"])
            result = client.service_status("test-openssh", "nginx.service", timeout_seconds=10)
            if result.exit_code != 0 or result.stdout != "active\n":
                raise RuntimeError(f"unexpected service status result: exit={result.exit_code}, stdout={result.stdout!r}")

            disk_usage = client.disk_usage("test-openssh", timeout_seconds=10)
            if disk_usage.exit_code != 0 or not is_root_disk_usage_output(disk_usage.stdout):
                raise RuntimeError(f"unexpected root disk usage result: {disk_usage.stdout!r}")

            transfer_id = uuid.uuid4().hex
            staging_path = f"/home/yukinal/.yukinal-{transfer_id}.part"
            target_path = f"/home/yukinal/yukinal-{transfer_id}.txt"
            collision_staging_path = f"/home/yukinal/.yukinal-{transfer_id}.collision.part"
            payload = b"OpenSSH hardlink no-replace integration\x00payload"
            # Missing and existing paths outside the configured home have the
            # same policy error: the service must not expose an existence oracle.
            denied_paths = ("/etc/passwd", "/etc/yukinal-does-not-exist", "/home/yukinal/outside-root/passwd")
            denied_messages = set()
            for denied_path in denied_paths:
                try:
                    client.sftp_read("test-openssh", denied_path, 128, timeout_seconds=10)
                except Exception as error:
                    if getattr(error, "code", None) != "denied_by_policy":
                        raise RuntimeError("an outside-root path leaked a different failure") from error
                    denied_messages.add(str(error))
                else:
                    raise RuntimeError("an outside-root or symlink-escaped path was readable")
            if len(denied_messages) != 1:
                raise RuntimeError("outside-root failures exposed path existence information")
            for invalid_path in ("/home/yukinal/../etc/passwd", "/home/yukinal/newline\nfile"):
                try:
                    client.sftp_read("test-openssh", invalid_path, 128, timeout_seconds=10)
                except ValueError:
                    pass
                else:
                    raise RuntimeError("a traversal/control path was not rejected before transport")
            for forbidden_write in ("/etc/yukinal-must-not-exist", "/home/yukinal/outside-root/yukinal-must-not-exist"):
                try:
                    client.sftp_create_exclusive("test-openssh", forbidden_write, b"denied", timeout_seconds=10)
                except Exception as error:
                    if getattr(error, "code", None) != "denied_by_policy":
                        raise RuntimeError("an outside-root write leaked a different failure") from error
                else:
                    raise RuntimeError("an outside-root or symlink-escaped path was writable")
            literal_path = "/home/yukinal/literal ; `touch INJECTION` ' file"
            client.sftp_create_exclusive("test-openssh", literal_path, b"literal path", timeout_seconds=10)
            if client.sftp_read("test-openssh", literal_path, 32, timeout_seconds=10) != b"literal path":
                raise RuntimeError("SFTP did not preserve a metacharacter filename")
            listing = client.sftp_list("test-openssh", "/home/yukinal", timeout_seconds=10)
            if not any(entry["name"] == literal_path.rsplit("/", 1)[1] for entry in listing):
                raise RuntimeError("the canonical SFTP directory listing lost the literal filename")
            client.sftp_remove_file("test-openssh", literal_path, timeout_seconds=10)
            client.sftp_create_exclusive("test-openssh", staging_path, payload, timeout_seconds=10)
            published = client.sftp_publish_no_replace(
                "test-openssh", staging_path, target_path, timeout_seconds=10
            )
            if published.stdout != '{"published":true}' or client.sftp_read(
                "test-openssh", target_path, len(payload), timeout_seconds=10
            ) != payload:
                raise RuntimeError("OpenSSH hardlink extension did not publish the staged bytes intact")
            client.sftp_create_exclusive(
                "test-openssh", collision_staging_path, b"must not replace", timeout_seconds=10
            )
            try:
                client.sftp_publish_no_replace(
                    "test-openssh", collision_staging_path, target_path, timeout_seconds=10
                )
            except Exception as error:
                if getattr(error, "code", None) != "already_exists":
                    raise RuntimeError(
                        f"wrong no-replace collision result: {getattr(error, 'code', type(error).__name__)}"
                    ) from error
            else:
                raise RuntimeError("OpenSSH hardlink publication replaced an existing destination")
            if client.sftp_read("test-openssh", target_path, len(payload), timeout_seconds=10) != payload:
                raise RuntimeError("a no-replace collision changed the existing destination")
            for remote_path in (staging_path, target_path, collision_staging_path):
                client.sftp_remove_file("test-openssh", remote_path, timeout_seconds=10)

            try:
                client.service_status("test-bad-auth", "nginx.service", timeout_seconds=5)
            except Exception as error:
                if getattr(error, "code", None) != "authentication_failed":
                    raise RuntimeError(f"wrong SSH auth error classification: {getattr(error, 'code', type(error).__name__)}") from error
                if environment["YUKINAL_TEST_BAD_PASSWORD"] in str(error):
                    raise RuntimeError("SSH failure response exposed a credential") from error
            else:
                raise RuntimeError("invalid SSH credentials unexpectedly succeeded")

            timeout_started = time.monotonic()
            try:
                client.service_status("test-openssh", "slow.service", timeout_seconds=1)
            except Exception as error:
                if getattr(error, "code", None) not in {"timeout", "execution_cancelled"}:
                    raise RuntimeError(f"wrong timeout error classification: {getattr(error, 'code', type(error).__name__)}") from error
            else:
                raise RuntimeError("slow SSH operation unexpectedly succeeded within its deadline")
            if time.monotonic() - timeout_started >= 4:
                raise RuntimeError("expired SSH execution waited for the remote five-second command")

            try:
                client.service_status("test-openssh", "large.service", timeout_seconds=10)
            except Exception as error:
                if getattr(error, "code", None) != "output_limit":
                    raise RuntimeError(f"wrong output-limit classification: {getattr(error, 'code', type(error).__name__)}") from error
            else:
                raise RuntimeError("oversized SSH output unexpectedly succeeded")

            records = [json.loads(line) for line in audit_path.read_text(encoding="utf-8").splitlines()]
            if not {"agent_tool", "user_pty"}.issubset({record["source"] for record in records}):
                raise RuntimeError("the SSH stream audit lost the command origin")
            if any("byte_count" not in record or not record.get("session_id") or not record.get("execution_id") for record in records):
                raise RuntimeError("the SSH stream audit lost byte provenance")
            for record in records:
                if record["source"] == "user_pty" and record["kind"] == "pty_stdin":
                    if "sha256" in record:
                        raise RuntimeError("PTY key digests expose low-entropy interactive passwords")
                elif len(record.get("sha256", "")) != 64:
                    raise RuntimeError("the SSH stream audit lost its non-interactive digest")
            audit_text = audit_path.read_text(encoding="utf-8")
            if literal_env in audit_text or environment["YUKINAL_TEST_BAD_PASSWORD"] in audit_text:
                raise RuntimeError("the SSH stream audit recorded plaintext input or credentials")

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
            print("PASS SSH leases, literal arguments, SFTP roots/symlinks/no-replace, PTY source/colours/resize, audit, auth, timeout, and host keys")
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
