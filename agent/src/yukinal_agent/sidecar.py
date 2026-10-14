from __future__ import annotations

import asyncio
import json
import os
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO

from yukinal_ssh import SshServiceClient

from .audit import JsonlAuditSink
from .ssh_tools import register_ssh_tools
from .tools import ToolRegistry

RPC_VERSION = "1.0"
MAX_SIDECAR_FRAME_BYTES = 24 * 1024 * 1024
RPC_PARSE_ERROR = -32700
RPC_INVALID_REQUEST = -32600
RPC_METHOD_NOT_FOUND = -32601
RPC_INVALID_PARAMS = -32602
RPC_INTERNAL_ERROR = -32603
RPC_NOT_IMPLEMENTED = -32604

METHODS = {
    "initialize": True,
    "system.ping": True,
    "tools.list": True,
    "system.describe": True,
    "agent.run.start": False,
    "agent.run.stop": False,
    "agent.approval.respond": False,
    "provider.models": False,
    "provider.test": False,
}


@dataclass(frozen=True)
class _RpcFault(Exception):
    code: int
    message: str
    data: Any = None


class PythonSidecar:
    """Minimal version-1 desktop JSON-RPC boundary for the Python Agent Node."""

    def __init__(self, registry: ToolRegistry, *, agent_version: str = "0.1.0") -> None:
        self._registry = registry
        self._agent_version = agent_version
        self._initialized = False

    def handle(self, request: object) -> dict[str, Any] | None:
        request_id: int | None = None
        if isinstance(request, dict) and isinstance(request.get("id"), int) and not isinstance(request.get("id"), bool):
            request_id = request["id"]
        if not isinstance(request, dict) or request.get("jsonrpc") != "2.0" or not isinstance(request.get("method"), str):
            return _failure(request_id, RPC_INVALID_REQUEST, "Invalid JSON-RPC request")
        method = request["method"]
        if request_id is None:
            # Notifications do not receive a response. The Python sidecar has no
            # client-originated notification methods in protocol version 1.0.
            return None
        params = request.get("params", {})
        try:
            if not self._initialized and method != "initialize":
                raise _RpcFault(RPC_INVALID_REQUEST, "initialize must be the first call")
            if method == "initialize":
                result = self._initialize(params)
            elif method == "system.ping":
                result = self._ping(params)
            elif method == "tools.list":
                _strict_object(params, set())
                result = {"tools": [self._declaration(spec) for spec in self._registry.list()]}
            elif method == "system.describe":
                _strict_object(params, set())
                result = {
                    "providers": [],
                    "toolCount": len(self._registry.list()),
                    "permissionPolicyIds": [],
                    "toolNameCollisions": [],
                    "implemented": dict(METHODS),
                }
            elif method in METHODS:
                raise _RpcFault(RPC_NOT_IMPLEMENTED, f"Method {method} is not implemented by this Agent Node")
            else:
                raise _RpcFault(RPC_METHOD_NOT_FOUND, f"Unknown method {method}")
            return {"jsonrpc": "2.0", "id": request_id, "result": result}
        except _RpcFault as error:
            response = _failure(request_id, error.code, error.message)
            if error.data is not None:
                response["error"]["data"] = error.data
            return response
        except Exception:
            return _failure(request_id, RPC_INTERNAL_ERROR, "Internal sidecar error")

    def _initialize(self, params: object) -> dict[str, Any]:
        if self._initialized:
            raise _RpcFault(RPC_INVALID_REQUEST, "initialize has already completed")
        parsed = _strict_object(params, {"protocolVersion", "clientVersion", "dataDir"})
        if (
            parsed.get("protocolVersion") != RPC_VERSION
            or not isinstance(parsed.get("clientVersion"), str)
            or not parsed["clientVersion"].strip()
            or len(parsed["clientVersion"]) > 128
            or not isinstance(parsed.get("dataDir"), str)
            or len(parsed["dataDir"]) > 4096
        ):
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        self._initialized = True
        return {
            "protocolVersion": RPC_VERSION,
            "agentVersion": self._agent_version,
            "capabilities": {
                "streaming": False,
                "toolCalling": False,
                "cancellation": False,
                "mcp": False,
            },
        }

    @staticmethod
    def _ping(params: object) -> dict[str, str]:
        parsed = _strict_object(params, {"echo"})
        echo = parsed.get("echo", "pong")
        if not isinstance(echo, str) or len(echo) > 1000:
            raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
        return {"pong": echo}

    @staticmethod
    def _declaration(spec: Any) -> dict[str, Any]:
        return {
            "name": spec.name,
            "description": spec.description,
            "risk": {"read": "read", "write": "medium", "destructive": "critical"}[spec.risk_level],
            "timeoutMs": 30_000,
            "cancellable": False,
            "retry": {"maxAttempts": 1, "backoffMs": 0},
            "inputSchema": spec.input_schema,
            "origin": {"kind": "builtin"},
            "effectful": spec.risk_level != "read",
        }


