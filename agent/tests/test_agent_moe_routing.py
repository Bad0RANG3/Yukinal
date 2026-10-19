"""Real sidecar/team admission and model selection; no network or credentials.

The provider-store snapshot intentionally allows two eligible providers so the
selection boundary can be tested independently of the production store's
single-active-provider policy. No provider-selection/admission method is mocked.
"""
from __future__ import annotations

import asyncio
import copy
import unittest
from unittest.mock import patch

from langchain_core.messages import AIMessageChunk, HumanMessage, ToolMessage

from yukinal_agent import MemoryAuditSink, ToolRegistry
from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.models import ModelFactoryError
from yukinal_agent.sidecar import PythonSidecar


PROVIDERS = [
    {"id": "provider-a", "kind": "openai-compatible", "label": "Fixture A",
     "baseUrl": "http://127.0.0.1:1/a", "model": "a-default", "enabled": True},
    {"id": "provider-b", "kind": "openai-compatible", "label": "Fixture B",
     "baseUrl": "http://127.0.0.1:2/b", "model": "b-default", "enabled": True},
]


class ProviderSnapshot:
    def __init__(self):
        self.providers = copy.deepcopy(PROVIDERS)

    def list_ai(self):
        return copy.deepcopy(self.providers)

    def normalize_ai_state(self, usable_ids):
        return [{**item, "enabled": item["enabled"] and item["id"] in usable_ids}
                for item in self.list_ai()]


class ModelSpy:
    def __init__(self, factory, provider, model):
        self.factory, self.provider, self.model = factory, provider, model
        self.tools, self.histories, self.effort = set(), [], None
        self.closed = 0

    @property
    def route(self):
        return self.provider, self.model

    def bind_tools(self, tools, *, parallel_tool_calls):
        assert parallel_tool_calls is False
        self.tools = {tool["function"]["name"] for tool in tools}
        return self

    def bind(self, **kwargs):
        assert kwargs.get("max_tokens", 0) > 0
        return self

    async def astream(self, messages):
        # Record only public user content. Private system instructions never
        # leave the model boundary or enter test diagnostics/artifacts.
        humans = [message.content for message in messages if isinstance(message, HumanMessage)]
        self.histories.append(humans)
        prompt = humans[-1]
        try:
            if "teams__dispatch" in self.tools:
                planned = self.factory.plans.get(prompt)
                if planned and not isinstance(messages[-1], ToolMessage):
                    yield AIMessageChunk(content="", tool_calls=[{
                        "name": "teams__dispatch", "args": {"tasks": planned},
                        "id": "fixture-dispatch", "type": "tool_call"}])
                    return
                yield AIMessageChunk(content=f"{self.provider}/{self.model} foreground")
                return
            self.factory.workers.setdefault(prompt, []).append(self)
            self.factory.started.setdefault(prompt, asyncio.Event()).set()
            if prompt in self.factory.gates:
                await self.factory.gates[prompt].wait()
            yield AIMessageChunk(content=f"{self.provider}/{self.model} verified {prompt}")
        finally:
            self.closed += 1


class FactorySpy:
    def __init__(self):
        self.created, self.plans, self.workers = [], {}, {}
        self.started, self.gates = {}, {}

    def create_with_api_key(self, config, api_key):
        assert api_key is None, "The local routing fixture must not receive credentials"
        provider = next(item["id"] for item in PROVIDERS if item["baseUrl"] == config.base_url)
        allowed = {"provider-a": {"a-default", "a-selected", "a-alternate"},
                   "provider-b": {"b-default", "b-selected"}}
        if config.model not in allowed[provider]:
            raise ModelFactoryError("invalid_params", "Fixture provider does not offer that model")
        model = ModelSpy(self, provider, config.model)
        self.created.append(model)
        return model


def role(identifier, **binding):
    return {"id": identifier, "team": "blue", "label": identifier,
            "systemPrompt": "Return only the assigned fixture result.", **binding}


def task(identifier):
    return {"key": identifier, "roleId": identifier, "title": identifier,
            "goal": f"{identifier} isolated brief", "access": "read"}


