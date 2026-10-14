from __future__ import annotations

import argparse
import asyncio
import base64
import hashlib
import io
import json
import logging
import os
import queue
import socket
import stat
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
    from paramiko import sftp as paramiko_sftp
except ImportError as error:
    raise SystemExit("install the optional test dependency with: python -m pip install -e 'agent[test]'") from error

from yukinal_ssh import SshServiceClient, SshServiceError


TOKEN = "paramiko-live-ssh-test-token-long-enough-for-service"
PASSWORD = "test-only-password-for-local-ssh-fixture"
BAD_PASSWORD = "intentionally-wrong-local-password"
EXEC_COMMANDS: list[bytes] = []
AUTH_EVENTS: list[str] = []
TERMINAL_DIMENSIONS: list[tuple[int, int]] = []


def unused_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


class ExecServer(paramiko.ServerInterface):
    def __init__(self, public_key: paramiko.PKey) -> None:
        self.command: bytes | None = None
        self.command_ready = threading.Event()
        self.terminal_ready = threading.Event()
        self.subsystem_ready = threading.Event()
        self.public_key = public_key

    def get_allowed_auths(self, username: str) -> str:
        return "password,publickey"

    def check_auth_password(self, username: str, password: str) -> int:
        AUTH_EVENTS.append("password")
        if username == "test-user" and password == PASSWORD:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_auth_publickey(self, username: str, key: paramiko.PKey) -> int:
        AUTH_EVENTS.append("publickey")
        if username == "test-user" and key.asbytes() == self.public_key.asbytes():
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_channel_request(self, kind: str, chanid: int) -> int:
        if kind == "session":
            return paramiko.OPEN_SUCCEEDED
        return paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_exec_request(self, channel: paramiko.Channel, command: bytes) -> bool:
        self.command = command
        EXEC_COMMANDS.append(command)
        self.command_ready.set()
        return True

    def check_channel_pty_request(
        self,
        channel: paramiko.Channel,
        term: bytes,
        width: int,
        height: int,
        pixelwidth: int,
        pixelheight: int,
        modes: bytes,
    ) -> bool:
        TERMINAL_DIMENSIONS.append((width, height))
        return term == b"xterm-256color"

    def check_channel_shell_request(self, channel: paramiko.Channel) -> bool:
        self.terminal_ready.set()
        return True

    def check_channel_window_change_request(
        self,
        channel: paramiko.Channel,
        width: int,
        height: int,
        pixelwidth: int,
        pixelheight: int,
    ) -> bool:
        TERMINAL_DIMENSIONS.append((width, height))
        return True

    def check_channel_subsystem_request(self, channel: paramiko.Channel, name: str) -> bool:
        self.subsystem_ready.set()
        return super().check_channel_subsystem_request(channel, name)


class StaticSftpInterface(paramiko.SFTPServerInterface):
    CONTENTS = b"alpha\x00beta\n"
    LARGE_CONTENTS = b"Z" * (1024 * 1024 + 32)
    _files: dict[str, bytes] = {
        "/settings.toml": CONTENTS,
        "/large.bin": LARGE_CONTENTS,
    }
    _files_lock = threading.Lock()

    def list_folder(self, path: str) -> list[paramiko.SFTPAttributes]:
        directory = paramiko.SFTPAttributes()
        directory.filename = "configs"
        directory.st_mode = stat.S_IFDIR | 0o755
        directory.st_size = 0
        file = paramiko.SFTPAttributes()
        file.filename = "settings.toml"
        file.st_mode = stat.S_IFREG | 0o640
        file.st_size = len(self._files["/settings.toml"])
        file.st_atime = 123456789
        file.st_mtime = 123456789
        return [directory, file]

    def stat(self, path: str) -> paramiko.SFTPAttributes:
        attributes = paramiko.SFTPAttributes()
        if path == "/":
            attributes.st_mode = stat.S_IFDIR | 0o755
            attributes.st_size = 0
            attributes.st_nlink = 1
        elif path in self._files:
            attributes.st_mode = stat.S_IFREG | 0o640
            attributes.st_size = len(self._files[path])
            attributes.st_atime = 123456789
            attributes.st_mtime = 123456789
            attributes.st_nlink = 1
        else:
            return paramiko.SFTP_NO_SUCH_FILE
        return attributes

    def lstat(self, path: str) -> paramiko.SFTPAttributes:
        return self.stat(path)

    def open(self, path: str, flags: int, attr: paramiko.SFTPAttributes) -> paramiko.SFTPHandle:
        with self._files_lock:
            exists = path in self._files
            if path not in {"/settings.toml", "/large.bin", "/write.bin", "/backup.bin"}:
                return paramiko.SFTP_PERMISSION_DENIED
            if flags & (os.O_WRONLY | os.O_RDWR):
                if flags & os.O_EXCL and exists:
                    return paramiko.SFTP_PERMISSION_DENIED
                if not exists and not flags & os.O_CREAT:
                    return paramiko.SFTP_NO_SUCH_FILE
                initial = b"" if flags & os.O_TRUNC else self._files.get(path, b"")
                handle = MemorySftpHandle(flags)
                handle.writefile = io.BytesIO(initial)

                def save(content: bytes) -> None:
                    with self._files_lock:
                        self._files[path] = content

                handle.on_close = save
                return handle
            if not exists:
                return paramiko.SFTP_NO_SUCH_FILE
            handle = MemorySftpHandle(flags)
            handle.readfile = io.BytesIO(self._files[path])
            return handle

    def remove(self, path: str) -> int:
        with self._files_lock:
            if path not in self._files:
                return paramiko.SFTP_NO_SUCH_FILE
            del self._files[path]
        return paramiko.SFTP_OK


