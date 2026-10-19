from __future__ import annotations

import asyncio
import json
import tempfile
import threading
import unittest
from collections import deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import httpx

from yukinal_agent.audit import MemoryAuditSink
from yukinal_agent.checkpoints import open_checkpoint_store
from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.deepseek_model import DeepSeekChatModel
from yukinal_agent.model_agent import AgentBudget, ModelDrivenAgent
from yukinal_agent.sidecar import PythonSidecar
from yukinal_agent.tools import ToolRegistry, ToolSpec


PRIVATE_REASONING = "PRIVATE_REASONING_LOOP_SENTINEL"
FINAL_REASONING = "PRIVATE_FINAL_REASONING_SENTINEL"
FOLLOWUP_REASONING = "PRIVATE_FOLLOWUP_REASONING_SENTINEL"
CALL_ID = "fixture-provider-call"


def _sse_round(*deltas: dict[str, Any], finish: str = "stop") -> bytes:
    frames = []
    for delta in deltas:
        frames.append({"id": "fixture-completion", "object": "chat.completion.chunk",
                       "created": 1, "model": "deepseek-flash",
                       "choices": [{"index": 0, "delta": delta, "finish_reason": None}]})
    frames.append({"id": "fixture-completion", "object": "chat.completion.chunk",
                   "created": 1, "model": "deepseek-flash",
                   "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]})
    frames.append({"id": "fixture-completion", "object": "chat.completion.chunk",
                   "created": 1, "model": "deepseek-flash", "choices": [],
                   "usage": {"prompt_tokens": 12, "completion_tokens": 7, "total_tokens": 19}})
    return ("".join("data: " + json.dumps(frame) + "\n\n" for frame in frames)
            + "data: [DONE]\n\n").encode("ascii")


def _tool_round(name: str) -> bytes:
    # Split reasoning and JSON arguments across actual HTTP/SSE frames, so the
    # OpenAI SDK and LangChain must assemble both before the tool is dispatched.
    midpoint = len(PRIVATE_REASONING) // 2
    return _sse_round(
        {"role": "assistant", "content": "", "reasoning_content": PRIVATE_REASONING[:midpoint]},
        {"content": "", "reasoning_content": PRIVATE_REASONING[midpoint:], "tool_calls": [
            {"index": 0, "id": CALL_ID, "type": "function", "function": {
                "name": name, "arguments": '{"value":"loc'}}]},
        {"content": "", "tool_calls": [{"index": 0, "function": {"arguments": 'al"}'}}]},
        finish="tool_calls",
    )


def _answer_round(text: str, reasoning: str = FINAL_REASONING) -> bytes:
    return _sse_round({"role": "assistant", "content": "", "reasoning_content": reasoning},
                      {"content": text})


class _LoopbackCompletions:
    """Real loopback HTTP/SSE, with a finite script and no provider access."""

    def __init__(self, rounds: list[bytes]) -> None:
        self.requests: list[dict[str, Any]] = []
        self.errors: list[str] = []
        self.remaining = deque(rounds)
        self.lock = threading.Lock()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_POST(self) -> None:
                self.connection.settimeout(3.0)
                length = int(self.headers.get("Content-Length", "0"))
                if self.path != "/v1/chat/completions" or not 0 < length <= 256 * 1024:
                    fixture.errors.append("Unexpected request path or size")
                    self.send_error(400)
                    return
                try:
                    payload = json.loads(self.rfile.read(length))
                except (ValueError, OSError):
                    fixture.errors.append("Invalid fixture request")
                    self.send_error(400)
                    return
                with fixture.lock:
                    fixture.requests.append(payload)
                    response = fixture.remaining.popleft() if fixture.remaining else None
                if response is None:
                    fixture.errors.append("Unexpected extra model request")
                    self.send_error(500)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream; charset=utf-8")
                self.send_header("Content-Length", str(len(response)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(response)
                self.wfile.flush()
                self.close_connection = True

            def log_message(self, _format: str, *_args: Any) -> None:
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever,
                                       kwargs={"poll_interval": 0.05}, daemon=True)
        self.thread.start()

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}/v1"

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3.0)
        if self.thread.is_alive():
            raise AssertionError("Loopback fixture did not stop")


class _SdkSidecar(PythonSidecar):
    def __init__(self, model: DeepSeekChatModel, store: ConversationStore,
                 registry: ToolRegistry) -> None:
        self.model = model
        self.events: list[dict[str, Any]] = []
        super().__init__(registry, conversation_store=store,
                         notification_sink=lambda _method, event: self.events.append(event))
        initialized = self.handle({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                                   "params": {"protocolVersion": "1.0", "clientVersion": "reasoning-fixture",
                                              "dataDir": ""}})
        if initialized is None or "error" in initialized:
            raise AssertionError("Fixture initialization failed")

    async def _run_provider_model(self, _provider: Any, _model: Any) -> tuple[Any, int]:
        # Replace only provider selection. Streaming, messages, graph execution,
        # transcript persistence and public events are production code paths.
        return self.model, 3000

    async def run_turn(self, run_id: str) -> dict[str, Any]:
        result = await asyncio.wait_for(self.handle_async({
            "jsonrpc": "2.0", "id": 2, "method": "agent.run.start", "params": {
                "runId": run_id, "sessionId": "reasoning-session", "delivery": "sync",
                "prompt": "Read the local fixture value and report the result.", "resume": True,
                "mode": "readonly", "model": "deepseek-flash"}}), timeout=10.0)
        if result is None:
            raise AssertionError("Missing fixture RPC response")
        return result


def _registry(executions: list[dict[str, Any]], *, write: bool = False) -> ToolRegistry:
    registry = ToolRegistry(MemoryAuditSink())

    async def handler(arguments: dict[str, Any], _context: Any) -> str:
        executions.append(arguments)
        return "local fixture value accepted"

    registry.register(ToolSpec(
        name="fixture.write" if write else "fixture.read", description="In-memory fixture only.",
        input_schema={"type": "object", "properties": {"value": {"type": "string"}},
                      "required": ["value"], "additionalProperties": False},
        risk_level="write" if write else "read", requires_approval=write,
        handler=handler, target_id_argument=None,
    ))
    return registry


class AgentReasoningLoopTests(unittest.IsolatedAsyncioTestCase):
    def sdk_fixture(self, rounds: list[bytes]) -> tuple[_LoopbackCompletions, DeepSeekChatModel]:
        transport = _LoopbackCompletions(rounds)
        self.addAsyncCleanup(asyncio.to_thread, transport.close)
        sync_client = httpx.Client(trust_env=False, timeout=3.0)
        async_client = httpx.AsyncClient(trust_env=False, timeout=3.0)
        self.addCleanup(sync_client.close)
        self.addAsyncCleanup(async_client.aclose)
        model = DeepSeekChatModel(
            model="deepseek-flash", api_key="fixture-only", base_url=transport.base_url,
            max_retries=0, timeout=3.0, http_client=sync_client, http_async_client=async_client,
            http_socket_options=(),
        )
        return transport, model

    def assert_replayed_exchange(self, payload: dict[str, Any]) -> None:
        self.assertTrue(payload["stream"])
        self.assertTrue(payload["stream_options"]["include_usage"])
        self.assertIn("max_tokens", payload)
        self.assertNotIn("max_completion_tokens", payload)
        messages = payload["messages"]
        calls = [(index, message) for index, message in enumerate(messages)
                 if message.get("tool_calls")]
        self.assertEqual(len(calls), 1)
        index, assistant = calls[0]
        self.assertEqual(assistant["role"], "assistant")
        self.assertEqual(assistant["reasoning_content"], PRIVATE_REASONING)
        self.assertNotIn(PRIVATE_REASONING, assistant.get("content") or "")
        self.assertEqual(assistant["tool_calls"][0]["id"], CALL_ID)
        self.assertEqual(json.loads(assistant["tool_calls"][0]["function"]["arguments"]), {"value": "local"})
        self.assertEqual(messages[index + 1]["role"], "tool")
        self.assertEqual(messages[index + 1]["tool_call_id"], CALL_ID)
        self.assertIn("local fixture value accepted", messages[index + 1]["content"])

    def assert_public(self, value: Any) -> None:
        encoded = json.dumps(value)
        for private in (PRIVATE_REASONING, FINAL_REASONING, FOLLOWUP_REASONING):
            self.assertNotIn(private, encoded)
        self.assertNotIn('"reasoning_content"', encoded)

    async def test_sdk_tool_round_and_reopened_sidecar_history_keep_reasoning_private(self) -> None:
        transport, model = self.sdk_fixture([
            _tool_round("fixture__read"), _answer_round("Local read complete."),
            _answer_round("Follow-up complete.", FOLLOWUP_REASONING),
        ])
        executions: list[dict[str, Any]] = []
        registry = _registry(executions)
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / "conversation.sqlite"
            store = ConversationStore(database)
            first = _SdkSidecar(model, store, registry)
            try:
                response = await first.run_turn("reasoning-first")
                self.assertNotIn("error", response)
                self.assertEqual(response["result"]["result"]["state"], "completed")
                self.assertEqual(response["result"]["result"]["text"], "Local read complete.")
                self.assertEqual(executions, [{"value": "local"}])
                self.assertEqual(len(transport.requests), 2)
                self.assert_replayed_exchange(transport.requests[1])
                self.assertEqual([event["type"] for event in first.events
                                  if event["type"] in ("agent.completed", "agent.failed")], ["agent.completed"])
                self.assertEqual([event["type"] for event in first.events if event["type"] == "agent.tool_result"],
                                 ["agent.tool_result"])
                self.assertFalse(any(event["type"] == "agent.thinking" for event in first.events))
                self.assert_public([first.events, response])
            finally:
                await first.shutdown()
                store.close()

            reopened = ConversationStore(database)
            second = _SdkSidecar(model, reopened, registry)
            try:
                followup = await second.run_turn("reasoning-followup")
                self.assertNotIn("error", followup)
                self.assertEqual(followup["result"]["result"]["text"], "Follow-up complete.")
                self.assertEqual(len(transport.requests), 3)
                self.assert_replayed_exchange(transport.requests[2])
                self.assertEqual(executions, [{"value": "local"}], "History must not execute the earlier tool again")
                self.assert_public([second.events, followup])
                self.assertGreater(followup["result"]["result"]["context"]["includedTurns"], 0)
            finally:
                await second.shutdown()
                reopened.close()
        self.assertEqual(transport.errors, [])
        self.assertEqual(len(transport.remaining), 0)

    async def test_sdk_reasoning_survives_checkpoint_reopen_and_exact_approval(self) -> None:
        transport, model = self.sdk_fixture([
            _tool_round("fixture__write"), _answer_round("Approved local write complete."),
        ])
        executions: list[dict[str, Any]] = []
        registry = _registry(executions, write=True)
        events: list[dict[str, Any]] = []
        budget = AgentBudget(max_model_rounds=3, max_total_seconds=10)
        run = {"task_id": "task-reasoning", "tenant_id": "local", "user_id": "fixture",
               "prompt": "Update the in-memory fixture after explicit approval.",
               "allowed_tools": {"fixture.write"}, "allowed_target_ids": set(),
               "run_id": "reasoning-checkpoint", "checkpoint_binding": "fixture-binding"}
        with tempfile.TemporaryDirectory() as directory:
            async with open_checkpoint_store(directory) as checkpointer:
                first_agent = ModelDrivenAgent(registry, model, budget, checkpointer=checkpointer,
                                               event_sink=events.append)
                first = await asyncio.wait_for(first_agent.run(**run), timeout=10.0)
            self.assertEqual(first.status, "awaiting_approval", first.error_code)
            self.assertEqual(executions, [])
            self.assertEqual(len(transport.requests), 1)
            self.assertIsNotNone(first.checkpoint_id)
            self.assertEqual(len(first.approval_requests), 1)
            approval = first.approval_requests[0]

            async with open_checkpoint_store(directory) as checkpointer:
                restored_agent = ModelDrivenAgent(registry, model, budget, checkpointer=checkpointer,
                                                  event_sink=events.append)
                restored = await asyncio.wait_for(restored_agent.run(**run, require_checkpoint=True), timeout=10.0)
                self.assertEqual(restored.status, "awaiting_approval")
                self.assertEqual(restored.checkpoint_id, first.checkpoint_id)
                self.assertEqual(len(transport.requests), 1, "Reopening must not repeat the first model request")
                self.assertEqual(executions, [])
                result = await asyncio.wait_for(restored_agent.resume_after_approval(
                    restored.checkpoint_id or "", approval.fingerprint, "fixture-approval-once", approved=True,
                ), timeout=10.0)
            self.assertEqual(result.status, "succeeded")
            self.assertEqual(result.answer, "Approved local write complete.")
            self.assertEqual(executions, [{"value": "local"}])
            self.assertEqual(len(transport.requests), 2)
            self.assert_replayed_exchange(transport.requests[1])
            self.assert_public([events, result.answer])
        self.assertEqual(transport.errors, [])
        self.assertEqual(len(transport.remaining), 0)


if __name__ == "__main__":
    unittest.main()
