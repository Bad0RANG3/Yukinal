import json
import os
from pathlib import Path
import shutil
import subprocess
import unittest

from langchain_core.messages import AIMessage

from yukinal_agent import ModelDrivenAgent, ToolCallError
from yukinal_agent.tools import approval_fingerprint
from yukinal_agent.shell_policy import prepare_model_tool_arguments
from test_agent_model_loop import ScriptedModel, _server_exec_registry


def local_shell():
    found = shutil.which("sh")
    git_shell = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "Git/usr/bin/sh.exe"
    return found or (str(git_shell) if git_shell.is_file() else None)


class ModelShellPolicyTests(unittest.IsolatedAsyncioTestCase):
    async def test_long_command_approval_preserves_tail_and_redacts_secrets(self):
        executed = []
        secret = "sk-test-" + "x" * 32
        command = "# reviewed line\n" * 700 + f"# {secret}\nprintf 'command-tail'\n"
        arguments = {"command": command}

        async def handler(value, _context):
            executed.append(dict(value))
            return {"checked": True}

        model = ScriptedModel([
            AIMessage(content="", tool_calls=[{"name": "server__exec", "args": arguments,
                "id": "long-command", "type": "tool_call"}]),
            AIMessage(content="Checked."),
        ])
        events = []
        agent = ModelDrivenAgent(_server_exec_registry(handler), model, event_sink=events.append)
        paused = await agent.run(task_id="long-review", tenant_id="tenant-1", user_id="user-1",
            prompt="Review this complete command", allowed_tools={"server.exec"},
            allowed_target_ids=set())
        self.assertEqual(paused.status, "awaiting_approval")
        approval = paused.approval_requests[0]
        self.assertEqual(approval.arguments["command"], "set -e\n" + command.replace(secret, "[redacted]"))
        self.assertNotIn(secret, json.dumps(approval.arguments))
        self.assertEqual(approval.host_input["command"], "set -e\n" + command)
        self.assertEqual(executed, [])
        call = next(event for event in events if event.get("type") == "agent.tool_call")
        self.assertTrue(call["input"]["command"].endswith("…[truncated]"))
        result = await agent.resume_after_approval(paused.checkpoint_id, approval.fingerprint,
            "approved-once", approved=True)
        self.assertEqual(result.status, "succeeded")
        self.assertEqual(executed, [approval.host_input])
        self.assertEqual(arguments["command"], command)

    def test_reused_command_does_not_accumulate_fail_fast_prefixes(self):
        original = {"command": "set -e\n" * 5 + "printf checked", "timeoutMs": 1000}
        prepared = prepare_model_tool_arguments("server.exec", original)
        self.assertEqual(prepared["command"], "set -e\nprintf checked")
        self.assertEqual(prepare_model_tool_arguments("server.exec", prepared), prepared)
        self.assertEqual(original["command"], "set -e\n" * 5 + "printf checked")

    async def exercise_approved_shell(self, command):
        shell = local_shell()
        if shell is None:
            self.skipTest("A local POSIX shell is required for execution semantics")
        executed = []

        async def handler(arguments, _context):
            executed.append(dict(arguments))
            process = subprocess.run([shell, "-c", arguments["command"]],
                                     capture_output=True, text=True, timeout=5)
            result = {"state": "completed", "exitCode": process.returncode,
                      "stdout": process.stdout, "stderr": process.stderr,
                      "stdoutTruncated": False, "stderrTruncated": False, "durationMs": 1}
            if process.returncode:
                raise ToolCallError("execution_failed", "Command failed", result=result)
            return result

        model = ScriptedModel([
            AIMessage(content="", tool_calls=[{"name": "server__exec",
                "args": {"command": command}, "id": "exec", "type": "tool_call"}]),
            AIMessage(content="Verified the command result."),
        ])
        events = []
        agent = ModelDrivenAgent(_server_exec_registry(handler), model,
                                 event_sink=events.append)
        paused = await agent.run(task_id="shell", tenant_id="tenant-1", user_id="user-1",
                                 prompt="Run the check", allowed_tools={"server.exec"},
                                 allowed_target_ids=set())
        self.assertEqual(paused.status, "awaiting_approval")
        self.assertEqual(executed, [])
        approval = paused.approval_requests[0]
        reviewed_input = approval.host_input
        self.assertEqual(reviewed_input, prepare_model_tool_arguments("server.exec", {"command": command}))
        result = await agent.resume_after_approval(paused.checkpoint_id, approval.fingerprint,
                                                  "approved-once", approved=True)
        self.assertEqual(executed, [reviewed_input])
        emitted = next(event for event in events if event.get("type") == "agent.tool_call")
        self.assertEqual(emitted["input"], reviewed_input)
        self.assertEqual(model.seen_messages[1][-2].tool_calls[0]["args"], reviewed_input)
        return result, json.loads(model.seen_messages[1][-1].content)

    async def test_failed_action_cannot_be_hidden_by_trailing_success_output(self):
        result, feedback = await self.exercise_approved_shell("(exit 7); printf 'false success'")
        self.assertEqual(result.tool_results[-1]["status"], "failed")
        self.assertEqual(feedback["exitCode"], 7)
        self.assertNotIn("false success", feedback["stdout"])

    async def test_expected_nonzero_probe_in_conditional_can_continue(self):
        result, feedback = await self.exercise_approved_shell(
            "if (exit 7); then printf 'unexpected'; else printf 'missing dependency'; fi; printf ' checked'")
        self.assertEqual(result.tool_results[-1]["status"], "succeeded")
        self.assertEqual(feedback["exitCode"], 0)
        self.assertEqual(feedback["stdout"], "missing dependency checked")

    async def test_raw_command_approval_cannot_authorize_prepared_command(self):
        executed = []

        async def handler(arguments, _context):
            executed.append(arguments)

        raw = {"command": "printf checked"}
        fingerprint = approval_fingerprint("server.exec", None, raw,
            task_id="bound", tenant_id="tenant-1", user_id="user-1")
        model = ScriptedModel([AIMessage(content="", tool_calls=[{"name": "server__exec",
            "args": raw, "id": "exec", "type": "tool_call"}])])
        result = await ModelDrivenAgent(_server_exec_registry(handler), model).run(
            task_id="bound", tenant_id="tenant-1", user_id="user-1", prompt="Run check",
            allowed_tools={"server.exec"}, allowed_target_ids=set(),
            approved_actions={fingerprint: "old-approval"})
        self.assertEqual(result.status, "awaiting_approval")
        self.assertNotEqual(result.approval_requests[0].fingerprint, fingerprint)
        self.assertEqual(raw, {"command": "printf checked"})
        self.assertEqual(executed, [])