class MemorySftpHandle(paramiko.SFTPHandle):
    def close(self) -> None:
        writefile = getattr(self, "writefile", None)
        on_close = getattr(self, "on_close", None)
        if writefile is not None and on_close is not None:
            on_close(writefile.getvalue())
        super().close()

    def read(self, offset: int, length: int) -> bytes:
        if self.readfile is None:
            return b""
        self.readfile.seek(offset)
        return self.readfile.read(length)


class FsyncSftpServer(paramiko.SFTPServer):
    def _process(self, request_type: int, request_number: int, message: object) -> None:
        if request_type == paramiko_sftp.CMD_EXTENDED:
            extension = message.get_text()
            if extension == "fsync@openssh.com":
                handle_id = message.get_binary()
                handle = self.file_table.get(handle_id)
                if handle is None:
                    self._send_status(request_number, paramiko_sftp.SFTP_FAILURE)
                    return
                writefile = getattr(handle, "writefile", None)
                if writefile is not None:
                    writefile.flush()
                self._send_status(request_number, paramiko_sftp.SFTP_OK)
                return
            self._send_status(request_number, paramiko_sftp.SFTP_OP_UNSUPPORTED)
            return
        super()._process(request_type, request_number, message)


def start_ssh_mock(
    host_key: paramiko.RSAKey, user_key: paramiko.PKey
) -> tuple[socket.socket, threading.Event]:
    StaticSftpInterface._files = {
        "/settings.toml": StaticSftpInterface.CONTENTS,
        "/large.bin": StaticSftpInterface.LARGE_CONTENTS,
    }
    EXEC_COMMANDS.clear()
    AUTH_EVENTS.clear()
    TERMINAL_DIMENSIONS.clear()
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
            transport.set_subsystem_handler("sftp", FsyncSftpServer, StaticSftpInterface)
            server = ExecServer(user_key)
            transport.start_server(server=server)
            channel = transport.accept(10)
            if channel is None:
                return
            request_deadline = time.monotonic() + 5
            while (
                not server.subsystem_ready.is_set()
                and not server.command_ready.is_set()
                and not server.terminal_ready.is_set()
            ):
                if time.monotonic() >= request_deadline:
                    return
                time.sleep(0.01)
            if server.subsystem_ready.is_set():
                close_deadline = time.monotonic() + 5
                while not channel.closed and time.monotonic() < close_deadline:
                    time.sleep(0.02)
                return
            if server.terminal_ready.is_set():
                channel.settimeout(0.2)
                channel.sendall(b"terminal-ready\n")
                while not channel.closed:
                    try:
                        data = channel.recv(4096)
                    except socket.timeout:
                        continue
                    if not data:
                        break
                    channel.sendall(b"echo:" + data)
                return
            command = server.command or b""
            if command == b"systemctl is-active -- nginx.service":
                channel.sendall(b"active\n")
                channel.send_exit_status(0)
            elif b"systemctl list-units --type=service" in command:
                channel.sendall(b"nginx.service loaded active running Nginx Web Server\n\n__YUKINAL_SOURCE__=systemd\n")
                channel.send_exit_status(0)
            elif command == b"systemctl is-active -- slow.service":
                time.sleep(4)
                channel.sendall(b"active\n")
                channel.send_exit_status(0)
            elif command == b"systemctl is-active -- large.service":
                channel.sendall(b"A" * 70000)
                channel.send_exit_status(0)
            elif command.startswith(b"stat -c %h '") and b"' 2>/dev/null || stat -f %l '" in command:
                channel.sendall(b"1\n")
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
                 timeout: int, expected_code: str, *, max_elapsed: float | None = None) -> None:
    started = time.monotonic()
    try:
        client.service_status(target_id, service, timeout)
    except SshServiceError as error:
        if error.code != expected_code:
            raise AssertionError(f"{service}: expected {expected_code}, received {error.code}: {error}") from error
        if BAD_PASSWORD in str(error):
            raise AssertionError("SSH error response exposed a credential") from error
        elapsed = time.monotonic() - started
        if max_elapsed is not None and elapsed > max_elapsed:
            raise AssertionError(
                f"{service}: {expected_code} took {elapsed:.2f}s; limit was {max_elapsed:.2f}s"
            )
    else:
        raise AssertionError(f"{service}: expected {expected_code}")


