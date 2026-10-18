from __future__ import annotations

import asyncio
import json
import os
import queue
import time
import unittest
from pathlib import Path
from typing import Any

from langchain_core.language_models.chat_models import BaseChatModel
from langchain_core.messages import AIMessage, AIMessageChunk, HumanMessage, ToolMessage, message_chunk_to_message
from langchain_core.outputs import ChatGenerationChunk

from yukinal_agent.audit import MemoryAuditSink
from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.deepseek_model import DeepSeekChatModel
from yukinal_agent.model_agent import AgentBudget, ModelDrivenAgent
from yukinal_agent.sidecar import PythonSidecar, serve_stdio
from yukinal_agent.tools import ToolRegistry, ToolSpec


class Script:
    def __init__(self, rounds):
        self.rounds = list(rounds)
        self.messages = []
        self.kwargs = []
        self.closed = 0


class FixtureModel(BaseChatModel):
    """Fake transport, real LangChain bind/astream and chunk handling."""
    script: Any

    @property
    def _llm_type(self):
        return "local-streaming-fixture"

    def bind_tools(self, tools, *, parallel_tool_calls):
        return self.bind(tools=tools, parallel_tool_calls=parallel_tool_calls)

    def _generate(self, *_args, **_kwargs):
        raise AssertionError("The streaming fixture must never use invoke/ainvoke")

    async def _astream(self, messages, stop=None, run_manager=None, **kwargs):
        self.script.messages.append(messages)
        self.script.kwargs.append(kwargs)
        if not self.script.rounds:
            raise AssertionError("Unexpected extra model request")
        make_round = self.script.rounds.pop(0)
        try:
            async for chunk in make_round():
                yield ChatGenerationChunk(message=chunk)
        finally:
            self.script.closed += 1


def chunks(*values):
    async def generate():
        for value in values:
            yield value
    return generate


class FixtureSidecar(PythonSidecar):
    def __init__(self, script, registry=None):
        self.script = script
        self.store = ConversationStore()
        super().__init__(registry or ToolRegistry(MemoryAuditSink()), conversation_store=self.store)

    async def _run_provider_model(self, *_args, **_kwargs):
        return FixtureModel(script=self.script), 1000


class QueueReader:
    def __init__(self):
        self.lines = queue.Queue()

    def push(self, value):
        self.lines.put(json.dumps(value, ensure_ascii=False).encode("utf-8") + b"\n")

    def readline(self, _limit):
        return self.lines.get()


class FlushedWriter:
    def __init__(self, reader):
        self.reader = reader
        self.buffer = bytearray()
        self.frames = []
        self.times = []
        self.changed = asyncio.Event()

    def write(self, data):
        self.buffer.extend(data)
        return len(data)

    def flush(self):
        while b"\n" in self.buffer:
            raw, _, rest = self.buffer.partition(b"\n")
            self.buffer = bytearray(rest)
            frame = json.loads(raw)
            self.frames.append(frame)
            self.times.append(time.perf_counter())
            # Only deterministic, empty host context/catalog. No process, model,
            # filesystem, credential service, SSH or external MCP is contacted.
            method = frame.get("method")
            if method == "host.mcp.catalog":
                self.reader.push({"jsonrpc": "2.0", "id": frame["id"], "result": {"servers": []}})
            elif method == "host.chat.context":
                params = frame["params"]
                self.reader.push({"jsonrpc": "2.0", "id": frame["id"], "result": {
                    "session": {"id": params["sessionId"], "serverId": params.get("serverId")}, "messages": []}})
        self.changed.set()

    async def wait(self, predicate):
        async def find():
            while True:
                self.changed.clear()
                match = next((frame for frame in self.frames if predicate(frame)), None)
                if match is not None:
                    return match
                await self.changed.wait()
        return await asyncio.wait_for(find(), 4.0)

    def events(self, kind=None):
        events = [frame["params"] for frame in self.frames if frame.get("method") == "agent.stream"]
        return events if kind is None else [event for event in events if event["type"] == kind]


