from __future__ import annotations

import asyncio
import json
import os
import re
import secrets
import socket
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import urlsplit


class SshServiceProcessError(RuntimeError):
    """A configured local SSH service could not be started safely."""


class SshServiceProcess:
    """Own the optional C++ SSH service used by the Python Agent runtime."""

    _RUNTIME_ENV = (
        "PATH", "SystemRoot", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "HOME",
        "USERPROFILE", "TMPDIR", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH",
    )

    def __init__(self, binary: Path, config: Path) -> None:
        self._binary = binary
        self._config = config
        self._process: asyncio.subprocess.Process | None = None
        self._token_env = ""
        self._token_value = ""
        self._secret_env: set[str] = set()
        self._secret_values: dict[str, str] | None = None
        self._temporary_directory: tempfile.TemporaryDirectory[str] | None = None
        self._port = 0

    @classmethod
    def from_environment(cls) -> SshServiceProcess | None:
        binary_value = os.environ.get("YUKINAL_SSH_SERVICE_BINARY", "").strip()
        config_value = os.environ.get("YUKINAL_SSH_SERVICE_CONFIG", "").strip()
        if not config_value:
            return None
        if not binary_value:
            raise SshServiceProcessError(
                "set both YUKINAL_SSH_SERVICE_BINARY and YUKINAL_SSH_SERVICE_CONFIG to manage the C++ SSH service"
            )

        config = Path(config_value).expanduser()
        try:
            if config.stat().st_size > 1024 * 1024:
                raise SshServiceProcessError("the configured SSH service file exceeds the size limit")
            document = json.loads(config.read_text(encoding="utf-8"))
        except SshServiceProcessError:
            raise
        except (OSError, UnicodeError, json.JSONDecodeError):
            raise SshServiceProcessError("the configured SSH service file could not be read") from None
        if not isinstance(document, dict):
            raise SshServiceProcessError("the configured SSH service file must contain a JSON object")
        port = document.get("listen_port")
        token_env = document.get("api_token_env")
        targets = document.get("targets")
        if isinstance(port, bool) or not isinstance(port, int) or not 1 <= port <= 65535:
            raise SshServiceProcessError("the SSH service listen_port is invalid")
        if not _valid_env_name(token_env):
            raise SshServiceProcessError("the SSH service api_token_env is invalid")
        if token_env != "YUKINAL_SSH_API_TOKEN":
            raise SshServiceProcessError(
                "desktop SSH service config must use YUKINAL_SSH_API_TOKEN for api_token_env"
            )
        if not isinstance(targets, list) or not targets:
            raise SshServiceProcessError("the SSH service target allowlist is empty or invalid")

        secret_env: set[str] = {token_env}
        auth_env_names: set[str] = set()
        uses_agent = False
        for target in targets:
            if not isinstance(target, dict):
                raise SshServiceProcessError("the SSH service target allowlist is invalid")
            auth = target.get("auth")
            if not isinstance(auth, dict):
                raise SshServiceProcessError("the SSH service target authentication configuration is invalid")
            if auth.get("type") == "agent":
                uses_agent = True
            for key in ("password_env", "passphrase_env", "private_key_env"):
                name = auth.get(key)
                if name is not None:
                    if not _valid_credential_env_name(name):
                        raise SshServiceProcessError("the SSH service credential environment reference is invalid")
                    if any(existing != name and existing.casefold() == name.casefold() for existing in secret_env):
                        raise SshServiceProcessError("SSH service environment references differ only by letter case")
                    secret_env.add(name)
                    auth_env_names.add(name)
            if "proxy" in target:
                proxy_auth_env = _validate_http_connect_proxy(target["proxy"])
                if proxy_auth_env is not None:
                    if any(
                        existing != proxy_auth_env
                        and existing.casefold() == proxy_auth_env.casefold()
                        for existing in secret_env
                    ):
                        raise SshServiceProcessError(
                            "SSH service environment references differ only by letter case"
                        )
                    secret_env.add(proxy_auth_env)
                    auth_env_names.add(proxy_auth_env)

        if uses_agent:
            if any(name.casefold() == "ssh_auth_sock" for name in auth_env_names):
                raise SshServiceProcessError("SSH_AUTH_SOCK cannot also be used as a credential reference")
            socket_path = os.environ.get("SSH_AUTH_SOCK")
            if os.name != "nt" and socket_path is None:
                raise SshServiceProcessError("SSH_AUTH_SOCK is not set for the configured SSH agent target")
            if socket_path is not None and not _valid_agent_socket_path(socket_path):
                raise SshServiceProcessError("SSH_AUTH_SOCK is not a valid socket path")
            if socket_path is not None:
                secret_env.add("SSH_AUTH_SOCK")

        token = os.environ.get(token_env) or os.environ.get("YUKINAL_SSH_API_TOKEN", "")
        if not token:
            raise SshServiceProcessError(f"the SSH service token environment variable {token_env} is not set")
        base_url = os.environ.get("YUKINAL_SSH_API_URL", f"http://127.0.0.1:{port}")
        try:
            parsed = urlsplit(base_url)
            url_port = parsed.port
        except ValueError:
            raise SshServiceProcessError("YUKINAL_SSH_API_URL is invalid") from None
        if (
            parsed.scheme != "http"
            or parsed.hostname != "127.0.0.1"
            or parsed.username is not None
            or parsed.password is not None
            or parsed.path not in ("", "/")
            or parsed.query
            or parsed.fragment
            or url_port != port
        ):
            raise SshServiceProcessError(
                "YUKINAL_SSH_API_URL must use the configured port on the local HTTP loopback"
            )

        binary = Path(binary_value).expanduser()
        if not binary.is_file():
            raise SshServiceProcessError("the packaged C++ SSH service executable is missing")
        if not config.is_file():
            raise SshServiceProcessError("the configured SSH service file does not exist")

        instance = cls(binary, config)
        instance._token_env = token_env
        instance._token_value = token
        instance._secret_env = secret_env
        instance._secret_env.discard(token_env)
        instance._port = port
        return instance

    @classmethod
    def for_desktop_target(
        cls, binary: Path, payload: object
    ) -> tuple[SshServiceProcess, str, str]:
        """Create an ephemeral one-target config from a trusted desktop Host RPC result."""
        if not binary.is_file():
            raise SshServiceProcessError("the packaged C++ SSH service executable is missing")
        if not isinstance(payload, dict):
            raise SshServiceProcessError("the desktop host returned an invalid SSH target")
        target = payload.get("target")
        credentials = payload.get("credentials")
        if not isinstance(target, dict) or not isinstance(credentials, dict):
            raise SshServiceProcessError("the desktop host returned an invalid SSH target")
        required_target_fields = {
            "target_id", "host", "port", "username", "host_key_fingerprint", "auth",
        }
        if (
            not required_target_fields.issubset(target)
            or set(target) - required_target_fields not in (set(), {"proxy"})
        ):
            raise SshServiceProcessError("the desktop host returned an invalid SSH target")
        target_id = target.get("target_id")
        host = target.get("host")
        username = target.get("username")
        port = target.get("port")
        fingerprint = target.get("host_key_fingerprint")
        auth = target.get("auth")
        if (
            not isinstance(target_id, str)
            or not re.fullmatch(r"srv_[a-z0-9]+", target_id)
            or not isinstance(host, str)
            or not host
            or len(host) > 253
            or any(ord(character) <= 0x20 or ord(character) == 0x7f for character in host)
            or isinstance(port, bool)
            or not isinstance(port, int)
            or not 1 <= port <= 65535
            or not isinstance(username, str)
            or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.-]{0,63}", username)
            or not isinstance(fingerprint, str)
            or not re.fullmatch(r"SHA256:[A-Za-z0-9+/]{43}", fingerprint)
            or not isinstance(auth, dict)
        ):
            raise SshServiceProcessError("the desktop host returned an invalid SSH target")

        if (
            auth.get("type") == "password"
            and set(auth) == {"type", "password_env"}
            and isinstance(auth.get("password_env"), str)
        ):
            credential_names = {auth["password_env"]}
        elif auth.get("type") == "private_key" and set(auth) in (
            {"type", "private_key_env"},
            {"type", "private_key_env", "passphrase_env"},
        ) and isinstance(auth.get("private_key_env"), str) and (
            "passphrase_env" not in auth or isinstance(auth.get("passphrase_env"), str)
        ):
            credential_names = {auth["private_key_env"]}
            if "passphrase_env" in auth:
                credential_names.add(auth["passphrase_env"])
        elif auth == {"type": "agent"}:
            if "SSH_AUTH_SOCK" in credentials:
                credential_names = {"SSH_AUTH_SOCK"}
                socket_path = credentials["SSH_AUTH_SOCK"]
                if not _valid_agent_socket_path(socket_path):
                    raise SshServiceProcessError("the desktop host returned an invalid SSH agent socket path")
            else:
                credential_names = set()
        else:
            raise SshServiceProcessError("the desktop host returned an unsupported SSH identity")
        if "proxy" in target:
            proxy_auth_env = _validate_http_connect_proxy(target["proxy"])
            if proxy_auth_env is not None:
                credential_names.add(proxy_auth_env)
        if (
            not all(_valid_credential_env_name(name) for name in credential_names)
            or set(credentials) != credential_names
            or not all(
                isinstance(value, str)
                and bool(value)
                and "\x00" not in value
                and len(value.encode("utf-8")) <= 1024 * 1024
                for value in credentials.values()
            )
        ):
            raise SshServiceProcessError("the desktop host returned invalid SSH credentials")

        listen_port = _unused_loopback_port()
        token = secrets.token_urlsafe(32)
        document = {
            "listen_port": listen_port,
            "api_token_env": "YUKINAL_SSH_API_TOKEN",
            "targets": [target],
        }
        temporary_directory = tempfile.TemporaryDirectory(prefix="yukinal-ssh-service-")
        config = Path(temporary_directory.name) / "service.json"
        try:
            config.write_text(
                json.dumps(document, separators=(",", ":"), ensure_ascii=False),
                encoding="utf-8",
            )
            if os.name != "nt":
                config.chmod(0o600)
        except OSError:
            temporary_directory.cleanup()
            raise SshServiceProcessError("the temporary SSH service configuration could not be created") from None

        instance = cls(binary, config)
        instance._token_env = "YUKINAL_SSH_API_TOKEN"
        instance._token_value = token
        instance._secret_env = set(credential_names)
        instance._secret_values = dict(credentials)
        instance._port = listen_port
        instance._temporary_directory = temporary_directory
        return instance, token, f"http://127.0.0.1:{listen_port}"

    async def start(self) -> None:
        if self._process is not None:
            if self._process.returncode is None:
                return
            raise SshServiceProcessError("the C++ SSH service has already exited")

        if await _health_check(self._port):
            raise SshServiceProcessError(
                "another HTTP service is already responding on the configured SSH service port"
            )

        runtime_names = {key.upper() for key in self._RUNTIME_ENV}
        env = {
            name: value
            for name, value in os.environ.items()
            if name.upper() in runtime_names
        }
        env[self._token_env] = self._token_value
        for name in self._secret_env:
            value = (
                self._secret_values.get(name)
                if self._secret_values is not None
                else os.environ.get(name)
            )
            if not value:
                raise SshServiceProcessError(f"the SSH service credential environment variable {name} is not set")
            env[name] = value

        creationflags = getattr(subprocess, "CREATE_NO_WINDOW", 0) if os.name == "nt" else 0
        try:
            self._process = await asyncio.create_subprocess_exec(
                str(self._binary),
                "--config",
                str(self._config),
                "--parent-pid",
                str(os.getpid()),
                stdin=asyncio.subprocess.DEVNULL,
                stdout=asyncio.subprocess.DEVNULL,
                stderr=None,
                env=env,
                creationflags=creationflags,
            )
        except OSError:
            raise SshServiceProcessError("the C++ SSH service could not be launched") from None

        deadline = asyncio.get_running_loop().time() + 8.0
        while asyncio.get_running_loop().time() < deadline:
            if self._process.returncode is not None:
                code = self._process.returncode
                self._process = None
                raise SshServiceProcessError(f"the C++ SSH service exited during startup (code {code})")
            if await _health_check(self._port):
                await asyncio.sleep(0.05)
                if self._process.returncode is not None:
                    code = self._process.returncode
                    self._process = None
                    raise SshServiceProcessError(
                        f"the C++ SSH service exited during startup (code {code})"
                    )
                print(f"C++ SSH service ready on 127.0.0.1:{self._port}", file=sys.stderr, flush=True)
                return
            await asyncio.sleep(0.1)

        await self.stop()
        raise SshServiceProcessError("the C++ SSH service did not become healthy before the startup deadline")

    async def stop(self) -> None:
        process, self._process = self._process, None
        try:
            if process is None:
                return
            if process.returncode is not None:
                await process.wait()
                return
            try:
                process.terminate()
                await asyncio.wait_for(process.wait(), timeout=3.0)
            except TimeoutError:
                try:
                    process.kill()
                except ProcessLookupError:
                    pass
                await process.wait()
            except ProcessLookupError:
                await process.wait()
        finally:
            if self._secret_values is not None:
                self._secret_values.clear()
                self._secret_values = None
            self._token_value = ""
            if self._temporary_directory is not None:
                self._temporary_directory.cleanup()
                self._temporary_directory = None