def exercise_cpp_terminal(
    service_binary: Path,
    root: Path,
    target: dict[str, object],
    environment: dict[str, str],
) -> None:
    terminal_config = root / "terminal-service.json"
    terminal_config.write_text(
        json.dumps({"listen_port": 8765, "api_token_env": "YUKINAL_TEST_API_TOKEN", "targets": [target]}),
        encoding="utf-8",
    )
    process = subprocess.Popen(
        [str(service_binary), "--config", str(terminal_config), "--terminal", "--cols", "80", "--rows", "24"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
        bufsize=1,
        env=environment,
    )
    if process.stdin is None or process.stdout is None:
        process.kill()
        raise RuntimeError("could not open the C++ terminal worker pipes")

    events: queue.Queue[dict[str, object]] = queue.Queue()

    def read_events() -> None:
        assert process.stdout is not None
        for line in process.stdout:
            events.put(json.loads(line))

    reader = threading.Thread(target=read_events, daemon=True)
    reader.start()

    def next_event() -> dict[str, object]:
        try:
            return events.get(timeout=8)
        except queue.Empty as error:
            raise AssertionError("C++ terminal worker did not produce an event") from error

    try:
        ready = next_event()
        if ready.get("type") != "ready":
            raise AssertionError(f"C++ terminal failed to start: {ready!r}")
        output = next_event()
        if output.get("type") != "output" or base64.b64decode(str(output.get("data", ""))) != b"terminal-ready\n":
            raise AssertionError(f"C++ terminal output did not match the PTY banner: {output!r}")

        process.stdin.write(json.dumps({"type": "write", "data": "ping\n"}) + "\n")
        process.stdin.flush()
        echoed = next_event()
        if echoed.get("type") != "output" or base64.b64decode(str(echoed.get("data", ""))) != b"echo:ping\n":
            raise AssertionError(f"C++ terminal did not forward interactive input: {echoed!r}")

        process.stdin.write(json.dumps({"type": "resize", "columns": 100, "rows": 30}) + "\n")
        process.stdin.flush()
        deadline = time.monotonic() + 3
        while (100, 30) not in TERMINAL_DIMENSIONS and time.monotonic() < deadline:
            time.sleep(0.01)
        if (100, 30) not in TERMINAL_DIMENSIONS:
            raise AssertionError(f"C++ terminal did not forward the PTY resize: {TERMINAL_DIMENSIONS!r}")

        process.stdin.write(json.dumps({"type": "close"}) + "\n")
        process.stdin.flush()
        closed = next_event()
        if closed.get("type") != "closed":
            raise AssertionError(f"C++ terminal did not report a clean close: {closed!r}")
        if process.wait(timeout=5) != 0:
            raise AssertionError(f"C++ terminal worker exited with {process.returncode}")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=3)


