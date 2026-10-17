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
    _allowed_tools_for_run,
    _parse_run_request,
    _RpcFault,
    serve_stdio,
)
from yukinal_agent.system_snapshot import SYSTEM_SNAPSHOT_COMMAND_IDS


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

    def test_plan_bound_host_actions_are_hidden_without_a_durable_remote_plan(self) -> None:
        registry = _registry()

        async def handler(_arguments: dict[str, Any], _context: ExecutionContext) -> str:
            return "ok"

        registry.register(ToolSpec(
            name="filesystem.edit",
            description="Edit one guarded remote file after approval.",
            input_schema={"type": "object", "properties": {}, "additionalProperties": False},
            risk_level="write",
            requires_approval=True,
            handler=handler,
            target_id_argument=None,
        ))
        request = {
            "taskId": "task-1",
            "hostTarget": {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
        }
        self.assertEqual(
            _allowed_tools_for_run(registry, "goal", request),
            {"ssh.service_status"},
        )

        request["planBinding"] = {
            "planId": "plan-1", "planStepId": "step-1", "evidenceIds": [],
        }
        self.assertEqual(
            _allowed_tools_for_run(registry, "goal", request),
            {"ssh.service_status", "filesystem.edit"},
        )
        self.assertEqual(
            _allowed_tools_for_run(registry, "readonly", request),
            {"ssh.service_status"},
        )

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
        ping = sidecar.handle(_request(5, "system.ping", {"echo": "hello"}))["result"]
        self.assertEqual(ping["pong"], "hello")
        self.assertGreater(ping["agentPid"], 0)

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

    def test_snapshot_normalization_rpc_is_strict_and_returns_ui_shape(self) -> None:
        sidecar = PythonSidecar(_registry())
        sidecar.handle(_request(1, "initialize", {
            "protocolVersion": "1.0", "clientVersion": "desktop", "dataDir": "C:/data",
        }))
        outputs = {
            command_id: {"exitCode": 0, "stdout": f"{command_id}\n"}
            for command_id in SYSTEM_SNAPSHOT_COMMAND_IDS
        }
        outputs["os_detect"] = {"exitCode": 0, "stdout": "Linux\n"}
        outputs["os_release"] = {
            "exitCode": 0, "stdout": 'NAME="Fixture Linux"\nVERSION_ID="9"\n'
        }
        result = sidecar.handle(_request(2, "server.snapshot.normalize", {"outputs": outputs}))
        data = result["result"]["snapshotData"]
        self.assertEqual(data["os"]["distribution"], "Fixture Linux")
        self.assertEqual(data["os"]["version"], "9")
        self.assertEqual(data["health"], "unknown")
        self.assertEqual(len(data["collectors"]), 7)
        self.assertNotIn("snapshotOutputs", data)
        extended = sidecar.handle(_request(4, "server.snapshot.normalize", {
            "outputs": {**outputs, "network_routes4": {"exitCode": 0, "stdout": "[]"},
                        "network_dns": {"exitCode": 1, "stdout": ""}},
        }))["result"]["snapshotData"]["networkDetails"]
        self.assertEqual(extended["ipv4Routes"], [])
        self.assertNotIn("dnsServers", extended)
        self.assertIn("dnsServers", extended["errors"])

        invalid = sidecar.handle(_request(3, "server.snapshot.normalize", {
            "outputs": {**outputs, "extra": {"exitCode": 0, "stdout": ""}},
        }))
        self.assertEqual(invalid["error"]["code"], RPC_INVALID_PARAMS)

    def test_readonly_ssh_output_normalization_rpcs_preserve_ui_shapes_and_filters(self) -> None:
        sidecar = PythonSidecar(_registry())
        sidecar.handle(_request(1, "initialize", {
            "protocolVersion": "1.0", "clientVersion": "desktop", "dataDir": "C:/data",
        }))

        logs = sidecar.handle(_request(2, "server.logs.normalize", {
            "stdout": "panic: service failed\nwarning: retrying\n__YUKINAL_SOURCE__=journalctl\n",
            "arguments": {"sinceSeconds": 60, "unit": "api.service"},
        }))
        self.assertEqual(logs["result"], {
            "ok": True,
            "output": {
                "source": "journalctl",
                "lines": [
                    {"text": "panic: service failed", "level": "error"},
                    {"text": "warning: retrying", "level": "warning"},
                ],
            },
        })

        services = sidecar.handle(_request(3, "server.services.normalize", {
            "stdout": "api\trunning\tUp 3 days\tnginx:latest\n"
            "old\texited\tExited (0) 1 day ago\talpine\n"
            "__YUKINAL_SOURCE__=docker\n",
            "arguments": {"state": "running"},
        }))
        self.assertEqual(services["result"]["output"], {
            "source": "docker",
            "services": [{
                "name": "api", "state": "running", "status": "Up 3 days",
                "description": "nginx:latest",
            }],
        })

        unsupported = sidecar.handle(_request(4, "server.logs.normalize", {
            "stdout": "line\n__YUKINAL_SOURCE__=syslog\n",
            "arguments": {"unit": "api.service"},
        }))
        self.assertEqual(unsupported["result"]["code"], "unsupported")
        malformed = sidecar.handle(_request(5, "server.services.normalize", {
            "stdout": "__YUKINAL_SOURCE__=unavailable\n",
            "arguments": {"state": []},
        }))
        self.assertEqual(malformed["error"]["code"], RPC_INVALID_PARAMS)

        docker_ps = sidecar.handle(_request(6, "docker.ps.normalize", {
            "stdout": '{"Names":"web,old","Image":"nginx:1.27","State":"running","Status":"Up"}\n',
            "arguments": {},
        }))
        self.assertEqual(docker_ps["result"]["output"], [{
            "name": "web", "image": "nginx:1.27", "state": "running",
            "status": "Up", "restartCount": 0,
        }])

        docker_logs = sidecar.handle(_request(7, "docker.logs.normalize", {
            "stdout": "line one\nline two\nline three\n",
            "arguments": {"container": "web", "tail": 2},
        }))
        self.assertEqual(docker_logs["result"]["output"], {
            "container": "web", "lines": ["line one", "line two"], "truncated": True,
        })

        systemd = sidecar.handle(_request(8, "systemd.inspect.normalize", {
            "stdout": "loaded\nactive\nrunning\nNginx web server\n",
            "arguments": {"service": "nginx.service"},
        }))
        self.assertEqual(systemd["result"]["output"], {
            "service": "nginx.service", "loadState": "loaded", "activeState": "active",
            "subState": "running", "description": "Nginx web server",
        })

        package = sidecar.handle(_request(9, "package.inspect.normalize", {
            "stdout": "installed\t1:1.27.0~bookworm\n",
            "arguments": {"manager": "apt", "package": "nginx"},
        }))
        self.assertEqual(package["result"]["output"], {
            "manager": "apt", "package": "nginx", "installed": True,
            "version": "1:1.27.0~bookworm",
        })

        docker_inspect = sidecar.handle(_request(10, "docker.inspect.normalize", {
            "stdout": '{"Id":"abc","Name":"/api","Config":{"Image":"nginx"},'
            '"State":{"Status":"running","StartedAt":"",'
            '"Health":{"Status":"healthy"}}}\n',
            "arguments": {"container": "api"},
        }))
        self.assertEqual(docker_inspect["result"]["output"], {
            "id": "abc", "name": "api", "image": "nginx", "state": "running",
            "status": "running", "restartCount": 0, "health": "healthy",
        })

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
                (ApprovalRequest(
                    "a" * 64,
                    "server.exec",
                    "srv_stage1",
                    "destructive",
                    {"password": "[redacted]"},
                    {"command": "systemctl restart api.service"},
                ),),
                1,
                0,
                "approval_required",
                "agent-checkpoint-1",
            )
            approval_task = asyncio.create_task(sidecar._await_approval({
                "taskId": "task-ui",
                "hostTarget": {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
                "planBinding": {"planId": "plan-ui", "planStepId": "step-ui", "evidenceIds": ["ev-ui"]},
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
            self.assertEqual(event["approval"]["input"], {"password": "[redacted]"})
            self.assertEqual(event["approval"]["hostInput"], {"command": "systemctl restart api.service"})
            self.assertEqual(event["approval"]["planId"], "plan-ui")
            self.assertEqual(event["approval"]["planStepId"], "step-ui")
            self.assertEqual(event["approval"]["evidenceIds"], ["ev-ui"])
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

    def test_run_request_validates_and_preserves_durable_plan_binding(self) -> None:
        parsed = _parse_run_request({
            "runId": "run-plan",
            "sessionId": "session-plan",
            "taskId": "task-plan",
            "prompt": "run the approved step",
            "focusServerId": "srv_stage1",
            "target": {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
            "planBinding": {
                "planId": "plan-current",
                "planStepId": "step-action",
                "evidenceIds": ["ev-1", "ev-2"],
            },
        })
        self.assertEqual(parsed["planBinding"], {
            "planId": "plan-current",
            "planStepId": "step-action",
            "evidenceIds": ["ev-1", "ev-2"],
        })
        with self.assertRaises(_RpcFault) as raised:
            _parse_run_request({
                "runId": "run-plan",
                "sessionId": "session-plan",
                "taskId": "task-plan",
                "prompt": "run the approved step",
                "target": {"host": "local", "environment": "local"},
                "planBinding": {
                    "planId": "plan-current",
                    "planStepId": "step-action",
                    "evidenceIds": ["ev-1", "ev-1"],
                },
            })
        self.assertEqual(raised.exception.code, RPC_INVALID_PARAMS)

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
        self.assertEqual(response_by_id[2]["result"]["pong"], "pong")
        self.assertGreater(response_by_id[2]["result"]["agentPid"], 0)
        self.assertEqual([frame["method"] for frame in host_requests], ["host.mcp.catalog"])

    def test_python_module_entrypoint_completes_desktop_handshake(self) -> None:
        async def run() -> tuple[int, bytes, bytes, int | None, int | None]:
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
                state_dir = Path(data_dir) / "agent-state"
                checkpoint_path = state_dir / "langgraph-checkpoints.sqlite"
                directory_mode = state_dir.stat().st_mode & 0o777 if state_dir.exists() else None
                checkpoint_mode = checkpoint_path.stat().st_mode & 0o777 if checkpoint_path.exists() else None
                return process.returncode or 0, stdout, stderr, directory_mode, checkpoint_mode

        code, stdout, stderr, directory_mode, checkpoint_mode = asyncio.run(run())
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
        self.assertIsNotNone(checkpoint_mode)
        if os.name != "nt":
            self.assertEqual(directory_mode, 0o700)
            self.assertEqual(checkpoint_mode, 0o600)


if __name__ == "__main__":
    unittest.main()
