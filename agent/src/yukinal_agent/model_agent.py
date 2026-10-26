from __future__ import annotations

import asyncio
import hashlib
import json
import re
import time
import uuid
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from typing import Any, TypedDict

from langchain_core.messages import AIMessage, AIMessageChunk, HumanMessage, SystemMessage, ToolMessage, message_chunk_to_message
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.graph import END, START, StateGraph

from .host_tools import HOST_APPROVAL_TOOLS
from .agent_stage import STAGE_SET, derive_report_stage, derive_stage, stage_for_start
from .tools import ExecutionContext, ToolCallError, ToolRegistry
from .visual_output import VISUAL_OUTPUT_GUIDE, VISUAL_SELECTION_GUIDE
from .response_policy import RESPONSE_STYLE
from .model_metrics import accumulate_usage, request_metrics
from .model_context import model_messages
from .goal_loop import GOAL_GUIDE, REVIEW_GUIDE, parse_review, review_payload, successful_evidence
from .operations_policy import build_system_prompt
from .shell_policy import prepare_model_tool_arguments

# The safety contract and the operating procedure live in `operations_policy` so
# the model input and the context budget assembly cannot drift apart. Presentation
# guidance stays here because it describes this application's renderer.
# ADR 0004: tools are dot-namespaced internally (`server.info`) but most
# OpenAI-compatible endpoints restrict function names to `[A-Za-z0-9_-]`. The
# provider boundary uses `__`, and the mapping is applied in both directions so a
# trace never shows a name the registry does not know.
_PROVIDER_TOOL_NAME_PATTERN = re.compile(r"^[A-Za-z0-9_-]{1,64}$")


# Private model guidance: never add it to user messages, public events or metadata.
_UI_DISPLAY_GUIDE = """For useful presentation, prefer ordinary Markdown. When it materially helps,
you may include a fenced `yukinal-ui` JSON block: {"version":1,"widgets":[...]}.
Widgets are display-only, never tool calls, actions, HTML or executable/external resources.
Supported objects (type is the literal name):
card: {type:"card",title,body,tone?}; tone is neutral (default), info, success, warning or error.
steps: {type:"steps",title,items:[{title,status,detail?}]}; status is pending, active, done or error.
metrics: {type:"metrics",title,items:[{label,value,unit?,trend?}]}; value is a string or finite number; trend is finite numbers.
progress: {type:"progress",title,value,max,detail?}; finite numbers, max>0 and 0<=value<=max.
details: {type:"details",title,body}.
Use valid JSON with quoted keys. At most 12 widgets, 20 steps/widget, 8 metrics/widget,
40 trend points/metric; title/label<=160 characters, body/detail<=8000, unit<=32,
metric string value<=120; keep the whole JSON within 64 KiB of text.
Use only actual known data; never invent progress, percentages, token usage or execution status.
Omit unavailable values and use Markdown to explain uncertainty. Do not force widgets into short replies.
"""
# The renderer-specific guides are appended to every run's system prompt by
# `build_system_prompt`; they are not part of the operations procedure itself.
RENDERER_GUIDE = "\n\n".join((RESPONSE_STYLE, _UI_DISPLAY_GUIDE, VISUAL_SELECTION_GUIDE, VISUAL_OUTPUT_GUIDE))


class _InvalidModelResponse(ValueError):
    pass


class _StreamOutputExceeded(ValueError):
    pass


def _to_provider_tool_name(internal_name: str) -> str:
    provider_name = internal_name.replace(".", "__")
    if not _PROVIDER_TOOL_NAME_PATTERN.fullmatch(provider_name):
        raise ValueError(
            f"tool name {internal_name!r} cannot be represented at the provider boundary"
        )
    return provider_name


def _from_provider_tool_name(provider_name: str) -> str:
    if not isinstance(provider_name, str) or not _PROVIDER_TOOL_NAME_PATTERN.fullmatch(provider_name):
        raise ValueError("invalid provider tool name")
    return provider_name.replace("__", ".")


@dataclass(frozen=True)
class AgentBudget:
    max_tool_calls: int | None = None
    max_model_rounds: int | None = None
    max_total_seconds: float = 120.0
    max_tool_seconds: float = 30.0
    max_output_bytes: int = 1024 * 1024
    max_model_input_bytes: int = 1024 * 1024
    max_model_output_tokens: int = 2048
    max_parallel_tools: int = 3

    def __post_init__(self) -> None:
        if type(self.max_parallel_tools) is not int or not 1 <= self.max_parallel_tools <= 4:
            raise ValueError("max_parallel_tools must be between 1 and 4")
        if self.max_tool_calls is not None and (isinstance(self.max_tool_calls, bool) or not isinstance(self.max_tool_calls, int) or not 1 <= self.max_tool_calls <= 100):
            raise ValueError("max_tool_calls must be None or between 1 and 100")
        if self.max_model_rounds is not None and (isinstance(self.max_model_rounds, bool) or not isinstance(self.max_model_rounds, int) or not 1 <= self.max_model_rounds <= 128):
            raise ValueError("max_model_rounds must be None or between 1 and 128")
        if isinstance(self.max_total_seconds, bool) or not isinstance(self.max_total_seconds, (int, float)) or not 0.01 <= self.max_total_seconds <= 3600:
            raise ValueError("max_total_seconds must be between 0.01 and 3600")
        if isinstance(self.max_tool_seconds, bool) or not isinstance(self.max_tool_seconds, (int, float)) or not 0.01 <= self.max_tool_seconds <= 300:
            raise ValueError("max_tool_seconds must be between 0.01 and 300")
        if isinstance(self.max_output_bytes, bool) or not isinstance(self.max_output_bytes, int) or not 1 <= self.max_output_bytes <= 16 * 1024 * 1024:
            raise ValueError("max_output_bytes must be between 1 and 16777216")
        if isinstance(self.max_model_input_bytes, bool) or not isinstance(self.max_model_input_bytes, int) or not 1024 <= self.max_model_input_bytes <= 16 * 1024 * 1024:
            raise ValueError("max_model_input_bytes must be between 1024 and 16777216")
        if isinstance(self.max_model_output_tokens, bool) or not isinstance(self.max_model_output_tokens, int) or not 1 <= self.max_model_output_tokens <= 32768:
            raise ValueError("max_model_output_tokens must be between 1 and 32768")


@dataclass(frozen=True)
class ApprovalRequest:
    fingerprint: str
    tool_name: str
    target_id: str | None
    risk_level: str
    arguments: dict[str, Any]
    host_input: dict[str, Any] | None = None


@dataclass(frozen=True)
class ModelAgentResult:
    task_id: str
    status: str
    answer: str
    tool_results: tuple[dict[str, Any], ...]
    approval_requests: tuple[ApprovalRequest, ...]
    tool_calls: int
    output_bytes: int
    error_code: str | None = None
    checkpoint_id: str | None = None


class _AgentState(TypedDict, total=False):
    token_usage: dict[str, Any]
    messages: list[Any]
    turn_start_index: int
    task_id: str
    tenant_id: str
    user_id: str
    allowed_tools: list[str]
    allowed_target_ids: list[str]
    approved_actions: dict[str, str]
    host_target: dict[str, Any]
    host_task_id: str
    plan_id: str
    plan_step_id: str
    evidence_ids: list[str]
    run_id: str
    trace_id: str
    checkpoint_binding: str
    budget_binding: dict[str, Any]
    calls: list[dict[str, Any]]
    call_index: int
    model_rounds: int
    tool_call_count: int
    output_bytes: int
    streamed_bytes: int
    answer: str
    tool_results: list[dict[str, Any]]
    approval_requests: list[dict[str, Any]]
    recovery_read_required: bool
    recovery_diagnosis_required: bool
    status: str
    error_code: str | None
    rejected_call_ids: list[str]
    rejection_codes: dict[str, str]
    counted_call_ids: list[str]
    goal_loop_enabled: bool
    goal_review_pending: bool
    goal_reviewed_calls: int
    goal_last_tool_count: int
    goal_stalls: int
    stage: str