def exercise_agent_runtime(
    client: SshServiceClient, service_binary: Path, desktop_target: dict[str, object]
) -> None:
    from yukinal_agent import AgentWorkflow, MemoryAuditSink, ToolRegistry, register_ssh_tools
    from yukinal_agent.ssh_tools import register_desktop_ssh_tools

    registry = ToolRegistry(MemoryAuditSink())
    register_ssh_tools(registry, client)
    workflow = AgentWorkflow(registry)

    async def run() -> None:
        succeeded = await workflow.run(
            task_id="agent-ssh-selected-target",
            tenant_id="integration-tenant",
            user_id="integration-operator",
            allowed_tools={"ssh.service_status"},
            allowed_target_ids={"srv_stage1"},
            tool_calls=[{
                "name": "ssh.service_status",
                "arguments": {"service": "nginx.service"},
            }],
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
        )
        if succeeded.status != "succeeded" or not succeeded.results:
            raise AssertionError(f"Agent SSH tool did not succeed: {succeeded!r}")
        result = succeeded.results[0].get("result", {})
        if result.get("target_id") != "srv_stage1" or result.get("stdout") != "active\n":
            raise AssertionError(f"Agent SSH tool did not use the selected server: {result!r}")

        denied = await workflow.run(
            task_id="agent-ssh-disallowed-target",
            tenant_id="integration-tenant",
            user_id="integration-operator",
            allowed_tools={"ssh.service_status"},
            allowed_target_ids={"srv_stage1"},
            tool_calls=[{
                "name": "ssh.service_status",
                "arguments": {"service": "nginx.service"},
            }],
            host_target={"host": "remote", "serverId": "srv_bad_auth", "environment": "staging"},
        )
        if denied.error_code != "target_not_allowed":
            raise AssertionError(f"Agent did not reject a server outside the task allowlist: {denied!r}")

        forged = await workflow.run(
            task_id="agent-ssh-forged-target",
            tenant_id="integration-tenant",
            user_id="integration-operator",
            allowed_tools={"ssh.service_status"},
            allowed_target_ids={"srv_stage1"},
            tool_calls=[{
                "name": "ssh.service_status",
                "arguments": {"target_id": "srv_bad_auth", "service": "nginx.service"},
            }],
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
        )
        if forged.error_code != "invalid_arguments":
            raise AssertionError(f"Agent accepted a model-supplied target override: {forged!r}")

        class DesktopHost:
            async def request(self, method: str, params: dict[str, object], *, timeout_seconds: float) -> object:
                if method != "host.ssh.service_target" or params != {"serverId": "srv_stage1"}:
                    raise AssertionError(f"unexpected desktop SSH target request: {method} {params!r}")
                return desktop_target

        desktop_registry = ToolRegistry(MemoryAuditSink())
        register_desktop_ssh_tools(desktop_registry, DesktopHost(), service_binary)
        desktop_workflow = AgentWorkflow(desktop_registry)
        desktop_result = await desktop_workflow.run(
            task_id="desktop-managed-ssh-target",
            tenant_id="integration-tenant",
            user_id="integration-operator",
            allowed_tools={"ssh.service_status"},
            allowed_target_ids={"srv_stage1"},
            tool_calls=[{
                "name": "ssh.service_status",
                "arguments": {"service": "nginx.service"},
            }],
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
        )
        if desktop_result.status != "succeeded" or not desktop_result.results:
            raise AssertionError(f"desktop-managed C++ SSH path failed: {desktop_result!r}")
        desktop_output = desktop_result.results[0].get("result", {})
        if desktop_output.get("target_id") != "srv_stage1" or desktop_output.get("stdout") != "active\n":
            raise AssertionError(f"desktop-managed C++ SSH path used the wrong target: {desktop_output!r}")

    asyncio.run(run())


