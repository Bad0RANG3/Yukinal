"""The stage is an observation of executed results, so it must match that claim.

These tests pin the two ways a stage map can lie: claiming a stage the run did not
reach (a denied or failed call is not progress), and reporting a change of stage
when nothing changed (event spam that also misleads a reader into thinking the
agent restarted a phase).
"""
from __future__ import annotations

import unittest

from langchain_core.messages import AIMessage

from test_agent_model_loop import ScriptedModel, _registry
from yukinal_agent import AgentBudget, ModelDrivenAgent, ToolRegistry, MemoryAuditSink
from yukinal_agent.agent_stage import STAGES, derive_report_stage, derive_stage
from yukinal_agent.tools import ToolCallError, ToolSpec, approval_fingerprint


class DeriveStageTests(unittest.TestCase):
    def test_successful_reads_move_from_intake_to_evidence_and_stay_there(self):
        stage = derive_stage(None, tool="server.info", risk="read", succeeded=True)
        self.assertEqual(stage, "evidence")
        self.assertEqual(derive_stage(stage, tool="server.logs", risk="read", succeeded=True), "evidence")

    def test_a_failed_read_is_diagnosis_but_an_authority_answer_is_not(self):
        self.assertEqual(derive_stage("evidence", tool="server.logs", risk="read", succeeded=False,
                                      error_code="execution_failed"), "diagnosis")
        for code in ("approval_rejected", "tool_not_allowed", "invalid_arguments", "tool_repetition_detected"):
            with self.subTest(code=code):
                self.assertEqual(derive_stage("evidence", tool="server.logs", risk="read",
                                              succeeded=False, error_code=code), "evidence")

    def test_a_change_is_verify_once_a_read_succeeds_after_it(self):
        changed = derive_stage("evidence", tool="systemd.restart", risk="write", succeeded=True)
        self.assertEqual(changed, "change")
        verified = derive_stage(changed, tool="systemd.inspect", risk="read", succeeded=True)
        self.assertEqual(verified, "verify")
        self.assertEqual(derive_stage(verified, tool="server.info", risk="read", succeeded=True), "verify")
        # A further change restarts the cycle rather than silently staying "verified".
        self.assertEqual(derive_stage(verified, tool="systemd.restart", risk="write", succeeded=True), "change")

    def test_a_denied_or_unknown_change_does_not_claim_a_change(self):
        # Not run yet: the host is waiting for the user, so nothing changed.
        self.assertEqual(derive_stage("evidence", tool="server.exec", risk="destructive",
                                      succeeded=False, error_code="approval_required"), "evidence")
        self.assertEqual(derive_stage("evidence", tool="server.exec", risk="destructive",
                                      succeeded=False, error_code="tool_not_allowed"), "evidence")
        # An unknown outcome may have changed the target, so it is not evidence-only.
        self.assertEqual(derive_stage("diagnosis", tool="server.exec", risk="destructive",
                                      succeeded=False, phase="outcome_unknown"), "change")
        # A dispatched-but-failed effectful call is also not verifiable as harmless.
        self.assertEqual(derive_stage("diagnosis", tool="server.exec", risk="destructive",
                                      succeeded=False, error_code="execution_failed"), "change")

    def test_the_recorded_plan_is_its_own_stage(self):
        self.assertEqual(derive_stage("evidence", tool="investigation.plan", risk="read", succeeded=True), "plan")
        self.assertEqual(derive_stage("plan", tool="server.logs", risk="read", succeeded=True), "plan")

    def test_the_final_report_is_a_stage_the_runtime_actually_produces(self):
        # `report` sat in `STAGES` with no producer: the desktop inferred it locally.
        # These assertions pin both halves — the transition exists, and it is idempotent.
        self.assertEqual(derive_report_stage("verify"), "report")
        self.assertEqual(derive_report_stage(None), "report")
        self.assertEqual(derive_report_stage("report"), "report")

    def test_every_derived_value_is_a_declared_stage(self):
        for stage in (None, *STAGES):
            for risk in ("read", "write", "destructive"):
                for succeeded in (True, False):
                    with self.subTest(stage=stage, risk=risk, succeeded=succeeded):
                        self.assertIn(derive_stage(stage, tool="server.info", risk=risk, succeeded=succeeded), STAGES)


