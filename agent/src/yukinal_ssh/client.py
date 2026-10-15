from __future__ import annotations

import base64
import binascii
import http.client
import json
import re
import secrets
import socket
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any

from .errors import SshServiceError, SshServiceTransportError

_TARGET_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$")
_SERVICE_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.@:-]{0,127}$")
_CONTAINER_REF = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$")
_PACKAGE_REF = re.compile(r"^[A-Za-z0-9][A-Za-z0-9+._:-]{0,127}$")
_PACKAGE_VERSION = re.compile(r"^[A-Za-z0-9][A-Za-z0-9+._:~^-]{0,127}$")
_MAX_BODY_BYTES = 2 * 1024 * 1024
_TOKEN = re.compile(r"^[A-Za-z0-9._~-]{32,256}$")
_MAX_SFTP_WRITE_BYTES = 512 * 1024
_MAX_SFTP_EXCLUSIVE_BYTES = 1024 * 1024
_MAX_SFTP_READ_BYTES = 1024 * 1024
_SYSTEM_SNAPSHOT_COMMAND_IDS = (
    "os_detect",
    "os_release",
    "os_kernel",
    "os_arch",
    "os_hostname",
    "cpu_detect",
    "cpu_ticks_a",
    "cpu_ticks_b",
    "cpu_cores",
    "cpu_model",
    "cpu_load",
    "memory_detect",
    "memory_info",
    "uptime_detect",
    "uptime_info",
    "disk_detect",
    "disk_usage",
    "network_detect",
    "network_info",
    "docker_detect",
    "docker_info",
    "docker_ps",
)


@dataclass(frozen=True)
class SshOperationResult:
    target_id: str
    operation: str
    exit_code: int
    stdout: str
    stderr: str
    duration_ms: int
    output_truncated: bool


@dataclass(frozen=True)
class SshServerExecResult:
    exit_code: int
    stdout: str
    stderr: str
    stdout_truncated: bool
    stderr_truncated: bool
    duration_ms: int


ServiceStatusResult = SshOperationResult


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req: urllib.request.Request, fp: Any, code: int, msg: str,
                         headers: Any, newurl: str) -> None:
        return None


def _parse_system_snapshot_output(output: str, nonce: str) -> dict[str, tuple[int, str]]:
    if (
        not isinstance(output, str)
        or not isinstance(nonce, str)
        or re.fullmatch(r"[0-9a-f]{32}", nonce) is None
    ):
        raise SshServiceTransportError(
            "invalid_response", "SSH service returned an invalid system snapshot"
        )

    results: dict[str, tuple[int, str]] = {}
    for command_id in _SYSTEM_SNAPSHOT_COMMAND_IDS:
        start_marker = f"__YK_SNAPSHOT_{nonce}_{command_id}_BEGIN__\n"
        if output.count(start_marker) != 1:
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned an incomplete system snapshot"
            )
        data_start = output.find(start_marker) + len(start_marker)
        end_prefix = f"\n__YK_SNAPSHOT_{nonce}_{command_id}_END_"
        end = output.find(end_prefix, data_start)
        if end < 0 or output.find(end_prefix, end + len(end_prefix)) >= 0:
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned an incomplete system snapshot"
            )
        status_start = end + len(end_prefix)
        status_end = output.find("__\n", status_start)
        status = output[status_start:status_end] if status_end >= 0 else ""
        if re.fullmatch(r"[0-9]{1,3}", status) is None or int(status) > 255:
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned an invalid system snapshot status"
            )
        results[command_id] = (int(status), output[data_start:end])
    return results


