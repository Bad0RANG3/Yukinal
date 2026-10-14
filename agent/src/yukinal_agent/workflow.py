from __future__ import annotations

import asyncio
import json
from dataclasses import dataclass
from typing import Any, TypedDict

from langgraph.checkpoint.memory import InMemorySaver
from langgraph.graph import END, START, StateGraph

from .tools import ExecutionContext, ToolCallError, ToolRegistry


@dataclass(frozen=True)
class TaskBudget:
    max_tool_calls: int = 12
    max_total_seconds: float = 120.0
    max_output_bytes: int = 1024 * 1024
    max_tool_seconds: float = 30.0

    def __post_init__(self) -> None:
        if isinstance(self.max_tool_calls, bool) or not isinstance(self.max_tool_calls, int) or not 1 <= self.max_tool_calls <= 100:
            raise ValueError("max_tool_calls must be between 1 and 100")
        if isinstance(self.max_total_seconds, bool) or not isinstance(self.max_total_seconds, (int, float)) or not 0.01 <= self.max_total_seconds <= 3600:
            raise ValueError("max_total_seconds must be between 0.01 and 3600")
        if isinstance(self.max_output_bytes, bool) or not isinstance(self.max_output_bytes, int) or not 1 <= self.max_output_bytes <= 16 * 1024 * 1024:
            raise ValueError("max_output_bytes must be between 1 and 16777216")
        if isinstance(self.max_tool_seconds, bool) or not isinstance(self.max_tool_seconds, (int, float)) or not 0.01 <= self.max_tool_seconds <= 300:
            raise ValueError("max_tool_seconds must be between 0.01 and 300")


@dataclass(frozen=True)
class WorkflowResult:
    task_id: str
    status: str
    results: tuple[dict[str, Any], ...]
    tool_calls: int
    output_bytes: int
    error_code: str | None = None


class _WorkflowState(TypedDict, total=False):
    task_id: str
    tenant_id: str
    user_id: str
    allowed_tools: list[str]
    allowed_target_ids: list[str]
    approved_actions: dict[str, str]
    tool_calls: list[dict[str, Any]]
    result_items: list[dict[str, Any]]
    current_index: int
    output_bytes: int
    status: str
    error_code: str | None


