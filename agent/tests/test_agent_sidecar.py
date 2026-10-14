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

from yukinal_agent import ApprovalRequest, ExecutionContext, MemoryAuditSink, ModelAgentResult, ToolRegistry, ToolSpec
from yukinal_agent.sidecar import (
    MAX_SIDECAR_FRAME_BYTES,
    RPC_INVALID_PARAMS,
    RPC_INVALID_REQUEST,
    RPC_METHOD_NOT_FOUND,
    RPC_NOT_IMPLEMENTED,
    PythonSidecar,
    _allowed_tool_names,
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
    def test_tool_allowlist_keeps_writes_out_of_plan_and_readonly_modes(self) -> None:
        registry = _registry()

        async def handler(_arguments: dict[str, Any], _context: ExecutionContext) -> str:
            return "ok"

        for name, risk in (("ops.write", "write"), ("ops.destroy", "destructive")):
            registry.register(ToolSpec(
                name=name,
                description="An effectful operation requiring approval.",
                input_schema={"type": "object", "properties": {}, "additionalProperties": False},
                risk_level=risk,
                requires_approval=True,
                handler=handler,
                target_id_argument=None,
            ))

        self.assertEqual(_allowed_tool_names(registry, "goal"), {
            "ssh.service_status", "ops.write", "ops.destroy",
        })
        self.assertEqual(_allowed_tool_names(registry, "plan"), {"ssh.service_status"})
        self.assertEqual(_allowed_tool_names(registry, "readonly"), {"ssh.service_status"})

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

    def test_approval_rpc_resumes_the_matching_pending_call(self) -> None:
        async def run_approval() -> tuple[list[tuple[str, dict[str, Any]]], list[tuple[Any, ...]], dict[str, Any]]:
            notifications: list[tuple[str, dict[str, Any]]] = []
            sidecar = PythonSidecar(
                _registry(),
                notification_sink=lambda method, params: notifications.append((method, params)),
            )
            sidecar.handle(_request(1, "initialize", {
                "protocolVersion": "1.0", "clientVersion": "desktop", "dataDir": "C:/data",
            }))

            class PausedAgent:
                def __init__(self) -> None:
                    self.resumes: list[tuple[Any, ...]] = []

                async def resume_after_approval(
                    self,
                    checkpoint_id: str,
                    fingerprint: str,
                    approval_ref: str | None,
                    *,
                    approved: bool,
                ) -> ModelAgentResult:
                    self.resumes.append((checkpoint_id, fingerprint, approval_ref, approved))
                    return ModelAgentResult("task-ui", "succeeded", "approved", (), (), 1, 10)

            agent = PausedAgent()
            result = ModelAgentResult(
                "task-ui",
                "awaiting_approval",
                "",
                ({"call_id": "call-ui-1", "status": "awaiting_approval"},),
                (ApprovalRequest("a" * 64, "ops.write", "srv_stage1", "write", {"password": "[redacted]"}),),
                1,
                0,
                "approval_required",
                "agent-checkpoint-1",
            )
            approval_task = asyncio.create_task(sidecar._await_approval({
                "taskId": "task-ui",
                "hostTarget": {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            }, "run-ui-1", agent, result))
            await asyncio.sleep(0)
            method, event = notifications[-1]
            wrong_run = sidecar.handle(_request(2, "agent.approval.respond", {
                "approvalId": event["approval"]["approvalId"],
                "runId": "run-other",
                "decision": "approve_once",
                "respondedAt": "2026-10-15T00:00:00Z",
            }))
            self.assertFalse(wrong_run["result"]["accepted"])
            response = sidecar.handle(_request(3, "agent.approval.respond", {
                "approvalId": event["approval"]["approvalId"],
                "runId": "run-ui-1",
                "decision": "approve_once",
                "respondedAt": "2026-10-15T00:00:00Z",
            }))
            resumed = await approval_task
            self.assertEqual(method, "agent.stream")
            self.assertEqual(event["type"], "agent.waiting_approval")
            self.assertEqual(event["approval"]["callId"], "call-ui-1")
            self.assertEqual(event["approval"]["inputFingerprint"], "a" * 64)
            self.assertFalse(event["approval"]["sessionGrantable"])
            self.assertTrue(response["result"]["accepted"])
            self.assertEqual(resumed.answer, "approved")
            return notifications, agent.resumes, event

        notifications, resumes, event = asyncio.run(run_approval())
        self.assertEqual([method for method, _ in notifications], ["agent.stream"])
        self.assertEqual(resumes, [
            ("agent-checkpoint-1", "a" * 64, event["approval"]["approvalId"], True),
        ])

    def test_rejected_approval_rpc_resumes_without_an_approval_reference(self) -> None:
        async def run_rejection() -> tuple[ModelAgentResult, list[tuple[Any, ...]], dict[str, Any]]:
            notifications: list[tuple[str, dict[str, Any]]] = []
            sidecar = PythonSidecar(
                _registry(),
                notification_sink=lambda method, params: notifications.append((method, params)),
            )
            sidecar.handle(_request(1, "initialize", {
                "protocolVersion": "1.0", "clientVersion": "desktop", "dataDir": "C:/data",
            }))

            class PausedAgent:
                def __init__(self) -> None:
                    self.resumes: list[tuple[Any, ...]] = []

                async def resume_after_approval(
                    self,
                    checkpoint_id: str,
                    fingerprint: str,
                    approval_ref: str | None,
                    *,
                    approved: bool,
                    rejection_code: str = "approval_rejected",
                ) -> ModelAgentResult:
                    self.resumes.append((checkpoint_id, fingerprint, approval_ref, approved, rejection_code))
                    return ModelAgentResult("task-ui", "succeeded", "stopped", (), (), 1, 10)

            agent = PausedAgent()
            result = ModelAgentResult(
                "task-ui", "awaiting_approval", "",
                ({"call_id": "call-ui-2", "status": "awaiting_approval"},),
                (ApprovalRequest("b" * 64, "ops.write", "srv_stage1", "write", {}),),
                1, 0, "approval_required", "agent-checkpoint-2",
            )
            task = asyncio.create_task(sidecar._await_approval(
                {"taskId": "task-ui"}, "run-ui-2", agent, result,
            ))
            await asyncio.sleep(0)
            event = notifications[-1][1]
            response = sidecar.handle(_request(9, "agent.approval.respond", {
                "approvalId": event["approval"]["approvalId"],
                "runId": "run-ui-2",
                "decision": "reject",
                "respondedAt": "2026-10-15T00:00:00Z",
            }))
            resumed = await task
            self.assertTrue(response["result"]["accepted"])
            return resumed, agent.resumes, event

        resumed, calls, event = asyncio.run(run_rejection())
        self.assertEqual(resumed.answer, "stopped")
        self.assertEqual(calls, [("agent-checkpoint-2", "b" * 64, None, False, "approval_rejected")])

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
