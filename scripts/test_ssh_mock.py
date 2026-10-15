from __future__ import annotations

import argparse
import asyncio
import base64
import datetime
import hashlib
import http.server
import ipaddress
import io
import json
import logging
import os
import queue
import re
import select
import socket
import ssl
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, rsa
from cryptography.hazmat.primitives import hashes
from cryptography import x509
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "agent" / "src"))

try:
    import paramiko
    from paramiko import sftp as paramiko_sftp
except ImportError as error:
    raise SystemExit("install the optional test dependency with: python -m pip install -e 'agent[test]'") from error

from yukinal_ssh import SshServiceClient, SshServiceError
from yukinal_ssh.client import _SYSTEM_SNAPSHOT_COMMAND_IDS


TOKEN = "paramiko-live-ssh-test-token-long-enough-for-service"
PASSWORD = "test-only-password-for-local-ssh-fixture"
BAD_PASSWORD = "intentionally-wrong-local-password"
EXEC_COMMANDS: list[bytes] = []
AUTH_EVENTS: list[str] = []
TERMINAL_DIMENSIONS: list[tuple[int, int]] = []
PROXY_CONNECT_TARGETS: list[str] = []
HTTPS_PROXY_CONNECT_TARGETS: list[str] = []
SOCKS5_CONNECT_TARGETS: list[tuple[str, int, int]] = []


def unused_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


class ExecServer(paramiko.ServerInterface):
    def __init__(self, public_key: paramiko.PKey, keyboard_interactive_only: bool = False) -> None:
        self.command: bytes | None = None
        self.command_ready = threading.Event()
        self.terminal_ready = threading.Event()
        self.subsystem_ready = threading.Event()
        self.public_key = public_key
        self.keyboard_interactive_only = keyboard_interactive_only

    def get_allowed_auths(self, username: str) -> str:
        if self.keyboard_interactive_only:
            return "keyboard-interactive"
        return "password,publickey"

    def check_auth_password(self, username: str, password: str) -> int:
        AUTH_EVENTS.append("password")
        if self.keyboard_interactive_only:
            return paramiko.AUTH_FAILED
        if username == "test-user" and password == PASSWORD:
            return paramiko.AUTH_SUCCESSFUL
        return paramiko.AUTH_FAILED

    def check_auth_interactive(self, username: str, submethods: str) -> paramiko.InteractiveQuery:
        AUTH_EVENTS.append("keyboard-interactive-request")
        if username != "test-user" or not self.keyboard_interactive_only:
            return paramiko.AUTH_FAILED
        return paramiko.InteractiveQuery(
            "Second factor",
            "Enter the one-time code",
            ("One-time code: ", False),
        )

    def check_auth_interactive_response(self, responses: list[str]) -> int:
        AUTH_EVENTS.append("keyboard-interactive-response")
        if self.keyboard_interactive_only and responses == ["654321"]:
            AUTH_EVENTS.append("keyboard-interactive-success")
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
    _directories: set[str] = {"/"}
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
        elif path in self._directories:
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
            if path not in {"/settings.toml", "/large.bin", "/write.bin", "/backup.bin", "/backup-published.bin", "/transfer-stream.bin", "/stream-failure.bin"}:
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

    def mkdir(self, path: str, attr: paramiko.SFTPAttributes) -> int:
        if path != "/created-by-cpp":
            return paramiko.SFTP_PERMISSION_DENIED
        if path in self._directories:
            return paramiko.SFTP_FAILURE
        self._directories.add(path)
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
    advertise_hardlink = True

    def _send_server_version(self) -> int:
        request_type, data = self._read_packet()
        if request_type != paramiko_sftp.CMD_INIT:
            raise paramiko.SFTPError("incompatible SFTP protocol")
        version = struct.unpack(">I", data[:4])[0]
        message = paramiko.Message()
        message.add_int(paramiko_sftp._VERSION)
        if self.advertise_hardlink:
            message.add("hardlink@openssh.com", "1")
        message.add("check-file", "md5,sha1")
        self._send_packet(paramiko_sftp.CMD_VERSION, message)
        return version

    def _process(self, request_type: int, request_number: int, message: object) -> None:
        if request_type == paramiko_sftp.CMD_EXTENDED:
            extension = message.get_text()
            if extension == "hardlink@openssh.com":
                source = message.get_text()
                destination = message.get_text()
                with StaticSftpInterface._files_lock:
                    if destination in StaticSftpInterface._files or destination in StaticSftpInterface._directories:
                        self._send_status(request_number, 11, "File already exists")
                    elif source not in StaticSftpInterface._files:
                        self._send_status(request_number, paramiko_sftp.SFTP_NO_SUCH_FILE)
                    else:
                        StaticSftpInterface._files[destination] = StaticSftpInterface._files[source]
                        self._send_status(request_number, paramiko_sftp.SFTP_OK)
                return
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
    host_key: paramiko.PKey,
    user_key: paramiko.PKey,
    *,
    keyboard_interactive_only: bool = False,
) -> tuple[socket.socket, threading.Event]:
    StaticSftpInterface._files = {
        "/settings.toml": StaticSftpInterface.CONTENTS,
        "/large.bin": StaticSftpInterface.LARGE_CONTENTS,
    }
    StaticSftpInterface._directories = {"/"}
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
            server = ExecServer(user_key, keyboard_interactive_only)
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
            snapshot_match = re.search(rb"__YK_SNAPSHOT_([0-9a-f]{32})_os_detect_BEGIN__", command)
            if snapshot_match:
                nonce = snapshot_match.group(1).decode("ascii")
                snapshot_values = {
                    "os_detect": b"Linux\n",
                    "os_release": b'NAME="Test Linux"\nVERSION_ID="1"\n',
                    "os_kernel": b"6.1.0\n",
                    "os_arch": b"x86_64\n",
                    "os_hostname": b"mock-host\n",
                    "cpu_detect": b"ok\n",
                    "cpu_ticks_a": b"cpu 100 0 100 800\n",
                    "cpu_wait": b"",
                    "cpu_ticks_b": b"cpu 120 0 110 830\n",
                    "cpu_cores": b"4\n",
                    "cpu_model": b" Mock CPU\n",
                    "cpu_load": b"0.10 0.20 0.30 1/20 99\n",
                    "memory_detect": b"ok\n",
                    "memory_info": b"MemTotal: 1024 kB\nMemAvailable: 512 kB\n",
                    "uptime_detect": b"ok\n",
                    "uptime_info": b"120.0 100.0\n",
                    "disk_detect": b"ok\n",
                    "disk_usage": b"Filesystem 1B-blocks Used Available Use% Mounted on\n/dev/vda1 1000 250 750 25% /\n",
                    "network_detect": b"ok\n",
                    "network_info": b"Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n eth0: 100 1 0 0 0 0 0 0 200 2 0 0 0 0 0 0\n",
                    "docker_detect": b"0\n",
                    "docker_info": b"",
                    "docker_ps": b'{"Names":"api","Image":"example/api","State":"running","Status":"Up","RestartCount":0}\n',
                }
                response = bytearray()
                for operation, value in snapshot_values.items():
                    response.extend(f"__YK_SNAPSHOT_{nonce}_{operation}_BEGIN__\n".encode())
                    response.extend(value)
                    response.extend(f"\n__YK_SNAPSHOT_{nonce}_{operation}_END_0__\n".encode())
                channel.sendall(response)
                channel.send_exit_status(0)
            elif command in (
                b"docker ps --format '{{json .}}' 2>/dev/null",
                b"docker ps -a --format '{{json .}}' 2>/dev/null",
            ):
                channel.sendall(
                    b'{"Names":"api","Image":"example/api","State":"running","Status":"Up","RestartCount":0}\n'
                )
                channel.send_exit_status(0)
            elif command == b"docker logs --tail 2 --timestamps -- 'api' 2>&1":
                channel.sendall(b"2026-10-15T10:00:00Z started\n2026-10-15T10:00:01Z ready\n")
                channel.send_exit_status(0)
            elif command == b"docker inspect --format '{{json .}}' -- 'api' 2>/dev/null":
                channel.sendall(
                    b'{"Id":"container-id","Name":"/api","Config":{"Image":"example/api"},"State":{"Status":"running","StartedAt":"2026-10-15T10:00:00Z","Health":{"Status":"healthy"}},"RestartCount":1}\n'
                )
                channel.send_exit_status(0)
            elif command == b"systemctl show --no-pager --property=LoadState,ActiveState,SubState,Description --value -- 'nginx.service' 2>&1":
                channel.sendall(b"loaded\nactive\nrunning\nNginx web server\n")
                channel.send_exit_status(0)
            elif command == b"if command -v dpkg-query >/dev/null 2>&1; then dpkg-query --showformat='${Status}\\t${Version}\\n' --show -- 'nginx' 2>/dev/null || printf '%s\\n' absent; else printf '%s\\n' unsupported; fi":
                channel.sendall(b"install ok installed\t1.27.0-1\n")
                channel.send_exit_status(0)
            elif command == b"docker restart --time 15 -- 'api' 2>&1":
                channel.sendall(b"api\n")
                channel.send_exit_status(0)
            elif command == b"systemctl restart --no-ask-password --wait -- 'nginx.service' 2>&1":
                channel.send_exit_status(0)
            elif command == b"DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends --no-remove -- 'nginx=1.27.0' 2>&1":
                channel.sendall(b"Setting up nginx (1.27.0)\n")
                channel.send_exit_status(0)
            elif command == b"systemctl is-active -- nginx.service":
                channel.sendall(b"active\n")
                channel.send_exit_status(0)
            elif command == b"cd '/srv/app' && env LANG='C' /bin/sh -c 'printf server-exec-ok'":
                channel.sendall(b"authorized-output\n")
                channel.send_exit_status(0)
            elif command == b"/bin/sh -c 'server-exec-large-output'":
                channel.sendall(b"A" * 5000)
                channel.send_stderr(b"B" * 5000)
                channel.send_exit_status(0)
            elif b"systemctl list-units --type=service" in command:
                channel.sendall(b"nginx.service loaded active running Nginx Web Server\n\n__YUKINAL_SOURCE__=systemd\n")
                channel.send_exit_status(0)
            elif b"journalctl -n 500" in command:
                if b"--since '-3600 seconds'" not in command or b"--unit 'nginx.service'" not in command:
                    channel.sendall(b"unexpected journal filters\n")
                    channel.send_exit_status(2)
                else:
                    channel.sendall(
                        b"2026-10-15T06:00:00+0800 host nginx[12]: warning: worker restarted\n"
                        b"__YUKINAL_SOURCE__=journalctl\n"
                    )
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