class Fixture:
    def __init__(self):
        self.store, self.providers, self.factory = ConversationStore(), ProviderSnapshot(), FactorySpy()
        self.events, self.changed, self.next_id = [], asyncio.Event(), 0
        self.sidecar = PythonSidecar(ToolRegistry(MemoryAuditSink()), model_factory=self.factory,
                                    provider_store=self.providers, conversation_store=self.store,
                                    notification_sink=self.emit)
        response = self.sidecar.handle({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                                       "params": {"protocolVersion": "1.0", "clientVersion": "fixture", "dataDir": ""}})
        assert "error" not in response

    def emit(self, _name, value):
        self.events.append(value)
        self.changed.set()

    async def rpc(self, method, params):
        self.next_id += 1
        response = await self.sidecar.handle_async({"jsonrpc": "2.0", "id": self.next_id,
                                                   "method": method, "params": params})
        if "error" in response:
            raise AssertionError(response["error"])
        return response["result"]

    async def run(self, run_id, roles, *, prompt="dispatch", session="foreground", defaults=False, **extra):
        params = {"runId": run_id, "sessionId": session, "prompt": prompt, "delivery": "sync",
                  "coordination": {"version": 1, "roles": roles, "servers": []}}
        if not defaults:
            params.update(providerId="provider-a", model="a-selected")
        params.update(extra)
        response = await self.rpc("agent.run.start", params)
        assert response["result"]["state"] == "completed", response["result"].get("error", "Unexpected fixture terminal state")
        return response

    async def settle(self):
        active = list(self.sidecar._coordination.active.values())
        if active:
            await asyncio.wait_for(asyncio.gather(*active, return_exceptions=True), 3)

    async def wait_for(self, predicate):
        async def find():
            while True:
                self.changed.clear()
                if predicate():
                    return
                await self.changed.wait()
        await asyncio.wait_for(find(), 3)

    def jobs(self, session="foreground"):
        return [item for item in self.sidecar._coordination.jobs.values() if item["sessionId"] == session]

    async def close(self):
        await self.sidecar.shutdown()
        self.store.close()


class MoeRoutingTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.fixture = Fixture()
        self.addAsyncCleanup(self.fixture.close)

    async def test_real_workers_use_bound_model_or_inherit_without_mutating_foreground(self):
        fixture = self.fixture
        roles = [role("bound", providerId="provider-b", model="b-selected"),
                 role("inherit"), role("model-only", model="a-alternate")]
        fixture.factory.plans["dispatch"] = [task(item["id"]) for item in roles]
        response = await fixture.run("lead", roles)
        await fixture.settle()
        self.assertEqual(response["result"]["text"], "provider-a/a-selected foreground")
        for name, route in (("bound", ("provider-b", "b-selected")),
                            ("inherit", ("provider-a", "a-selected")),
                            ("model-only", ("provider-a", "a-alternate"))):
            worker = fixture.factory.workers[task(name)["goal"]][0]
            self.assertEqual(worker.route, route)
            self.assertNotIn("teams__dispatch", worker.tools)
        self.assertEqual({job["state"] for job in fixture.jobs()}, {"completed"})
        self.assertEqual(len({id(model) for model in fixture.factory.created}), 4)
        await fixture.run("followup", roles, prompt="ordinary followup")
        self.assertEqual(fixture.factory.created[-1].route, ("provider-a", "a-selected"))

    async def test_provider_only_binding_uses_selected_provider_default_including_same_provider(self):
        fixture = self.fixture
        roles = [role("other-default", providerId="provider-b"), role("same-default", providerId="provider-a")]
        fixture.factory.plans["dispatch"] = [task(item["id"]) for item in roles]
        await fixture.run("lead-defaults", roles)
        await fixture.settle()
        self.assertEqual(fixture.factory.workers[task("other-default")["goal"]][0].route,
                         ("provider-b", "b-default"))
        self.assertEqual(fixture.factory.workers[task("same-default")["goal"]][0].route,
                         ("provider-a", "a-default"))
        self.assertEqual({job["state"] for job in fixture.jobs()}, {"completed"})

    async def test_absent_parent_selection_uses_provider_default_for_foreground_and_worker(self):
        fixture = self.fixture
        roles = [role("default-inherit")]
        fixture.factory.plans["dispatch"] = [task("default-inherit")]
        await fixture.run("lead-auto", roles, defaults=True)
        await fixture.settle()
        self.assertEqual([model.route for model in fixture.factory.created],
                         [("provider-a", "a-default"), ("provider-a", "a-default")])

    async def test_unavailable_bound_provider_fails_once_without_parent_fallback_or_session_leak(self):
        fixture = self.fixture
        fixture.providers.providers[1]["enabled"] = False
        roles = [role("disabled", providerId="provider-b"), role("missing", providerId="provider-missing")]
        fixture.factory.plans["dispatch"] = [task(item["id"]) for item in roles]
        await fixture.run("lead-unavailable", roles)
        await fixture.settle()
        self.assertEqual([model.route for model in fixture.factory.created], [("provider-a", "a-selected")])
        self.assertEqual({job["state"] for job in fixture.jobs()}, {"failed"})
        self.assertEqual(fixture.sidecar._run_sessions, {})
        self.assertEqual(fixture.sidecar._active_runs, {})
        await fixture.run("lead-still-works", roles, prompt="ordinary followup")
        self.assertEqual(len(fixture.factory.created), 2)
        self.assertEqual(len(fixture.jobs()), 2)

    async def test_effort_follows_only_inherited_same_model_not_model_only_overrides(self):
        fixture = self.fixture
        roles = [role("inherit"), role("same-model", model="a-selected"),
                 role("changed-model", model="a-alternate"),
                 role("explicit", providerId="provider-a", model="a-selected"),
                 role("default-b", providerId="provider-b")]
        fixture.factory.plans["dispatch"] = [task(item["id"]) for item in roles[:4]]
        fixture.factory.plans["default-effort"] = [task("default-b")]
        efforts = []

        def bind_effort(model, effort):
            if model.model != "a-selected":
                raise ModelFactoryError("unsupported_reasoning_effort", "Fixture model does not support effort")
            efforts.append((model, effort))
            model.effort = effort
            return model

        with patch("yukinal_agent.sidecar.with_reasoning_effort", side_effect=bind_effort):
            await fixture.run("lead-effort", roles, reasoningEffort="max")
            await fixture.settle()
            await fixture.run("lead-default-effort", roles, prompt="default-effort", reasoningEffort="max")
            await fixture.settle()
        self.assertEqual({job["state"] for job in fixture.jobs()}, {"completed"})
        self.assertEqual(len(efforts), 4)  # Two foreground turns, inherited model, same-model override.
        for name in ("inherit", "same-model"):
            self.assertEqual(fixture.factory.workers[task(name)["goal"]][0].effort, "max")
        for name in ("changed-model", "explicit", "default-b"):
            self.assertIsNone(fixture.factory.workers[task(name)["goal"]][0].effort)

    async def test_worker_sessions_and_model_instances_remain_isolated_across_stop_and_followup(self):
        fixture = self.fixture
        roles = [role("alpha", model="a-alternate"), role("beta", providerId="provider-b", model="b-selected")]
        items = [task(item["id"]) for item in roles]
        fixture.factory.plans["dispatch"] = items
        fixture.factory.gates = {item["goal"]: asyncio.Event() for item in items}
        fixture.factory.started = {item["goal"]: asyncio.Event() for item in items}
        await fixture.run("lead-parallel", roles)
        await asyncio.wait_for(asyncio.gather(*(event.wait() for event in fixture.factory.started.values())), 3)
        alpha, beta = (next(job for job in fixture.jobs() if job["roleId"] == name) for name in ("alpha", "beta"))
        self.assertEqual(len({job["workerSessionId"] for job in fixture.jobs()}), 2)
        self.assertNotIn("foreground", {job["workerSessionId"] for job in fixture.jobs()})
        for item in items:
            worker = fixture.factory.workers[item["goal"]][0]
            self.assertEqual(worker.histories, [[item["goal"]]])
        await fixture.run("foreground-new-model", roles, prompt="chat continues", model="a-default")
        self.assertTrue(all(job["state"] == "running" for job in fixture.jobs()))
        self.assertTrue((await fixture.rpc("agent.run.stop", {"runId": beta["runId"]}))["stopped"])
        await fixture.wait_for(lambda: beta["state"] == "cancelled")
        self.assertEqual(alpha["state"], "running")
        fixture.factory.gates[task("beta")["goal"]].set()
        fixture.factory.gates[task("alpha")["goal"]].set()
        await fixture.settle()
        self.assertEqual(alpha["state"], "completed")
        self.assertEqual(beta["state"], "cancelled")
        terminal = [event for event in fixture.events if event["type"] == "agent.completed" and event["runId"] == beta["runId"]]
        self.assertEqual(len(terminal), 1)
        self.assertEqual(terminal[0]["result"]["state"], "cancelled")
        self.assertEqual(fixture.sidecar._run_sessions, {})
        listed = await fixture.rpc("agent.teams.list", {"sessionId": "unrelated"})
        self.assertEqual(listed["tasks"], [])
        self.assertNotIn("verified", str(fixture.store.context("foreground", None).messages))
        await fixture.run("other-session-run", roles, session="other-session")
        await fixture.settle()
        other_jobs = fixture.jobs("other-session")
        self.assertEqual(len(other_jobs), 2)
        self.assertTrue({job["workerSessionId"] for job in other_jobs}.isdisjoint(
            job["workerSessionId"] for job in fixture.jobs()))
        self.assertEqual(len(fixture.factory.workers[task("beta")["goal"]]), 2)
        self.assertEqual(beta["state"], "cancelled")

    async def test_existing_task_key_is_not_replayed_on_later_role_model_change(self):
        fixture = self.fixture
        roles = [role("bound", model="a-alternate")]
        fixture.factory.plans["dispatch"] = [task("bound")]
        await fixture.run("first-turn", roles)
        await fixture.settle()
        first_job = copy.deepcopy(fixture.jobs()[0])
        fixture.factory.plans["dispatch-again"] = [task("bound")]
        await fixture.run("second-turn", [role("bound", providerId="provider-b", model="b-selected")], prompt="dispatch-again")
        await fixture.settle()
        self.assertEqual(fixture.jobs(), [first_job])
        self.assertEqual(len(fixture.factory.workers[task("bound")["goal"]]), 1)
        self.assertEqual([model.route for model in fixture.factory.created],
                         [("provider-a", "a-selected"), ("provider-a", "a-alternate"), ("provider-a", "a-selected")])


if __name__ == "__main__":
    unittest.main()
