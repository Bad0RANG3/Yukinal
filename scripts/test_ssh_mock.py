from __future__ import annotations

import argparse
import base64
import json
import logging
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "agent" / "src"))

try:
    import paramiko
except ImportError as error:
    raise SystemExit("install the optional test dependency with: python -m pip install -e 'agent[test]'") from error

from yukinal_ssh import SshServiceClient, SshServiceError


TOKEN = "paramiko-live-ssh-test-token-long-enough-for-service"
PASSWORD = "test-only-password-for-local-ssh-fixture"
BAD_PASSWORD = "intentionally-wrong-local-password"


def unused_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


class ExecServer(paramiko.ServerInterface):
    def __init__(self) -> None:
        self.command: bytes | None = None
        self.command_ready = threading.Event()

    def get_allowed_auths(self, username: str) -> str:
        return "password"

    def check_auth_password(self, username: str, password: str) -> int:
        if username == "test-user" and password == PASSWORD:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_channel_request(self, kind: str, chanid: int) -> int:
        if kind == "session":
            return paramiko.OPEN_SUCCEEDED
        return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_exec_request(self, channel: paramiko.Channel, command: bytes) -> bool:
        self.command = command
        self.command_ready.set()
        return True


def start_ssh_mock(host_key: paramiko.RSAKey) -> tuple[socket.socket, threading.Event]:
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(8)
    listener.settimeout(0.2)
    stopping = threading.Event()

    def handle_connection(connection: socket.socket) -> None:
        transport: paramiko.Transport | None = None
        try:
            transport = paramiko.Transport(connection)
            transport.add_server_key(host_key)
            server = ExecServer()
            transport.start_server(server=server)
            channel = transport.accept(10)
            if channel is None or not server.command_ready.wait(5):
                return
            command = server.command or b""
            if command == b"systemctl is-active -- nginx.service":
                channel.sendall(b"active\n")
                channel.send_exit_status(0)
            elif command == b"systemctl is-active -- slow.service":
                time.sleep(3)
                channel.sendall(b"active\n")
                channel.send_exit_status(0)
            elif command == b"systemctl is-active -- large.service":
                channel.sendall(b"A" * 70000)
                channel.send_exit_status(0)
            else:
                channel.sendall(b"inactive\n")
                channel.send_exit_status(3)
            channel.shutdown_write()

            # Keep the transport alive until the client has consumed output or
            # closed its channel after a timeout/output-limit response.
            close_deadline = time.monotonic() + 5
            while not channel.closed and time.monotonic() < close_deadline:
                time.sleep(0.02)
        except (EOFError, OSError, paramiko.SSHException):
            # The C++ service intentionally closes the channel on timeout and
            # output overflow.
            pass
        finally:
            if transport is not None:
                transport.close()
            else:
                connection.close()

    def accept_connections() -> None:
        while not stopping.is_set():
            try:
                connection, _ = listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            threading.Thread(target=handle_connection, args=(connection,), daemon=True).start()

    threading.Thread(target=accept_connections, daemon=True).start()
    return listener, stopping


def expect_error(client: SshServiceClient, target_id: str, service: str,
                 timeout: int, expected_code: str) -> None:
    try:
        client.service_status(target_id, service, timeout)
    except SshServiceError as error:
        if error.code != expected_code:
            raise AssertionError(f"{service}: expected {expected_code}, received {error.code}: {error}") from error
        if BAD_PASSWORD in str(error):
            raise AssertionError("SSH error response exposed a credential") from error
    else:
        raise AssertionError(f"{service}: expected {expected_code}")


def run_test(service_binary: Path) -> None:
    logging.getLogger("paramiko").setLevel(logging.CRITICAL)
    host_key = paramiko.RSAKey.generate(2048)
    wrong_host_key = paramiko.RSAKey.generate(2048)
    listener, stopping = start_ssh_mock(host_key)
    ssh_port = listener.getsockname()[1]

    with tempfile.TemporaryDirectory(prefix="yukinal-paramiko-ssh-") as temp:
        root = Path(temp)
        known_hosts = root / "known_hosts"

        def write_known_host(key: paramiko.RSAKey) -> None:
            known_hosts.write_text(
                f"[127.0.0.1]:{ssh_port} {key.get_name()} "
                f"{base64.b64encode(key.asbytes()).decode()}\n",
                encoding="ascii",
            )

        write_known_host(host_key)
        service_port = unused_port()
        while service_port == ssh_port:
            service_port = unused_port()
        config = {
            "listen_port": service_port,
            "api_token_env": "YUKINAL_TEST_API_TOKEN",
            "targets": [
                {
                    "target_id": "mock-ssh",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "mock-ssh-bad-auth",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_BAD_PASSWORD"},
                },
            ],
        }
        config_path = root / "service.json"
        config_path.write_text(json.dumps(config), encoding="utf-8")
        environment = os.environ.copy()
        environment["YUKINAL_TEST_API_TOKEN"] = TOKEN
        environment["YUKINAL_TEST_SSH_PASSWORD"] = PASSWORD
        environment["YUKINAL_TEST_BAD_PASSWORD"] = BAD_PASSWORD
        process = subprocess.Popen(
            [str(service_binary), "--config", str(config_path)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            env=environment,
        )
        client = SshServiceClient(TOKEN, f"http://127.0.0.1:{service_port}")
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError(f"C++ service exited with code {process.returncode}")
                try:
                    client.health()
                    break
                except SshServiceError:
                    time.sleep(0.05)
            else:
                raise RuntimeError("C++ service did not become healthy")

            result = client.service_status("mock-ssh", "nginx.service", timeout_seconds=5)
            if result.stdout != "active\n" or result.exit_code != 0:
                raise AssertionError(f"unexpected read-only result: {result!r}")

            expect_error(client, "mock-ssh-bad-auth", "nginx.service", 5, "authentication_failed")
            expect_error(client, "mock-ssh", "slow.service", 1, "timeout")
            expect_error(client, "mock-ssh", "large.service", 5, "output_limit")

            known_hosts.write_text("", encoding="ascii")
            expect_error(client, "mock-ssh", "nginx.service", 5, "host_key_unverified")
            write_known_host(wrong_host_key)
            expect_error(client, "mock-ssh", "nginx.service", 5, "host_key_unverified")
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            stopping.set()
            listener.close()


def main() -> int:
    parser = argparse.ArgumentParser(description="Exercise SSH behavior against an in-process Paramiko mock server")
    parser.add_argument("--service-binary", default=os.environ.get("YUKINAL_SSH_SERVICE_BINARY"))
    args = parser.parse_args()
    if not args.service_binary or not Path(args.service_binary).is_file():
        parser.error("pass --service-binary or set YUKINAL_SSH_SERVICE_BINARY to the built executable")
    run_test(Path(args.service_binary).resolve())
    print("PASS SSH success, authentication rejection, timeout, output cap, and unknown/changed host-key rejection")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