def write_https_proxy_certificates(root: Path) -> tuple[Path, Path, Path, Path, Path]:
    now = datetime.datetime.now(datetime.timezone.utc)
    ca_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    ca_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Yukinal HTTPS proxy test CA")])
    ca_certificate = (
        x509.CertificateBuilder()
        .subject_name(ca_name)
        .issuer_name(ca_name)
        .public_key(ca_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(minutes=1))
        .not_valid_after(now + datetime.timedelta(days=1))
        .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
        .add_extension(
            x509.KeyUsage(
                digital_signature=True,
                content_commitment=False,
                key_encipherment=False,
                data_encipherment=False,
                key_agreement=False,
                key_cert_sign=True,
                crl_sign=True,
                encipher_only=False,
                decipher_only=False,
            ),
            critical=True,
        )
        .sign(ca_key, hashes.SHA256())
    )
    ca_path = root / "https-proxy-test-ca.pem"
    ca_path.write_bytes(ca_certificate.public_bytes(serialization.Encoding.PEM))

    def write_leaf(name: str, *, wrong_host: bool) -> tuple[Path, Path]:
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, name)])
        sans = (
            [x509.DNSName("proxy-mismatch.invalid")]
            if wrong_host
            else [
                x509.IPAddress(ipaddress.ip_address("127.0.0.1")),
                x509.DNSName("localhost"),
            ]
        )
        certificate = (
            x509.CertificateBuilder()
            .subject_name(subject)
            .issuer_name(ca_name)
            .public_key(key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(minutes=1))
            .not_valid_after(now + datetime.timedelta(days=1))
            .add_extension(x509.SubjectAlternativeName(sans), critical=False)
            .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
            .add_extension(
                x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), critical=False
            )
            .sign(ca_key, hashes.SHA256())
        )
        cert_path = root / f"{name}.pem"
        key_path = root / f"{name}-key.pem"
        cert_path.write_bytes(
            certificate.public_bytes(serialization.Encoding.PEM)
            + ca_certificate.public_bytes(serialization.Encoding.PEM)
        )
        key_path.write_bytes(
            key.private_bytes(
                serialization.Encoding.PEM,
                serialization.PrivateFormat.PKCS8,
                serialization.NoEncryption(),
            )
        )
        return cert_path, key_path

    good_certificate, good_key = write_leaf("https-proxy-good", wrong_host=False)
    bad_certificate, bad_key = write_leaf("https-proxy-bad", wrong_host=True)
    return ca_path, good_certificate, good_key, bad_certificate, bad_key


def start_https_krl_server(
    certificate: Path, private_key: Path
) -> tuple[http.server.ThreadingHTTPServer, threading.Thread, int]:
    def ssh_string(value: bytes) -> bytes:
        return struct.pack(">I", len(value)) + value

    krl = (
        b"SSHKRL\n\x00"
        + struct.pack(">IQQQ", 1, 1, 0, 0)
        + ssh_string(b"")
        + ssh_string(b"SOCKS KRL integration fixture")
    )

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            if self.path != "/revoked.krl":
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(krl)))
            self.end_headers()
            self.wfile.write(krl)

        def log_message(self, _format: str, *_args: object) -> None:
            return

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(certificate, private_key)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, thread, int(server.server_address[1])


