"""The record tools that let a model publish evidence, findings, briefs and artifacts.

Before these existed the host routes `host.investigation.evidence.record`,
`host.investigation.finding.record`, `.brief.record` and `.artifact.record` were
unreachable from the Agent, so `decisionBrief` never appeared in
`investigation_task_get` and the desktop UI's only plan-approval surface stayed
dead. These tests pin the contract: durable identity, derived ids, host-shaped
bodies, and a strict reply check.
"""

from __future__ import annotations

import unittest
from typing import Any

from yukinal_agent.audit import MemoryAuditSink
from yukinal_agent.host_tools import register_host_tools
from yukinal_agent.investigation_tools import derived_id
from yukinal_agent.tools import ExecutionContext, ToolCallError, ToolRegistry


class FakeRecordHost:
    """Records the exact body each tool sends and replays a chosen reply."""

    def __init__(self, *, reply: dict[str, Any] | None = None, record: bool = True) -> None:
        self.calls: list[tuple[str, dict[str, Any], float]] = []
        self.reply = reply
        self.record = record

    async def request(self, method: str, params: dict[str, Any], *, timeout_seconds: float) -> dict[str, Any]:
        self.calls.append((method, params, timeout_seconds))
        if self.reply is not None:
            return self.reply
        if not self.record:
            return {
                "recorded": False,
                "error": {"code": "denied_by_policy", "message": "recording is closed"},
            }
        if "evidence" in params:
            # The host attests the stored content hash itself and answers with the id only.
            return {"recorded": True, "evidenceId": params["evidence"]["id"], "reused": False}
        for key in ("finding", "brief", "artifact"):
            if key in params:
                return {"recorded": True, key: params[key]}
        raise AssertionError(f"unexpected record body: {params}")