class AgentWorkflow:
    """Minimal, deterministic LangGraph workflow that executes bounded tool requests."""

    def __init__(self, registry: ToolRegistry, budget: TaskBudget | None = None) -> None:
        self._registry = registry
        self._budget = budget or TaskBudget()
        graph = StateGraph(_WorkflowState)
        graph.add_node("execute_next", self._execute_next)
        graph.add_edge(START, "execute_next")
        graph.add_conditional_edges("execute_next", self._route, {"continue": "execute_next", "finish": END})
        self._graph = graph.compile(checkpointer=InMemorySaver())

    async def run(
        self,
        *,
        task_id: str,
        tenant_id: str,
        user_id: str,
        allowed_tools: set[str] | frozenset[str],
        allowed_target_ids: set[str] | frozenset[str],
        tool_calls: list[dict[str, Any]],
        approved_actions: dict[str, str] | None = None,
    ) -> WorkflowResult:
        try:
            ExecutionContext(
                task_id=task_id,
                tenant_id=tenant_id,
                user_id=user_id,
                allowed_tools=frozenset(allowed_tools),
                allowed_target_ids=frozenset(allowed_target_ids),
                approved_actions=approved_actions or {},
            )
        except (TypeError, ValueError):
            return WorkflowResult(task_id, "rejected", (), 0, 0, "invalid_execution_context")
        if not tool_calls:
            return WorkflowResult(task_id, "succeeded", (), 0, 0)
        if len(tool_calls) > self._budget.max_tool_calls:
            return WorkflowResult(task_id, "rejected", (), 0, 0, "tool_call_limit_exceeded")
        try:
            if not _has_string_object_keys(tool_calls):
                raise ValueError("tool call contains a non-string object key")
            request_size = len(
                json.dumps(tool_calls, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8")
            )
        except (TypeError, ValueError):
            return WorkflowResult(task_id, "rejected", (), 0, 0, "invalid_tool_calls")
        if request_size > 1024 * 1024:
            return WorkflowResult(task_id, "rejected", (), 0, 0, "tool_call_payload_too_large")
        initial: _WorkflowState = {
            "task_id": task_id,
            "tenant_id": tenant_id,
            "user_id": user_id,
            "allowed_tools": sorted(allowed_tools),
            "allowed_target_ids": sorted(allowed_target_ids),
            "approved_actions": dict(approved_actions or {}),
            "tool_calls": tool_calls,
            "result_items": [],
            "current_index": 0,
            "output_bytes": 0,
            "status": "running",
            "error_code": None,
        }
        config = {"configurable": {"thread_id": task_id}}
        timed_out = False
        try:
            async with asyncio.timeout(self._budget.max_total_seconds):
                state = await self._graph.ainvoke(initial, config=config)
        except TimeoutError:
            timed_out = True
            try:
                snapshot = await self._graph.aget_state(config)
                state = dict(snapshot.values)
            except Exception:
                state = initial
        except Exception:
            try:
                snapshot = await self._graph.aget_state(config)
                state = dict(snapshot.values)
            except Exception:
                state = initial
            return WorkflowResult(
                task_id=task_id,
                status="failed",
                results=tuple(state.get("result_items", ())),
                tool_calls=len(state.get("result_items", ())),
                output_bytes=int(state.get("output_bytes", 0)),
                error_code="workflow_failed",
            )

        if timed_out:
            status, error_code = "timed_out", "task_timeout"
        else:
            status = str(state.get("status", "failed"))
            error_code = state.get("error_code")
        items = tuple(state.get("result_items", ()))
        return WorkflowResult(
            task_id=task_id,
            status=status,
            results=items,
            tool_calls=len(items),
            output_bytes=int(state.get("output_bytes", 0)),
            error_code=error_code,
        )

    async def _execute_next(self, state: _WorkflowState) -> dict[str, Any]:
        index = state["current_index"]
        request = state["tool_calls"][index]
        if not isinstance(request, dict) or set(request) != {"name", "arguments"}:
            item = {"status": "failed", "error_code": "invalid_tool_call"}
            return {
                "result_items": [*state["result_items"], item],
                "current_index": index + 1,
                "status": "failed",
                "error_code": "invalid_tool_call",
            }

        name = request.get("name")
        arguments = request.get("arguments")
        if not isinstance(name, str) or not isinstance(arguments, dict) or any(not isinstance(key, str) for key in arguments):
            item = {"status": "failed", "error_code": "invalid_tool_call"}
            return {
                "result_items": [*state["result_items"], item],
                "current_index": index + 1,
                "status": "failed",
                "error_code": "invalid_tool_call",
            }
        remaining_output = self._budget.max_output_bytes - state["output_bytes"]
        if remaining_output <= 0:
            item = {"tool": name, "status": "failed", "error_code": "output_budget_exceeded"}
            return {
                "result_items": [*state["result_items"], item],
                "current_index": index + 1,
                "status": "failed",
                "error_code": "output_budget_exceeded",
            }

        context = ExecutionContext(
            task_id=state["task_id"],
            tenant_id=state["tenant_id"],
            user_id=state["user_id"],
            allowed_tools=frozenset(state["allowed_tools"]),
            allowed_target_ids=frozenset(state["allowed_target_ids"]),
            approved_actions=state.get("approved_actions", {}),
        )
        try:
            result = await self._registry.execute(
                name,
                arguments,
                context,
                timeout_seconds=self._budget.max_tool_seconds,
                max_output_bytes=min(remaining_output, 4 * 1024 * 1024),
            )
        except ToolCallError as error:
            item = {"tool": name, "status": "failed", "error_code": error.code}
            return {
                "result_items": [*state["result_items"], item],
                "current_index": index + 1,
                "status": "failed",
                "error_code": error.code,
            }
        except Exception:
            item = {"tool": name, "status": "failed", "error_code": "tool_failed"}
            return {
                "result_items": [*state["result_items"], item],
                "current_index": index + 1,
                "status": "failed",
                "error_code": "tool_failed",
            }

        new_total = state["output_bytes"] + result.output_bytes
        item = {
            "tool": name,
            "status": "succeeded",
            "result": result.value,
            "duration_ms": result.duration_ms,
        }
        return {
            "result_items": [*state["result_items"], item],
            "current_index": index + 1,
            "output_bytes": new_total,
            "status": "succeeded" if index + 1 >= len(state["tool_calls"]) else "running",
            "error_code": None,
        }

    @staticmethod
    def _route(state: _WorkflowState) -> str:
        if state.get("status") in {"failed", "succeeded"} or state["current_index"] >= len(state["tool_calls"]):
            return "finish"
        return "continue"


def _has_string_object_keys(value: Any, depth: int = 0) -> bool:
    if depth > 32:
        return False
    if isinstance(value, dict):
        return all(isinstance(key, str) and _has_string_object_keys(item, depth + 1) for key, item in value.items())
    if isinstance(value, list):
        return all(_has_string_object_keys(item, depth + 1) for item in value)
    return value is None or isinstance(value, (str, int, float, bool))