class StdioFixture:
    def __init__(self, script, registry=None):
        self.sidecar = FixtureSidecar(script, registry)
        self.reader = QueueReader()
        self.writer = FlushedWriter(self.reader)
        self.next_id = 0
        self.started = time.perf_counter()
        self.task = asyncio.create_task(serve_stdio(self.sidecar, reader=self.reader, writer=self.writer))

    async def initialize(self):
        result = await self.rpc("initialize", {"protocolVersion": "1.0", "clientVersion": "fixture", "dataDir": ""})
        if "error" in result:
            raise AssertionError(result["error"]["message"])
        return result["result"]

    def send(self, method, params):
        self.next_id += 1
        self.reader.push({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        return self.next_id

    async def rpc(self, method, params):
        request_id = self.send(method, params)
        return await self.writer.wait(lambda frame: frame.get("id") == request_id and "method" not in frame)

    async def event(self, kind):
        frame = await self.writer.wait(lambda frame: frame.get("method") == "agent.stream" and frame["params"]["type"] == kind)
        return frame["params"]

    async def close(self):
        self.reader.lines.put(b"")
        await asyncio.wait_for(self.task, 4.0)
        self.sidecar.store.close()


def request(**extra):
    return {"runId": "run-stream", "sessionId": "session-stream", "messageId": "message-stream",
            "taskId": "task-stream", "prompt": "A local computation fixture.", "toolAccess": "none", **extra}


class StdioStreamingTests(unittest.IsolatedAsyncioTestCase):
    async def fixture(self, script, registry=None):
        fixture = StdioFixture(script, registry)
        self.addAsyncCleanup(fixture.close)
        initialized = await fixture.initialize()
        self.assertTrue(initialized["capabilities"]["streaming"])
        return fixture

    def assert_one_terminal(self, fixture, kind, state=None):
        terminals = [event for event in fixture.writer.events() if event["type"] in ("agent.completed", "agent.failed")]
        self.assertEqual(len(terminals), 1)
        self.assertEqual(terminals[0]["type"], kind)
        if state is not None:
            self.assertEqual(terminals[0]["result"]["state"], state)
        self.assertFalse(fixture.sidecar._active_runs)
        self.assertFalse(fixture.sidecar._run_sessions)
        return terminals[0]

    async def test_sdk_astream_flushes_before_gate_and_completed_without_replay(self):
        gate = asyncio.Event()
        async def stream():
            yield AIMessageChunk(content="首块🙂")
            await gate.wait()
            yield AIMessageChunk(content="末块", usage_metadata={"input_tokens": 19, "output_tokens": 4, "total_tokens": 23})
        script = Script([stream])
        fixture = await self.fixture(script)
        started = await fixture.rpc("agent.run.start", request(persona={"id": "role-fixture", "label": "Fixture",
            "systemPrompt": "PRIVATE_SYSTEM_SENTINEL_STREAM"}))
        self.assertNotIn("error", started)
        await fixture.event("agent.text")
        self.assertFalse(gate.is_set())
        self.assertFalse(fixture.writer.events("agent.completed"))
        self.assertEqual(script.closed, 0)
        first_frame_index = next(i for i, frame in enumerate(fixture.writer.frames)
                                 if frame.get("method") == "agent.stream" and frame["params"]["type"] == "agent.text")
        first_at = fixture.writer.times[first_frame_index]
        release_at = time.perf_counter()
        gate.set()
        await fixture.event("agent.completed")
        terminal = self.assert_one_terminal(fixture, "agent.completed", "completed")
        answer = "".join(event["textDelta"] for event in fixture.writer.events("agent.text"))
        self.assertEqual(answer, "首块🙂末块")
        self.assertEqual(terminal["result"]["text"], answer)
        self.assertEqual(len(fixture.writer.events("agent.text")), 2)
        self.assertLess(first_at, release_at)
        self.assertEqual(fixture.writer.events("agent.usage")[-1]["usage"]["outputTokens"], 4)
        self.assertEqual(script.kwargs[0]["max_tokens"], 2048)
        system = script.messages[0][0].content
        self.assertIn("yukinal-ui", system)
        self.assertIn("PRIVATE_SYSTEM_SENTINEL_STREAM", system)
        for token in ('type:"card"', 'type:"steps"', 'type:"metrics"', 'type:"progress"', 'type:"details"'):
            self.assertIn(token, system)
        # Guide/persona stay in the private request/checkpoint, never in visible
        # host context, transcripts, notifications or ordinary metadata.
        projection = json.dumps(fixture.writer.frames, ensure_ascii=False)
        self.assertNotIn("PRIVATE_SYSTEM_SENTINEL_STREAM", projection)
        row = fixture.sidecar.store._connection.execute(
            "SELECT metadata_json, messages_json FROM conversation_turns WHERE run_id = 'run-stream'").fetchone()
        self.assertNotIn("PRIVATE_SYSTEM_SENTINEL_STREAM", str(tuple(row)))
        self.assertNotIn("yukinal-ui", str(tuple(row)))
        timing = {"firstTextMs": round((first_at - fixture.started) * 1000, 3),
                  "gateReleasedMs": round((release_at - fixture.started) * 1000, 3),
                  "completedMs": round((fixture.writer.times[-1] - fixture.started) * 1000, 3),
                  "firstTextBeforeCompletion": True, "completedCount": 1, "textDeltaCount": 2,
                  "aggregateMatchesTerminal": answer == terminal["result"]["text"], "paidRequests": 0}
        output = os.environ.get("YUKINAL_STREAM_TEST_TIMING_PATH")
        if output:
            Path(output).write_text(json.dumps(timing, indent=2) + "\n", encoding="utf-8")
        print("STREAMING_TIMING " + json.dumps(timing, separators=(",", ":")))

    async def test_sync_delivery_streams_before_rpc_response(self):
        gate = asyncio.Event()
        async def stream():
            yield AIMessageChunk(content="first")
            await gate.wait()
            yield AIMessageChunk(content=" last")
        fixture = await self.fixture(Script([stream]))
        rpc_id = fixture.send("agent.run.start", request(delivery="sync"))
        await fixture.event("agent.text")
        self.assertFalse(any(frame.get("id") == rpc_id and "method" not in frame for frame in fixture.writer.frames))
        gate.set()
        response = await fixture.writer.wait(lambda frame: frame.get("id") == rpc_id and "method" not in frame)
        self.assertTrue(response["result"]["started"])
        self.assertEqual(response["result"]["result"]["text"], "first last")
        self.assert_one_terminal(fixture, "agent.completed", "completed")

    async def test_partial_tool_call_waits_for_real_approval_then_streams_next_round(self):
        executed = []
        registry = ToolRegistry(MemoryAuditSink())
        async def handler(arguments, _context):
            executed.append(arguments)
            return "local fixture accepted"
        registry.register(ToolSpec(name="ops.write", description="Fixture-only write requiring explicit approval.",
            input_schema={"type": "object", "properties": {"value": {"type": "string"}},
                          "required": ["value"], "additionalProperties": False},
            risk_level="write", requires_approval=True, handler=handler, target_id_argument=None))
        first = chunks(AIMessageChunk(content="Reviewing. "),
            AIMessageChunk(content="", tool_call_chunks=[{"name": "ops__write", "args": '{"value":"frag',
                "id": "provider-call", "index": 0, "type": "tool_call_chunk"}]),
            AIMessageChunk(content="", tool_call_chunks=[{"name": None, "args": 'mented"}',
                "id": None, "index": 0, "type": "tool_call_chunk"}]))
        script = Script([first, chunks(AIMessageChunk(content="Approved result."))])
        fixture = await self.fixture(script, registry)
        params = request()
        params.pop("toolAccess")
        await fixture.rpc("agent.run.start", params)
        waiting = await fixture.event("agent.waiting_approval")
        self.assertEqual(executed, [])
        self.assertEqual(len(script.messages), 1)
        self.assertEqual([event["textDelta"] for event in fixture.writer.events("agent.text")], ["Reviewing. "])
        self.assertFalse(waiting["approval"]["sessionGrantable"])
        decision = {"runId": waiting["runId"], "approvalId": waiting["approval"]["approvalId"],
                    "decision": "approve_once", "respondedAt": "2026-10-18T00:00:00Z"}
        accepted = await fixture.rpc("agent.approval.respond", decision)
        self.assertTrue(accepted["result"]["accepted"])
        await fixture.event("agent.completed")
        terminal = self.assert_one_terminal(fixture, "agent.completed", "completed")
        self.assertEqual(executed, [{"value": "fragmented"}])
        self.assertEqual(terminal["result"]["text"], "Approved result.")
        self.assertEqual([event["textDelta"] for event in fixture.writer.events("agent.text")], ["Reviewing. ", "Approved result."])
        self.assertEqual(len(fixture.writer.events("agent.tool_call")), 1)
        self.assertTrue(any(isinstance(message, ToolMessage) for message in script.messages[1]))
        duplicate = await fixture.rpc("agent.approval.respond", decision)
        self.assertFalse(duplicate["result"]["accepted"])
        self.assertEqual(len(executed), 1)

    async def test_stop_during_generation_closes_stream_and_emits_one_cancelled(self):
        gate = asyncio.Event()
        async def stream():
            yield AIMessageChunk(content="partial")
            await gate.wait()
            yield AIMessageChunk(content="must not arrive")
        script = Script([stream])
        fixture = await self.fixture(script)
        await fixture.rpc("agent.run.start", request())
        await fixture.event("agent.text")
        stopped = await fixture.rpc("agent.run.stop", {"runId": "run-stream"})
        self.assertTrue(stopped["result"]["stopped"])
        await fixture.event("agent.completed")
        self.assert_one_terminal(fixture, "agent.completed", "cancelled")
        self.assertEqual(script.closed, 1)
        gate.set()
        await asyncio.sleep(0)
        self.assertEqual([event["textDelta"] for event in fixture.writer.events("agent.text")], ["partial"])
        saved = fixture.sidecar.store._connection.execute("SELECT status FROM conversation_turns WHERE run_id = 'run-stream'").fetchone()
        self.assertEqual(saved["status"], "cancelled")

    async def test_stream_error_preserves_partial_and_fails_once_without_invoke_retry(self):
        class RateLimitFailure(Exception):
            status_code = 429
        async def stream():
            yield AIMessageChunk(content="partial")
            raise RateLimitFailure("PRIVATE_PROVIDER_ERROR_SENTINEL")
        script = Script([stream])
        fixture = await self.fixture(script)
        await fixture.rpc("agent.run.start", request())
        await fixture.event("agent.failed")
        failed = self.assert_one_terminal(fixture, "agent.failed")
        self.assertEqual(failed["error"], "model_rate_limited")
        self.assertEqual([event["textDelta"] for event in fixture.writer.events("agent.text")], ["partial"])
        self.assertEqual(len(script.messages), 1)
        self.assertEqual(script.closed, 1)
        self.assertNotIn("PRIVATE_PROVIDER_ERROR_SENTINEL", json.dumps(fixture.writer.frames))

    async def test_non_bmp_chunks_fit_utf16_contract_and_thinking_excludes_private_fields(self):
        script = Script([chunks(AIMessageChunk(content=[
            {"type": "thinking", "thinking": "Public summary.", "signature": "PRIVATE_SIGNATURE"},
            {"type": "text", "text": "🙂" * 21000},
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": "A public reason."}], "encrypted_content": "PRIVATE_ENCRYPTED"},
            {"type": "tool_use", "text": "PRIVATE_TOOL_ARGUMENT"}],
            additional_kwargs={"reasoning_content": "PRIVATE_DEEPSEEK_REASONING"}))])
        fixture = await self.fixture(script)
        await fixture.rpc("agent.run.start", request())
        await fixture.event("agent.completed")
        terminal = self.assert_one_terminal(fixture, "agent.completed", "completed")
        deltas = fixture.writer.events("agent.text")
        self.assertEqual(len(deltas), 3)
        self.assertTrue(all(len(event["textDelta"].encode("utf-16-le")) // 2 <= 20000 for event in deltas))
        self.assertEqual("".join(event["textDelta"] for event in deltas), terminal["result"]["text"])
        self.assertEqual("".join(event["textDelta"] for event in fixture.writer.events("agent.thinking")), "Public summary.A public reason.")
        frames = json.dumps(fixture.writer.frames)
        for private in ("PRIVATE_SIGNATURE", "PRIVATE_ENCRYPTED", "PRIVATE_TOOL_ARGUMENT", "PRIVATE_DEEPSEEK_REASONING"):
            self.assertNotIn(private, frames)


class ModelStreamingBoundaryTests(unittest.IsolatedAsyncioTestCase):
    async def run_model(self, script, budget=None, events=None):
        return await ModelDrivenAgent(ToolRegistry(MemoryAuditSink()), FixtureModel(script=script), budget,
            event_sink=(events.append if events is not None else None)).run(
                task_id="task-boundary", tenant_id="local", user_id="fixture", prompt="Local fixture.",
                allowed_tools=set(), allowed_target_ids=set(), run_id="run-boundary")

    async def test_public_stream_budget_fails_closed_before_oversized_emission(self):
        events = []
        script = Script([chunks(AIMessageChunk(content="1234"), AIMessageChunk(content="56789"))])
        result = await self.run_model(script, AgentBudget(max_output_bytes=8), events)
        self.assertEqual(result.error_code, "output_budget_exceeded")
        self.assertEqual([event["textDelta"] for event in events if event["type"] == "agent.text"], ["1234"])
        self.assertEqual(script.closed, 1)

    async def test_empty_sdk_stream_is_invalid_and_does_not_invoke_again(self):
        script = Script([chunks()])
        result = await self.run_model(script)
        self.assertEqual(result.error_code, "model_request_failed")
        self.assertEqual(len(script.messages), 1)
        self.assertEqual(script.closed, 1)


class DeepSeekStreamingAdapterTests(unittest.TestCase):
    def test_sdk_delta_conversion_keeps_private_reasoning_tool_fragments_and_actual_usage(self):
        model = DeepSeekChatModel(model="deepseek-reasoner", api_key="unused-fixture-key", base_url="http://127.0.0.1:1")
        self.assertTrue(model.stream_usage)
        raw = [
            {"choices": [{"delta": {"role": "assistant", "content": "", "reasoning_content": "private "}, "index": 0}]},
            {"choices": [{"delta": {"content": "", "reasoning_content": "thinking", "tool_calls": [{"index": 0,
                "id": "fixture-call", "type": "function", "function": {"name": "ops__read", "arguments": '{"value":'}}]}, "index": 0}]},
            {"choices": [{"delta": {"content": "", "tool_calls": [{"index": 0, "function": {"arguments": '"ok"}'}}]}, "index": 0,
                          "finish_reason": "tool_calls"}]},
            {"choices": [], "usage": {"prompt_tokens": 17, "completion_tokens": 9, "total_tokens": 26}},
        ]
        combined = None
        for delta in raw:
            converted = model._convert_chunk_to_generation_chunk(delta, AIMessageChunk, None)
            self.assertIsNotNone(converted)
            combined = converted.message if combined is None else combined + converted.message
        message = message_chunk_to_message(combined)
        self.assertIsInstance(message, AIMessage)
        self.assertEqual(message.additional_kwargs["reasoning_content"], "private thinking")
        self.assertEqual(message.tool_calls[0]["args"], {"value": "ok"})
        self.assertEqual(message.usage_metadata["output_tokens"], 9)
        payload = model._get_request_payload([HumanMessage(content="fixture"), message,
            ToolMessage(content="ok", tool_call_id="fixture-call")], max_tokens=32)
        self.assertEqual(payload["messages"][1]["reasoning_content"], "private thinking")
        self.assertEqual(message.content, "")
        self.assertEqual(payload["max_tokens"], 32)


if __name__ == "__main__":
    unittest.main()