class ModelDrivenAgent:
    """LangGraph model/tool loop; all tool execution remains behind ToolRegistry."""

    def __init__(
        self,
        registry: ToolRegistry,
        model: Any,
        budget: AgentBudget | None = None,
        event_sink: Any | None = None,
        checkpointer: Any | None = None,
        transcript_sink: Any | None = None,
        tool_names: frozenset[str] | None = None,
        inference_profile: dict[str, Any] | None = None,
        goal_loop: bool = False,
    ) -> None:
        self._registry = registry
        self._budget = budget or AgentBudget()
        self._event_sink = event_sink
        self._transcript_sink = transcript_sink
        self._inference_profile = dict(inference_profile or {})
        self._goal_loop = bool(goal_loop)
        self._review_model = model.bind(max_tokens=min(1536, self._budget.max_model_output_tokens)) if goal_loop else None
        self._request_timing: dict[str, Any] = {}
        self._approval_checkpoints: set[str] = set()
        tool_specs = tuple(spec for spec in registry.list() if tool_names is None or spec.name in tool_names)
        provider_to_internal: dict[str, str] = {}
        for spec in tool_specs:
            provider_name = _to_provider_tool_name(spec.name)
            existing = provider_to_internal.get(provider_name)
            if existing is not None and existing != spec.name:
                raise ValueError("registered tool names collide at the provider boundary")
            provider_to_internal[provider_name] = spec.name
        self._provider_to_internal = provider_to_internal
        tool_definitions = [
            {
                "type": "function",
                "function": {
                    "name": _to_provider_tool_name(spec.name),
                    "description": spec.description[:2000],
                    "parameters": spec.input_schema,
                },
            }
            for spec in tool_specs
        ]
        self._tool_definition_bytes = len(
            json.dumps(tool_definitions, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        )
        if self._tool_definition_bytes > self._budget.max_model_input_bytes:
            raise ValueError("registered tool definitions exceed the model input budget")
        try:
            # Providers may propose a batch; the executor alone decides which
            # explicitly safe reads can overlap. Changes remain ordered.
            bound = model.bind_tools(tool_definitions, parallel_tool_calls=True)
            self._model = bound.bind(max_tokens=self._budget.max_model_output_tokens)
        except Exception:
            raise ValueError("configured model does not support bounded tool calling") from None
        graph = StateGraph(_AgentState)
        graph.add_node("model", self._call_model_recorded)
        graph.add_node("execute_tool", self._execute_tool_recorded)
        graph.add_node("goal_review", self._review_goal_recorded)
        graph.add_edge(START, "model")
        graph.add_conditional_edges("model", self._route_model, {"execute": "execute_tool", "review": "goal_review", "continue": "model", "finish": END})
        graph.add_conditional_edges("goal_review", lambda state: "model" if state.get("status") == "running" else "finish", {"model": "model", "finish": END})
        graph.add_conditional_edges(
            "execute_tool", self._route_tool,
            {"execute": "execute_tool", "model": "model", "review": "goal_review", "finish": END},
        )
        self._graph = graph.compile(
            checkpointer=checkpointer if checkpointer is not None else InMemorySaver()
        )

    def _system_prompt(self, prompt: str, persona: dict[str, str] | None) -> str:
        # One assembly for both the real model input and the context budget; a
        # second copy is how a budget starts lying about what was sent.
        return build_system_prompt(
            goal_guide=GOAL_GUIDE if self._goal_loop else "",
            persona=persona,
            user_text=prompt,
            extra=RENDERER_GUIDE,
        )

    def conversation_budget_bytes(self, prompt: str, persona: dict[str, str] | None = None) -> int:
        system = self._system_prompt(prompt, persona)
        current_bytes = len(json.dumps(
            [SystemMessage(content=system).model_dump(mode="json"), HumanMessage(content=prompt).model_dump(mode="json")],
            ensure_ascii=False, separators=(",", ":"),
        ).encode("utf-8"))
        # The registry and current message take priority over archive excerpts.
        # Leave room for the provider envelope and the first tool response.
        return min(144 * 1024, max(0, self._budget.max_model_input_bytes - self._tool_definition_bytes - current_bytes - 8192))

    async def run(
        self,
        *,
        task_id: str,
        tenant_id: str,
        user_id: str,
        prompt: str,
        allowed_tools: set[str] | frozenset[str],
        allowed_target_ids: set[str] | frozenset[str],
        approved_actions: dict[str, str] | None = None,
        host_target: dict[str, Any] | None = None,
        host_task_id: str | None = None,
        plan_id: str | None = None,
        plan_step_id: str | None = None,
        evidence_ids: tuple[str, ...] = (),
        run_id: str = "",
        trace_id: str = "",
        checkpoint_binding: str = "",
        require_checkpoint: bool = False,
        persona: dict[str, str] | None = None,
        conversation_messages: list[Any] | None = None,
    ) -> ModelAgentResult:
        if not isinstance(require_checkpoint, bool):
            return _failed_result(task_id, "invalid_checkpoint_requirement")
        try:
            context = ExecutionContext(
                task_id=task_id,
                tenant_id=tenant_id,
                user_id=user_id,
                allowed_tools=frozenset(allowed_tools),
                allowed_target_ids=frozenset(allowed_target_ids),
                approved_actions=approved_actions or {},
                host_target=host_target,
                host_task_id=host_task_id,
                plan_id=plan_id,
                plan_step_id=plan_step_id,
                evidence_ids=evidence_ids,
                run_id=run_id,
                trace_id=trace_id,
                context_target_id=(
                    host_target.get("serverId")
                    if isinstance(host_target, dict) and host_target.get("host") == "remote"
                    else None
                ),
            )
        except (TypeError, ValueError):
            return _failed_result(task_id, "invalid_execution_context")
        if not isinstance(prompt, str) or not prompt.strip() or len(prompt.encode("utf-8")) > 64 * 1024:
            return _failed_result(task_id, "invalid_prompt")
        history = conversation_messages or []
        if not isinstance(history, list) or any(not isinstance(item, (HumanMessage, AIMessage, ToolMessage)) for item in history):
            return _failed_result(task_id, "invalid_conversation_context")
        if persona is not None:
            if (
                not isinstance(persona, dict) or set(persona) != {"id", "label", "systemPrompt"}
                or any(not isinstance(value, str) or not value.strip() for value in persona.values())
                or len(persona["id"]) > 256 or len(persona["label"]) > 200 or len(persona["systemPrompt"]) > 20_000
            ):
                return _failed_result(task_id, "invalid_persona")
        system = self._system_prompt(prompt, persona)

        initial: _AgentState = {
            "messages": [SystemMessage(content=system), *history, HumanMessage(content=prompt)],
            "turn_start_index": 1 + len(history),
            "task_id": context.task_id,
            "tenant_id": context.tenant_id,
            "user_id": context.user_id,
            "allowed_tools": sorted(context.allowed_tools),
            "allowed_target_ids": sorted(context.allowed_target_ids),
            "approved_actions": dict(context.approved_actions),
            **({"host_target": dict(host_target)} if host_target is not None else {}),
            **({"host_task_id": host_task_id} if host_task_id else {}),
            **({"plan_id": plan_id, "plan_step_id": plan_step_id, "evidence_ids": list(evidence_ids)} if plan_id is not None else {}),
            "run_id": run_id,
            "trace_id": trace_id,
            "checkpoint_binding": checkpoint_binding,
            "budget_binding": asdict(self._budget),
            "calls": [],
            "call_index": 0,
            "model_rounds": 0,
            "tool_call_count": 0,
            "output_bytes": 0,
            "streamed_bytes": 0,
            "answer": "",
            "tool_results": [],
            "approval_requests": [],
            "recovery_read_required": False,
            "recovery_diagnosis_required": False,
            "status": "running",
            "error_code": None,
            "rejected_call_ids": [],
            "rejection_codes": {},
            "counted_call_ids": [],
            "goal_loop_enabled": self._goal_loop,
            "goal_review_pending": False,
            "goal_reviewed_calls": 0,
            "goal_last_tool_count": -1,
            "goal_stalls": 0,
            "stage": "intake",
        }
        if run_id:
            thread_material = "\x1f".join((context.tenant_id, context.user_id, context.task_id, run_id))
            checkpoint_id = f"agent-{hashlib.sha256(thread_material.encode('utf-8')).hexdigest()}"
        else:
            checkpoint_id = f"agent-{uuid.uuid4().hex}"
        config = self._graph_config(checkpoint_id)
        try:
            async with asyncio.timeout(self._budget.max_total_seconds):
                snapshot = await self._graph.aget_state(config)
                saved_state = snapshot.values
                if saved_state:
                    if not _checkpoint_matches(saved_state, initial):
                        return _failed_result(task_id, "checkpoint_context_mismatch")
                    if saved_state.get("status") in {"awaiting_approval", "succeeded", "failed", "cancelled", "timed_out"}:
                        state = saved_state
                    else:
                        state = await self._graph.ainvoke(None, config=config)
                elif require_checkpoint:
                    return _failed_result(task_id, "checkpoint_missing")
                else:
                    state = await self._graph.ainvoke(initial, config=config)
        except TimeoutError:
            return _failed_result(task_id, "task_timeout", status="timed_out")
        except Exception:
            return _failed_result(task_id, "agent_workflow_failed")
        if state.get("status") == "awaiting_approval":
            self._approval_checkpoints.add(checkpoint_id)
        return self._result_from_state(
            state,
            task_id=task_id,
            checkpoint_id=checkpoint_id if state.get("status") == "awaiting_approval" else None,
        )

    async def _call_model_recorded(self, state: _AgentState) -> dict[str, Any]:
        self._request_timing = {}
        update = await self._call_model(state)
        return self._record_model_update(state, update)

    def _record_model_update(self, state: _AgentState, update: dict[str, Any], response: AIMessage | None = None) -> dict[str, Any]:
        messages = update.get("messages", [])
        if self._request_timing:
            if response is None:
                response = messages[-1] if len(messages) > len(state["messages"]) and isinstance(messages[-1], AIMessage) else None
            usage = accumulate_usage(state.get("token_usage", {}), response)
            self._request_timing["durationMs"] = round((time.perf_counter() - self._request_timing["started"]) * 1000, 2)
            usage["lastRequest"] = request_metrics(response, self._request_timing, self._inference_profile)
            update["token_usage"] = usage
            if self._event_sink is not None and state.get("run_id"):
                self._event_sink({"type": "agent.usage", "runId": state["run_id"], "usage": usage, "at": datetime.now(UTC).isoformat()})
        self._advance_answer_stage(state, update)
        self._record_transcript(state, update)
        return update

    def _advance_answer_stage(self, state: _AgentState, update: dict[str, Any]) -> None:
        """The final answer is the only thing that can reach the ``report`` stage.

        It is not derived from tool results (an answer is not evidence), so it is
        kept separate from ``_advance_stage``. The stage is emitted once, and only
        when an answer actually exists, so a run that blocks or times out never
        claims it has finished reporting.
        """
        answer = update.get("answer")
        if not isinstance(answer, str) or not answer.strip():
            return
        stage = derive_report_stage(state.get("stage"))
        if state.get("stage") == stage and stage in STAGE_SET:
            return
        update["stage"] = stage
        if self._event_sink is None:
            return
        self._event_sink({
            "type": "agent.stage",
            "runId": state.get("run_id", ""),
            **({"taskId": state["host_task_id"]} if state.get("host_task_id") else {}),
            "stage": stage,
            "previous": state.get("stage") or "intake",
            "at": datetime.now(UTC).isoformat(),
        })

    async def _review_goal_recorded(self, state: _AgentState) -> dict[str, Any]:
        self._request_timing = {}
        update = await self._review_goal(state)
        response = update.pop("_review_response", None)
        return self._record_model_update(state, update, response)

    async def _review_goal(self, state: _AgentState) -> dict[str, Any]:
        next_round = state["model_rounds"] + 1
        if self._budget.max_model_rounds is not None and next_round > self._budget.max_model_rounds:
            return {"status": "failed", "error_code": "model_round_limit_exceeded"}
        prompt = state["messages"][state["turn_start_index"]].content
        messages = [SystemMessage(content=REVIEW_GUIDE), HumanMessage(content=review_payload(str(prompt), state["answer"], state["tool_results"]))]
        if len(json.dumps([message.model_dump(mode="json") for message in messages], ensure_ascii=False).encode("utf-8")) > self._budget.max_model_input_bytes:
            return {"status": "failed", "error_code": "model_input_budget_exceeded"}
        try:
            response, streamed = await asyncio.wait_for(self._stream_model({**state, "messages": messages}, model=self._review_model, public=False), timeout=min(90.0, self._budget.max_total_seconds))
        except _StreamOutputExceeded:
            return {"status": "failed", "error_code": "output_budget_exceeded"}
        except _InvalidModelResponse:
            return {"status": "failed", "error_code": "invalid_model_response"}
        except Exception as error:
            return {"status": "failed", "error_code": _model_error_code(error)}
        text = _message_text(response.content)
        size = len(text.encode("utf-8"))
        update: dict[str, Any] = {"model_rounds": next_round, "streamed_bytes": streamed,
                                  "output_bytes": state["output_bytes"] + size,
                                  "goal_review_pending": False, "goal_reviewed_calls": state["tool_call_count"],
                                  "_review_response": response}
        reason = response.response_metadata.get("finish_reason") or response.response_metadata.get("stop_reason")
        review = parse_review(text, state["tool_results"]) if not response.tool_calls and not response.invalid_tool_calls and reason not in {"length", "max_tokens", "max_output_tokens", "content_filter", "refusal", "safety"} and response.response_metadata.get("status") != "incomplete" else None
        if state["output_bytes"] + size > self._budget.max_output_bytes:
            return {**update, "status": "failed", "error_code": "output_budget_exceeded"}
        if review is None:
            return {**update, "status": "failed", "error_code": "goal_review_failed"}
        if review["state"] == "completed":
            if not state.get("answer", "").strip():
                return {**update, "messages": [*state["messages"], SystemMessage(content=
                    "Completion evidence satisfies the original goal. Return a concise final result with verified evidence and any limitations; do not repeat completed operations.")],
                    "calls": [], "status": "running", "error_code": None}
            return {**update, "status": "succeeded", "error_code": None}
        if review["state"] == "blocked":
            if self._event_sink is not None:
                self._event_sink({"type": "agent.text", "runId": state.get("run_id", ""), "textDelta": "\n\n任务阻塞：" + _redact_sensitive_text(review["reason"]) + "\n", "at": datetime.now(UTC).isoformat()})
            return {**update, "status": "failed", "error_code": "goal_blocked"}
        usable = successful_evidence(state["tool_results"])
        progress_count = len({(item.get("tool"), item.get("result_digest", item["call_id"]))
                              for item in state["tool_results"] if item.get("call_id") in usable})
        stalls = state.get("goal_stalls", 0) + 1 if progress_count == state.get("goal_last_tool_count") else 0
        if stalls >= 2:
            return {**update, "status": "failed", "error_code": "goal_no_progress"}
        remaining = json.dumps(review["remaining"], ensure_ascii=False)
        instruction = SystemMessage(content="Completion review found unfinished requirements in the original goal: " + remaining +
                                    ". Continue this SAME run using the existing evidence and allowed tools. Do not ask for 'continue', replay completed actions, expand scope, or bypass approvals. " +
                                    f"Used {state['tool_call_count']} tool calls and {next_round} model requests. Work still shares the admitted time, output, context and any explicit task budget.")
        if self._event_sink is not None:
            direction = _truncate_utf16(_redact_sensitive_text(review["remaining"][0]), 120)
            self._event_sink({"type": "agent.text", "runId": state.get("run_id", ""), "textDelta": "\n\n接下来：" + direction + "\n", "at": datetime.now(UTC).isoformat()})
        return {**update, "messages": [*state["messages"], instruction], "answer": "", "calls": [],
                "goal_stalls": stalls, "goal_last_tool_count": progress_count, "status": "running", "error_code": None}

    async def _execute_tool_recorded(self, state: _AgentState) -> dict[str, Any]:
        # Keep host-created bindings per call, without changing the original
        # request/task scope used to verify a restored checkpoint.
        working = dict(state)
        group = []
        for index in range(state["call_index"], min(len(state["calls"]), state["call_index"] + self._budget.max_parallel_tools)):
            spec = self._registry.get(state["calls"][index]["name"])
            if spec is None or not spec.parallel_safe:
                break
            group.append(index)
        if len(group) > 1 and self._budget.max_output_bytes - state["output_bytes"] >= len(group):
            update = await self._execute_read_batch(state, group)
        else:
            update = await self._execute_tool(working)
        self._advance_stage(state, update)
        combined = {**state, **update}
        if (combined.get("goal_loop_enabled") and combined.get("status") == "running"
                and not combined.get("goal_review_pending")
                and combined["call_index"] >= len(combined["calls"])
                and combined["tool_call_count"] - combined.get("goal_reviewed_calls", 0) >= 20):
            # A progress checkpoint redirects the same run; it is not a call
            # quota. Finish every admitted call in the current batch first.
            update.update(goal_review_pending=True, answer="", calls=[], call_index=0)
        self._record_transcript(state, update)
        return update

    def _advance_stage(self, state: _AgentState, update: dict[str, Any]) -> None:
        """Derive the visible stage from the results this step actually produced.

        A stage is emitted only when it changes, and only from executed outcomes —
        the model cannot claim a stage it did not reach by writing text. A batch is
        folded in order, so a change followed by its verification read still moves
        the run forward rather than reporting only the last read.
        """
        stage = stage_for_start(state.get("stage"))
        seen = {(item.get("call_id"), item.get("status")) for item in state.get("tool_results", ())}
        for item in update.get("tool_results", ()):
            if (item.get("call_id"), item.get("status")) in seen:
                continue
            spec = self._registry.get(str(item.get("tool", "")))
            stage = derive_stage(
                stage,
                tool=str(item.get("tool", "")),
                risk=getattr(spec, "risk_level", "read"),
                succeeded=item.get("status") == "succeeded",
                phase=item.get("phase"),
                error_code=item.get("error_code"),
            )
        if stage == state.get("stage"):
            return
        update["stage"] = stage
        if self._event_sink is not None:
            self._event_sink({
                "type": "agent.stage",
                "runId": state.get("run_id", ""),
                # A durable task binds its own progress to this event; a chat run
                # without a task simply omits the field.
                **({"taskId": state["host_task_id"]} if state.get("host_task_id") else {}),
                "stage": stage,
                "previous": state.get("stage") or "intake",
                "at": datetime.now(UTC).isoformat(),
            })

    async def _execute_read_batch(self, state: _AgentState, indices: list[int]) -> dict[str, Any]:
        # Only consecutive, explicitly independent reads overlap. A write, plan
        # mutation, unknown tool, or coordination call is an ordering barrier.
        share = (self._budget.max_output_bytes - state["output_bytes"]) // len(indices)
        tasks = [asyncio.create_task(self._execute_tool({**state, "call_index": index}, output_allowance=share)) for index in indices]
        try:
            updates = await asyncio.gather(*tasks)
        except BaseException:
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            raise
        messages = list(state["messages"])
        results = list(state["tool_results"])
        counted = list(state.get("counted_call_ids", []))
        pending = []
        first_pending_approval = None
        output_bytes = state["output_bytes"]
        failure = None
        recovery_read_required = bool(state.get("recovery_read_required"))
        recovery_diagnosis_required = bool(state.get("recovery_diagnosis_required"))
        for index, update in zip(indices, updates):
            call_id = state["calls"][index]["call_id"]
            messages.extend(update.get("messages", [])[len(state["messages"]):])
            results = [item for item in results if item.get("call_id") != call_id]
            results.extend(item for item in update.get("tool_results", []) if item.get("call_id") == call_id)
            if call_id not in counted:
                counted.append(call_id)
            output_bytes += update.get("output_bytes", state["output_bytes"]) - state["output_bytes"]
            # A sibling that failed with a diagnosis keeps the run going, but its
            # recovery guards must apply to the rest of this run: a successful read
            # in the same batch may be that diagnosis, a later effectful call must
            # still wait for one when the outcome is unknown. Fold the batch in
            # order so the rules match the sequential path exactly.
            sibling = self._registry.get(state["calls"][index]["name"])
            successful_read = (
                sibling is not None and sibling.risk_level == "read"
                and update.get("status") == "running"
                and any(item.get("call_id") == call_id and item.get("status") == "succeeded"
                        for item in update.get("tool_results", ()))
            )
            if successful_read:
                recovery_read_required = False
                recovery_diagnosis_required = False
            else:
                recovery_read_required = bool(update.get("recovery_read_required", recovery_read_required))
                recovery_diagnosis_required = bool(update.get("recovery_diagnosis_required", recovery_diagnosis_required))
            if update.get("status") == "awaiting_approval":
                pending.append(update["calls"][index])
                if first_pending_approval is None:
                    first_pending_approval = update["approval_requests"][-1]
            elif update.get("status") != "running" and failure is None:
                failure = update
        merged = {"messages": messages, "tool_results": results, "counted_call_ids": counted,
                  "tool_call_count": len(counted), "output_bytes": output_bytes,
                  "call_index": indices[-1] + 1, "status": "running", "error_code": None,
                  "recovery_read_required": recovery_read_required,
                  "recovery_diagnosis_required": recovery_diagnosis_required}
        if failure is not None:
            merged.update(status="failed", error_code=failure.get("error_code", "tool_failed"))
        elif pending:
            # A custom authorization policy may still require approval. Keep
            # only unfinished calls so successful siblings never replay, and
            # expose one pending decision at a time like the sequential path.
            merged.update(calls=[*pending, *state["calls"][indices[-1] + 1:]], call_index=0,
                          approval_requests=[first_pending_approval], status="awaiting_approval", error_code="approval_required")
        return merged

    def _record_transcript(self, state: _AgentState, update: dict[str, Any]) -> None:
        if self._transcript_sink is not None:
            messages = update.get("messages", state["messages"])
            self._transcript_sink([message for message in messages[state.get("turn_start_index", 1):] if not isinstance(message, SystemMessage)])

    def _graph_config(self, checkpoint_id: str) -> dict[str, Any]:
        # LangGraph defaults to 25 graph transitions, unrelated to our admitted
        # action budget. A valid long tool loop must not trip that hidden limit.
        transitions = (self._budget.max_tool_calls + self._budget.max_model_rounds * 2 + 8
                       if self._budget.max_tool_calls is not None and self._budget.max_model_rounds is not None else 2**31 - 1)
        return {"configurable": {"thread_id": checkpoint_id}, "recursion_limit": transitions}

    async def resume_after_approval(
        self,
        checkpoint_id: str,
        approval_fingerprint: str,
        approval_ref: str | None,
        *,
        approved: bool,
        rejection_code: str = "approval_rejected",
    ) -> ModelAgentResult:
        """Resume one paused graph after a trusted host returns its approval decision."""
        if (
            not isinstance(checkpoint_id, str)
            or checkpoint_id not in self._approval_checkpoints
            or not isinstance(approval_fingerprint, str)
            or not approval_fingerprint
            or not isinstance(approved, bool)
            or rejection_code not in ("approval_rejected", "approval_timeout")
            or approved and (not isinstance(approval_ref, str) or not approval_ref.strip())
        ):
            return _failed_result("approval", "invalid_approval_resume")
        config = self._graph_config(checkpoint_id)
        state: _AgentState = {}
        try:
            snapshot = await self._graph.aget_state(config)
            state = snapshot.values
            pending = next(
                (item for item in state.get("approval_requests", []) if item.get("fingerprint") == approval_fingerprint),
                None,
            )
            if state.get("status") != "awaiting_approval" or pending is None:
                return _failed_result(str(state.get("task_id", "approval")), "approval_not_pending")
            calls = state.get("calls", [])
            call_index = state.get("call_index", -1)
            if not 0 <= call_index < len(calls):
                return _failed_result(str(state.get("task_id", "approval")), "approval_state_invalid")
            call_id = calls[call_index].get("call_id")
            if not isinstance(call_id, str) or not call_id:
                return _failed_result(str(state.get("task_id", "approval")), "approval_state_invalid")

            remaining_requests = [
                item for item in state.get("approval_requests", [])
                if item.get("fingerprint") != approval_fingerprint
            ]
            update: dict[str, Any] = {
                "approval_requests": remaining_requests,
                "status": "running",
                "error_code": None,
            }
            if approved:
                approved_actions = dict(state.get("approved_actions", {}))
                approved_actions[approval_fingerprint] = approval_ref
                update["approved_actions"] = approved_actions
            else:
                update["rejected_call_ids"] = [*state.get("rejected_call_ids", []), call_id]
                rejection_codes = dict(state.get("rejection_codes", {}))
                rejection_codes[call_id] = rejection_code
                update["rejection_codes"] = rejection_codes
            async with asyncio.timeout(self._budget.max_total_seconds):
                await self._graph.aupdate_state(config, update, as_node="execute_tool")
                resumed_state = await self._graph.ainvoke(None, config=config)
        except TimeoutError:
            return _failed_result(str(state.get("task_id", "approval")), "task_timeout", status="timed_out")
        except Exception:
            return _failed_result("approval", "agent_workflow_failed")

        still_pending = resumed_state.get("status") == "awaiting_approval"
        if not still_pending:
            self._approval_checkpoints.discard(checkpoint_id)
        return self._result_from_state(
            resumed_state,
            task_id=str(resumed_state.get("task_id", "approval")),
            checkpoint_id=checkpoint_id if still_pending else None,
        )

    @staticmethod
    def _result_from_state(
        state: _AgentState,
        *,
        task_id: str,
        checkpoint_id: str | None,
    ) -> ModelAgentResult:
        return ModelAgentResult(
            task_id=task_id,
            status=str(state.get("status", "failed")),
            answer=str(state.get("answer", "")),
            tool_results=tuple(state.get("tool_results", ())),
            approval_requests=tuple(_approval_from_state(item) for item in state.get("approval_requests", ())),
            tool_calls=int(state.get("tool_call_count", 0)),
            output_bytes=int(state.get("output_bytes", 0)),
            error_code=state.get("error_code"),
            checkpoint_id=checkpoint_id,
        )

    async def _call_model(self, state: _AgentState) -> dict[str, Any]:
        next_round = state["model_rounds"] + 1
        if self._budget.max_model_rounds is not None and next_round > self._budget.max_model_rounds:
            return {"status": "failed", "error_code": "model_round_limit_exceeded"}
        try:
            projected_messages = model_messages(state["messages"])
            message_bytes = len(json.dumps(
                [message.model_dump(mode="json") for message in projected_messages],
                ensure_ascii=False,
                separators=(",", ":"),
                default=str,
            ).encode("utf-8"))
        except (TypeError, ValueError):
            return {"status": "failed", "error_code": "invalid_model_input"}
        if message_bytes + self._tool_definition_bytes > self._budget.max_model_input_bytes:
            return {"status": "failed", "error_code": "model_input_budget_exceeded"}
        try:
            response, streamed_bytes = await asyncio.wait_for(
                self._stream_model(
                    {**state, "messages": projected_messages},
                    public=not bool(state.get("recovery_diagnosis_required")),
                ), timeout=min(90.0, self._budget.max_total_seconds)
            )
        except _InvalidModelResponse:
            return {"status": "failed", "error_code": "invalid_model_response"}
        except _StreamOutputExceeded:
            return {"status": "failed", "error_code": "output_budget_exceeded"}
        except asyncio.TimeoutError:
            return {"status": "failed", "error_code": "model_timeout"}
        except Exception as error:
            return {"status": "failed", "error_code": _model_error_code(error)}
        if not isinstance(response, AIMessage):
            return {"status": "failed", "error_code": "invalid_model_response"}
        reason = response.response_metadata.get("finish_reason") or response.response_metadata.get("stop_reason")
        reason = reason.lower() if isinstance(reason, str) else reason
        if reason in {"length", "max_tokens", "max_output_tokens"} or (
            response.response_metadata.get("status") == "incomplete"
        ):
            return {"messages": [*state["messages"], response], "model_rounds": next_round,
                    "streamed_bytes": streamed_bytes, "status": "failed", "error_code": "model_output_truncated"}
        if reason in {"content_filter", "refusal", "safety", "recitation"}:
            return {"messages": [*state["messages"], response], "model_rounds": next_round,
                    "status": "failed", "error_code": "model_response_blocked"}
        calls = getattr(response, "tool_calls", None)
        invalid_calls = getattr(response, "invalid_tool_calls", None)
        if invalid_calls:
            return {"messages": [*state["messages"], response], "status": "failed", "error_code": "invalid_model_tool_call"}
        if not isinstance(calls, list):
            calls = []
        if self._budget.max_tool_calls is not None and state["tool_call_count"] + len(calls) > self._budget.max_tool_calls:
            return {"messages": [*state["messages"], response], "status": "failed", "error_code": "tool_call_limit_exceeded"}
        if not calls:
            if state.get("recovery_diagnosis_required"):
                # Do not let an assistant's premature final response end recovery. Its
                # text was not streamed while recovery was pending; request another
                # model round that must obtain new evidence from an allowed read tool.
                return {
                    "messages": [
                        *state["messages"],
                        SystemMessage(content=(
                            "A tool call failed and no successful diagnostic read has followed it. "
                            "Do not answer yet. Choose a different allowed read-only tool for the same "
                            "target, inspect what it establishes, then continue the original task. "
                            "Do not repeat an unchanged failed call or attempt an effectful action. "
                            "If all available reads fail, try another distinct read route until the "
                            "task's tool or time budget is reached; then state the concrete blocker."
                        )),
                    ],
                    "model_rounds": next_round,
                    "streamed_bytes": streamed_bytes,
                    "calls": [],
                    "call_index": 0,
                    "answer": "",
                    "status": "running",
                    "error_code": None,
                }
            answer = _message_text(response.content)
            if not answer.strip():
                return {"messages": [*state["messages"], response], "model_rounds": next_round,
                        "status": "failed", "error_code": "empty_model_response"}
            answer_size = len(answer.encode("utf-8"))
            if state["output_bytes"] + answer_size > self._budget.max_output_bytes:
                return {"messages": [*state["messages"], response], "status": "failed", "error_code": "output_budget_exceeded"}
            last_phase = state["tool_results"][-1].get("phase") if state["tool_results"] else None
            unrepaired_input = last_phase in {"input_validation", "repetition_guard"} and not state.get("goal_loop_enabled", False)
            needs_review = state.get("goal_loop_enabled", False) and not unrepaired_input
            return {
                "messages": [*state["messages"], response],
                "model_rounds": next_round,
                "answer": answer,
                "output_bytes": state["output_bytes"] + answer_size,
                "streamed_bytes": streamed_bytes,
                "status": "failed" if unrepaired_input else "running" if needs_review else "succeeded",
                "goal_review_pending": needs_review,
                "calls": [],
                "error_code": ("tool_repetition_detected" if last_phase == "repetition_guard" else "invalid_arguments") if unrepaired_input else None,
            }
        clean_calls: list[dict[str, Any]] = []
        prepared_provider_calls: list[dict[str, Any]] = []
        for call in calls:
            if (
                not isinstance(call, dict)
                or not isinstance(call.get("name"), str)
                or not isinstance(call.get("args"), dict)
                or any(not isinstance(key, str) for key in call["args"])
            ):
                return {"messages": [*state["messages"], response], "status": "failed", "error_code": "invalid_model_tool_call"}
            provider_name = call["name"]
            internal_name = self._provider_to_internal.get(provider_name)
            if internal_name is None:
                # Be lenient about a model that echoed the internal dotted spelling or
                # named an unregistered tool: the registry reports `unknown_tool` later.
                if _PROVIDER_TOOL_NAME_PATTERN.fullmatch(provider_name):
                    internal_name = provider_name.replace("__", ".")
                else:
                    internal_name = provider_name
            call_id = call.get("id")
            if not isinstance(call_id, str) or not call_id:
                call_id = f"call-{next_round}-{len(clean_calls)}"
            arguments = prepare_model_tool_arguments(internal_name, call["args"])
            prepared_provider_calls.append({**call, "args": arguments, "id": call_id})
            clean_calls.append({
                "name": internal_name,
                "provider_name": provider_name,
                "arguments": arguments,
                "id": call_id,
                "call_id": f"call-{uuid.uuid4().hex}",
                "event_emitted": False,
            })
        response = response.model_copy(update={"tool_calls": prepared_provider_calls})
        return {
            "messages": [*state["messages"], response],
            "calls": clean_calls,
            "streamed_bytes": streamed_bytes,
            "call_index": 0,
            "model_rounds": next_round,
            "status": "running",
            "error_code": None,
        }

    async def _stream_model(self, state: _AgentState, *, model: Any | None = None, public: bool = True) -> tuple[AIMessage, int]:
        self._request_timing = {"started": time.perf_counter()}
        streamed_bytes = state.get("streamed_bytes", 0)

        async def publish(content: Any) -> None:
            nonlocal streamed_bytes
            for event_type, value in _public_content_parts(content):
                # JS validates UTF-16 units; 8k code points are at most 16k units.
                for offset in range(0, len(value), 8000):
                    delta = value[offset:offset + 8000]
                    size = len(delta.encode("utf-8"))
                    if streamed_bytes + size > self._budget.max_output_bytes:
                        raise _StreamOutputExceeded()
                    streamed_bytes += size
                    if public and event_type == "agent.text":
                        self._request_timing.setdefault("firstAnswerMs", round((time.perf_counter() - self._request_timing["started"]) * 1000, 2))
                    if public and self._event_sink is not None:
                        self._event_sink({"type": event_type, "runId": state.get("run_id", ""),
                                          "textDelta": delta, "at": datetime.now(UTC).isoformat()})
                    # stdio notifications flush on the event loop, including for
                    # SDKs/fakes that can yield consecutive chunks without I/O.
                    await asyncio.sleep(0)

        active_model = model if model is not None else self._model
        stream = getattr(active_model, "astream", None)
        if not callable(stream):
            # Compatibility for non-streaming local adapters. Never retry a
            # partially consumed stream with a second model request.
            response = await active_model.ainvoke(state["messages"])
            self._request_timing["firstChunkMs"] = round((time.perf_counter() - self._request_timing["started"]) * 1000, 2)
            if not isinstance(response, AIMessage):
                raise _InvalidModelResponse()
            await publish(response.content)
            return response, streamed_bytes

        iterator = stream(state["messages"])
        combined: AIMessageChunk | AIMessage | None = None
        try:
            async for chunk in iterator:
                self._request_timing.setdefault("firstChunkMs", round((time.perf_counter() - self._request_timing["started"]) * 1000, 2))
                if isinstance(chunk, AIMessageChunk):
                    if combined is not None and not isinstance(combined, AIMessageChunk):
                        raise _InvalidModelResponse()
                    combined = chunk if combined is None else combined + chunk
                elif isinstance(chunk, AIMessage) and combined is None:
                    combined = chunk
                else:
                    raise _InvalidModelResponse()
                # Only explicit public content is displayable. Tool argument
                # fragments and provider-private additional_kwargs stay private.
                await publish(chunk.content)
        finally:
            close = getattr(iterator, "aclose", None)
            if callable(close):
                try:
                    await close()
                except Exception:
                    pass  # Cleanup cannot replace the original failure/cancel.
        if combined is None:
            raise _InvalidModelResponse()
        response = message_chunk_to_message(combined) if isinstance(combined, AIMessageChunk) else combined
        if not isinstance(response, AIMessage):
            raise _InvalidModelResponse()
        return response, streamed_bytes

    async def _execute_tool(self, state: _AgentState, *, output_allowance: int | None = None) -> dict[str, Any]:
        request = state["calls"][state["call_index"]]
        context = ExecutionContext(
            task_id=state["task_id"],
            tenant_id=state["tenant_id"],
            user_id=state["user_id"],
            allowed_tools=frozenset(state["allowed_tools"]),
            allowed_target_ids=frozenset(state["allowed_target_ids"]),
            approved_actions=state["approved_actions"],
            host_target=state.get("host_target"),
            host_task_id=state.get("host_task_id"),
            plan_id=state.get("plan_id"),
            plan_step_id=state.get("plan_step_id"),
            evidence_ids=tuple(state.get("evidence_ids", [])),
            run_id=state.get("run_id", ""),
            trace_id=state.get("trace_id", ""),
            context_target_id=(
                state["host_target"].get("serverId")
                if isinstance(state.get("host_target"), dict) and state["host_target"].get("host") == "remote"
                else None
            ),
        )
        remaining = self._budget.max_output_bytes - state["output_bytes"]
        if output_allowance is not None:
            remaining = min(remaining, output_allowance)
        if remaining <= 0:
            return {"status": "failed", "error_code": "output_budget_exceeded"}
        spec = self._registry.get(request["name"])
        call_id = request["call_id"]
        started_at = request.get("started_at") or datetime.now(UTC).isoformat()
        event_base = _tool_event_base(request, state, spec, call_id)
        counted_call_ids = state.get("counted_call_ids", [])
        already_counted = call_id in counted_call_ids
        next_counted_call_ids = counted_call_ids if already_counted else [*counted_call_ids, call_id]
        next_tool_call_count = state["tool_call_count"] + (0 if already_counted else 1)
        if not request.get("event_emitted"):
            self._emit_tool_call(event_base)
        if call_id in state.get("rejected_call_ids", []):
            rejection_code = state.get("rejection_codes", {}).get(call_id, "approval_rejected")
            answer = (
                "审批已过期，当前操作未执行。此前已完成的操作未撤销。"
                if rejection_code == "approval_timeout"
                else "已拒绝当前操作，后续执行已停止。此前已完成的操作未撤销。"
            )
            self._emit_tool_result(
                event_base,
                started_at=started_at,
                status="failed",
                output_summary="Approval expired" if rejection_code == "approval_timeout" else "Approval rejected",
                error="Approval was not granted; the tool was not executed",
                error_code=rejection_code,
            )
            results = [
                item for item in state["tool_results"]
                if not (item.get("call_id") == call_id and item.get("status") == "awaiting_approval")
            ]
            results.append({
                "call_id": call_id,
                "tool": request["name"],
                "status": "failed",
                "error_code": rejection_code,
            })
            answer_size = len(answer.encode("utf-8"))
            if state["output_bytes"] + answer_size > self._budget.max_output_bytes:
                return {
                    "tool_results": results,
                    "counted_call_ids": next_counted_call_ids,
                    "tool_call_count": next_tool_call_count,
                    "status": "failed",
                    "error_code": "output_budget_exceeded",
                }
            return {
                "tool_results": results,
                "counted_call_ids": next_counted_call_ids,
                "tool_call_count": next_tool_call_count,
                "answer": answer,
                "output_bytes": state["output_bytes"] + answer_size,
                "status": "succeeded",
                "error_code": None,
            }
        tool_timeout = min(
            self._budget.max_tool_seconds,
            (spec.timeout_ms / 1000) if spec is not None else self._budget.max_tool_seconds,
        )
        schema_validated = False
        try:
            self._registry.validate_arguments(request["name"], request["arguments"])
            schema_validated = True
            if _repeated_unchanged_call(request, state["tool_results"]):
                raise ToolCallError("tool_repetition_detected", "已阻止重复调用：该操作已返回不变的结果，或此前执行结果尚未确认。此次未执行，请使用已有证据调整下一步。")
            if (
                state.get("recovery_read_required") or state.get("recovery_diagnosis_required")
            ) and spec is not None and spec.risk_level != "read":
                raise ToolCallError(
                    "read_only_verification_required",
                    "A prior tool failure has not been diagnosed; complete a successful read-only check before another effectful action.",
                )
            result = await self._registry.execute(
                request["name"],
                request["arguments"],
                context,
                timeout_seconds=tool_timeout,
                max_output_bytes=min(remaining, 4 * 1024 * 1024),
                call_id=call_id,
            )
        except asyncio.CancelledError:
            self._emit_tool_result(
                event_base,
                started_at=started_at,
                status="cancelled",
                output_summary="Tool call cancelled",
                error="Tool call cancelled",
                error_code="cancelled",
            )
            raise
        except ToolCallError as error:
            event_code = _event_error_code(error.code)
            event_status = "cancelled" if event_code == "cancelled" else "failed"
            pending = (
                error.code == "approval_required"
                and error.approval_fingerprint is not None
                and spec is not None
            )
            completed_command_result = _completed_server_exec_failure(
                spec.name if spec is not None else None,
                error,
                remaining,
            )
            if not pending:
                self._emit_tool_result(
                    event_base,
                    started_at=started_at,
                    status=event_status,
                    output_summary=(
                        _tool_output_summary(completed_command_result[0])
                        if completed_command_result is not None
                        else f"Tool call failed ({event_code})"
                    ),
                    error=error.args[0] if error.args else "Tool call failed",
                    error_code=event_code,
                )
            results = [
                item for item in state["tool_results"]
                if not (item.get("call_id") == call_id and item.get("status") == "awaiting_approval")
            ]
            failed_item: dict[str, Any] = {
                "call_id": call_id,
                "tool": request["name"],
                "status": "awaiting_approval" if pending else "failed",
                "error_code": error.code,
                "request_digest": _request_digest(request),
            }
            if completed_command_result is not None:
                failed_item["result"] = completed_command_result[0]
            results.append(failed_item)
            unknown_tool_recoveries = sum(
                1 for item in state["tool_results"]
                if item.get("error_code") == "unknown_tool"
            )
            if error.code == "unknown_tool" and unknown_tool_recoveries == 0:
                # Models can occasionally invent a tool name. Give the model one
                # bounded chance to recover using the registered tool definitions
                # instead of failing the entire task on the first unknown call.
                content = (
                    '{"error":"unknown_tool","message":"That tool is not registered. '
                    'Continue only with tools from the current registered tool list."}'
                )
                output_size = len(content.encode("utf-8"))
                if state["output_bytes"] + output_size <= self._budget.max_output_bytes:
                    return {
                        "messages": [
                            *state["messages"],
                            ToolMessage(
                                content=content,
                                tool_call_id=request["id"],
                                name=request.get("provider_name", request["name"]),
                            ),
                        ],
                        # Discard any sibling calls from the same model response;
                        # the next model round must choose tools again explicitly.
                        "calls": [],
                        "call_index": 0,
                        "tool_results": results,
                        "counted_call_ids": next_counted_call_ids,
                        "tool_call_count": next_tool_call_count,
                        "output_bytes": state["output_bytes"] + output_size,
                        "status": "running",
                        "error_code": None,
                    }
            if (state.get("goal_loop_enabled") and schema_validated and not pending
                    and error.code in {"timeout", "execution_timeout"}
                    and (len(state["calls"]) == 1 or all(
                        (sibling := self._registry.get(call["name"])) is not None
                        and sibling.risk_level == "read" for call in state["calls"]))):
                # A timeout is not a denial, nor proof that a remote operation
                # never ran. Return that uncertainty to the same goal; do not
                # retry the handler or silently replay the command. A read-only
                # batch can finish its siblings; a mixed/write batch must not
                # run dependent mutations after an unknown outcome.
                failed_item.update(phase="outcome_unknown", request_digest=_request_digest(request))
                encoded = json.dumps({
                    "state": "outcome_unknown", "errorCode": error.code,
                    "notice": "The tool timed out. It may have run partially or completed remotely; this is not success evidence. Do not replay this operation. First inspect actual state using a narrow read-only tool, then adjust the unfinished work. Use bounded probes and explicit network timeouts; report a real external blocker if inspection cannot resolve it.",
                }, separators=(",", ":"))
                output_size = len(encoded.encode("utf-8"))
                if output_size <= remaining:
                    return {
                        "messages": [*state["messages"], ToolMessage(content=encoded,
                            tool_call_id=request["id"], name=request.get("provider_name", request["name"]))],
                        "call_index": state["call_index"] + 1,
                        "tool_results": results, "counted_call_ids": next_counted_call_ids,
                        "tool_call_count": next_tool_call_count,
                        "output_bytes": state["output_bytes"] + output_size,
                        "recovery_read_required": bool(
                            state.get("recovery_read_required")
                            or (spec is not None and spec.risk_level != "read")
                        ),
                        "status": "running", "error_code": None,
                    }
            if error.code == "tool_repetition_detected":
                failed_item["phase"] = "repetition_guard"
                failed_item["request_digest"] = _request_digest(request)
                prior_guards = sum(item.get("phase") == "repetition_guard" for item in state["tool_results"])
                # One model repair opportunity, with explicit non-execution.
                # Never reuse an old result as a newly executed operation.
                if prior_guards == 0 or (state.get("goal_loop_enabled") and len(state["calls"]) == 1):
                    encoded = json.dumps({
                        "state": "not_executed", "errorCode": error.code,
                        "notice": "This repeats an operation with unchanged results or an unresolved timeout outcome. This call did not execute. Reuse the existing evidence and inspect actual state with a different narrow read, or report the concrete blocker. Do not repeat this call or merely reword its purpose.",
                    }, separators=(",", ":"))
                    output_size = len(encoded.encode("utf-8"))
                    if output_size <= remaining:
                        return {
                            "messages": [*state["messages"], ToolMessage(content=encoded,
                                tool_call_id=request["id"], name=request.get("provider_name", request["name"]))],
                            "call_index": state["call_index"] + 1,
                            "tool_results": results, "counted_call_ids": next_counted_call_ids,
                            "tool_call_count": next_tool_call_count,
                            "output_bytes": state["output_bytes"] + output_size,
                            "status": "running", "error_code": None,
                            **({"goal_review_pending": True, "answer": "", "calls": [], "call_index": 0}
                               if state.get("goal_loop_enabled") and len(state["calls"]) == 1 else {}),
                        }
            # Local schema rejection happened before preparation or execution.
            # Rejected arguments never reached the handler. In a sustained
            # goal, repeated repair failures go through completion review and
            # its no-progress guard instead of a lifetime error quota.
            prior_repairs = 0
            for item in reversed(state["tool_results"]):
                if item.get("phase") != "input_validation":
                    break
                prior_repairs += 1
            if (not schema_validated and error.code == "invalid_arguments" and spec is not None
                    and len(state["calls"]) == 1 and (prior_repairs < 2 or state.get("goal_loop_enabled"))):
                failed_item["phase"] = "input_validation"
                encoded = json.dumps({
                    "state": "not_executed", "errorCode": "invalid_arguments",
                    "notice": "Input validation rejected this call before execution. Correct the arguments using only the registered schema; no operation ran.",
                    "allowedFields": sorted(spec.input_schema.get("properties", {})),
                    "requiredFields": spec.input_schema.get("required", []),
                    "validationError": str(error),
                    "inputSchema": spec.input_schema,
                }, separators=(",", ":"))
                output_size = len(encoded.encode("utf-8"))
                if output_size <= remaining:
                    return {
                        "messages": [*state["messages"], ToolMessage(content=encoded,
                            tool_call_id=request["id"], name=request.get("provider_name", request["name"]))],
                        "call_index": state["call_index"] + 1,
                        "tool_results": results, "counted_call_ids": next_counted_call_ids,
                        "tool_call_count": next_tool_call_count,
                        "output_bytes": state["output_bytes"] + output_size,
                        "status": "running", "error_code": None,
                        **({"goal_review_pending": True, "answer": "", "calls": [], "call_index": 0}
                           if state.get("goal_loop_enabled") and prior_repairs >= 2 else {}),
                    }
            if completed_command_result is not None:
                _result, encoded, output_size = completed_command_result
                return {
                    "messages": [
                        *state["messages"],
                        ToolMessage(
                            content=encoded,
                            tool_call_id=request["id"],
                            name=request.get("provider_name", request["name"]),
                        ),
                    ],
                    "calls": [],
                    "call_index": 0,
                    "tool_results": results,
                    "counted_call_ids": next_counted_call_ids,
                    "tool_call_count": next_tool_call_count,
                    "output_bytes": state["output_bytes"] + output_size,
                    "recovery_read_required": bool(
                        state.get("recovery_read_required")
                        or (spec is not None and spec.risk_level != "read")
                    ),
                    "status": "running",
                    "error_code": None,
                }
            if (
                schema_validated
                and spec is not None
                and not pending
                and error.code not in {
                    # Authority answers and local input checks are not environment
                    # evidence. Asking the model again would turn a denial into an
                    # invitation to reword the same action, and replay a call whose
                    # arguments the schema already rejected. Repetition has its own
                    # bounded repair above; it must not fall through to diagnosis.
                    "approval_rejected", "approval_timeout", "cancelled",
                    "tool_not_allowed", "target_not_allowed", "target_required",
                    "risk_not_allowed", "authorization_failed", "permission_denied",
                    "invalid_arguments", "tool_repetition_detected",
                }
                # A timeout of an effectful call has an unknown outcome. In a batch
                # that also contains effectful siblings, continuing would run a
                # dependent mutation after an operation that may have happened; the
                # whole batch stops instead. A single call may still be diagnosed.
                and not (
                    error.code in {"timeout", "execution_timeout"}
                    and spec.risk_level != "read"
                    and len(state["calls"]) > 1
                )
            ):
                # A failed observation or effectful operation is useful evidence,
                # not a reason to end the user's task. Return it to the model so
                # it can diagnose with allowed reads before deciding what follows.
                #
                # This must also apply when the model proposed several calls in one
                # response: the run is one task, and a single failed read among them
                # used to end it with no model turn at all. Siblings stay queued, so
                # every tool call still gets a message; an effectful sibling after a
                # failure is refused by the recovery guard above as its own
                # diagnosis, not executed silently.
                message = error.args[0] if error.args else "Tool call failed"
                effectful_verification_required = (
                    bool(state.get("recovery_read_required")) or spec.risk_level != "read"
                )
                diagnosis_read_attempted = bool(state.get("recovery_diagnosis_required"))
                recovery = (
                    "The effectful operation may have partially completed or its outcome is unknown. "
                    "Do not repeat it or issue another effectful operation. First inspect the actual "
                    "state with an allowed read-only tool, then continue only if the evidence shows "
                    "what is still needed and authorization remains valid."
                    if effectful_verification_required
                    else (
                        "This alternate read-only diagnostic call also failed and changed no target state. "
                        "Do not repeat it or the original failed call. Use an independent read route only "
                        "if it can add evidence; otherwise report the concrete blocker."
                        if diagnosis_read_attempted
                        else (
                            "This was a read-only call and did not change target state. Diagnose the "
                            "failure with other allowed read-only tools, adapt to the detected "
                            "environment, and continue the original task. First try a different "
                            "allowed read-only tool. Do not repeat this unchanged call."
                        )
                    )
                )
                encoded = json.dumps({
                    "state": "failed",
                    "errorCode": error.code,
                    "message": _truncate_utf16(_redact_sensitive_text(message), 1_200),
                    "recovery": recovery,
                }, ensure_ascii=False, separators=(",", ":"))
                output_size = len(encoded.encode("utf-8"))
                if output_size <= remaining:
                    failed_item["phase"] = "failure_diagnosis"
                    return {
                        "messages": [
                            *state["messages"],
                            ToolMessage(
                                content=encoded,
                                tool_call_id=request["id"],
                                name=request.get("provider_name", request["name"]),
                            ),
                        ],
                        "call_index": state["call_index"] + 1,
                        "tool_results": results,
                        "counted_call_ids": next_counted_call_ids,
                        "tool_call_count": next_tool_call_count,
                        "output_bytes": state["output_bytes"] + output_size,
                        "recovery_read_required": (
                            effectful_verification_required or diagnosis_read_attempted
                        ),
                        # Require one distinct diagnostic read after an error. If that
                        # alternate read also fails, allow a concrete blocker report;
                        # uncertain effectful outcomes remain guarded separately.
                        "recovery_diagnosis_required": (
                            not diagnosis_read_attempted or spec.risk_level != "read"
                        ),
                        "status": "running",
                        "error_code": None,
                    }
            update: dict[str, Any] = {
                "tool_results": results,
                "counted_call_ids": next_counted_call_ids,
                "tool_call_count": next_tool_call_count,
                "status": "awaiting_approval" if pending else "failed",
                "error_code": error.code,
            }
            if pending:
                calls = [dict(item) for item in state["calls"]]
                calls[state["call_index"]]["event_emitted"] = True
                calls[state["call_index"]]["started_at"] = started_at
                approval = {
                    "fingerprint": error.approval_fingerprint,
                    "tool_name": spec.name,
                    "target_id": error.target_id,
                    "risk_level": spec.risk_level,
                    "arguments": _redact_arguments(request["arguments"]),
                }
                if spec.name in HOST_APPROVAL_TOOLS:
                    approval["host_input"] = request["arguments"]
                update["calls"] = calls
                update["approval_requests"] = [
                    *(item for item in state["approval_requests"] if item["fingerprint"] != approval["fingerprint"]), approval]
            return update
        except Exception:
            self._emit_tool_result(
                event_base,
                started_at=started_at,
                status="failed",
                output_summary="Tool call failed (internal)",
                error="Tool execution failed",
                error_code="internal",
            )
            results = [
                item for item in state["tool_results"]
                if not (item.get("call_id") == call_id and item.get("status") == "awaiting_approval")
            ]
            results.append({
                "call_id": call_id,
                "tool": request["name"],
                "status": "failed",
                "error_code": "tool_failed",
            })
            return {
                "tool_results": results,
                "counted_call_ids": next_counted_call_ids,
                "tool_call_count": next_tool_call_count,
                "status": "failed",
                "error_code": "tool_failed",
            }

        self._emit_tool_result(
            event_base,
            started_at=started_at,
            status="success",
            output_summary=_tool_output_summary(result.value),
            duration_ms=result.duration_ms,
        )
        output_size = result.output_bytes
        item = {
            "call_id": call_id,
            "tool": result.tool_name,
            "status": "succeeded",
            "result": result.value,
            "duration_ms": result.duration_ms,
            "request_digest": _request_digest(request),
            "result_digest": _result_digest(result.value),
        }
        tool_message = ToolMessage(
            content=json.dumps(result.value, ensure_ascii=False, separators=(",", ":")),
            tool_call_id=request["id"],
            name=request.get("provider_name", request["name"]),
        )
        next_index = state["call_index"] + 1
        update: dict[str, Any] = {
            "messages": [*state["messages"], tool_message],
            "tool_results": [
                *(
                    entry for entry in state["tool_results"]
                    if not (entry.get("call_id") == call_id and entry.get("status") == "awaiting_approval")
                ),
                item,
            ],
            "counted_call_ids": next_counted_call_ids,
            "tool_call_count": next_tool_call_count,
            "output_bytes": state["output_bytes"] + output_size,
            "call_index": next_index,
            "status": "running",
            "error_code": None,
        }
        if state.get("recovery_read_required") and spec is not None and spec.risk_level == "read":
            update["recovery_read_required"] = False
        if state.get("recovery_diagnosis_required") and spec is not None and spec.risk_level == "read":
            update["recovery_diagnosis_required"] = False
        if request["name"] == "investigation.plan" and isinstance(result.value, dict):
            plan_id = result.value.get("planId")
            plan_step_id = result.value.get("currentStepId")
            evidence_ids = result.value.get("evidenceIds")
            if (
                isinstance(plan_id, str)
                and isinstance(plan_step_id, str)
                and isinstance(evidence_ids, list)
                and all(isinstance(item, str) for item in evidence_ids)
            ):
                update.update({
                    "plan_id": plan_id,
                    "plan_step_id": plan_step_id,
                    "evidence_ids": evidence_ids,
                })
        return update

    def _emit_tool_call(self, event: dict[str, Any]) -> None:
        if self._event_sink is not None:
            self._event_sink({"type": "agent.tool_call", **event, "at": datetime.now(UTC).isoformat()})

    def _emit_tool_result(
        self,
        event: dict[str, Any],
        *,
        started_at: str,
        status: str,
        output_summary: str,
        error: str | None = None,
        error_code: str | None = None,
        duration_ms: int | None = None,
    ) -> None:
        if self._event_sink is None:
            return
        ended_at = datetime.now(UTC).isoformat()
        result = {
            "type": "agent.tool_result",
            **event,
            "status": status,
            # Result summaries are redacted before serialization; redacting
            # the serialized JSON again can consume its quoting/delimiters.
            "outputSummary": _truncate_utf16(output_summary, 4000),
            "startedAt": started_at,
            "endedAt": ended_at,
            "durationMs": duration_ms if duration_ms is not None else _elapsed_ms(started_at, ended_at),
            "at": ended_at,
        }
        if error is not None:
            result["error"] = _truncate_utf16(_redact_sensitive_text(error), 4000)
        if error_code is not None:
            result["errorCode"] = error_code
        self._event_sink(result)

    @staticmethod
    def _route_model(state: _AgentState) -> str:
        if state.get("status") == "running" and state.get("goal_review_pending"):
            return "review"
        if state.get("status") == "running" and state.get("recovery_diagnosis_required") and not state.get("calls"):
            return "continue"
        if state.get("status") != "running" or not state.get("calls"):
            return "finish"
        return "execute"

    @staticmethod
    def _route_tool(state: _AgentState) -> str:
        if state.get("status") != "running":
            return "finish"
        if state.get("goal_review_pending"):
            return "review"
        if state["call_index"] < len(state["calls"]):
            return "execute"
        return "model"


def _checkpoint_matches(saved: _AgentState, initial: _AgentState) -> bool:
    binding_fields = (
        "task_id", "tenant_id", "user_id", "allowed_tools", "allowed_target_ids",
        "host_target", "host_task_id", "run_id", "trace_id", "checkpoint_binding",
        "budget_binding",
        "goal_loop_enabled",
    )
    if any(saved.get(field) != initial.get(field) for field in binding_fields):
        return False
    saved_messages = saved.get("messages", [])
    initial_messages = initial.get("messages", [])
    if len(saved_messages) < 2 or len(initial_messages) < 2:
        return False
    return all(
        type(saved_message) is type(initial_message)
        and saved_message.content == initial_message.content
        for saved_message, initial_message in zip(saved_messages[:2], initial_messages[:2])
    )


def _failed_result(task_id: str, error_code: str, *, status: str = "failed") -> ModelAgentResult:
    return ModelAgentResult(task_id, status, "", (), (), 0, 0, error_code)


def _public_content_parts(content: Any) -> list[tuple[str, str]]:
    if isinstance(content, str):
        return [("agent.text", content)] if content else []
    if not isinstance(content, list):
        return []
    result: list[tuple[str, str]] = []
    for block in content:
        if isinstance(block, str):
            if block:
                result.append(("agent.text", block))
        elif isinstance(block, dict):
            kind = block.get("type")
            if kind in (None, "text", "output_text") and isinstance(block.get("text"), str):
                result.append(("agent.text", block["text"]))
            elif kind == "thinking" and isinstance(block.get("thinking"), str):
                result.append(("agent.thinking", block["thinking"]))
            elif kind == "reasoning":
                reasoning = block.get("reasoning")
                if isinstance(reasoning, str):
                    result.append(("agent.thinking", reasoning))
                # OpenAI exposes a public reasoning summary separately from its
                # encrypted/private reasoning state. Never inspect opaque fields.
                summary = block.get("summary")
                if isinstance(summary, list):
                    result.extend(("agent.thinking", item["text"]) for item in summary
                                  if isinstance(item, dict) and item.get("type") == "summary_text"
                                  and isinstance(item.get("text"), str))
    return result


def _message_text(content: Any) -> str:
    return "".join(value for event_type, value in _public_content_parts(content) if event_type == "agent.text")


def _redact_arguments(arguments: dict[str, Any]) -> dict[str, Any]:
    def visit(value: Any, depth: int = 0) -> Any:
        if depth > 16:
            return "[truncated]"
        if isinstance(value, dict):
            return {
                str(key): "[redacted]" if _is_sensitive_key(key) else visit(child, depth + 1)
                for key, child in value.items()
            }
        if isinstance(value, list):
            return [visit(child, depth + 1) for child in value]
        if isinstance(value, str):
            return _redact_sensitive_text(value)
        if value is None or isinstance(value, (int, float, bool)):
            return value
        return "[unsupported]"

    # The registry already bounds validated inputs. Approval review must retain
    # their full non-secret content, including command tails and batch items.
    # Short event summaries are a separate presentation path.
    return visit(arguments)


def _tool_event_base(
    request: dict[str, Any],
    state: _AgentState,
    spec: Any,
    call_id: str,
) -> dict[str, Any]:
    target = state.get("host_target")
    if not isinstance(target, dict):
        target = {"host": "local", "environment": "unknown"}
    risk = {"read": "read", "write": "medium", "destructive": "critical"}.get(
        getattr(spec, "risk_level", "destructive"), "critical"
    )
    allowed = request["name"] in state.get("allowed_tools", [])
    target_id = target.get("serverId") if target.get("host") == "remote" else None
    target_allowed = target_id is None or target_id in state.get("allowed_target_ids", [])
    if spec is None or not allowed or not target_allowed:
        decision = "deny"
    elif getattr(spec, "requires_approval", False):
        decision = "ask"
    elif getattr(spec, "target_required", False) and target_id is None:
        decision = "deny"
    else:
        decision = "auto"
    origin = getattr(spec, "origin", None) or {"kind": "builtin"}
    return {
        "runId": state.get("run_id", ""),
        "traceId": state.get("trace_id") or call_id,
        "stepId": call_id,
        "callId": call_id,
        "toolName": str(request.get("name", "unknown_tool"))[:256],
        "input": _redact_event_value(request.get("arguments", {})),
        "target": dict(target),
        "riskLevel": risk,
        "decision": decision,
        "origin": dict(origin),
    }


def _event_error_code(code: str) -> str:
    allowed = {
        "invalid_input", "plan_deviation", "denied_by_policy", "approval_rejected",
        "approval_timeout", "timeout", "cancelled", "not_found", "transport",
        "unsupported", "execution_failed", "internal",
    }
    if code in allowed:
        return code
    mapped = {
        "tool_not_allowed": "denied_by_policy",
        "target_required": "denied_by_policy",
        "target_not_allowed": "denied_by_policy",
        "risk_not_allowed": "denied_by_policy",
        "authorization_failed": "denied_by_policy",
        "approval_required": "denied_by_policy",
        "invalid_arguments": "invalid_input",
        "invalid_target": "invalid_input",
        "unknown_tool": "not_found",
        "tool_repetition_detected": "execution_failed",
        "execution_timeout": "timeout",
        "execution_cancelled": "cancelled",
        "host_unavailable": "transport",
        "host_tool_failed": "execution_failed",
    }
    return mapped.get(code, "internal")


def _request_digest(request: dict[str, Any]) -> str:
    # Purpose is explanatory metadata, not a change to the actual operation.
    arguments = {key: value for key, value in request["arguments"].items() if key != "purpose"}
    encoded = json.dumps([request["name"], arguments], sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def _result_digest(value: Any) -> str:
    # Host timing changes on every read even when its evidence is unchanged.
    if isinstance(value, dict):
        value = {key: item for key, item in value.items() if key not in {"durationMs", "startedAt", "endedAt"}}
    encoded = json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def _repeated_unchanged_call(request: dict[str, Any], results: list[dict[str, Any]]) -> bool:
    digest = _request_digest(request)
    if any(item.get("phase") == "outcome_unknown" and item.get("request_digest") == digest for item in results):
        return True
    if any(item.get("phase") == "failure_diagnosis" and item.get("request_digest") == digest for item in results):
        return True
    if results and results[-1].get("phase") == "repetition_guard" and results[-1].get("request_digest") == digest:
        return True
    return (len(results) >= 2
            and all(item.get("status") == "succeeded" and item.get("request_digest") == digest for item in results[-2:])
            and results[-1].get("result_digest") == results[-2].get("result_digest"))


def _tool_output_summary(value: Any) -> str:
    if (isinstance(value, dict) and value.get("state") == "completed"
            and isinstance(value.get("stdout"), str) and isinstance(value.get("stderr"), str)):
        # A byte-cut JSON envelope cannot be rendered as readable command
        # output. Bound the redacted fields first and keep the envelope valid.
        streams = {key: _redact_sensitive_text(value[key]) for key in ("stdout", "stderr")}
        width = 768
        while True:
            command = {key: item for key, item in value.items()
                       if key in {"state", "exitCode", "durationMs", "stdoutTruncated", "stderrTruncated"}}
            for key, raw in streams.items():
                clipped = len(raw) > width * 2
                command[key] = raw[:width] + "\n…[excerpt]…\n" + raw[-width:] if clipped and width else "" if clipped else raw
                command[key + "Truncated"] = bool(value.get(key + "Truncated")) or clipped
            encoded = json.dumps(command, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
            if _utf16_length(encoded) <= 4000:
                return encoded
            if width == 0:
                break
            width //= 2
    safe = _redact_event_value(value)
    try:
        encoded = json.dumps(safe, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
    except (TypeError, ValueError):
        return "Tool returned a result that could not be summarized"
    if _utf16_length(encoded) > 4000:
        return _truncate_utf16(encoded, 3988) + "…[truncated]"
    return encoded


def _completed_server_exec_failure(
    tool_name: str | None,
    error: ToolCallError,
    output_budget: int,
) -> tuple[dict[str, Any], str, int] | None:
    value = error.result
    if (
        tool_name != "server.exec"
        or error.code != "execution_failed"
        or not isinstance(value, dict)
        or value.get("state") != "completed"
        or isinstance(value.get("exitCode"), bool)
        or not isinstance(value.get("exitCode"), int)
        or not 1 <= value["exitCode"] <= 255
        or not isinstance(value.get("stdout"), str)
        or not isinstance(value.get("stderr"), str)
        or not isinstance(value.get("stdoutTruncated"), bool)
        or not isinstance(value.get("stderrTruncated"), bool)
        or isinstance(value.get("durationMs"), bool)
        or not isinstance(value.get("durationMs"), int)
        or value["durationMs"] < 0
        or set(value) != {
            "state", "exitCode", "stdout", "stderr", "stdoutTruncated", "stderrTruncated", "durationMs"
        }
    ):
        return None
    try:
        encoded = json.dumps(
            value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        )
        encoded_size = len(encoded.encode("utf-8"))
    except (TypeError, ValueError):
        return None
    if encoded_size > output_budget:
        return None
    return dict(value), encoded, encoded_size


def _redact_event_value(value: Any, depth: int = 0) -> Any:
    if depth > 16:
        return "[truncated]"
    if isinstance(value, dict):
        return {
            str(key): "[redacted]" if _is_sensitive_key(key) else _redact_event_value(child, depth + 1)
            for key, child in list(value.items())[:256]
        }
    if isinstance(value, (list, tuple)):
        return [_redact_event_value(child, depth + 1) for child in value[:100]]
    if isinstance(value, str):
        redacted = _redact_sensitive_text(value)
        return redacted if len(redacted) <= 2048 else redacted[:2048] + "…[truncated]"
    if value is None or isinstance(value, (int, float, bool)):
        return value
    return "[unsupported]"


_SENSITIVE_TEXT_PATTERNS = (
    re.compile(r"-----BEGIN(?: [A-Z0-9]+)? PRIVATE KEY-----[\s\S]*?-----END(?: [A-Z0-9]+)? PRIVATE KEY-----"),
    re.compile(r"\bsk-(?:proj-)?[A-Za-z0-9_-]{8,}\b"),
    re.compile(r"\b(?:ghp|gho|ghu|ghs)_[A-Za-z0-9_]{20,}\b"),
    re.compile(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
    re.compile(r"\bAIza[0-9A-Za-z_-]{30,}\b"),
    re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{12,}\b"),
    re.compile(r"\b(?:Bearer|Basic)\s+[A-Za-z0-9._~+/=:-]{8,}\b", re.IGNORECASE),
    re.compile(r"([\"']?(?:[A-Za-z][A-Za-z0-9_-]*(?:key|token|secret|password|passwd|credential|passphrase)|key|token|secret|password|passwd|credential|passphrase|authorization|cookie)[\"']?\s*[:=]\s*)(?:\[[^\]]*\]|\"[^\"]*\"|'[^']*'|[^\s,;)}\]]+)", re.IGNORECASE),
    re.compile(r"\b([a-z][a-z0-9+.-]*://[^:\s/@]+:)[^@\s/]+(@)", re.IGNORECASE),
)
_SENSITIVE_KEY_PATTERN = re.compile(
    r"(?:api|access|auth|bearer|client|private|secret|session|service|signing)?(?:key|token|secret|password|passwd|credential|passphrase)|authorization|cookie",
    re.IGNORECASE,
)


def _is_sensitive_key(key: Any) -> bool:
    normalized = re.sub(r"[^a-z0-9]", "", str(key), flags=re.IGNORECASE)
    return _SENSITIVE_KEY_PATTERN.search(normalized) is not None


def _redact_sensitive_text(value: str) -> str:
    redacted = value
    for pattern in _SENSITIVE_TEXT_PATTERNS:
        redacted = pattern.sub(_replace_sensitive_text_match, redacted)
    return redacted


def _replace_sensitive_text_match(match: re.Match[str]) -> str:
    if not match.lastindex:
        return "[redacted]"
    suffix = match.group(2) if match.lastindex > 1 else ""
    return f"{match.group(1)}[redacted]{suffix or ''}"


def _utf16_length(value: str) -> int:
    return sum(2 if ord(character) > 0xFFFF else 1 for character in value)


def _truncate_utf16(value: str, max_units: int) -> str:
    if _utf16_length(value) <= max_units:
        return value
    result: list[str] = []
    units = 0
    for character in value:
        character_units = 2 if ord(character) > 0xFFFF else 1
        if units + character_units > max_units:
            break
        result.append(character)
        units += character_units
    return "".join(result)


def _elapsed_ms(started_at: str, ended_at: str) -> int:
    try:
        started = datetime.fromisoformat(started_at)
        ended = datetime.fromisoformat(ended_at)
        return max(0, round((ended - started).total_seconds() * 1000))
    except (TypeError, ValueError):
        return 0


def _approval_from_state(item: dict[str, Any]) -> ApprovalRequest:
    return ApprovalRequest(
        fingerprint=item["fingerprint"],
        tool_name=item["tool_name"],
        target_id=item.get("target_id"),
        risk_level=item["risk_level"],
        arguments=item["arguments"],
        host_input=item.get("host_input"),
    )


def _model_error_code(error: Exception) -> str:
    name = type(error).__name__.lower()
    status = getattr(error, "status_code", None) or getattr(error, "http_status", None)
    if isinstance(error, (TimeoutError, asyncio.TimeoutError)) or "timeout" in name:
        return "model_timeout"
    if status in (401, 403) or "authentication" in name or "permission" in name:
        return "model_authentication_failed"
    if status == 429 or "ratelimit" in name or "rate_limit" in name:
        return "model_rate_limited"
    if isinstance(status, int) and status >= 500 or any(token in name for token in ("connection", "unavailable", "serviceunavailable")):
        return "model_unavailable"
    return "model_request_failed"
