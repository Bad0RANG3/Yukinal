"""Auto-approval: the user chose "let it run", so no run waits for a person.

The safety boundary does not disappear — the host still mints an exact one-shot
ticket bound to run, call, target, input and any plan step, and the command
blacklist still applies before that ticket is consumed. What changes is that the
sidecar does not block and the UI shows no pending decision.
"""
from __future__ import annotations

import asyncio
import unittest

from yukinal_agent import MemoryAuditSink, ModelAgentResult
from yukinal_agent.model_agent import ApprovalRequest
from yukinal_agent.sidecar import PythonSidecar
from yukinal_agent.tools import ToolRegistry, approval_fingerprint


def _registry() -> ToolRegistry:
    return ToolRegistry(MemoryAuditSink())


class _Agent:
    def __init__(self) -> None:
        self.resumes: list[tuple[tuple, dict]] = []

    async def resume_after_approval(self, *args, **kwargs) -> ModelAgentResult:
        self.resumes.append((args, kwargs))
        return ModelAgentResult("task-auto", "succeeded", "done", (), (), 1, 10)


def _result(host_input: dict | None) -> ModelAgentResult:
    return ModelAgentResult(
        "task-auto", "awaiting_approval", "",
        ({"call_id": "call-auto-1", "status": "awaiting_approval"},),
        (ApprovalRequest("a" * 64, "server.exec", "srv_stage1", "destructive",
                         {"command": "set -e\ntrue"}, host_input),),
        1, 0, "approval_required", "checkpoint-auto",
    )


class AutoApprovalTests(unittest.IsolatedAsyncioTestCase):
    async def run_wait(self, *, permission_mode: str, auto_marker: bool) -> tuple[list[dict], _Agent]:
        events: list[dict] = []
        sidecar = PythonSidecar(_registry(), notification_sink=lambda _method, payload: events.append(payload))
        sidecar.handle({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                        "params": {"protocolVersion": "1.0", "clientVersion": "test", "dataDir": ""}})
        agent = _Agent()
        host_input = {"command": "set -e\ntrue"} if auto_marker else None
        request = {
            "taskId": None, "permissionMode": permission_mode,
            "hostTarget": {"host": "remote", "serverId": "srv_stage1", "environment": "development"},
        }
        await asyncio.wait_for(
            sidecar._await_approval(request, "run-auto", agent, _result(host_input)), 5
        )
        return [event for event in events if event.get("type") == "agent.waiting_approval"], agent

    async def test_auto_mode_resumes_immediately_and_marks_the_event(self):
        events, agent = await self.run_wait(permission_mode="auto", auto_marker=True)
        self.assertEqual(len(events), 1)
        self.assertTrue(events[0]["approval"]["autoApproved"])
        self.assertEqual(len(agent.resumes), 1)
        _args, kwargs = agent.resumes[0]
        self.assertTrue(kwargs["approved"])
        # The ticket id is the resume reference the host compares against.
        self.assertTrue(agent.resumes[0][0][2].startswith("apr_"))

    async def test_ask_mode_still_waits_for_a_person(self):
        # Nobody answers within the TTL, so the call is rejected — never silently
        # approved. The TTL is shortened here so the test does not wait 120 s.
        events: list[dict] = []
        sidecar = PythonSidecar(
            _registry(), notification_sink=lambda _method, payload: events.append(payload),
            approval_ttl_seconds=1,
        )
        sidecar.handle({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                        "params": {"protocolVersion": "1.0", "clientVersion": "test", "dataDir": ""}})
        agent = _Agent()
        request = {"taskId": None, "permissionMode": "ask",
                   "hostTarget": {"host": "remote", "serverId": "srv_stage1", "environment": "development"}}
        await asyncio.wait_for(
            sidecar._await_approval(
                request, "run-ask", agent, _result({"command": "set -e\ntrue"})
            ),
            5,
        )
        waits = [event for event in events if event.get("type") == "agent.waiting_approval"]
        self.assertEqual(len(waits), 1)
        self.assertNotIn("autoApproved", waits[0]["approval"])
        self.assertEqual(len(agent.resumes), 1)
        _args, kwargs = agent.resumes[0]
        self.assertFalse(kwargs["approved"])
        self.assertEqual(kwargs["rejection_code"], "approval_timeout")


class ApprovalContractTests(unittest.TestCase):
    def test_a_conversation_server_exec_approval_is_a_complete_binding(self):
        # Regression: the shared gate used to demand planId/planStepId for every
        # server.exec approval, so a conversation approval was rejected by the UI
        # gate and never rendered — the run then expired after 120 s.
        from pathlib import Path

        source = (Path(__file__).resolve().parents[2] / "packages" / "shared" / "src" /
                  "schemas" / "agent.ts").read_text(encoding="utf-8")
        self.assertIn("planId and planStepId must be present together", source)
        self.assertNotIn(
            '"traceId", "callId", "inputFingerprint", "planId", "planStepId"',
            source,
            "the gate must not require a plan on a conversation approval",
        )


if __name__ == "__main__":
    unittest.main()