def make_context(**overrides: Any) -> ExecutionContext:
    values: dict[str, Any] = {
        "task_id": "agent-task-1",
        "tenant_id": "local",
        "user_id": "agent-session-1",
        "allowed_tools": frozenset(
            {
                "investigation.evidence",
                "investigation.finding",
                "investigation.brief",
                "investigation.artifact",
            }
        ),
        "allowed_target_ids": frozenset({"srv_stage1"}),
        "approved_actions": {},
        "context_target_id": "srv_stage1",
        "host_target": {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
        "host_task_id": "task_durable_1",
        "run_id": "run_1",
        "call_id": "call-record-1",
        "plan_id": "plan_1",
        "plan_step_id": "collect-df",
        "evidence_ids": ("ev_1", "ev_2"),
    }
    values.update(overrides)
    return ExecutionContext(**values)


class InvestigationRecordToolTests(unittest.IsolatedAsyncioTestCase):
    def registry(self, host: FakeRecordHost) -> ToolRegistry:
        registry = ToolRegistry(MemoryAuditSink())
        register_host_tools(registry, host)  # type: ignore[arg-type]
        return registry

    def test_the_record_tools_are_approval_free_reads(self) -> None:
        registry = self.registry(FakeRecordHost())
        for name in (
            "investigation.evidence",
            "investigation.finding",
            "investigation.brief",
            "investigation.artifact",
        ):
            spec = registry.get(name)
            self.assertIsNotNone(spec, name)
            assert spec is not None
            self.assertEqual(spec.risk_level, "read")
            self.assertFalse(spec.requires_approval)
            # Recording writes the local investigation store, never the target.
            self.assertFalse(spec.target_required)

    async def test_evidence_records_the_run_scope_and_redacts_secrets(self) -> None:
        host = FakeRecordHost()
        registry = self.registry(host)

        result = await registry.execute(
            "investigation.evidence",
            {
                "kind": "tool_result",
                "sourceTool": "ssh.disk_usage",
                "inputSummary": "df -Pk /",
                "contentType": "text",
                "content": "/dev/vda3 92% /  Authorization: Bearer tok_9f8e7d6c",
            },
            make_context(),
            call_id="call-evidence-1",
        )

        evidence = host.calls[0][1]["evidence"]
        self.assertEqual(host.calls[0][0], "host.investigation.evidence.record")
        self.assertEqual(result.value["evidenceId"], evidence["id"])
        self.assertEqual(evidence["id"], derived_id("ev", "task_durable_1", "call-evidence-1"))
        # The scope comes from the run target, never from model text.
        self.assertEqual(
            evidence["scope"],
            {"host": "remote", "serverId": "srv_stage1", "environment": "staging"},
        )
        # No digest is claimed: the host hashes what it actually stores.
        self.assertNotIn("contentHash", evidence)
        self.assertEqual(evidence["redactionStatus"], "redacted")
        self.assertNotIn("tok_9f8e7d6c", evidence["content"])
        self.assertEqual(evidence["runId"], "run_1")
        self.assertFalse(evidence["truncated"])

    async def test_evidence_leaves_clean_content_untouched(self) -> None:
        host = FakeRecordHost()
        registry = self.registry(host)

        await registry.execute(
            "investigation.evidence",
            {
                "kind": "snapshot",
                "sourceTool": "server.info",
                "contentType": "json",
                "content": {"filesystems": [{"/": "3%"}]},
                "truncated": True,
            },
            make_context(),
            call_id="call-evidence-2",
        )

        evidence = host.calls[0][1]["evidence"]
        self.assertEqual(evidence["redactionStatus"], "clean")
        self.assertEqual(evidence["content"], {"filesystems": [{"/": "3%"}]})
        self.assertTrue(evidence["truncated"])
        self.assertNotIn("inputSummary", evidence)

    async def test_evidence_derives_the_content_kind_from_what_it_stores(self) -> None:
        # A model sometimes writes a description ("server.info snapshot on srv_x")
        # into `contentType`. The host stores exactly two kinds, so the tool derives
        # the kind from the payload instead of failing the call over the wording.
        host = FakeRecordHost()
        registry = self.registry(host)

        await registry.execute(
            "investigation.evidence",
            {
                "kind": "snapshot",
                "sourceTool": "server.info",
                "contentType": "server.info snapshot on srv_stage1",
                "content": {"capabilities": {"docker": False}},
            },
            make_context(),
            call_id="call-evidence-3",
        )
        await registry.execute(
            "investigation.evidence",
            {
                "kind": "tool_result",
                "sourceTool": "ssh.disk_usage",
                "contentType": "df output, human readable",
                "content": "/dev/vda3 92% /",
            },
            make_context(),
            call_id="call-evidence-4",
        )

        self.assertEqual(host.calls[0][1]["evidence"]["contentType"], "json")
        self.assertEqual(host.calls[1][1]["evidence"]["contentType"], "text")

    async def test_evidence_needs_a_run_target_scope(self) -> None:
        host = FakeRecordHost()
        registry = self.registry(host)

        with self.assertRaises(ToolCallError) as caught:
            await registry.execute(
                "investigation.evidence",
                {"kind": "log", "sourceTool": "server.logs", "contentType": "text", "content": "x"},
                make_context(host_target={}, plan_id=None, plan_step_id=None, evidence_ids=()),
                call_id="call-evidence-3",
            )

        self.assertEqual(caught.exception.code, "target_required")
        self.assertEqual(host.calls, [])

        host = FakeRecordHost()
        registry = self.registry(host)

        result = await registry.execute(
            "investigation.finding",
            {
                "title": "磁盘使用率 92%",
                "kind": "fact",
                "statement": "/ 分区已用 92%，剩余 3.1 GiB。",
                "confidence": "high",
            },
            make_context(),
            call_id="call-finding-1",
        )

        finding = result.value["finding"]
        self.assertEqual(finding["taskId"], "task_durable_1")
        self.assertEqual(finding["id"], derived_id("finding", "task_durable_1", "call-finding-1"))
        self.assertEqual(finding["evidenceIds"], ["ev_1", "ev_2"])
        self.assertEqual(finding["confidence"], "high")
        self.assertNotIn("nextVerification", finding)
        self.assertEqual(host.calls[0][0], "host.investigation.finding.record")
        self.assertEqual(host.calls[0][2], 15.0)
        self.assertEqual(host.calls[0][1], {"finding": finding})

    async def test_a_finding_can_carry_its_own_evidence_instead_of_the_bound_set(self) -> None:
        host = FakeRecordHost()
        registry = self.registry(host)

        await registry.execute(
            "investigation.finding",
            {
                "title": "swap 未启用",
                "kind": "inference",
                "statement": "SwapTotal 为 0，目标没有交换分区。",
                "confidence": "medium",
                "nextVerification": "确认 vm.swappiness 与启动参数",
                "evidenceIds": ["ev_9"],
            },
            make_context(),
            call_id="call-finding-2",
        )

        finding = host.calls[0][1]["finding"]
        self.assertEqual(finding["evidenceIds"], ["ev_9"])
        self.assertEqual(finding["nextVerification"], "确认 vm.swappiness 与启动参数")

    async def test_a_brief_becomes_the_only_plan_approval_surface(self) -> None:
        host = FakeRecordHost()
        registry = self.registry(host)

        await registry.execute(
            "investigation.brief",
            {
                "options": [
                    {
                        "title": "压缩日志后重启服务",
                        "summary": "先归档旧日志再重启，风险中等。",
                        "impact": "服务中断约 5 秒。",
                        "riskLevel": "medium",
                        "verification": "重启后检查 /health 与日志留存。",
                        "continuation": "start_plan",
                    },
                    {
                        "title": "只观察，不动手",
                        "summary": "继续收集证据。",
                        "impact": "磁盘压力持续。",
                        "riskLevel": "read",
                        "verification": "下一轮重新采样磁盘使用率。",
                        "continuation": "wait_user",
                    },
                ]
            },
            make_context(plan_id="plan_1", plan_step_id="answer"),
            call_id="call-brief-1",
        )

        brief = host.calls[0][1]["brief"]
        self.assertEqual(host.calls[0][0], "host.investigation.brief.record")
        self.assertEqual(brief["id"], derived_id("brief", "task_durable_1", "call-brief-1"))
        self.assertEqual(brief["status"], "presented")
        self.assertEqual(brief["planId"], "plan_1")
        self.assertEqual(len(brief["options"]), 2)
        self.assertEqual(
            brief["options"][0]["id"], f"{brief['id']}-option-1"
        )
        self.assertEqual(brief["options"][0]["status"], "available")
        self.assertTrue(brief["options"][0]["requiresApproval"])
        self.assertEqual(brief["options"][0]["continuation"], "start_plan")
        self.assertEqual(brief["options"][0]["evidenceIds"], ["ev_1", "ev_2"])
        self.assertEqual(brief["options"][1]["continuation"], "wait_user")

    async def test_a_brief_without_a_plan_stays_unbound(self) -> None:
        host = FakeRecordHost()
        registry = self.registry(host)

        await registry.execute(
            "investigation.brief",
            {
                "options": [
                    {
                        "title": "等待",
                        "summary": "继续观察。",
                        "impact": "无。",
                        "riskLevel": "read",
                        "verification": "下一轮采样。",
                    }
                ]
            },
            make_context(plan_id=None, plan_step_id=None, evidence_ids=()),
            call_id="call-brief-2",
        )

        brief = host.calls[0][1]["brief"]
        self.assertNotIn("planId", brief)

    async def test_an_artifact_forwards_plan_binding_at_the_top_level(self) -> None:
        host = FakeRecordHost()
        registry = self.registry(host)

        await registry.execute(
            "investigation.artifact",
            {
                "kind": "baseline",
                "title": "重启前基线",
                "summary": "记录当前磁盘与内存快照。",
                "content": {"disk": "92%", "swap": 0},
            },
            make_context(plan_id="plan_1", plan_step_id="collect-df"),
            call_id="call-artifact-1",
        )

        method, body, _timeout = host.calls[0]
        self.assertEqual(method, "host.investigation.artifact.record")
        self.assertEqual(body["planId"], "plan_1")
        self.assertEqual(body["planStepId"], "collect-df")
        artifact = body["artifact"]
        self.assertEqual(artifact["id"], derived_id("artifact", "task_durable_1", "call-artifact-1"))
        self.assertEqual(artifact["phase"], "investigating")
        self.assertEqual(artifact["status"], "ready")
        self.assertEqual(artifact["runId"], "run_1")
        self.assertEqual(artifact["content"], {"disk": "92%", "swap": 0})

    async def test_a_rejected_record_surfaces_the_host_code(self) -> None:
        host = FakeRecordHost(record=False)
        registry = self.registry(host)

        with self.assertRaises(ToolCallError) as raised:
            await registry.execute(
                "investigation.finding",
                {
                    "title": "无证据的结论",
                    "kind": "fact",
                    "statement": "虚构。",
                    "confidence": "low",
                },
                make_context(),
                call_id="call-finding-3",
            )

        self.assertEqual(raised.exception.code, "denied_by_policy")
        self.assertEqual(str(raised.exception), "recording is closed")

    async def test_a_record_needs_a_durable_task(self) -> None:
        host = FakeRecordHost()
        registry = self.registry(host)

        with self.assertRaises(ToolCallError) as raised:
            await registry.execute(
                "investigation.artifact",
                {"kind": "verification", "title": "检查", "summary": "无任务。"},
                make_context(host_task_id=None, plan_id=None, plan_step_id=None, evidence_ids=()),
                call_id="call-artifact-2",
            )

        self.assertEqual(raised.exception.code, "durable_task_required")
        self.assertEqual(host.calls, [])

    async def test_a_reply_without_the_recorded_identity_is_refused(self) -> None:
        host = FakeRecordHost(reply={"recorded": True, "finding": {"id": "finding_1"}})
        registry = self.registry(host)

        with self.assertRaises(ToolCallError) as raised:
            await registry.execute(
                "investigation.finding",
                {
                    "title": "缺任务归属",
                    "kind": "fact",
                    "statement": "x",
                    "confidence": "high",
                },
                make_context(),
                call_id="call-finding-4",
            )

        self.assertEqual(raised.exception.code, "invalid_host_response")

    async def test_extra_fields_the_host_echoes_back_are_tolerated(self) -> None:
        host = FakeRecordHost(
            reply={
                "recorded": True,
                "finding": {
                    "id": "finding_1",
                    "taskId": "task_durable_1",
                    "createdAt": "2026-11-27T00:00:00Z",
                },
            }
        )
        registry = self.registry(host)

        result = await registry.execute(
            "investigation.finding",
            {"title": "回显字段", "kind": "fact", "statement": "x", "confidence": "high"},
            make_context(),
            call_id="call-finding-5",
        )

        self.assertEqual(result.value["finding"]["createdAt"], "2026-11-27T00:00:00Z")


if __name__ == "__main__":
    unittest.main()