def run_test(service_binary: Path, *, agent_runtime_only: bool = False) -> None:
    logging.getLogger("paramiko").setLevel(logging.CRITICAL)
    host_key = paramiko.RSAKey.generate(2048)
    wrong_host_key = paramiko.RSAKey.generate(2048)
    user_key = paramiko.RSAKey.generate(2048)
    listener, stopping = start_ssh_mock(host_key, user_key)
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
        private_key_buffer = io.StringIO()
        user_key.write_private_key(private_key_buffer)
        private_key_pem = private_key_buffer.getvalue()
        pinned_fingerprint = "SHA256:" + base64.b64encode(
            hashlib.sha256(host_key.asbytes()).digest()
        ).decode("ascii").rstrip("=")
        wrong_fingerprint = "SHA256:" + base64.b64encode(
            hashlib.sha256(wrong_host_key.asbytes()).digest()
        ).decode("ascii").rstrip("=")
        service_port = unused_port()
        while service_port == ssh_port:
            service_port = unused_port()
        config = {
            "listen_port": service_port,
            "api_token_env": "YUKINAL_TEST_API_TOKEN",
            "targets": [
                {
                    "target_id": "srv_stage1",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_bad_auth",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_BAD_PASSWORD"},
                },
                {
                    "target_id": "srv_desktop_style",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "host_key_fingerprint": pinned_fingerprint,
                    "auth": {"type": "private_key", "private_key_env": "YK_SSH_TEST_PRIVATE_KEY"},
                },
                {
                    "target_id": "srv_bad_pin",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "host_key_fingerprint": wrong_fingerprint,
                    "auth": {"type": "private_key", "private_key_env": "YK_SSH_TEST_PRIVATE_KEY"},
                },
                {
                    "target_id": "srv_probe_only",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "ssh_probe",
                    "probe_only": True,
                    "auth": {"type": "probe"},
                },
            ],
        }
        config_path = root / "service.json"
        config_path.write_text(json.dumps(config), encoding="utf-8")
        environment = os.environ.copy()
        environment["YUKINAL_TEST_API_TOKEN"] = TOKEN
        environment["YUKINAL_TEST_SSH_PASSWORD"] = PASSWORD
        environment["YUKINAL_TEST_BAD_PASSWORD"] = BAD_PASSWORD
        environment["YK_SSH_TEST_PRIVATE_KEY"] = private_key_pem
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

            probe = client._execute_operation(
                "srv_probe_only", "host_key_probe", {}, timeout_seconds=8
            )
            if probe.stdout != pinned_fingerprint or probe.exit_code != 0:
                raise AssertionError(f"C++ host-key probe returned an unexpected fingerprint: {probe!r}")
            if AUTH_EVENTS:
                raise AssertionError(f"host-key probe attempted SSH authentication: {AUTH_EVENTS!r}")

            desktop_target = {
                "target": {
                    "target_id": "srv_stage1",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "host_key_fingerprint": pinned_fingerprint,
                    "auth": {"type": "private_key", "private_key_env": "YK_SSH_PRIVATE_KEY"},
                },
                "credentials": {"YK_SSH_PRIVATE_KEY": private_key_pem},
            }

            if agent_runtime_only:
                exercise_agent_runtime(client, service_binary, desktop_target)
                return

            inventory = client.service_inventory("srv_stage1", timeout_seconds=5)
            if inventory.operation != "service_inventory" or "__YUKINAL_SOURCE__=systemd" not in inventory.stdout:
                raise AssertionError(f"unexpected read-only service inventory: {inventory!r}")

            result = client.service_status("srv_stage1", "nginx.service", timeout_seconds=5)
            if result.stdout != "active\n" or result.exit_code != 0:
                raise AssertionError(f"unexpected read-only result: {result!r}")

            desktop_style_result = client.service_status(
                "srv_desktop_style", "nginx.service", timeout_seconds=5
            )
            if desktop_style_result.stdout != "active\n" or desktop_style_result.exit_code != 0:
                raise AssertionError(f"desktop keychain/pin path failed: {desktop_style_result!r}")

            listing = client._execute_operation(
                "srv_desktop_style", "sftp_list", {"path": "/"}, timeout_seconds=5
            )
            try:
                listing_entries = json.loads(listing.stdout)
            except json.JSONDecodeError as error:
                raise AssertionError(f"SFTP listing was not JSON: {listing.stdout!r}") from error
            if listing_entries != [
                {"name": "configs", "file_type": "directory", "size": 0},
                {
                    "name": "settings.toml",
                    "file_type": "file",
                    "size": len(StaticSftpInterface.CONTENTS),
                },
            ]:
                raise AssertionError(f"unexpected C++ SFTP listing: {listing_entries!r}")

            stat_result = client._execute_operation(
                "srv_desktop_style", "sftp_stat", {"path": "/settings.toml"}, timeout_seconds=5
            )
            stat_metadata = json.loads(stat_result.stdout)
            if stat_metadata != {
                "kind": "file",
                "size": len(StaticSftpInterface.CONTENTS),
                "modified": 123456789,
            }:
                raise AssertionError(f"unexpected C++ SFTP stat result: {stat_metadata!r}")

            read_result = client._execute_operation(
                "srv_desktop_style",
                "sftp_read",
                {"path": "/settings.toml", "max_bytes": 5},
                timeout_seconds=5,
            )
            if base64.b64decode(read_result.stdout, validate=True) != StaticSftpInterface.CONTENTS[:6]:
                raise AssertionError("C++ SFTP read did not return the bounded binary prefix and sentinel byte")

            maximum_read = client._execute_operation(
                "srv_desktop_style",
                "sftp_read",
                {"path": "/large.bin", "max_bytes": 1024 * 1024},
                timeout_seconds=10,
            )
            maximum_bytes = base64.b64decode(maximum_read.stdout, validate=True)
            if maximum_bytes != StaticSftpInterface.LARGE_CONTENTS[: 1024 * 1024 + 1]:
                raise AssertionError("C++ SFTP read exceeded or missed its one-byte truncation bound")

            write_payload = bytes(range(256)) * 512
            write_result = client.sftp_write("srv_desktop_style", "/write.bin", write_payload, 8)
            if json.loads(write_result.stdout) != {"bytes_written": len(write_payload)}:
                raise AssertionError("C++ SFTP write did not report the complete binary payload")
            written_read = client._execute_operation(
                "srv_desktop_style",
                "sftp_read",
                {"path": "/write.bin", "max_bytes": len(write_payload)},
                timeout_seconds=8,
            )
            if base64.b64decode(written_read.stdout, validate=True) != write_payload:
                raise AssertionError("C++ SFTP write did not preserve binary file contents")
            if client.sftp_link_count("srv_desktop_style", "/settings'.toml", 8) != 1:
                raise AssertionError("C++ SFTP link-count probe did not return the structured count")
            if not any(b"'/settings'\\''.toml'" in command for command in EXEC_COMMANDS):
                raise AssertionError("C++ SFTP link-count probe did not safely quote a single quote")

            backup_payload = bytes(range(256)) * 4096
            create_result = client.sftp_create_exclusive(
                "srv_desktop_style", "/backup.bin", backup_payload, 8
            )
            if json.loads(create_result.stdout) != {"bytes_written": len(backup_payload)}:
                raise AssertionError("C++ exclusive SFTP create did not report the complete payload")
            backup_read = client._execute_operation(
                "srv_desktop_style",
                "sftp_read",
                {"path": "/backup.bin", "max_bytes": len(backup_payload)},
                timeout_seconds=8,
            )
            if base64.b64decode(backup_read.stdout, validate=True) != backup_payload:
                raise AssertionError("C++ exclusive SFTP create did not preserve the 1 MiB recovery copy")
            try:
                client.sftp_create_exclusive(
                    "srv_desktop_style", "/backup.bin", b"overwrite", 8
                )
            except SshServiceError as error:
                if error.code != "sftp_error":
                    raise AssertionError(f"exclusive SFTP create returned the wrong error: {error.code}") from error
            else:
                raise AssertionError("C++ exclusive SFTP create replaced an existing backup")
            remove_result = client.sftp_remove_file("srv_desktop_style", "/backup.bin", 8)
            if json.loads(remove_result.stdout) != {"removed": True}:
                raise AssertionError("C++ SFTP file removal was not confirmed")
            expect_error(client, "srv_bad_pin", "nginx.service", 5, "host_key_unverified")

            expect_error(client, "srv_bad_auth", "nginx.service", 5, "authentication_failed")
            expect_error(client, "srv_stage1", "slow.service", 2, "timeout", max_elapsed=3.5)
            expect_error(client, "srv_stage1", "large.service", 5, "output_limit")

            known_hosts.write_text("", encoding="ascii")
            expect_error(client, "srv_stage1", "nginx.service", 5, "host_key_unverified")
            write_known_host(wrong_host_key)
            expect_error(client, "srv_stage1", "nginx.service", 5, "host_key_unverified")

            terminal_target = {
                "target_id": "srv_desktop_style",
                "host": "127.0.0.1",
                "port": ssh_port,
                "username": "test-user",
                "host_key_fingerprint": pinned_fingerprint,
                "auth": {"type": "private_key", "private_key_env": "YK_SSH_TEST_PRIVATE_KEY"},
            }
            exercise_cpp_terminal(service_binary, root, terminal_target, environment)
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
    parser.add_argument(
        "--agent-runtime-only",
        action="store_true",
        help="exercise the selected-target Agent tool path; requires agent[agent] dependencies",
    )
    args = parser.parse_args()
    if not args.service_binary or not Path(args.service_binary).is_file():
        parser.error("pass --service-binary or set YUKINAL_SSH_SERVICE_BINARY to the built executable")
    run_test(Path(args.service_binary).resolve(), agent_runtime_only=args.agent_runtime_only)
    if args.agent_runtime_only:
        print("PASS Agent SSH tool selected-target binding, task target allowlist, and model target override rejection")
    else:
        print("PASS SSH exec, SFTP listing/stat/read/write/exclusive-create/remove/link-count, auth rejection, timeout, output cap, and host-key rejection")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
