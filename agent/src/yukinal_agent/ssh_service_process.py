from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
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
        self._port = 0

    @classmethod
    def from_environment(cls) -> SshServiceProcess | None:
        binary_value = os.environ.get("YUKINAL_SSH_SERVICE_BINARY", "").strip()
        config_value = os.environ.get("YUKINAL_SSH_SERVICE_CONFIG", "").strip()
        if not binary_value and not config_value:
            return None
        if not binary_value or not config_value:
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
        for target in targets:
            if not isinstance(target, dict):
                raise SshServiceProcessError("the SSH service target allowlist is invalid")
            auth = target.get("auth")
            if not isinstance(auth, dict):
                raise SshServiceProcessError("the SSH service target authentication configuration is invalid")
            for key in ("password_env", "passphrase_env", "private_key_env"):
                name = auth.get(key)
                if name is not None:
                    if not _valid_credential_env_name(name):
                        raise SshServiceProcessError("the SSH service credential environment reference is invalid")
                    if any(existing != name and existing.casefold() == name.casefold() for existing in secret_env):
                        raise SshServiceProcessError("SSH service environment references differ only by letter case")
                    secret_env.add(name)

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
            value = os.environ.get(name)
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