class StageEventTests(unittest.IsolatedAsyncioTestCase):
    async def run_agent(self, responses, registry, events):
        agent = ModelDrivenAgent(registry, ScriptedModel(responses), AgentBudget(max_model_rounds=8),
                                 event_sink=events.append)
        return await agent.run(task_id="stage", tenant_id="tenant-1", user_id="user-1", prompt="Inspect the server.",
                               allowed_tools={"ops.read"}, allowed_target_ids={"stage-1"},
                               run_id="run-stage")

    async def test_stage_events_are_emitted_only_when_the_stage_changes(self):
        executed = []
        async def read(arguments, _context):
            executed.append(arguments["value"])
            return {"observed": arguments["value"]}
        events: list[dict] = []
        responses = [
            AIMessage(content="", tool_calls=[
                {"name": "ops__read", "args": {"target_id": "stage-1", "value": "a"}, "id": "read-a", "type": "tool_call"},
                {"name": "ops__read", "args": {"target_id": "stage-1", "value": "b"}, "id": "read-b", "type": "tool_call"},
            ]),
            AIMessage(content="Both reads finished."),
        ]
        result = await self.run_agent(responses, _registry(read), events)
        self.assertEqual(result.status, "succeeded")
        stages = [(event["previous"], event["stage"]) for event in events if event["type"] == "agent.stage"]
        # Two reads at the same stage produce one transition, not two frames. The final
        # answer then adds exactly one transition into `report`.
        self.assertEqual(stages, [("intake", "evidence"), ("evidence", "report")])
        self.assertTrue(all(event["runId"] == "run-stage" for event in events if event["type"] == "agent.stage"))

    async def test_a_finished_run_reports_the_report_stage_once(self):
        events: list[dict] = []

        async def read(_arguments, _context):
            return {"observed": "value"}

        result = await self.run_agent([
            AIMessage(content="", tool_calls=[{"name": "ops__read", "args": {"target_id": "stage-1", "value": "a"}, "id": "read-a", "type": "tool_call"}]),
            AIMessage(content="The server is healthy."),
        ], _registry(read), events)
        self.assertEqual(result.status, "succeeded")
        stages = [event["stage"] for event in events if event["type"] == "agent.stage"]
        self.assertEqual(stages, ["evidence", "report"], "the answer is the only producer of `report`")
        self.assertEqual(
            [event["previous"] for event in events if event["type"] == "agent.stage"],
            ["intake", "evidence"],
        )

    async def test_a_denied_write_does_not_emit_a_change_stage(self):
        async def denied(_arguments, _context):
            raise ToolCallError("tool_not_allowed", "not allowed for this fixture")
        events: list[dict] = []
        result = await self.run_agent([
            AIMessage(content="", tool_calls=[{
                "name": "ops__write", "args": {"target_id": "stage-1", "value": "x"}, "id": "write", "type": "tool_call"}]),
        ], _registry(denied, write=True), events)
        self.assertNotEqual(result.status, "succeeded")
        stages = [event["stage"] for event in events if event["type"] == "agent.stage"]
        self.assertNotIn("change", stages)

    async def test_an_unknown_effectful_outcome_is_reported_as_a_change(self):
        async def unknown(_arguments, _context):
            raise ToolCallError("transport", "the remote result is unknown")
        registry = ToolRegistry(MemoryAuditSink())
        registry.register(ToolSpec(
            name="ops.write", description="Effectful fixture", risk_level="write", requires_approval=True,
            input_schema={"type": "object", "properties": {"value": {"type": "string"}},
                          "required": ["value"], "additionalProperties": False},
            handler=unknown, target_id_argument=None))
        # The call is pre-approved so the unknown-outcome path is exercised without
        # an approval round-trip; the transport failure is what the stage reacts to.
        arguments = {"value": "x"}
        fingerprint = approval_fingerprint("ops.write", None, arguments, task_id="stage",
                                           tenant_id="tenant-1", user_id="user-1")
        events: list[dict] = []
        agent = ModelDrivenAgent(registry, ScriptedModel([
            AIMessage(content="", tool_calls=[{"name": "ops__write", "args": {"value": "x"}, "id": "w", "type": "tool_call"}]),
            AIMessage(content="The write outcome is unknown."),
        ]), AgentBudget(max_model_rounds=6), event_sink=events.append)
        await agent.run(task_id="stage", tenant_id="tenant-1", user_id="user-1", prompt="Change it.",
                        allowed_tools={"ops.write"}, allowed_target_ids=set(), run_id="run-stage",
                        approved_actions={fingerprint: "fixture-approval"})
        stages = [event["stage"] for event in events if event["type"] == "agent.stage"]
        self.assertIn("change", stages)


if __name__ == "__main__":
    unittest.main()
