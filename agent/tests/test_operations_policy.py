"""The operations procedure and its runbooks are data, so they can rot silently.

These tests pin the two failure modes that do not show up at runtime: a runbook
that matches the wrong request, and a runbook that names a tool the product no
longer registers.
"""
from __future__ import annotations

import re
import unittest
from pathlib import Path

from yukinal_agent.audit import MemoryAuditSink
from yukinal_agent.host_tools import register_desktop_backup_metadata_tools, register_host_tools
from yukinal_agent.operations_policy import (
    RUNBOOKS,
    build_system_prompt,
    runbook_prompt,
    runbooks_payload,
    select_runbooks,
)
from yukinal_agent.ssh_tools import register_desktop_ssh_tools
from yukinal_agent.subagents import SubagentRuntime
from yukinal_agent.tools import ToolRegistry


class _NoRpcHost:
    async def request(self, *_args: object, **_kwargs: object) -> object:
        raise AssertionError("tool registration must not issue host RPCs")


def _desktop_registry() -> ToolRegistry:
    """The tool set a real desktop run can see, without a live host."""
    registry = ToolRegistry(MemoryAuditSink())
    host = _NoRpcHost()
    direct = set(register_desktop_backup_metadata_tools(registry, host))  # type: ignore[arg-type]
    direct.update(register_desktop_ssh_tools(registry, host, Path("offline-registration-only.exe")))  # type: ignore[arg-type]
    register_host_tools(registry, host, excluded_names=frozenset(direct))  # type: ignore[arg-type]
    return registry


class RunbookSelectionTests(unittest.TestCase):
    def test_common_failures_select_their_own_runbook(self):
        cases = {
            "服务器磁盘满了，帮我清理": "disk-full",
            "nginx 502 了，帮我看看": "service-down",
            "CPU 打满，网站很慢": "cpu-memory-high",
            "分析一下这个报错日志": "log-triage",
            "帮我部署新版本并验证": "deploy-change",
            "容器一直在重启": "container-unhealthy",
        }
        for prompt, runbook_id in cases.items():
            with self.subTest(prompt=prompt):
                selected = select_runbooks(prompt)
                self.assertIn(runbook_id, {item.id for item in selected})

    def test_explanatory_questions_and_code_blocks_do_not_select_a_runbook(self):
        self.assertEqual(select_runbooks("解释一下什么是 502 错误"), ())
        self.assertEqual(select_runbooks("```text\nnginx 502 disk full\n```"), ())
        self.assertEqual(select_runbooks(""), ())

    def test_selection_is_capped_for_prompt_size(self):
        selected = select_runbooks("磁盘满了，nginx 502，CPU 高，日志报错，重新部署容器")
        self.assertLessEqual(len(selected), 2)
        self.assertLess(len(runbook_prompt(select_runbooks("磁盘满了，nginx 502"))), 6000)


class RunbookContractTests(unittest.TestCase):
    def test_every_runbook_tool_name_is_registered(self):
        # A runbook that names a removed tool is worse than no runbook: it tells
        # the model to call something that cannot work.
        registered = {spec.name for spec in _desktop_registry().list()}
        registered.update({"subagents.start", "subagents.status", "subagents.stop"})
        for runbook in RUNBOOKS:
            for tool in runbook.evidence:
                with self.subTest(runbook=runbook.id, tool=tool):
                    self.assertIn(tool, registered, f"{runbook.id} names an unregistered tool")
        # The runtime-facing inventory must agree with the compiled data.
        self.assertEqual([item["id"] for item in runbooks_payload()], [item.id for item in RUNBOOKS])

    def test_runbook_ids_are_unique_and_each_has_verification_and_a_stop_condition(self):
        identifiers = [runbook.id for runbook in RUNBOOKS]
        self.assertEqual(len(identifiers), len(set(identifiers)))
        for runbook in RUNBOOKS:
            with self.subTest(runbook=runbook.id):
                self.assertTrue(runbook.verify)
                self.assertTrue(runbook.stop_when.strip())
                self.assertTrue(runbook.steps)


class SystemPromptTests(unittest.TestCase):
    def test_prompt_carries_the_procedure_and_the_matched_runbook(self):
        prompt = build_system_prompt(user_text="服务器磁盘满了，帮我清理", extra="yukinal-ui")
        self.assertIn("You own the outcome", prompt)
        self.assertIn("5. VERIFY", prompt)
        self.assertIn("[disk-full]", prompt)
        self.assertIn("untrusted data", prompt)
        # The live run asserted "/tmp other files unchanged" without reading the
        # directory. An unsupported claim is now named as a claim, not a courtesy.
        self.assertIn("Every sentence of your answer must be traceable", prompt)
        self.assertIn("yukinal-ui", prompt)
        # Wish-style persistence: work around a missing capability instead of
        # handing the goal back, and only ask when it is genuinely the user's call.
        self.assertIn("Persistence is expected", prompt)
        self.assertIn("do not hand the", prompt)
        self.assertIn("The user is responsible for the outcome", prompt)

    def test_a_question_prompt_carries_the_procedure_without_a_runbook(self):
        prompt = build_system_prompt(user_text="解释一下什么是 systemd")
        self.assertIn("You own the outcome", prompt)
        self.assertNotIn("[disk-full]", prompt)
        # The renderer guide is supplied by the loop, not by this module.
        self.assertNotIn("yukinal-ui", build_system_prompt(user_text="检查服务器状态"))

    def test_goal_guide_and_persona_are_appended_in_a_fixed_order(self):
        prompt = build_system_prompt(
            goal_guide="GOAL-SENTINEL",
            persona={"id": "ops", "label": "运维 Agent", "systemPrompt": "PERSONA-SENTINEL"},
            user_text="检查服务器状态",
            extra="RENDERER-SENTINEL",
        )
        order = [prompt.index(token) for token in ("You own the outcome", "GOAL-SENTINEL", "RENDERER-SENTINEL", "PERSONA-SENTINEL")]
        self.assertEqual(order, sorted(order))
        # Only the active persona is present; no second system role can be injected.
        self.assertEqual(len(re.findall("Active role:", prompt)), 1)


if __name__ == "__main__":
    unittest.main()