def _strict_object(value: object, allowed_keys: set[str]) -> dict[str, Any]:
    if not isinstance(value, dict) or any(not isinstance(key, str) for key in value):
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    if set(value) - allowed_keys:
        raise _RpcFault(RPC_INVALID_PARAMS, "invalid params")
    return value


def _failure(request_id: int | None, code: int, message: str) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


async def serve_stdio(
    sidecar: PythonSidecar,
    *,
    reader: BinaryIO | None = None,
    writer: BinaryIO | None = None,
) -> None:
    """Serve bounded NDJSON frames; stdout is protocol-only and diagnostics use stderr."""
    input_stream = reader or sys.stdin.buffer
    output_stream = writer or sys.stdout.buffer
    write_lock = asyncio.Lock()
    tasks: set[asyncio.Task[None]] = set()

    async def write_frame(frame: dict[str, Any]) -> None:
        encoded = json.dumps(frame, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8") + b"\n"
        if len(encoded) > MAX_SIDECAR_FRAME_BYTES:
            frame_id = frame.get("id")
            encoded = json.dumps(
                _failure(frame_id if isinstance(frame_id, int) else None, RPC_INTERNAL_ERROR, "Response exceeds the frame limit"),
                separators=(",", ":"),
            ).encode("utf-8") + b"\n"
        async with write_lock:
            output_stream.write(encoded)
            output_stream.flush()

    async def respond(value: object) -> None:
        result = sidecar.handle(value)
        if result is not None:
            await write_frame(result)

    while True:
        line = await asyncio.to_thread(input_stream.readline, MAX_SIDECAR_FRAME_BYTES + 1)
        if not line:
            break
        if len(line) > MAX_SIDECAR_FRAME_BYTES:
            print("dropped oversized sidecar frame", file=sys.stderr, flush=True)
            if not line.endswith(b"\n"):
                await _discard_to_newline(input_stream)
            continue
        try:
            value = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError):
            print("dropped malformed sidecar frame", file=sys.stderr, flush=True)
            continue
        task = asyncio.create_task(respond(value))
        tasks.add(task)
        task.add_done_callback(tasks.discard)
    if tasks:
        await asyncio.gather(*tasks, return_exceptions=True)


async def _discard_to_newline(stream: BinaryIO) -> None:
    while True:
        remainder = await asyncio.to_thread(stream.readline, 64 * 1024)
        if not remainder or remainder.endswith(b"\n"):
            return


def main() -> None:
    """Entry point for the Python Agent Node Sidecar."""
    data_dir = os.environ.get("YUKINAL_DATA_DIR", "").strip()
    audit = JsonlAuditSink(Path(data_dir) / "audit" / "python-agent-tools.jsonl") if data_dir else _StderrAuditSink()
    registry = ToolRegistry(audit)
    token = os.environ.get("YUKINAL_SSH_API_TOKEN", "")
    if token:
        try:
            register_ssh_tools(registry, SshServiceClient(token))
        except ValueError:
            print("SSH service credential configuration is invalid", file=sys.stderr, flush=True)
    sidecar = PythonSidecar(registry)
    print(f"Python Agent Node ready (tools={len(registry.list())})", file=sys.stderr, flush=True)
    asyncio.run(serve_stdio(sidecar))


class _StderrAuditSink:
    """Fail closed when a desktop did not supply its private data directory."""

    async def record(self, _event: Any) -> None:
        raise RuntimeError("YUKINAL_DATA_DIR is required for tool audit persistence")


if __name__ == "__main__":
    main()