class SshServiceClient:
    """Calls the loopback-only service using structured, allow-listed operations."""

    def __init__(self, token: str, base_url: str = "http://127.0.0.1:8765") -> None:
        if not isinstance(token, str) or not _TOKEN.fullmatch(token):
            raise ValueError("token must be 32 to 256 URL-safe characters")
        parsed = urllib.parse.urlsplit(base_url)
        if (
            parsed.scheme != "http"
            or parsed.hostname != "127.0.0.1"
            or parsed.username is not None
            or parsed.password is not None
            or parsed.path not in ("", "/")
            or parsed.query
            or parsed.fragment
            or parsed.port is None
            or not 1 <= parsed.port <= 65535
        ):
            raise ValueError("base_url must be an http URL on 127.0.0.1")
        self._base_url = f"http://127.0.0.1:{parsed.port}"
        self._token = token
        self._opener = urllib.request.build_opener(_NoRedirect())

    def health(self) -> bool:
        response = self._request("GET", "/health", None, timeout=3, authorized=False)
        if response != {"status": "healthy"}:
            raise SshServiceTransportError("invalid_response", "SSH service health response was invalid")
        return True

    def service_status(self, target_id: str, service: str, timeout_seconds: int = 10) -> ServiceStatusResult:
        if not isinstance(service, str) or not _SERVICE_NAME.fullmatch(service):
            raise ValueError("service must be a valid systemd unit name")
        return self._execute_operation(
            target_id, "service_status", {"service": service}, timeout_seconds
        )

    def connection_check(self, target_id: str, timeout_seconds: int = 10) -> SshOperationResult:
        """Verify SSH authentication for the configured target without running a command."""
        return self._execute_operation(target_id, "connection_check", {}, timeout_seconds)

    def service_inventory(self, target_id: str, timeout_seconds: int = 10) -> SshOperationResult:
        """List services through the fixed, read-only inventory operation."""
        return self._execute_operation(target_id, "service_inventory", {}, timeout_seconds)

    def docker_ps(
        self, target_id: str, include_all_containers: bool = False, timeout_seconds: int = 10
    ) -> SshOperationResult:
        if not isinstance(include_all_containers, bool):
            raise ValueError("include_all_containers must be a boolean")
        return self._execute_operation(
            target_id, "docker_ps", {"all": include_all_containers}, timeout_seconds
        )

    def docker_logs(
        self, target_id: str, container: str, tail: int = 120, timeout_seconds: int = 10
    ) -> SshOperationResult:
        if not isinstance(container, str) or not _CONTAINER_REF.fullmatch(container):
            raise ValueError("container must be a safe container reference")
        if isinstance(tail, bool) or not isinstance(tail, int) or not 1 <= tail <= 500:
            raise ValueError("tail must be an integer from 1 to 500")
        return self._execute_operation(
            target_id, "docker_logs", {"container": container, "tail": tail}, timeout_seconds
        )

    def docker_inspect(
        self, target_id: str, container: str, timeout_seconds: int = 10
    ) -> SshOperationResult:
        if not isinstance(container, str) or not _CONTAINER_REF.fullmatch(container):
            raise ValueError("container must be a safe container reference")
        return self._execute_operation(
            target_id, "docker_inspect", {"container": container}, timeout_seconds
        )

    def docker_restart(
        self, target_id: str, container: str, grace_period_seconds: int = 10
    ) -> SshOperationResult:
        """Restart one validated Docker container through the structured operation."""
        if not isinstance(container, str) or not _CONTAINER_REF.fullmatch(container):
            raise ValueError("container must be a safe container reference")
        if (
            isinstance(grace_period_seconds, bool)
            or not isinstance(grace_period_seconds, int)
            or not 1 <= grace_period_seconds <= 120
        ):
            raise ValueError("grace_period_seconds must be an integer from 1 to 120")
        return self._execute_operation(
            target_id,
            "docker_restart",
            {"container": container, "grace_period_seconds": grace_period_seconds},
            30,
        )

    def systemd_inspect(
        self, target_id: str, service: str, timeout_seconds: int = 10
    ) -> SshOperationResult:
        if (
            not isinstance(service, str)
            or not _SERVICE_NAME.fullmatch(service)
            or not service.endswith(".service")
        ):
            raise ValueError("service must be a valid .service unit name")
        return self._execute_operation(
            target_id, "systemd_inspect", {"service": service}, timeout_seconds
        )

    def systemd_restart(
        self, target_id: str, service: str, timeout_seconds: int = 10
    ) -> SshOperationResult:
        """Restart one validated systemd unit through the structured operation."""
        if (
            not isinstance(service, str)
            or not _SERVICE_NAME.fullmatch(service)
            or not service.endswith(".service")
        ):
            raise ValueError("service must be a valid .service unit name")
        if (
            isinstance(timeout_seconds, bool)
            or not isinstance(timeout_seconds, int)
            or not 1 <= timeout_seconds <= 30
        ):
            raise ValueError("timeout_seconds must be an integer from 1 to 30")
        return self._execute_operation(
            target_id,
            "systemd_restart",
            {"service": service, "timeout_seconds": timeout_seconds},
            timeout_seconds,
        )

    def package_inspect(
        self, target_id: str, manager: str, package: str, timeout_seconds: int = 10
    ) -> SshOperationResult:
        if manager not in ("apt", "dnf"):
            raise ValueError("manager must be apt or dnf")
        if not isinstance(package, str) or not _PACKAGE_REF.fullmatch(package):
            raise ValueError("package must be a safe package reference")
        return self._execute_operation(
            target_id, "package_inspect", {"manager": manager, "package": package}, timeout_seconds
        )

    def package_install(
        self,
        target_id: str,
        manager: str,
        package: str,
        version: str | None = None,
        timeout_seconds: int = 180,
    ) -> SshOperationResult:
        """Install one validated apt or dnf package through the structured operation."""
        if manager not in ("apt", "dnf"):
            raise ValueError("manager must be apt or dnf")
        if not isinstance(package, str) or not _PACKAGE_REF.fullmatch(package):
            raise ValueError("package must be a safe package reference")
        if version is not None and (
            not isinstance(version, str) or not _PACKAGE_VERSION.fullmatch(version)
        ):
            raise ValueError("version must be a safe package version")
        if (
            isinstance(timeout_seconds, bool)
            or not isinstance(timeout_seconds, int)
            or not 1 <= timeout_seconds <= 600
        ):
            raise ValueError("timeout_seconds must be an integer from 1 to 600")
        parameters: dict[str, Any] = {"manager": manager, "package": package}
        if version is not None:
            parameters["version"] = version
        parameters["timeout_seconds"] = timeout_seconds
        return self._execute_operation(
            target_id,
            "package_install",
            parameters,
            timeout_seconds,
            max_timeout_seconds=600,
        )

    def server_exec(
        self,
        target_id: str,
        command: str,
        timeout_ms: int,
        max_output_bytes: int,
    ) -> SshServerExecResult:
        """Run one approved, bounded command through the one-shot server.exec capability."""
        if not isinstance(target_id, str) or not _TARGET_ID.fullmatch(target_id):
            raise ValueError("target_id has an invalid format")
        if (
            not isinstance(command, str)
            or not command.strip()
            or "\x00" in command
            or len(command.encode("utf-8")) > 131_072
        ):
            raise ValueError("command must contain 1 to 131072 UTF-8 bytes and no NUL")
        if (
            isinstance(timeout_ms, bool)
            or not isinstance(timeout_ms, int)
            or not 1 <= timeout_ms <= 120_000
        ):
            raise ValueError("timeout_ms must be an integer from 1 to 120000")
        if (
            isinstance(max_output_bytes, bool)
            or not isinstance(max_output_bytes, int)
            or not 1 <= max_output_bytes <= 65_536
        ):
            raise ValueError("max_output_bytes must be an integer from 1 to 65536")

        timeout_seconds = (timeout_ms + 999) // 1000
        body = self._request(
            "POST",
            "/v1/ssh/exec",
            {
                "target_id": target_id,
                "operation": "server_exec",
                "parameters": {
                    "command": command,
                    "max_output_bytes": max_output_bytes,
                    "timeout_ms": timeout_ms,
                },
                "timeout_seconds": timeout_seconds,
            },
            timeout=timeout_seconds + 5,
            authorized=True,
        )
        expected = {
            "status", "target_id", "operation", "exit_code", "stdout", "stderr",
            "stdout_truncated", "stderr_truncated", "duration_ms", "output_truncated", "error",
        }
        if not isinstance(body, dict) or set(body) != expected:
            raise SshServiceTransportError("invalid_response", "SSH service returned an invalid server.exec result")
        exit_code = body.get("exit_code")
        stdout = body.get("stdout")
        stderr = body.get("stderr")
        stdout_truncated = body.get("stdout_truncated")
        stderr_truncated = body.get("stderr_truncated")
        duration_ms = body.get("duration_ms")
        if (
            body.get("status") != "succeeded"
            or body.get("target_id") != target_id
            or body.get("operation") != "server_exec"
            or isinstance(exit_code, bool)
            or not isinstance(exit_code, int)
            or not 0 <= exit_code <= 255
            or not isinstance(stdout, str)
            or not isinstance(stderr, str)
            or not isinstance(stdout_truncated, bool)
            or not isinstance(stderr_truncated, bool)
            or isinstance(duration_ms, bool)
            or not isinstance(duration_ms, int)
            or duration_ms < 0
            or not isinstance(body.get("output_truncated"), bool)
            or body["output_truncated"] != (stdout_truncated or stderr_truncated)
            or body.get("error") is not None
            or len(stdout.encode("utf-8")) + len(stderr.encode("utf-8")) > max_output_bytes * 3
        ):
            raise SshServiceTransportError("invalid_response", "SSH service returned an invalid server.exec result")
        return SshServerExecResult(
            exit_code=exit_code,
            stdout=stdout,
            stderr=stderr,
            stdout_truncated=stdout_truncated,
            stderr_truncated=stderr_truncated,
            duration_ms=duration_ms,
        )

    def server_logs(
        self,
        target_id: str,
        since_seconds: int | None = None,
        unit: str | None = None,
        timeout_seconds: int = 10,
    ) -> SshOperationResult:
        """Read a bounded journal or system log snapshot through a fixed operation."""
        parameters: dict[str, Any] = {}
        if since_seconds is not None:
            if (
                isinstance(since_seconds, bool)
                or not isinstance(since_seconds, int)
                or not 1 <= since_seconds <= 604_800
            ):
                raise ValueError("since_seconds must be an integer from 1 to 604800")
            parameters["since_seconds"] = since_seconds
        if unit is not None:
            if not isinstance(unit, str) or not _SERVICE_NAME.fullmatch(unit) or not unit.endswith(".service"):
                raise ValueError("unit must be a bounded .service reference")
            parameters["unit"] = unit
        return self._execute_operation(target_id, "server_logs", parameters, timeout_seconds)

    def system_snapshot(
        self, target_id: str, timeout_seconds: int = 10
    ) -> dict[str, tuple[int, str]]:
        """Collect fixed system snapshot commands and split their nonce markers."""
        nonce = secrets.token_hex(16)
        result = self._execute_operation(
            target_id, "system_snapshot", {"nonce": nonce}, timeout_seconds
        )
        if result.exit_code != 0:
            raise SshServiceError(
                "execution_failed", "could not collect a system snapshot", http_status=502
            )
        return _parse_system_snapshot_output(result.stdout, nonce)

    def sftp_write(
        self, target_id: str, path: str, content: bytes, timeout_seconds: int = 10
    ) -> SshOperationResult:
        """Overwrite one remote file with a bounded binary payload."""
        if not isinstance(content, bytes) or len(content) > _MAX_SFTP_WRITE_BYTES:
            raise ValueError("content must be bytes no larger than 512 KiB")
        self._validate_sftp_path(path)
        return self._execute_operation(
            target_id,
            "sftp_write",
            {"path": path, "content_base64": base64.b64encode(content).decode("ascii")},
            timeout_seconds,
        )

    def sftp_read(
        self, target_id: str, path: str, max_bytes: int, timeout_seconds: int = 10
    ) -> bytes:
        """Read at most max_bytes plus one truncation byte from a remote file."""
        if (
            isinstance(max_bytes, bool)
            or not isinstance(max_bytes, int)
            or not 0 <= max_bytes <= _MAX_SFTP_READ_BYTES
        ):
            raise ValueError("max_bytes must be an integer from 0 to 1 MiB")
        self._validate_sftp_path(path)
        result = self._execute_operation(
            target_id, "sftp_read", {"path": path, "max_bytes": max_bytes}, timeout_seconds
        )
        try:
            content = base64.b64decode(result.stdout, validate=True)
        except (ValueError, binascii.Error):
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned invalid SFTP file contents"
            ) from None
        if len(content) > max_bytes + 1:
            raise SshServiceTransportError(
                "invalid_response", "SSH service exceeded the requested SFTP read limit"
            )
        return content

    def sftp_stat(
        self, target_id: str, path: str, timeout_seconds: int = 10
    ) -> dict[str, int | str | None]:
        """Read no-follow metadata for one remote path."""
        self._validate_sftp_path(path)
        result = self._execute_operation(target_id, "sftp_stat", {"path": path}, timeout_seconds)
        try:
            payload = json.loads(result.stdout)
        except json.JSONDecodeError:
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned invalid SFTP file metadata"
            ) from None
        if not isinstance(payload, dict) or set(payload) != {"kind", "size", "modified"}:
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned invalid SFTP file metadata"
            )
        kind = payload["kind"]
        size = payload["size"]
        modified = payload["modified"]
        if (
            not isinstance(kind, str)
            or kind not in {"file", "directory", "symlink", "other"}
            or isinstance(size, bool)
            or not isinstance(size, int)
            or size < 0
            or (modified is not None and (
                isinstance(modified, bool) or not isinstance(modified, int) or not 0 <= modified <= 0xFFFFFFFF
            ))
        ):
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned invalid SFTP file metadata"
            )
        return {"kind": kind, "size": size, "modified": modified}

    def sftp_replace_guarded(
        self,
        target_id: str,
        path: str,
        content: bytes,
        *,
        expected_size: int,
        expected_modified: int | None,
        expected_digest: str,
        timeout_seconds: int = 10,
    ) -> dict[str, int | None]:
        """Atomically replace one regular file if its metadata and SHA-256 still match."""
        self._validate_sftp_path(path)
        if (
            not path.startswith("/")
            or "\r" in path
            or "\n" in path
            or not isinstance(content, bytes)
            or len(content) > 1024 * 1024
            or isinstance(expected_size, bool)
            or not isinstance(expected_size, int)
            or not 0 <= expected_size <= 1024 * 1024
            or (expected_modified is not None and (
                isinstance(expected_modified, bool)
                or not isinstance(expected_modified, int)
                or not 0 <= expected_modified <= 0xFFFFFFFF
            ))
            or not isinstance(expected_digest, str)
            or len(expected_digest) != 64
            or re.fullmatch(r"[0-9a-fA-F]{64}", expected_digest) is None
        ):
            raise ValueError("guarded SFTP replacement parameters are invalid")
        result = self._execute_operation(
            target_id,
            "sftp_replace_guarded",
            {
                "path": path,
                "expected_size": expected_size,
                "expected_modified": expected_modified,
                "expected_digest": expected_digest,
                "content_base64": base64.b64encode(content).decode("ascii"),
            },
            timeout_seconds,
        )
        try:
            payload = json.loads(result.stdout)
        except json.JSONDecodeError:
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned invalid guarded replacement metadata"
            ) from None
        if not isinstance(payload, dict) or set(payload) != {"size", "modified"}:
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned invalid guarded replacement metadata"
            )
        size = payload["size"]
        modified = payload["modified"]
        if (
            isinstance(size, bool)
            or not isinstance(size, int)
            or size < 0
            or (modified is not None and (
                isinstance(modified, bool) or not isinstance(modified, int) or not 0 <= modified <= 0xFFFFFFFF
            ))
        ):
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned invalid guarded replacement metadata"
            )
        return {"size": size, "modified": modified}

    def sftp_create_exclusive(
        self, target_id: str, path: str, content: bytes, timeout_seconds: int = 10
    ) -> SshOperationResult:
        """Create one bounded remote file without replacing an existing path."""
        if not isinstance(content, bytes) or len(content) > _MAX_SFTP_EXCLUSIVE_BYTES:
            raise ValueError("content must be bytes no larger than 1 MiB")
        self._validate_sftp_path(path)
        return self._execute_operation(
            target_id,
            "sftp_create_exclusive",
            {"path": path, "content_base64": base64.b64encode(content).decode("ascii")},
            timeout_seconds,
        )

    def sftp_remove_file(
        self, target_id: str, path: str, timeout_seconds: int = 10
    ) -> SshOperationResult:
        """Remove one remote file path."""
        self._validate_sftp_path(path)
        return self._execute_operation(
            target_id, "sftp_remove_file", {"path": path}, timeout_seconds
        )

    def sftp_link_count(
        self, target_id: str, path: str, timeout_seconds: int = 10
    ) -> int | None:
        """Read a remote path's link count, returning None when the host cannot report it."""
        self._validate_sftp_path(path)
        if not path.startswith("/") or "\r" in path or "\n" in path:
            raise ValueError("path must be absolute and contain no line breaks")
        result = self._execute_operation(
            target_id, "sftp_link_count", {"path": path}, timeout_seconds
        )
        try:
            payload = json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned invalid SFTP link metadata"
            ) from error
        if not isinstance(payload, dict) or set(payload) != {"link_count"}:
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned invalid SFTP link metadata"
            )
        value = payload["link_count"]
        if value is not None and (isinstance(value, bool) or not isinstance(value, int) or value < 0):
            raise SshServiceTransportError(
                "invalid_response", "SSH service returned invalid SFTP link metadata"
            )
        return value

    @staticmethod
    def _validate_sftp_path(path: str) -> None:
        if (
            not isinstance(path, str)
            or not path
            or len(path.encode("utf-8")) > 4096
            or "\x00" in path
        ):
            raise ValueError("path must be a non-empty SFTP path of at most 4096 bytes")

    def _execute_operation(
        self,
        target_id: str,
        operation: str,
        parameters: dict[str, Any],
        timeout_seconds: int,
        *,
        max_timeout_seconds: int = 30,
    ) -> SshOperationResult:
        if not isinstance(target_id, str) or not _TARGET_ID.fullmatch(target_id):
            raise ValueError("target_id has an invalid format")
        if (
            isinstance(timeout_seconds, bool)
            or not isinstance(timeout_seconds, int)
            or not 1 <= timeout_seconds <= max_timeout_seconds
        ):
            raise ValueError(
                f"timeout_seconds must be an integer from 1 to {max_timeout_seconds}"
            )

        payload = {
            "target_id": target_id,
            "operation": operation,
            "parameters": parameters,
            "timeout_seconds": timeout_seconds,
        }
        body = self._request("POST", "/v1/ssh/exec", payload, timeout=timeout_seconds + 5, authorized=True)
        if not isinstance(body, dict) or body.get("status") != "succeeded":
            raise SshServiceTransportError("invalid_response", "SSH service returned an invalid operation result")
        expected = {"status", "target_id", "operation", "exit_code", "stdout", "stderr", "duration_ms", "output_truncated", "error"}
        if set(body) != expected:
            raise SshServiceTransportError("invalid_response", "SSH service returned an invalid operation result")
        if (
            body.get("target_id") != target_id
            or body.get("operation") != operation
            or isinstance(body.get("exit_code"), bool)
            or not isinstance(body.get("exit_code"), int)
            or not isinstance(body.get("stdout"), str)
            or not isinstance(body.get("stderr"), str)
            or isinstance(body.get("duration_ms"), bool)
            or not isinstance(body.get("duration_ms"), int)
            or body.get("duration_ms") < 0
            or not isinstance(body.get("output_truncated"), bool)
            or body.get("error") is not None
        ):
            raise SshServiceTransportError("invalid_response", "SSH service returned an invalid operation result")
        return SshOperationResult(
            target_id=target_id,
            operation=operation,
            exit_code=body["exit_code"],
            stdout=body["stdout"],
            stderr=body["stderr"],
            duration_ms=body["duration_ms"],
            output_truncated=body["output_truncated"],
        )

    def _request(self, method: str, path: str, payload: dict[str, Any] | None, *,
                 timeout: int, authorized: bool) -> dict[str, Any]:
        data = json.dumps(payload, separators=(",", ":")).encode("utf-8") if payload is not None else None
        if data is not None and len(data) > _MAX_BODY_BYTES:
            raise ValueError("request body exceeds the 2 MiB limit")
        headers = {"Accept": "application/json"}
        if data is not None:
            headers["Content-Type"] = "application/json"
        if authorized:
            headers["Authorization"] = f"Bearer {self._token}"
        request = urllib.request.Request(self._base_url + path, data=data, headers=headers, method=method)
        try:
            with self._opener.open(request, timeout=timeout) as response:
                raw = response.read(_MAX_BODY_BYTES + 1)
                if len(raw) > _MAX_BODY_BYTES:
                    raise SshServiceTransportError("response_too_large", "SSH service response exceeded the size limit")
                return self._decode_json(raw)
        except urllib.error.HTTPError as error:
            raw = error.read(_MAX_BODY_BYTES + 1)
            if len(raw) > _MAX_BODY_BYTES:
                raise SshServiceTransportError("response_too_large", "SSH service response exceeded the size limit",
                                               http_status=error.code) from None
            try:
                body = self._decode_json(raw)
            except SshServiceTransportError:
                raise SshServiceTransportError("invalid_error_response", "SSH service returned an invalid error response",
                                               http_status=error.code) from None
            error_info = body.get("error") if isinstance(body, dict) else None
            if isinstance(error_info, dict) and isinstance(error_info.get("code"), str) and isinstance(error_info.get("message"), str):
                raise SshServiceError(error_info["code"], error_info["message"], http_status=error.code) from None
            raise SshServiceTransportError("invalid_error_response", "SSH service returned an invalid error response",
                                           http_status=error.code) from None
        except (urllib.error.URLError, TimeoutError, socket.timeout, OSError, http.client.HTTPException) as error:
            raise SshServiceTransportError("service_unavailable", "Could not reach the local SSH service") from error

    @staticmethod
    def _decode_json(raw: bytes) -> dict[str, Any]:
        try:
            value = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise SshServiceTransportError("invalid_response", "SSH service returned invalid JSON") from None
        if not isinstance(value, dict):
            raise SshServiceTransportError("invalid_response", "SSH service response must be a JSON object")
        return value