def _valid_env_name(value: object) -> bool:
    if not isinstance(value, str) or not value or len(value) > 128:
        return False
    if not value[0].isascii() or not (value[0].isalpha() or value[0] == "_"):
        return False
    return all(character.isascii() and (character.isalnum() or character == "_") for character in value)


def _valid_credential_env_name(value: object) -> bool:
    if not _valid_env_name(value) or value.upper().startswith("YUKINAL_"):
        return False
    reserved = {name.upper() for name in SshServiceProcess._RUNTIME_ENV}
    return value.upper() not in reserved


def _validate_http_connect_proxy(value: object) -> str | None:
    if not isinstance(value, dict):
        raise SshServiceProcessError("the SSH service HTTP proxy configuration is invalid")
    required = {"type", "host", "port"}
    if set(value) not in (required, required | {"auth_env"}):
        raise SshServiceProcessError("the SSH service HTTP proxy configuration is invalid")
    host = value.get("host")
    port = value.get("port")
    if (
        value.get("type") != "http_connect"
        or not isinstance(host, str)
        or not host
        or len(host) > 253
        or any(ord(character) <= 0x20 or ord(character) == 0x7F for character in host)
        or isinstance(port, bool)
        or not isinstance(port, int)
        or not 1 <= port <= 65535
    ):
        raise SshServiceProcessError("the SSH service HTTP proxy configuration is invalid")
    if "auth_env" not in value:
        return None
    auth_env = value["auth_env"]
    if not _valid_credential_env_name(auth_env):
        raise SshServiceProcessError("the SSH service proxy credential reference is invalid")
    return auth_env


def _valid_agent_socket_path(value: object) -> bool:
    if not isinstance(value, str) or not value or "\x00" in value:
        return False
    try:
        return len(value.encode("utf-8")) <= 4096
    except UnicodeEncodeError:
        return False


async def _health_check(port: int) -> bool:
    writer: asyncio.StreamWriter | None = None
    try:
        async with asyncio.timeout(0.75):
            reader, writer = await asyncio.open_connection("127.0.0.1", port)
            writer.write(
                b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            )
            await writer.drain()
            status = await reader.readline()
            total = 0
            while True:
                line = await reader.readline()
                total += len(line)
                if not line or line in (b"\r\n", b"\n") or total > 4096:
                    break
            body = await reader.read(512)
            return status.startswith(b"HTTP/1.1 200 ") and body.strip() == b'{"status":"healthy"}'
    except (OSError, TimeoutError, asyncio.IncompleteReadError, ValueError):
        return False
    finally:
        if writer is not None:
            writer.close()
            try:
                await writer.wait_closed()
            except OSError:
                pass


def _unused_loopback_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as server:
        server.bind(("127.0.0.1", 0))
        return int(server.getsockname()[1])
