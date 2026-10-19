from __future__ import annotations

import asyncio
import ipaddress
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


class ServerExecDenylistError(ValueError):
    """A configured server.exec command denylist is malformed.

    Always names the offending offset and never echoes the pattern text.
    """


MAX_SERVER_EXEC_DENYLIST_ENTRIES = 256
MAX_SERVER_EXEC_DENYLIST_PATTERN_BYTES = 1024


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
        try:
            if "server_exec_denylist" in document:
                _validate_server_exec_denylist(
                    document["server_exec_denylist"], label="the configured SSH service file"
                )
        except ServerExecDenylistError as error:
            raise SshServiceProcessError(str(error)) from None
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
            try:
                if "server_exec_denylist" in target:
                    _validate_server_exec_denylist(
                        target["server_exec_denylist"],
                        label="the configured SSH service target",
                    )
            except ServerExecDenylistError as error:
                raise SshServiceProcessError(str(error)) from None
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
                proxy_auth_env = _validate_cpp_proxy(target["proxy"])
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
        required_target_fields = {"target_id", "host", "port", "username", "auth"}
        optional_target_fields = {
            "host_key_fingerprint", "host_certificate_authority", "proxy", "allow_server_exec",
            "server_exec_denylist",
        }
        trust_sources = {"host_key_fingerprint", "host_certificate_authority"} & set(target)
        if (
            not required_target_fields.issubset(target)
            or not (set(target) - required_target_fields).issubset(optional_target_fields)
            or len(trust_sources) != 1
            or ("allow_server_exec" in target and not isinstance(target["allow_server_exec"], bool))
        ):
            raise SshServiceProcessError("the desktop host returned an invalid SSH target")
        try:
            if "server_exec_denylist" in payload:
                _validate_server_exec_denylist(
                    payload["server_exec_denylist"], label="the desktop SSH target payload"
                )
            if "server_exec_denylist" in target:
                _validate_server_exec_denylist(
                    target["server_exec_denylist"], label="the desktop SSH target"
                )
        except ServerExecDenylistError:
            raise SshServiceProcessError("the desktop host returned an invalid SSH target") from None
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
            or (
                "host_key_fingerprint" in target
                and (
                    not isinstance(fingerprint, str)
                    or not re.fullmatch(r"SHA256:[A-Za-z0-9+/]{43}", fingerprint)
                )
            )
            or (
                "host_certificate_authority" in target
                and not _valid_host_certificate_authority(target["host_certificate_authority"])
            )
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
            {"type", "private_key_env", "public_key_file"},
            {"type", "private_key_env", "passphrase_env", "public_key_file"},
        ) and isinstance(auth.get("private_key_env"), str) and (
            "passphrase_env" not in auth or isinstance(auth.get("passphrase_env"), str)
        ) and (
            "public_key_file" not in auth
            or (
                isinstance(auth.get("public_key_file"), str)
                and bool(auth["public_key_file"])
                and len(auth["public_key_file"]) <= 4096
                and not any(ord(character) <= 0x1F or ord(character) == 0x7F for character in auth["public_key_file"])
                and "\x00" not in auth["public_key_file"]
            )
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
            proxy_auth_env = _validate_cpp_proxy(target["proxy"])
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


def _validate_server_exec_denylist(value: object, *, label: str) -> None:
    """Validate a `server_exec_denylist` array without echoing any pattern text."""
    if not isinstance(value, list):
        raise ServerExecDenylistError(f"{label} server_exec_denylist must be a JSON array")
    if len(value) > MAX_SERVER_EXEC_DENYLIST_ENTRIES:
        raise ServerExecDenylistError(
            f"{label} server_exec_denylist must contain at most "
            f"{MAX_SERVER_EXEC_DENYLIST_ENTRIES} entries"
        )
    for index, entry in enumerate(value):
        if isinstance(entry, str):
            pattern = entry
        elif isinstance(entry, dict):
            unknown = set(entry) - {"pattern", "regex", "case_insensitive"}
            if unknown:
                raise ServerExecDenylistError(
                    f"{label} server_exec_denylist[{index}] contains an unsupported key"
                )
            pattern = entry.get("pattern")
            for boolean_key in ("regex", "case_insensitive"):
                if boolean_key in entry and not isinstance(entry[boolean_key], bool):
                    raise ServerExecDenylistError(
                        f"{label} server_exec_denylist[{index}].{boolean_key} must be a boolean"
                    )
        else:
            raise ServerExecDenylistError(
                f"{label} server_exec_denylist[{index}] must be a string or an object"
            )
        if not isinstance(pattern, str) or not pattern.strip():
            raise ServerExecDenylistError(
                f"{label} server_exec_denylist[{index}].pattern must be a non-empty string"
            )
        if "\x00" in pattern:
            raise ServerExecDenylistError(
                f"{label} server_exec_denylist[{index}].pattern must not contain NUL"
            )
        try:
            pattern_bytes = len(pattern.encode("utf-8"))
        except UnicodeEncodeError:
            raise ServerExecDenylistError(
                f"{label} server_exec_denylist[{index}].pattern must be encodable as UTF-8"
            ) from None
        if pattern_bytes > MAX_SERVER_EXEC_DENYLIST_PATTERN_BYTES:
            raise ServerExecDenylistError(
                f"{label} server_exec_denylist[{index}].pattern must contain at most "
                f"{MAX_SERVER_EXEC_DENYLIST_PATTERN_BYTES} UTF-8 bytes"
            )


def _validate_cpp_proxy(value: object) -> str | None:
    if not isinstance(value, dict):
        raise SshServiceProcessError("the SSH service proxy configuration is invalid")
    auth_env = value.get("auth_env")
    allowed_suffix = {"auth_env", "no_proxy"}
    if "url" in value:
        if not set(value).issubset({"url", *allowed_suffix}) or not _valid_cpp_proxy_url(value["url"]):
            raise SshServiceProcessError("the SSH service proxy URL is invalid")
    else:
        if not set(value).issubset({"type", "host", "port", *allowed_suffix}):
            raise SshServiceProcessError("the SSH service proxy configuration is invalid")
        proxy_type = value.get("type")
        host = value.get("host")
        port = value.get("port")
        if (
            proxy_type not in {"http_connect", "https_connect", "socks5", "socks5h"}
            or not _valid_cpp_proxy_host(host)
            or (
                port is not None
                and (isinstance(port, bool) or not isinstance(port, int) or not 1 <= port <= 65535)
            )
        ):
            raise SshServiceProcessError("the SSH service proxy configuration is invalid")
    if "no_proxy" in value:
        bypasses = value["no_proxy"]
        if isinstance(bypasses, str) and len(bypasses) <= 65536:
            bypasses = [entry.strip() for entry in bypasses.split(",") if entry.strip()]
        if not isinstance(bypasses, list) or len(bypasses) > 256 or not all(
            _valid_cpp_no_proxy_entry(entry) for entry in bypasses
        ):
            raise SshServiceProcessError("the SSH service proxy bypass configuration is invalid")
    if "auth_env" not in value:
        return None
    if not _valid_credential_env_name(auth_env):
        raise SshServiceProcessError("the SSH service proxy credential reference is invalid")
    return auth_env


def _valid_cpp_proxy_url(value: object) -> bool:
    if (
        not isinstance(value, str)
        or not value
        or len(value) > 4096
        or any(ord(character) <= 0x20 or ord(character) == 0x7F for character in value)
        or "?" in value
        or "#" in value
    ):
        return False
    try:
        parsed = urlsplit(value)
        host = parsed.hostname
        port = parsed.port
    except ValueError:
        return False
    defaults = {"http": 80, "https": 443, "socks5": 1080, "socks5h": 1080}
    return (
        parsed.scheme.lower() in defaults
        and bool(parsed.netloc)
        and parsed.username is None
        and parsed.password is None
        and "@" not in parsed.netloc
        and parsed.path in {"", "/"}
        and not parsed.query
        and not parsed.fragment
        and _valid_cpp_proxy_host(host)
        and (port is None or 1 <= port <= 65535)
    )


def _valid_cpp_proxy_host(value: object) -> bool:
    if not isinstance(value, str) or not value or len(value) > 253 or not value.isascii():
        return False
    if any(ord(character) <= 0x20 or ord(character) == 0x7F for character in value):
        return False
    if ":" in value:
        if "%" in value:
            return False
        try:
            return isinstance(ipaddress.ip_address(value), ipaddress.IPv6Address)
        except ValueError:
            return False
    return all(character.isalnum() or character in ".-_" for character in value)


def _valid_host_certificate_authority(value: object) -> bool:
    if not isinstance(value, dict):
        return False
    required = {"ca_public_key", "principals"}
    optional = {"revocation_list_path", "revocation_list_url", "revocation_list_signers"}
    if not required.issubset(value) or not set(value).issubset(required | optional):
        return False
    ca_public_key = value["ca_public_key"]
    principals = value["principals"]
    if (
        not isinstance(ca_public_key, str)
        or not ca_public_key.strip()
        or len(ca_public_key) > 16384
        or any(ord(character) < 0x20 or ord(character) == 0x7F for character in ca_public_key)
        or not isinstance(principals, list)
        or not 1 <= len(principals) <= 256
        or not all(
            isinstance(principal, str)
            and 0 < len(principal) <= 253
            and not any(ord(character) < 0x20 or ord(character) == 0x7F for character in principal)
            for principal in principals
        )
        or len(set(principals)) != len(principals)
    ):
        return False
    revocation_path = value.get("revocation_list_path")
    revocation_url = value.get("revocation_list_url")
    for optional_value in (revocation_path, revocation_url):
        if optional_value is not None and (
            not isinstance(optional_value, str)
            or not optional_value.strip()
            or len(optional_value) > 4096
            or "\x00" in optional_value
            or any(ord(character) < 0x20 or ord(character) == 0x7F for character in optional_value)
        ):
            return False
    if revocation_path and not Path(revocation_path).is_absolute():
        return False
    if revocation_path and revocation_url:
        return False
    signers = value.get("revocation_list_signers", [])
    return (
        isinstance(signers, list)
        and len(signers) <= 16
        and all(
            isinstance(signer, str)
            and bool(signer.strip())
            and len(signer) <= 16384
            and not any(ord(character) < 0x20 or ord(character) == 0x7F for character in signer)
            for signer in signers
        )
    )


def _valid_cpp_no_proxy_entry(value: object) -> bool:
    if not isinstance(value, str) or not value or len(value) > 260:
        return False
    if value == "*":
        return True
    if "/" in value:
        try:
            ipaddress.ip_network(value, strict=False)
        except ValueError:
            return False
        return True
    host_entry = value
    port_text: str | None = None
    if value.startswith("["):
        close = value.find("]")
        if close == -1:
            return False
        suffix = value[close + 1 :]
        if suffix:
            if not suffix.startswith(":"):
                return False
            host_entry = value[: close + 1]
            port_text = suffix[1:]
    elif ":" in value:
        if value.count(":") != 1:
            return False
        host_entry, port_text = value.rsplit(":", 1)
    if port_text is not None and (
        not port_text.isascii()
        or not port_text.isdigit()
        or not 1 <= int(port_text) <= 65535
    ):
        return False
    if host_entry == "*":
        return True
    if host_entry.startswith("*."):
        host = host_entry[2:]
    elif host_entry.startswith("."):
        host = host_entry[1:]
    elif host_entry.endswith(".*"):
        prefix = host_entry[:-2]
        octets = prefix.split(".")
        return bool(prefix) and len(octets) <= 3 and all(
            octet
            and len(octet) <= 3
            and octet.isascii()
            and octet.isdigit()
            and int(octet) <= 255
            for octet in octets
        )
    else:
        host = host_entry
    bracketed_ipv6 = (
        len(host) >= 4
        and host.startswith("[")
        and host.endswith("]")
        and ":" in host
    )
    return (
        bool(host)
        and len(host) <= 253
        and not host.startswith(".")
        and not host.endswith(".")
        and ".." not in host
        and (":" not in host or bracketed_ipv6)
        and all(
            character.isascii()
            and (character.isalnum() or character in ".-_[]:")
            for character in host
        )
    )


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
