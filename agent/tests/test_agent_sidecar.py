from __future__ import annotations

import asyncio
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

from yukinal_agent import ExecutionContext, MemoryAuditSink, ToolRegistry, ToolSpec
from yukinal_agent.sidecar import (
    MAX_SIDECAR_FRAME_BYTES,
    RPC_INVALID_PARAMS,
    RPC_INVALID_REQUEST,
    RPC_METHOD_NOT_FOUND,
    RPC_NOT_IMPLEMENTED,
    PythonSidecar,
    serve_stdio,
)


def _registry() -> ToolRegistry:
    registry = ToolRegistry(MemoryAuditSink())

    async def handler(_arguments: dict[str, Any], _context: ExecutionContext) -> str:
        return "ok"

    registry.register(ToolSpec(
        name="ssh.service_status",
        description="Read one allow-listed service status.",
        input_schema={"type": "object", "properties": {}, "additionalProperties": False},
        risk_level="read",
        requires_approval=False,
        handler=handler,
        target_id_argument=None,
    ))
    return registry


def _request(request_id: int, method: str, params: object | None = None) -> dict[str, Any]:
    request: dict[str, Any] = {"jsonrpc": "2.0", "id": request_id, "method": method}
    if params is not None:
        request["params"] = params
    return request


class PythonSidecarTests(unittest.TestCase):
    def test_handshake_tool_catalog_and_capability_declaration(self) -> None:
        sidecar = PythonSidecar(_registry(), agent_version="0.3.0")
        before_init = sidecar.handle(_request(1, "system.ping"))
        self.assertEqual(before_init["error"]["code"], RPC_INVALID_REQUEST)

        initialized = sidecar.handle(_request(2, "initialize", {
            "protocolVersion": "1.0", "clientVersion": "1.0.0", "dataDir": "C:/data",
        }))
        self.assertEqual(initialized["result"]["agentVersion"], "0.3.0")
        self.assertEqual(initialized["result"]["capabilities"], {
            "streaming": False, "toolCalling": True, "cancellation": True, "mcp": False,
        })

        tools = sidecar.handle(_request(3, "tools.list"))["result"]["tools"]
        self.assertEqual(tools[0]["name"], "ssh.service_status")
        self.assertEqual(tools[0]["risk"], "read")
        self.assertEqual(tools[0]["origin"], {"kind": "builtin"})
        described = sidecar.handle(_request(4, "system.describe"))["result"]
        self.assertEqual(described["toolCount"], 1)
        self.assertEqual(described["permissionPolicyIds"], [])
        self.assertTrue(described["implemented"]["agent.run.start"])
        self.assertEqual(sidecar.handle(_request(5, "system.ping", {"echo": "hello"}))["result"], {"pong": "hello"})

    def test_invalid_params_unknown_and_unimplemented_methods_are_explicit(self) -> None:
        sidecar = PythonSidecar(_registry())
        sidecar.handle(_request(1, "initialize", {"protocolVersion": "1.0", "clientVersion": "desktop", "dataDir": ""}))
        invalid = sidecar.handle(_request(2, "system.ping", {"echo": 4}))
        unknown = sidecar.handle(_request(3, "not.a.method"))
        not_ready = sidecar.handle(_request(4, "agent.run.start", {}))
        duplicate = sidecar.handle(_request(5, "initialize", {"protocolVersion": "1.0", "clientVersion": "desktop", "dataDir": ""}))
        self.assertEqual(invalid["error"]["code"], RPC_INVALID_PARAMS)
        self.assertEqual(unknown["error"]["code"], RPC_METHOD_NOT_FOUND)
        self.assertEqual(not_ready["error"]["code"], RPC_NOT_IMPLEMENTED)
        self.assertEqual(duplicate["error"]["code"], RPC_INVALID_REQUEST)

    def test_notifications_receive_no_response_and_bad_frames_are_dropped(self) -> None:
        sidecar = PythonSidecar(_registry())
        self.assertIsNone(sidecar.handle({"jsonrpc": "2.0", "method": "system.ping"}))

        stream = io.BytesIO(
            b"not-json\n"
            + (b"x" * (MAX_SIDECAR_FRAME_BYTES + 1))
            + b"\n"
            + json.dumps(_request(1, "initialize", {
                "protocolVersion": "1.0", "clientVersion": "desktop", "dataDir": "",
            })).encode("utf-8") + b"\n"
            + json.dumps(_request(2, "system.ping")).encode("utf-8") + b"\n"
        )
        output = io.BytesIO()
        with patch("sys.stderr", new=io.StringIO()):
            asyncio.run(serve_stdio(sidecar, reader=stream, writer=output))
        frames = [json.loads(line) for line in output.getvalue().splitlines()]
        responses = [frame for frame in frames if "method" not in frame]
        host_requests = [frame for frame in frames if "method" in frame]
        response_by_id = {frame["id"]: frame for frame in responses}
        self.assertEqual(set(response_by_id), {1, 2})
        self.assertEqual(response_by_id[2]["result"], {"pong": "pong"})
        self.assertEqual([frame["method"] for frame in host_requests], ["host.mcp.catalog"])

    def test_python_module_entrypoint_completes_desktop_handshake(self) -> None:
        async def run() -> tuple[int, bytes, bytes]:
            with tempfile.TemporaryDirectory() as data_dir:
                env = dict(os.environ)
                env["YUKINAL_DATA_DIR"] = data_dir
                process = await asyncio.create_subprocess_exec(
                    sys.executable,
                    "-m",
                    "yukinal_agent.sidecar",
                    stdin=asyncio.subprocess.PIPE,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                    cwd=Path(__file__).resolve().parents[1],
                    env=env,
                )
                initialize = _request(41, "initialize", {
                    "protocolVersion": "1.0", "clientVersion": "1.0.0", "dataDir": data_dir,
                })
                ping = _request(42, "system.ping")
                payload = b"\n".join(json.dumps(item).encode("utf-8") for item in (initialize, ping)) + b"\n"
                stdout, stderr = await asyncio.wait_for(process.communicate(payload), timeout=15)
                return process.returncode or 0, stdout, stderr

        code, stdout, stderr = asyncio.run(run())
        self.assertEqual(code, 0, stderr.decode("utf-8", errors="replace"))
        frames = [json.loads(line) for line in stdout.splitlines()]
        responses = [frame for frame in frames if "method" not in frame]
        host_requests = [frame for frame in frames if "method" in frame]
        response_by_id = {frame["id"]: frame for frame in responses}
        self.assertEqual(set(response_by_id), {41, 42})
        self.assertEqual(response_by_id[41]["result"]["protocolVersion"], "1.0")
        self.assertEqual(response_by_id[42]["result"]["pong"], "pong")
        self.assertEqual([frame["method"] for frame in host_requests], ["host.mcp.catalog"])
        self.assertIn(b"Python Agent Node ready", stderr)


if __name__ == "__main__":
    unittest.main()