def start_http_connect_proxy(
    expected_credential: str,
    *,
    tls_context: ssl.SSLContext | None = None,
    target_log: list[str] = PROXY_CONNECT_TARGETS,
) -> tuple[socket.socket, threading.Event]:
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen()
    listener.settimeout(0.2)
    stopping = threading.Event()
    expected_authorization = "Basic " + base64.b64encode(
        expected_credential.encode("utf-8")
    ).decode("ascii")

    def handle_connection(connection: socket.socket) -> None:
        upstream: socket.socket | None = None
        try:
            if tls_context is not None:
                connection = tls_context.wrap_socket(connection, server_side=True)
            request = bytearray()
            while b"\r\n\r\n" not in request and len(request) < 8192:
                chunk = connection.recv(1)
                if not chunk:
                    return
                request.extend(chunk)
            if b"\r\n\r\n" not in request:
                connection.sendall(b"HTTP/1.1 431 Request Header Fields Too Large\r\n\r\n")
                return
            lines = request.decode("iso-8859-1").split("\r\n")
            if len(lines) < 2 or not lines[0].startswith("CONNECT "):
                connection.sendall(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n")
                return
            headers = {
                key.strip().lower(): value.strip()
                for line in lines[1:]
                if ":" in line
                for key, value in [line.split(":", 1)]
            }
            if headers.get("proxy-authorization") != expected_authorization:
                connection.sendall(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                return
            authority = lines[0][len("CONNECT "):].split(" ", 1)[0]
            host, separator, port_text = authority.rpartition(":")
            if not separator or not port_text.isdigit():
                connection.sendall(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                return
            if host.startswith("[") and host.endswith("]"):
                host = host[1:-1]
            upstream = socket.create_connection((host, int(port_text)), timeout=3)
            upstream.settimeout(None)
            target_log.append(authority)
            connection.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            while not stopping.is_set():
                readable, _, _ = select.select([connection, upstream], [], [], 1)
                for source in readable:
                    data = source.recv(65536)
                    if not data:
                        return
                    destination = upstream if source is connection else connection
                    destination.sendall(data)
        except (OSError, ValueError):
            pass
        finally:
            if upstream is not None:
                upstream.close()
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


def start_socks5_proxy(
    expected_credential: str,
) -> tuple[socket.socket, threading.Event]:
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen()
    listener.settimeout(0.2)
    stopping = threading.Event()

    def receive_exact(connection: socket.socket, size: int) -> bytes:
        result = bytearray()
        while len(result) < size:
            chunk = connection.recv(size - len(result))
            if not chunk:
                raise ConnectionError("SOCKS client closed during a handshake")
            result.extend(chunk)
        return bytes(result)

    def handle_connection(connection: socket.socket) -> None:
        upstream: socket.socket | None = None
        try:
            connection.settimeout(5)
            version, method_count = receive_exact(connection, 2)
            methods = receive_exact(connection, method_count)
            if version != 5 or 2 not in methods:
                connection.sendall(b"\x05\xff")
                return
            connection.sendall(b"\x05\x02")

            auth_version, username_length = receive_exact(connection, 2)
            username = receive_exact(connection, username_length)
            password_length = receive_exact(connection, 1)[0]
            password = receive_exact(connection, password_length)
            received_credential = username.decode("utf-8") + ":" + password.decode("utf-8")
            if auth_version != 1 or received_credential != expected_credential:
                connection.sendall(b"\x01\x01")
                return
            connection.sendall(b"\x01\x00")

            version, command, reserved, address_type = receive_exact(connection, 4)
            if version != 5 or command != 1 or reserved != 0:
                connection.sendall(b"\x05\x07\x00\x01\x00\x00\x00\x00\x00\x00")
                return
            if address_type == 1:
                host = socket.inet_ntop(socket.AF_INET, receive_exact(connection, 4))
            elif address_type == 4:
                host = socket.inet_ntop(socket.AF_INET6, receive_exact(connection, 16))
            elif address_type == 3:
                host_length = receive_exact(connection, 1)[0]
                host = receive_exact(connection, host_length).decode("ascii")
            else:
                connection.sendall(b"\x05\x08\x00\x01\x00\x00\x00\x00\x00\x00")
                return
            port = int.from_bytes(receive_exact(connection, 2), "big")
            SOCKS5_CONNECT_TARGETS.append((host, port, address_type))
            upstream_host = "127.0.0.1" if host == "::1" else host
            upstream = socket.create_connection((upstream_host, port), timeout=3)
            upstream.settimeout(None)
            connection.sendall(b"\x05\x00\x00\x01\x00\x00\x00\x00\x00\x00")
            while not stopping.is_set():
                readable, _, _ = select.select([connection, upstream], [], [], 1)
                for source in readable:
                    data = source.recv(65536)
                    if not data:
                        return
                    destination = upstream if source is connection else connection
                    destination.sendall(data)
        except (OSError, UnicodeError, ConnectionError):
            pass
        finally:
            if upstream is not None:
                upstream.close()
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
    *,
    keyboard_interactive: bool = False,
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
        if keyboard_interactive:
            if ready.get("type") != "auth_prompt":
                raise AssertionError(f"C++ terminal did not surface the SSH challenge: {ready!r}")
            if (
                ready.get("name") != "Second factor"
                or ready.get("instructions") != "Enter the one-time code"
                or ready.get("prompts") != [{"prompt": "One-time code: ", "echo": False}]
            ):
                raise AssertionError(f"C++ terminal changed the SSH challenge details: {ready!r}")
            process.stdin.write(json.dumps({"type": "auth_response", "responses": ["654321"]}) + "\n")
            process.stdin.flush()
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
        if keyboard_interactive and "keyboard-interactive-success" not in AUTH_EVENTS:
            raise AssertionError(f"SSH mock did not accept the keyboard-interactive response: {AUTH_EVENTS!r}")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=3)


def exercise_cpp_sftp_streams(
    service_binary: Path,
    root: Path,
    target: dict[str, object],
    environment: dict[str, str],
) -> None:
    sftp_config = root / "sftp-stream-service.json"
    sftp_config.write_text(
        json.dumps({"listen_port": 8765, "api_token_env": "YUKINAL_TEST_API_TOKEN", "targets": [target]}),
        encoding="utf-8",
    )

    download = subprocess.Popen(
        [str(service_binary), "--config", str(sftp_config), "--sftp-download", "--path", "/large.bin"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
        bufsize=1,
        env=environment,
    )
    if download.stdout is None:
        download.kill()
        raise RuntimeError("could not open the C++ SFTP download output pipe")
    try:
        ready = json.loads(download.stdout.readline())
        if ready.get("type") != "ready":
            raise AssertionError(f"C++ SFTP download did not become ready: {ready!r}")
        content = bytearray()
        while True:
            event = json.loads(download.stdout.readline())
            if event.get("type") == "data":
                content.extend(base64.b64decode(str(event.get("data", "")), validate=True))
            elif event.get("type") == "closed":
                if event.get("exit_code") != 0:
                    raise AssertionError(f"C++ SFTP download failed: {event!r}")
                break
            else:
                raise AssertionError(f"C++ SFTP download returned an invalid event: {event!r}")
        if bytes(content) != StaticSftpInterface.LARGE_CONTENTS:
            raise AssertionError("C++ SFTP streaming download changed the binary file contents")
        if download.wait(timeout=5) != 0:
            raise AssertionError(f"C++ SFTP download worker exited with {download.returncode}")
    finally:
        if download.poll() is None:
            download.kill()
            download.wait(timeout=3)

    upload_content = bytes((index * 31) % 256 for index in range(240_000))
    upload = subprocess.Popen(
        [str(service_binary), "--config", str(sftp_config), "--sftp-upload-exclusive", "--path", "/transfer-stream.bin"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
        bufsize=1,
        env=environment,
    )
    if upload.stdin is None or upload.stdout is None:
        upload.kill()
        raise RuntimeError("could not open the C++ SFTP upload worker pipes")
    try:
        ready = json.loads(upload.stdout.readline())
        if ready.get("type") != "ready":
            raise AssertionError(f"C++ SFTP upload did not become ready: {ready!r}")
        chunk_size = 32 * 1024
        for offset in range(0, len(upload_content), chunk_size):
            chunk = upload_content[offset:offset + chunk_size]
            frame = {"type": "data", "data": base64.b64encode(chunk).decode("ascii")}
            upload.stdin.write(json.dumps(frame) + "\n")
        upload.stdin.write(json.dumps({"type": "close"}) + "\n")
        upload.stdin.flush()
        closed = json.loads(upload.stdout.readline())
        if closed.get("type") != "closed" or closed.get("bytes") != len(upload_content):
            raise AssertionError(f"C++ SFTP upload returned an invalid completion: {closed!r}")
        if upload.wait(timeout=8) != 0:
            raise AssertionError(f"C++ SFTP upload worker exited with {upload.returncode}")
        if StaticSftpInterface._files.get("/transfer-stream.bin") != upload_content:
            raise AssertionError("C++ SFTP streaming upload changed the binary file contents")
    finally:
        if upload.poll() is None:
            upload.kill()
            upload.wait(timeout=3)

    existing = subprocess.run(
        [str(service_binary), "--config", str(sftp_config), "--sftp-upload-exclusive", "--path", "/settings.toml"],
        input="",
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=8,
        env=environment,
    )
    if existing.returncode == 0 or json.loads(existing.stdout.splitlines()[0]).get("type") != "error":
        raise AssertionError("C++ SFTP exclusive upload accepted an existing destination")

    failed_upload = subprocess.Popen(
        [str(service_binary), "--config", str(sftp_config), "--sftp-upload-exclusive", "--path", "/stream-failure.bin"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
        bufsize=1,
        env=environment,
    )
    if failed_upload.stdin is None or failed_upload.stdout is None:
        failed_upload.kill()
        raise RuntimeError("could not open the C++ SFTP failure-case pipes")
    try:
        ready = json.loads(failed_upload.stdout.readline())
        if ready.get("type") != "ready":
            raise AssertionError(f"C++ SFTP cleanup case did not become ready: {ready!r}")
        failed_upload.stdin.write(json.dumps({"type": "data", "data": "%%%"}) + "\n")
        failed_upload.stdin.flush()
        failure = json.loads(failed_upload.stdout.readline())
        if failure.get("type") != "error":
            raise AssertionError(f"C++ SFTP upload accepted malformed input: {failure!r}")
        if failed_upload.wait(timeout=5) == 0:
            raise AssertionError("C++ SFTP malformed upload exited successfully")
        if "/stream-failure.bin" in StaticSftpInterface._files:
            raise AssertionError("C++ SFTP malformed upload left a staging file behind")
    finally:
        if failed_upload.poll() is None:
            failed_upload.kill()
            failed_upload.wait(timeout=3)


def exercise_agent_runtime(
    client: SshServiceClient, service_binary: Path, desktop_target: dict[str, object]
) -> None:
    from yukinal_agent import AgentWorkflow, MemoryAuditSink, ToolRegistry, register_ssh_tools
    from yukinal_agent.ssh_tools import register_desktop_ssh_tools

    registry = ToolRegistry(MemoryAuditSink())
    register_ssh_tools(registry, client)
    workflow = AgentWorkflow(registry)
    socks_target_count = len(SOCKS5_CONNECT_TARGETS)

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
            snapshot_data: dict[str, object] | None = None

            async def request(self, method: str, params: dict[str, object], *, timeout_seconds: float) -> object:
                if method == "host.ssh.service_target" and params == {"serverId": "srv_stage1"}:
                    return desktop_target
                if method == "host.tool.execute" and params.get("toolName") == "server.info":
                    target = params.get("target")
                    tool_input = params.get("input")
                    if not isinstance(target, dict) or target.get("serverId") != "srv_stage1":
                        raise AssertionError(f"snapshot used a different target: {params!r}")
                    if not isinstance(tool_input, dict):
                        raise AssertionError(f"snapshot host input was invalid: {tool_input!r}")
                    data = tool_input.get("snapshotData")
                    if not isinstance(data, dict) or "cpu" not in data:
                        raise AssertionError(f"normalized snapshot data was invalid: {data!r}")
                    self.snapshot_data = data
                    cpu = data["cpu"]
                    if not isinstance(cpu, dict) or cpu.get("model") != "Mock CPU":
                        raise AssertionError(f"snapshot did not include the C++ SSH result: {cpu!r}")
                    return {"status": "success", "output": {
                        "serverId": "srv_stage1", "os": {"hostname": "mock-host"},
                        "cpu": {"model": "Mock CPU"},
                    }}
                raise AssertionError(f"unexpected desktop SSH request: {method} {params!r}")

        desktop_registry = ToolRegistry(MemoryAuditSink())
        desktop_host = DesktopHost()
        register_desktop_ssh_tools(desktop_registry, desktop_host, service_binary)
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

        snapshot_result = await desktop_workflow.run(
            task_id="desktop-managed-server-snapshot",
            tenant_id="integration-tenant",
            user_id="integration-operator",
            allowed_tools={"server.info"},
            allowed_target_ids={"srv_stage1"},
            tool_calls=[{"name": "server.info", "arguments": {}}],
            host_target={"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
        )
        if snapshot_result.status != "succeeded" or not snapshot_result.results:
            raise AssertionError(f"desktop-managed C++ snapshot route failed: {snapshot_result!r}")
        snapshot_output = snapshot_result.results[0].get("result", {})
        if (
            snapshot_output.get("serverId") != "srv_stage1"
            or snapshot_output.get("cpu", {}).get("model") != "Mock CPU"
            or desktop_host.snapshot_data is None
        ):
            raise AssertionError(f"desktop-managed snapshot result was not preserved: {snapshot_output!r}")
        target_payload = desktop_target.get("target")
        if isinstance(target_payload, dict) and "proxy" in target_payload:
            desktop_socks_targets = SOCKS5_CONNECT_TARGETS[socks_target_count:]
            if len(desktop_socks_targets) < 2 or any(
                target != ("localhost", target_payload.get("port"), 3)
                for target in desktop_socks_targets
            ):
                raise AssertionError(
                    "desktop Agent operations did not use the configured SOCKS5H route: "
                    f"{desktop_socks_targets!r}"
                )

    asyncio.run(run())


def run_test(service_binary: Path, *, agent_runtime_only: bool = False) -> None:
    logging.getLogger("paramiko").setLevel(logging.CRITICAL)

    def ed25519_key() -> tuple[paramiko.Ed25519Key, str]:
        private_key = ed25519.Ed25519PrivateKey.generate().private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.OpenSSH,
            serialization.NoEncryption(),
        )
        private_key_text = private_key.decode("ascii")
        return (
            paramiko.Ed25519Key.from_private_key(io.StringIO(private_key_text)),
            private_key_text,
        )

    if agent_runtime_only:
        host_key, _ = ed25519_key()
        wrong_host_key, _ = ed25519_key()
        user_key, private_key_pem = ed25519_key()
    else:
        host_key = paramiko.RSAKey.generate(2048)
        wrong_host_key = paramiko.RSAKey.generate(2048)
        user_key = paramiko.RSAKey.generate(2048)
        private_key_buffer = io.StringIO()
        user_key.write_private_key(private_key_buffer)
        private_key_pem = private_key_buffer.getvalue()
    listener, stopping = start_ssh_mock(host_key, user_key)
    ssh_port = listener.getsockname()[1]
    keyboard_listener, keyboard_stopping = start_ssh_mock(
        host_key, user_key, keyboard_interactive_only=True
    )
    keyboard_ssh_port = keyboard_listener.getsockname()[1]
    proxy_listener, proxy_stopping = start_http_connect_proxy("proxy-user:proxy-password")
    proxy_port = proxy_listener.getsockname()[1]
    socks_listener, socks_stopping = start_socks5_proxy("proxy-user:proxy-password")
    socks_port = socks_listener.getsockname()[1]
    PROXY_CONNECT_TARGETS.clear()
    HTTPS_PROXY_CONNECT_TARGETS.clear()
    SOCKS5_CONNECT_TARGETS.clear()

    with tempfile.TemporaryDirectory(prefix="yukinal-paramiko-ssh-") as temp:
        root = Path(temp)
        ca_bundle, good_certificate, good_key, bad_certificate, bad_key = (
            write_https_proxy_certificates(root)
        )

        def tls_server_context(certificate: Path, key: Path) -> ssl.SSLContext:
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            context.load_cert_chain(certificate, key)
            return context

        https_proxy_listener, https_proxy_stopping = start_http_connect_proxy(
            "proxy-user:proxy-password",
            tls_context=tls_server_context(good_certificate, good_key),
            target_log=HTTPS_PROXY_CONNECT_TARGETS,
        )
        https_proxy_port = https_proxy_listener.getsockname()[1]
        bad_tls_proxy_listener, bad_tls_proxy_stopping = start_http_connect_proxy(
            "proxy-user:proxy-password",
            tls_context=tls_server_context(bad_certificate, bad_key),
            target_log=HTTPS_PROXY_CONNECT_TARGETS,
        )
        bad_tls_proxy_port = bad_tls_proxy_listener.getsockname()[1]
        krl_https_server, krl_https_thread, krl_https_port = start_https_krl_server(
            good_certificate, good_key
        )
        bad_krl_https_server, bad_krl_https_thread, bad_krl_https_port = start_https_krl_server(
            bad_certificate, bad_key
        )
        known_hosts = root / "known_hosts"

        def write_known_host(key: paramiko.PKey) -> None:
            encoded_key = base64.b64encode(key.asbytes()).decode()
            with known_hosts.open("a", encoding="ascii") as known_hosts_file:
                known_hosts_file.write(
                    f"[127.0.0.1]:{ssh_port} {key.get_name()} {encoded_key}\n"
                    f"[localhost]:{ssh_port} {key.get_name()} {encoded_key}\n"
                )

        write_known_host(host_key)
        pinned_fingerprint = "SHA256:" + base64.b64encode(
            hashlib.sha256(host_key.asbytes()).digest()
        ).decode("ascii").rstrip("=")
        wrong_fingerprint = "SHA256:" + base64.b64encode(
            hashlib.sha256(wrong_host_key.asbytes()).digest()
        ).decode("ascii").rstrip("=")
        host_ca_public_key = (
            f"{host_key.get_name()} {base64.b64encode(host_key.asbytes()).decode('ascii')} test-krl-ca"
        )
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
                    "target_id": "srv_server_exec_enabled",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "host_key_fingerprint": pinned_fingerprint,
                    "auth": {"type": "private_key", "private_key_env": "YK_SSH_TEST_PRIVATE_KEY"},
                    "allow_server_exec": True,
                },
                {
                    "target_id": "srv_server_exec_bounded",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "host_key_fingerprint": pinned_fingerprint,
                    "auth": {"type": "private_key", "private_key_env": "YK_SSH_TEST_PRIVATE_KEY"},
                    "allow_server_exec": True,
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
                {
                    "target_id": "srv_proxy",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "proxy": {
                        "type": "http_connect",
                        "host": "127.0.0.1",
                        "port": proxy_port,
                        "auth_env": "YK_SSH_TEST_PROXY_CREDENTIAL",
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_proxy_bad_auth",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "proxy": {
                        "type": "http_connect",
                        "host": "127.0.0.1",
                        "port": proxy_port,
                        "auth_env": "YK_SSH_TEST_PROXY_BAD_CREDENTIAL",
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_proxy_bypass",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "proxy": {
                        "type": "http_connect",
                        "host": "127.0.0.1",
                        "port": proxy_port,
                        "no_proxy": ["127.0.0.0/8"],
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_proxy_probe_only",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "ssh_probe",
                    "probe_only": True,
                    "proxy": {
                        "type": "http_connect",
                        "host": "127.0.0.1",
                        "port": proxy_port,
                        "auth_env": "YK_SSH_TEST_PROXY_CREDENTIAL",
                    },
                    "auth": {"type": "probe"},
                },
                {
                    "target_id": "srv_socks5_local_dns",
                    "host": "localhost",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "proxy": {
                        "url": f"socks5://127.0.0.1:{socks_port}",
                        "auth_env": "YK_SSH_TEST_PROXY_CREDENTIAL",
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_socks5_remote_dns",
                    "host": "localhost",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "proxy": {
                        "url": f"socks5h://127.0.0.1:{socks_port}",
                        "auth_env": "YK_SSH_TEST_PROXY_CREDENTIAL",
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_socks5_bad_auth",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "proxy": {
                        "url": f"socks5://127.0.0.1:{socks_port}",
                        "auth_env": "YK_SSH_TEST_PROXY_BAD_CREDENTIAL",
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_krl_socks5h",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "host_certificate_authority": {
                        "ca_public_key": host_ca_public_key,
                        "principals": ["127.0.0.1"],
                        "revocation_list_url": f"https://localhost:{krl_https_port}/revoked.krl",
                    },
                    "proxy": {
                        "url": f"socks5h://127.0.0.1:{socks_port}",
                        "auth_env": "YK_SSH_TEST_PROXY_CREDENTIAL",
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_krl_socks5h_bypass_port",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "host_certificate_authority": {
                        "ca_public_key": host_ca_public_key,
                        "principals": ["127.0.0.1"],
                        "revocation_list_url": f"https://127.0.0.1:{krl_https_port}/revoked.krl",
                    },
                    "proxy": {
                        "url": f"socks5h://127.0.0.1:{socks_port}",
                        "auth_env": "YK_SSH_TEST_PROXY_CREDENTIAL",
                        "no_proxy": f"127.0.0.1:{krl_https_port}",
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_krl_socks5_local_dns",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "host_certificate_authority": {
                        "ca_public_key": host_ca_public_key,
                        "principals": ["127.0.0.1"],
                        "revocation_list_url": f"https://localhost:{krl_https_port}/revoked.krl",
                    },
                    "proxy": {
                        "url": f"socks5://127.0.0.1:{socks_port}",
                        "auth_env": "YK_SSH_TEST_PROXY_CREDENTIAL",
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_krl_socks5h_bad_certificate",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "host_certificate_authority": {
                        "ca_public_key": host_ca_public_key,
                        "principals": ["127.0.0.1"],
                        "revocation_list_url": f"https://127.0.0.1:{bad_krl_https_port}/revoked.krl",
                    },
                    "proxy": {
                        "url": f"socks5h://127.0.0.1:{socks_port}",
                        "auth_env": "YK_SSH_TEST_PROXY_CREDENTIAL",
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_https_proxy",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "proxy": {
                        "url": f"https://127.0.0.1:{https_proxy_port}",
                        "auth_env": "YK_SSH_TEST_PROXY_CREDENTIAL",
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                },
                {
                    "target_id": "srv_https_proxy_probe_only",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "ssh_probe",
                    "probe_only": True,
                    "proxy": {
                        "url": f"https://127.0.0.1:{https_proxy_port}",
                        "auth_env": "YK_SSH_TEST_PROXY_CREDENTIAL",
                    },
                    "auth": {"type": "probe"},
                },
                {
                    "target_id": "srv_https_proxy_bad_host",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": str(known_hosts),
                    "proxy": {
                        "url": f"https://127.0.0.1:{bad_tls_proxy_port}",
                        "auth_env": "YK_SSH_TEST_PROXY_CREDENTIAL",
                    },
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
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
        environment["YK_SSH_TEST_PROXY_CREDENTIAL"] = "proxy-user:proxy-password"
        environment["YK_SSH_TEST_PROXY_BAD_CREDENTIAL"] = "proxy-user:bad-proxy-password"
        environment["SSL_CERT_FILE"] = str(ca_bundle)
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

            proxy_probe = client._execute_operation(
                "srv_proxy_probe_only", "host_key_probe", {}, timeout_seconds=8
            )
            if proxy_probe.stdout != pinned_fingerprint or proxy_probe.exit_code != 0:
                raise AssertionError(f"HTTP CONNECT host-key probe returned an unexpected result: {proxy_probe!r}")
            if AUTH_EVENTS:
                raise AssertionError(f"HTTP CONNECT host-key probe attempted SSH authentication: {AUTH_EVENTS!r}")

            proxy_result = client.service_status("srv_proxy", "nginx.service", timeout_seconds=8)
            if proxy_result.exit_code != 0 or proxy_result.stdout != "active\n":
                raise AssertionError(f"HTTP CONNECT proxy did not carry the SSH operation: {proxy_result!r}")
            expected_authority = f"127.0.0.1:{ssh_port}"
            if PROXY_CONNECT_TARGETS != [expected_authority, expected_authority]:
                raise AssertionError(f"HTTP CONNECT used an unexpected target: {PROXY_CONNECT_TARGETS!r}")
            bypass_result = client.service_status("srv_proxy_bypass", "nginx.service", timeout_seconds=8)
            if bypass_result.exit_code != 0 or bypass_result.stdout != "active\n":
                raise AssertionError(f"NO_PROXY direct route did not carry the SSH operation: {bypass_result!r}")
            if PROXY_CONNECT_TARGETS != [expected_authority, expected_authority]:
                raise AssertionError(f"NO_PROXY target was incorrectly sent through HTTP CONNECT: {PROXY_CONNECT_TARGETS!r}")
            try:
                client.service_status("srv_proxy_bad_auth", "nginx.service", timeout_seconds=5)
            except SshServiceError as error:
                if error.code != "proxy_auth_required":
                    raise AssertionError(f"proxy authentication rejection used the wrong error: {error.code}") from error
                if "bad-proxy-password" in str(error):
                    raise AssertionError("proxy error response exposed the credential") from error
            else:
                raise AssertionError("C++ SSH accepted a rejected HTTP proxy credential")

            socks_local_result = client.service_status(
                "srv_socks5_local_dns", "nginx.service", timeout_seconds=8
            )
            if socks_local_result.exit_code != 0 or socks_local_result.stdout != "active\n":
                raise AssertionError(f"SOCKS5 local-DNS proxy did not carry SSH: {socks_local_result!r}")
            socks_remote_result = client.service_status(
                "srv_socks5_remote_dns", "nginx.service", timeout_seconds=8
            )
            if socks_remote_result.exit_code != 0 or socks_remote_result.stdout != "active\n":
                raise AssertionError(f"SOCKS5 remote-DNS proxy did not carry SSH: {socks_remote_result!r}")
            if len(SOCKS5_CONNECT_TARGETS) != 2:
                raise AssertionError(f"SOCKS5 proxy used an unexpected number of tunnels: {SOCKS5_CONNECT_TARGETS!r}")
            local_dns_target = SOCKS5_CONNECT_TARGETS[0]
            if (
                local_dns_target[0] not in {"127.0.0.1", "::1"}
                or local_dns_target[1] != ssh_port
                or local_dns_target[2] not in {1, 4}
            ):
                raise AssertionError(f"SOCKS5 did not resolve its target locally: {SOCKS5_CONNECT_TARGETS!r}")
            if SOCKS5_CONNECT_TARGETS[1] != ("localhost", ssh_port, 3):
                raise AssertionError(f"SOCKS5H did not delegate DNS resolution: {SOCKS5_CONNECT_TARGETS!r}")
            try:
                client.service_status("srv_socks5_bad_auth", "nginx.service", timeout_seconds=5)
            except SshServiceError as error:
                if error.code != "proxy_auth_required":
                    raise AssertionError(f"SOCKS proxy rejection used the wrong error: {error.code}") from error
                if "bad-proxy-password" in str(error):
                    raise AssertionError("SOCKS proxy error response exposed the credential") from error
            else:
                raise AssertionError("C++ SSH accepted a rejected SOCKS proxy credential")

            for target_id in (
                "srv_krl_socks5h",
                "srv_krl_socks5h_bypass_port",
                "srv_krl_socks5_local_dns",
            ):
                try:
                    client.service_status(target_id, "nginx.service", timeout_seconds=8)
                except SshServiceError as error:
                    if error.code != "host_key_unverified" or "plain host key" not in str(error):
                        raise AssertionError(
                            f"{target_id} did not finish KRL verification before rejecting the mock host key: {error}"
                        ) from error
                else:
                    raise AssertionError(f"{target_id} accepted a plain key under Host CA trust")
            krl_socks_targets = [
                item for item in SOCKS5_CONNECT_TARGETS if item[1] == krl_https_port
            ]
            if (
                len(krl_socks_targets) != 2
                or ("localhost", krl_https_port, 3) not in krl_socks_targets
                or not any(
                    target[1] == krl_https_port and target[2] in {1, 4}
                    for target in krl_socks_targets
                )
            ):
                raise AssertionError(
                    f"KRL HTTPS did not use SOCKS exactly once; port-qualified NO_PROXY should bypass it: {krl_socks_targets!r}"
                )
            try:
                client.service_status(
                    "srv_krl_socks5h_bad_certificate", "nginx.service", timeout_seconds=8
                )
            except SshServiceError as error:
                if (
                    error.code != "host_key_unverified"
                    or "Could not load the host certificate revocation list" not in str(error)
                    or "plain host key" in str(error)
                ):
                    raise AssertionError(
                        f"KRL retrieval did not reject a mismatched HTTPS certificate first: {error}"
                    ) from error
            else:
                raise AssertionError("C++ SSH accepted an HTTPS KRL server with a mismatched certificate")

            https_probe = client._execute_operation(
                "srv_https_proxy_probe_only", "host_key_probe", {}, timeout_seconds=8
            )
            if https_probe.stdout != pinned_fingerprint or https_probe.exit_code != 0:
                raise AssertionError(
                    f"verified HTTPS CONNECT host-key probe returned an unexpected result: {https_probe!r}"
                )
            https_result = client.service_status(
                "srv_https_proxy", "nginx.service", timeout_seconds=8
            )
            if https_result.exit_code != 0 or https_result.stdout != "active\n":
                raise AssertionError(
                    f"verified HTTPS CONNECT proxy did not carry SSH: {https_result!r}"
                )
            expected_authority = f"127.0.0.1:{ssh_port}"
            if HTTPS_PROXY_CONNECT_TARGETS != [expected_authority, expected_authority]:
                raise AssertionError(
                    f"HTTPS CONNECT used an unexpected target: {HTTPS_PROXY_CONNECT_TARGETS!r}"
                )
            try:
                client.service_status(
                    "srv_https_proxy_bad_host", "nginx.service", timeout_seconds=5
                )
            except SshServiceError as error:
                if error.code != "proxy_tls_failed":
                    raise AssertionError(
                        f"unmatched HTTPS proxy certificate returned the wrong error: {error.code}"
                    ) from error
                if "proxy-password" in str(error):
                    raise AssertionError("HTTPS proxy error response exposed its credential") from error
            else:
                raise AssertionError("C++ SSH accepted an HTTPS proxy certificate for another host")

            desktop_target = {
                "target": {
                    "target_id": "srv_stage1",
                    "host": "localhost" if agent_runtime_only else "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "host_key_fingerprint": pinned_fingerprint,
                    "auth": {"type": "private_key", "private_key_env": "YK_SSH_PRIVATE_KEY"},
                    **(
                        {
                            "proxy": {
                                "url": f"socks5h://127.0.0.1:{socks_port}",
                                "auth_env": "YK_SSH_PROXY_CREDENTIAL",
                                "no_proxy": "*.corp,10.0.0.0/8",
                            }
                        }
                        if agent_runtime_only
                        else {}
                    ),
                },
                "credentials": {
                    "YK_SSH_PRIVATE_KEY": private_key_pem,
                    **(
                        {"YK_SSH_PROXY_CREDENTIAL": "proxy-user:proxy-password"}
                        if agent_runtime_only
                        else {}
                    ),
                },
            }

            if agent_runtime_only:
                exercise_agent_runtime(client, service_binary, desktop_target)
                return

            snapshot = client.system_snapshot("srv_stage1", timeout_seconds=30)
            if snapshot["cpu_model"][0] != 0 or "Mock CPU" not in snapshot["cpu_model"][1]:
                raise AssertionError(f"unexpected structured system snapshot: {snapshot!r}")

            docker = client._execute_operation(
                "srv_stage1", "docker_ps", {"all": True}, timeout_seconds=5
            )
            if docker.exit_code != 0 or json.loads(docker.stdout.splitlines()[0])["Names"] != "api":
                raise AssertionError(f"unexpected structured Docker listing: {docker!r}")

            docker_logs = client._execute_operation(
                "srv_stage1",
                "docker_logs",
                {"container": "api", "tail": 2},
                timeout_seconds=5,
            )
            if docker_logs.exit_code != 0 or "ready" not in docker_logs.stdout:
                raise AssertionError(f"unexpected structured Docker logs: {docker_logs!r}")
            docker_inspect = client._execute_operation(
                "srv_stage1", "docker_inspect", {"container": "api"}, timeout_seconds=5
            )
            if docker_inspect.exit_code != 0 or json.loads(docker_inspect.stdout)["Name"] != "/api":
                raise AssertionError(f"unexpected structured Docker inspect: {docker_inspect!r}")

            systemd_inspect = client._execute_operation(
                "srv_stage1", "systemd_inspect", {"service": "nginx.service"}, timeout_seconds=5
            )
            if systemd_inspect.exit_code != 0 or systemd_inspect.stdout.splitlines()[2] != "running":
                raise AssertionError(f"unexpected structured systemd inspect: {systemd_inspect!r}")
            package_inspect = client._execute_operation(
                "srv_stage1",
                "package_inspect",
                {"manager": "apt", "package": "nginx"},
                timeout_seconds=5,
            )
            if package_inspect.exit_code != 0 or "1.27.0-1" not in package_inspect.stdout:
                raise AssertionError(f"unexpected structured package inspect: {package_inspect!r}")
            docker_restart = client._execute_operation(
                "srv_stage1",
                "docker_restart",
                {"container": "api", "grace_period_seconds": 15},
                timeout_seconds=30,
            )
            if docker_restart.exit_code != 0 or docker_restart.stdout != "api\n":
                raise AssertionError(f"unexpected structured Docker restart: {docker_restart!r}")
            systemd_restart = client._execute_operation(
                "srv_stage1",
                "systemd_restart",
                {"service": "nginx.service", "timeout_seconds": 30},
                timeout_seconds=30,
            )
            if systemd_restart.exit_code != 0:
                raise AssertionError(f"unexpected structured systemd restart: {systemd_restart!r}")
            package_install = client._execute_operation(
                "srv_stage1",
                "package_install",
                {
                    "manager": "apt",
                    "package": "nginx",
                    "version": "1.27.0",
                    "timeout_seconds": 30,
                },
                timeout_seconds=30,
            )
            if package_install.exit_code != 0 or "Setting up nginx" not in package_install.stdout:
                raise AssertionError(f"unexpected structured package install: {package_install!r}")

            inventory = client.service_inventory("srv_stage1", timeout_seconds=5)
            if inventory.operation != "service_inventory" or "__YUKINAL_SOURCE__=systemd" not in inventory.stdout:
                raise AssertionError(f"unexpected read-only service inventory: {inventory!r}")

            logs = client.server_logs(
                "srv_stage1", since_seconds=3600, unit="nginx.service", timeout_seconds=5
            )
            if logs.operation != "server_logs" or "__YUKINAL_SOURCE__=journalctl" not in logs.stdout:
                raise AssertionError(f"unexpected bounded server logs: {logs!r}")
            if b"--since '-3600 seconds'" not in EXEC_COMMANDS[-1] or b"--unit 'nginx.service'" not in EXEC_COMMANDS[-1]:
                raise AssertionError(f"C++ did not preserve validated log filters: {EXEC_COMMANDS[-1]!r}")
            try:
                client._execute_operation(
                    "srv_stage1", "server_logs", {"unit": "nginx.service;id"}, timeout_seconds=5
                )
            except SshServiceError as error:
                if error.code != "invalid_parameters":
                    raise AssertionError(f"invalid log unit returned the wrong error: {error}") from error
            else:
                raise AssertionError("C++ service accepted a shell fragment as a log unit")

            result = client.service_status("srv_stage1", "nginx.service", timeout_seconds=5)
            if result.stdout != "active\n" or result.exit_code != 0:
                raise AssertionError(f"unexpected read-only result: {result!r}")

            def run_server_exec(
                target_id: str,
                command: str,
                max_output_bytes: int,
                *,
                workdir: str | None = None,
                env: dict[str, str] | None = None,
            ):
                return client.server_exec(
                    target_id,
                    command,
                    5000,
                    max_output_bytes,
                    workdir=workdir,
                    env=env,
                )

            exec_result = run_server_exec(
                "srv_server_exec_enabled",
                "printf server-exec-ok",
                4096,
                workdir="/srv/app",
                env={"LANG": "C"},
            )
            if exec_result.exit_code != 0 or exec_result.stdout != "authorized-output\n":
                raise AssertionError(f"C++ server.exec returned an unexpected result: {exec_result!r}")
            bounded_result = run_server_exec(
                "srv_server_exec_bounded", "server-exec-large-output", 4096
            )
            if (
                len(bounded_result.stdout.encode("utf-8")) > 4096
                or not bounded_result.stdout_truncated
            ):
                raise AssertionError(f"C++ server.exec did not preserve its combined output cap: {bounded_result!r}")
            try:
                run_server_exec("srv_server_exec_enabled", "printf server-exec-ok", 4096)
            except SshServiceError as error:
                if error.code != "operation_not_allowed":
                    raise AssertionError(
                        f"replayed server.exec returned the wrong error: {error}"
                    ) from error
            else:
                raise AssertionError("C++ service reused a consumed server.exec authorization")

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

            existing_lstat = client._execute_operation(
                "srv_desktop_style", "sftp_lstat_optional", {"path": "/settings.toml"}, timeout_seconds=5
            )
            if json.loads(existing_lstat.stdout) != {
                "exists": True,
                "kind": "file",
                "size": len(StaticSftpInterface.CONTENTS),
                "modified": 123456789,
            }:
                raise AssertionError(f"unexpected optional C++ SFTP lstat result: {existing_lstat.stdout!r}")
            missing_lstat = client._execute_operation(
                "srv_desktop_style", "sftp_lstat_optional", {"path": "/missing.bin"}, timeout_seconds=5
            )
            if json.loads(missing_lstat.stdout) != {"exists": False}:
                raise AssertionError(f"missing C++ SFTP lstat did not return absent: {missing_lstat.stdout!r}")

            created_directory = client._execute_operation(
                "srv_desktop_style", "sftp_create_dir", {"path": "/created-by-cpp"}, timeout_seconds=5
            )
            if json.loads(created_directory.stdout) != {"created": True}:
                raise AssertionError("C++ SFTP directory creation was not confirmed")
            created_lstat = client._execute_operation(
                "srv_desktop_style", "sftp_lstat_optional", {"path": "/created-by-cpp"}, timeout_seconds=5
            )
            if json.loads(created_lstat.stdout).get("kind") != "directory":
                raise AssertionError("C++ SFTP directory creation did not produce a remote directory")

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
            publish_result = client._execute_operation(
                "srv_desktop_style",
                "sftp_publish_no_replace",
                {"source": "/backup.bin", "destination": "/backup-published.bin"},
                timeout_seconds=8,
            )
            if json.loads(publish_result.stdout) != {"published": True}:
                raise AssertionError("C++ SFTP hardlink publication was not confirmed")
            if StaticSftpInterface._files.get("/backup-published.bin") != backup_payload:
                raise AssertionError("C++ SFTP hardlink publication changed the staging contents")
            try:
                client._execute_operation(
                    "srv_desktop_style",
                    "sftp_publish_no_replace",
                    {"source": "/backup.bin", "destination": "/settings.toml"},
                    timeout_seconds=8,
                )
            except SshServiceError as error:
                if error.code != "already_exists":
                    raise AssertionError(
                        f"no-replace publication returned the wrong error: {error.code}"
                    ) from error
            else:
                raise AssertionError("C++ SFTP no-replace publication overwrote an existing destination")
            FsyncSftpServer.advertise_hardlink = False
            try:
                client._execute_operation(
                    "srv_desktop_style",
                    "sftp_publish_no_replace",
                    {"source": "/backup.bin", "destination": "/unsupported-published.bin"},
                    timeout_seconds=8,
                )
            except SshServiceError as error:
                if error.code != "replace_unsupported":
                    raise AssertionError(
                        f"unnegotiated hardlink publication returned the wrong error: {error.code}"
                    ) from error
            else:
                raise AssertionError("C++ SFTP used an unnegotiated hardlink extension")
            finally:
                FsyncSftpServer.advertise_hardlink = True
            if "/unsupported-published.bin" in StaticSftpInterface._files:
                raise AssertionError("unsupported hardlink publication created a destination")
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
            client.sftp_remove_file("srv_desktop_style", "/backup-published.bin", 8)
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
            keyboard_terminal_target = {
                "target_id": "srv_keyboard_interactive",
                "host": "127.0.0.1",
                "port": keyboard_ssh_port,
                "username": "test-user",
                "host_key_fingerprint": pinned_fingerprint,
                "auth": {"type": "password", "password_env": "YUKINAL_TEST_BAD_PASSWORD"},
            }
            exercise_cpp_terminal(
                service_binary,
                root,
                keyboard_terminal_target,
                environment,
                keyboard_interactive=True,
            )
            exercise_cpp_sftp_streams(service_binary, root, terminal_target, environment)
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
            keyboard_stopping.set()
            keyboard_listener.close()
            proxy_stopping.set()
            proxy_listener.close()
            socks_stopping.set()
            socks_listener.close()
            https_proxy_stopping.set()
            https_proxy_listener.close()
            bad_tls_proxy_stopping.set()
            bad_tls_proxy_listener.close()
            krl_https_server.shutdown()
            krl_https_server.server_close()
            krl_https_thread.join(timeout=5)
            bad_krl_https_server.shutdown()
            bad_krl_https_server.server_close()
            bad_krl_https_thread.join(timeout=5)


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
        print("PASS SSH exec, HTTP/HTTPS CONNECT with certificate verification, SFTP listing/stat/read/write/exclusive-create/remove/link-count, auth rejection, timeout, output cap, and host-key rejection")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
