from __future__ import annotations

import asyncio
import copy
import hashlib
import json
import re
import time
import uuid
from dataclasses import asdict, dataclass, is_dataclass, replace
from datetime import UTC, datetime
from typing import Any, Awaitable, Callable, Literal, Mapping, Protocol

from jsonschema import Draft202012Validator, ValidationError

from .audit import AuditEvent, AuditSink

ToolRisk = Literal["read", "write", "destructive"]
_TOOL_NAME = re.compile(r"^[A-Za-z][A-Za-z0-9_.:-]{0,127}$")
_IDENTIFIER = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:@/-]{0,255}$")
_ERROR_CODE = re.compile(r"^[a-z][a-z0-9_]{0,63}$")
ToolHandler = Callable[[dict[str, Any], "ExecutionContext"], Awaitable[Any]]


class ToolCallError(RuntimeError):
    def __init__(
        self,
        code: str,
        message: str,
        *,
        approval_fingerprint: str | None = None,
        target_id: str | None = None,
        result: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code if isinstance(code, str) and _ERROR_CODE.fullmatch(code) else "tool_failed"
        self.approval_fingerprint = approval_fingerprint
        self.target_id = target_id
        self.result = result


@dataclass(frozen=True)
class ToolSpec:
    name: str
    description: str
    input_schema: dict[str, Any]
    risk_level: ToolRisk
    requires_approval: bool
    handler: ToolHandler
    target_id_argument: str | None = "target_id"
    target_required: bool = False
    origin: dict[str, str] | None = None
    timeout_ms: int = 30_000
    cancellable: bool = False

    def __post_init__(self) -> None:
        if not isinstance(self.name, str) or not _TOOL_NAME.fullmatch(self.name):
            raise ValueError("tool name has an invalid format")
        if not isinstance(self.description, str) or not self.description.strip():
            raise ValueError("tool description cannot be empty")
        if self.risk_level not in ("read", "write", "destructive"):
            raise ValueError("tool risk level is invalid")
        if self.risk_level != "read" and not self.requires_approval:
            raise ValueError("write and destructive tools must require approval")
        if self.target_id_argument is not None and (
            not isinstance(self.target_id_argument, str) or not _TOOL_NAME.fullmatch(self.target_id_argument)
        ):
            raise ValueError("target id argument has an invalid format")
        if not isinstance(self.input_schema, dict):
            raise ValueError("tool input schema must be an object")
        if not callable(self.handler):
            raise ValueError("tool handler must be callable")
        if isinstance(self.timeout_ms, bool) or not isinstance(self.timeout_ms, int) or not 1 <= self.timeout_ms <= 300_000:
            raise ValueError("tool timeout_ms must be between 1 and 300000")
        if not isinstance(self.cancellable, bool):
            raise ValueError("tool cancellable must be a boolean")
        if self.origin is not None:
            kind = self.origin.get("kind") if isinstance(self.origin, dict) else None
            if (
                not isinstance(self.origin, dict)
                or not isinstance(kind, str)
                or kind not in {"builtin", "mcp", "provider"}
                or any(not isinstance(key, str) or not isinstance(value, str) for key, value in self.origin.items())
            ):
                raise ValueError("tool origin is invalid")
        try:
            encoded_schema = json.dumps(self.input_schema, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
        except (TypeError, ValueError):
            raise ValueError("tool input schema must be valid JSON") from None
        if len(encoded_schema.encode("utf-8")) > 128 * 1024:
            raise ValueError("tool input schema exceeds the size limit")
        _check_schema_complexity(self.input_schema)
        _reject_schema_references(self.input_schema)
        Draft202012Validator.check_schema(self.input_schema)
        if self.input_schema.get("type") != "object":
            raise ValueError("tool input schema must describe an object")


@dataclass(frozen=True)
class ExecutionContext:
    task_id: str
    tenant_id: str
    user_id: str
    allowed_tools: frozenset[str]
    allowed_target_ids: frozenset[str]
    approved_actions: Mapping[str, str]
    context_target_id: str | None = None
    host_target: Mapping[str, Any] | None = None
    host_task_id: str | None = None
    plan_id: str | None = None
    plan_step_id: str | None = None
    evidence_ids: tuple[str, ...] = ()
    run_id: str = ""
    trace_id: str = ""
    call_id: str = ""

    def __post_init__(self) -> None:
        for field_name in ("task_id", "tenant_id", "user_id"):
            value = getattr(self, field_name)
            if not isinstance(value, str) or not _IDENTIFIER.fullmatch(value):
                raise ValueError(f"{field_name} has an invalid format")
        if any(not isinstance(name, str) or not _TOOL_NAME.fullmatch(name) for name in self.allowed_tools):
            raise ValueError("allowed_tools contains an invalid tool name")
        if any(not isinstance(target, str) or not _IDENTIFIER.fullmatch(target) for target in self.allowed_target_ids):
            raise ValueError("allowed_target_ids contains an invalid target id")
        if len(self.allowed_tools) > 512 or len(self.allowed_target_ids) > 512:
            raise ValueError("execution context contains too many permissions")
        if self.context_target_id is not None and not _IDENTIFIER.fullmatch(self.context_target_id):
            raise ValueError("context_target_id has an invalid format")
        if self.host_target is not None and not isinstance(self.host_target, Mapping):
            raise ValueError("host_target must be an object")
        if self.host_task_id is not None and (not isinstance(self.host_task_id, str) or not _IDENTIFIER.fullmatch(self.host_task_id)):
            raise ValueError("host_task_id has an invalid format")
        for field_name in ("plan_id", "plan_step_id"):
            value = getattr(self, field_name)
            if value is not None and (not isinstance(value, str) or not _IDENTIFIER.fullmatch(value)):
                raise ValueError(f"{field_name} has an invalid format")
        if (self.plan_id is None) != (self.plan_step_id is None):
            raise ValueError("plan_id and plan_step_id must be provided together")
        if self.plan_id is None and self.evidence_ids:
            raise ValueError("evidence_ids require a plan binding")
        if (
            not isinstance(self.evidence_ids, tuple)
            or len(self.evidence_ids) > 128
            or any(not isinstance(item, str) or not _IDENTIFIER.fullmatch(item) for item in self.evidence_ids)
            or len(set(self.evidence_ids)) != len(self.evidence_ids)
        ):
            raise ValueError("evidence_ids contains an invalid evidence id")
        if self.plan_id is not None and (
            self.host_task_id is None
            or not isinstance(self.host_target, Mapping)
            or self.host_target.get("host") != "remote"
        ):
            raise ValueError("plan bindings require a durable remote task target")
        for field_name in ("run_id", "trace_id", "call_id"):
            value = getattr(self, field_name)
            if not isinstance(value, str) or (value and not _IDENTIFIER.fullmatch(value)):
                raise ValueError(f"{field_name} has an invalid format")
        if not isinstance(self.approved_actions, Mapping) or len(self.approved_actions) > 1000 or any(
            not isinstance(key, str)
            or not re.fullmatch(r"[0-9a-f]{64}", key)
            or not isinstance(reference, str)
            or not reference
            or len(reference) > 256
            for key, reference in self.approved_actions.items()
        ):
            raise ValueError("approved_actions contains an invalid approval reference")


@dataclass(frozen=True)
class ToolCallResult:
    tool_name: str
    value: Any
    duration_ms: int
    output_bytes: int
    call_id: str | None = None


class AuthorizationPolicy(Protocol):
    def authorize(self, context: ExecutionContext, spec: ToolSpec, arguments: dict[str, Any]) -> str: ...


class AllowlistAuthorization:
    """Default-deny tool and target checks with parameter-bound approval grants."""

    def authorize(self, context: ExecutionContext, spec: ToolSpec, arguments: dict[str, Any]) -> str:
        if spec.name not in context.allowed_tools:
            raise ToolCallError("tool_not_allowed", "Tool is not allowed for this task")

        target_id = context.context_target_id
        if spec.target_id_argument is not None and spec.target_id_argument in arguments:
            target_id = arguments[spec.target_id_argument]
        if spec.target_required and not isinstance(target_id, str):
            raise ToolCallError("target_required", "A target is required for this tool")
        if target_id is not None and (
            not isinstance(target_id, str)
            or target_id not in context.allowed_target_ids
        ):
            raise ToolCallError("target_not_allowed", "Target is not allowed for this task")

        if not spec.requires_approval:
            return "not_required"
        fingerprint = _approval_fingerprint(
            spec.name, target_id, arguments, task_id=context.task_id,
            tenant_id=context.tenant_id, user_id=context.user_id,
        )
        if not context.approved_actions.get(fingerprint):
            raise ToolCallError(
                "approval_required",
                "This tool call requires approval",
                approval_fingerprint=fingerprint,
                target_id=target_id,
            )
        return "approved"


class ToolRegistry:
    def __init__(self, audit: AuditSink, authorization: AuthorizationPolicy | None = None) -> None:
        self._audit = audit
        self._authorization = authorization or AllowlistAuthorization()
        self._tools: dict[str, ToolSpec] = {}

    def register(self, spec: ToolSpec) -> None:
        self.register_many((spec,))

    def register_many(self, specs: tuple[ToolSpec, ...] | list[ToolSpec]) -> None:
        names = [spec.name for spec in specs]
        if len(names) != len(set(names)) or any(name in self._tools for name in names):
            raise ValueError("duplicate tool name")
        copied = {
            spec.name: ToolSpec(
                name=spec.name,
                description=spec.description,
                input_schema=copy.deepcopy(spec.input_schema),
                risk_level=spec.risk_level,
                requires_approval=spec.requires_approval,
                handler=spec.handler,
                target_id_argument=spec.target_id_argument,
                target_required=spec.target_required,
                origin=copy.deepcopy(spec.origin),
                timeout_ms=spec.timeout_ms,
                cancellable=spec.cancellable,
            )
            for spec in specs
        }
        self._tools.update(copied)

    def get(self, name: str) -> ToolSpec | None:
        spec = self._tools.get(name)
        return None if spec is None else self._copy_spec(spec)

    def list(self) -> tuple[ToolSpec, ...]:
        return tuple(self._copy_spec(self._tools[name]) for name in sorted(self._tools))

    async def execute(
        self,
        name: str,
        arguments: object,
        context: ExecutionContext,
        *,
        timeout_seconds: float = 30.0,
        max_output_bytes: int = 256 * 1024,
        call_id: str | None = None,
    ) -> ToolCallResult:
        if (
            isinstance(timeout_seconds, bool)
            or not isinstance(timeout_seconds, (int, float))
            or not 0.001 <= timeout_seconds <= 300
        ):
            raise ValueError("timeout_seconds must be between 0.001 and 300")
        if isinstance(max_output_bytes, bool) or not isinstance(max_output_bytes, int) or not 1 <= max_output_bytes <= 4 * 1024 * 1024:
            raise ValueError("max_output_bytes must be between 1 and 4194304")

        spec = self._tools.get(name) if isinstance(name, str) else None
        started = datetime.now(UTC).isoformat()
        started_clock = time.monotonic()
        event_id = str(uuid.uuid4())
        safe_arguments = arguments if isinstance(arguments, dict) else {}
        target_id = _safe_target_id(safe_arguments, spec, context)
        await self._audit.record(
            _make_event(
                context,
                event_id=event_id,
                status="started",
                tool_name=spec.name if spec is not None else "<unregistered>",
                target_id=target_id,
                started_at=started,
                arguments=safe_arguments,
            )
        )

        approval_status = "not_required"
        try:
            if spec is None:
                raise ToolCallError("unknown_tool", "Tool is not registered")
            if not isinstance(arguments, dict):
                raise ToolCallError("invalid_arguments", "Tool arguments must be an object")
            if not _has_string_object_keys(arguments):
                raise ToolCallError("invalid_arguments", "Tool arguments must use string object keys")
            try:
                argument_bytes = json.dumps(
                    arguments, ensure_ascii=False, separators=(",", ":"), allow_nan=False
                ).encode("utf-8")
            except (TypeError, ValueError):
                raise ToolCallError("invalid_arguments", "Tool arguments must be valid JSON values") from None
            if len(argument_bytes) > 1024 * 1024:
                raise ToolCallError("invalid_arguments", "Tool arguments exceeded the size limit")
            try:
                Draft202012Validator(spec.input_schema).validate(arguments)
            except ValidationError:
                raise ToolCallError("invalid_arguments", "Tool arguments do not match the registered schema") from None
            approval_status = "required" if spec.requires_approval else "not_required"
            policy_status = self._authorization.authorize(context, spec, arguments)
            if policy_status not in {"not_required", "approved"}:
                raise ToolCallError("authorization_failed", "Authorization policy returned an invalid result")
            approval_status = policy_status
            handler_context = replace(
                context,
                call_id=call_id or event_id,
                trace_id=context.trace_id or event_id,
            )
            value = await asyncio.wait_for(spec.handler(arguments, handler_context), timeout=timeout_seconds)
            json_value = _to_json_value(value)
            try:
                encoded = json.dumps(
                    json_value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
                ).encode("utf-8")
            except (TypeError, ValueError):
                raise ToolCallError("invalid_tool_result", "Tool returned a value that cannot be serialized") from None
            if len(encoded) > max_output_bytes:
                raise ToolCallError("output_too_large", "Tool result exceeded the output size limit")
        except asyncio.TimeoutError:
            await self._record_finished(
                context, event_id, "failed", spec.name if spec is not None else "<unregistered>", target_id, started, safe_arguments,
                started_clock, error_code="execution_timeout", approval_status=approval_status,
            )
            raise ToolCallError("execution_timeout", "Tool execution exceeded its time limit") from None
        except asyncio.CancelledError:
            await self._record_finished(
                context, event_id, "cancelled", name, target_id, started, safe_arguments,
                started_clock, error_code="execution_cancelled", approval_status=approval_status,
            )
            raise
        except ToolCallError as error:
            status = "awaiting_approval" if error.code == "approval_required" else (
                "denied" if error.code in {"tool_not_allowed", "target_required", "target_not_allowed", "risk_not_allowed", "authorization_failed"}
                else "failed"
            )
            error_result = _bounded_error_result(error.result, max_output_bytes)
            if error.code == "approval_required" and spec is not None and isinstance(arguments, dict):
                target_for_approval = context.context_target_id
                if spec.target_id_argument is not None and spec.target_id_argument in arguments:
                    target_for_approval = arguments[spec.target_id_argument]
                error.target_id = target_for_approval if isinstance(target_for_approval, str) else None
                error.approval_fingerprint = _approval_fingerprint(
                    spec.name,
                    error.target_id,
                    arguments,
                    task_id=context.task_id,
                    tenant_id=context.tenant_id,
                    user_id=context.user_id,
                )
                approval_status = "awaiting_approval"
            await self._record_finished(
                context, event_id, status, spec.name if spec is not None else "<unregistered>", target_id, started, safe_arguments,
                started_clock,
                result_summary=(
                    _result_summary(error_result[0], error_result[1])
                    if error_result is not None else None
                ),
                error_code=error.code,
                approval_status=approval_status,
            )
            raise
        except Exception:
            await self._record_finished(
                context, event_id, "failed", spec.name if spec is not None else "<unregistered>", target_id, started, safe_arguments,
                started_clock, error_code="tool_failed", approval_status=approval_status,
            )
            raise ToolCallError("tool_failed", "Tool execution failed") from None

        duration_ms = max(0, round((time.monotonic() - started_clock) * 1000))
        result_summary = _result_summary(json_value, len(encoded))
        await self._record_finished(
            context, event_id, "succeeded", spec.name if spec is not None else "<unregistered>", target_id, started, safe_arguments,
            started_clock, result_summary=result_summary, approval_status=approval_status,
        )
        return ToolCallResult(name, json_value, duration_ms, len(encoded), handler_context.call_id)

    @staticmethod
    def _copy_spec(spec: ToolSpec) -> ToolSpec:
        return ToolSpec(
            name=spec.name,
            description=spec.description,
            input_schema=copy.deepcopy(spec.input_schema),
            risk_level=spec.risk_level,
            requires_approval=spec.requires_approval,
            handler=spec.handler,
            target_id_argument=spec.target_id_argument,
            target_required=spec.target_required,
            origin=copy.deepcopy(spec.origin),
            timeout_ms=spec.timeout_ms,
            cancellable=spec.cancellable,
        )

    async def _record_finished(
        self,
        context: ExecutionContext,
        event_id: str,
        status: str,
        tool_name: str,
        target_id: str | None,
        started_at: str,
        arguments: dict[str, Any],
        started_clock: float,
        *,
        result_summary: dict[str, object] | None = None,
        error_code: str | None = None,
        approval_status: str = "not_required",
    ) -> None:
        await self._audit.record(
            _make_event(
                context,
                event_id=event_id,
                status=status,
                tool_name=tool_name,
                target_id=target_id,
                started_at=started_at,
                finished_at=datetime.now(UTC).isoformat(),
                duration_ms=max(0, round((time.monotonic() - started_clock) * 1000)),
                arguments=arguments,
                result_summary=result_summary,
                error_code=error_code,
                approval_status=approval_status,
            )
        )


def approval_fingerprint(
    tool_name: str,
    target_id: str | None,
    arguments: dict[str, Any],
    *,
    task_id: str,
    tenant_id: str,
    user_id: str,
) -> str:
    """Stable key for an approval explicitly bound to tool, target and arguments."""
    return _approval_fingerprint(
        tool_name, target_id, arguments, task_id=task_id, tenant_id=tenant_id, user_id=user_id
    )


def _approval_fingerprint(
    tool_name: str,
    target_id: str | None,
    arguments: dict[str, Any],
    *,
    task_id: str,
    tenant_id: str,
    user_id: str,
) -> str:
    material = json.dumps(
        {
            "task_id": task_id,
            "tenant_id": tenant_id,
            "user_id": user_id,
            "tool": tool_name,
            "target_id": target_id,
            "arguments": arguments,
        },
        sort_keys=True,
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(material).hexdigest()


def _make_event(
    context: ExecutionContext,
    *,
    event_id: str,
    status: str,
    tool_name: str,
    target_id: str | None,
    started_at: str,
    arguments: dict[str, Any],
    finished_at: str | None = None,
    duration_ms: int | None = None,
    result_summary: dict[str, object] | None = None,
    error_code: str | None = None,
    approval_status: str = "not_required",
) -> AuditEvent:
    safe_pairs = sorted(
        (key[:128], type(value).__name__[:48])
        for key, value in arguments.items()
        if isinstance(key, str)
    )
    safe_keys = tuple(key for key, _ in safe_pairs)
    safe_types = tuple(safe_pairs)
    summary = dict(result_summary or {})
    if duration_ms is not None:
        summary["duration_ms"] = duration_ms
    return AuditEvent(
        event_id=event_id,
        status=status,
        task_id=context.task_id,
        tenant_id=context.tenant_id,
        user_id=context.user_id,
        tool_name=tool_name[:128] if isinstance(tool_name, str) and _TOOL_NAME.fullmatch(tool_name) else "<unregistered>",
        target_id=target_id,
        started_at=started_at,
        finished_at=finished_at,
        argument_keys=safe_keys,
        argument_types=safe_types,
        result_summary=summary or None,
        error_code=error_code,
        approval_status=approval_status,
    )


def _safe_target_id(
    arguments: dict[str, Any], spec: ToolSpec | None, context: ExecutionContext
) -> str | None:
    value = context.context_target_id
    if spec is not None and spec.target_id_argument is not None:
        candidate = arguments.get(spec.target_id_argument)
        if isinstance(candidate, str):
            value = candidate
    return value[:128] if isinstance(value, str) and _IDENTIFIER.fullmatch(value) else None


def _result_summary(value: Any, encoded_size: int) -> dict[str, object]:
    if isinstance(value, dict):
        return {"kind": "object", "keys": sorted(str(key)[:128] for key in value)[:64], "bytes": encoded_size}
    if isinstance(value, list):
        return {"kind": "array", "items": len(value), "bytes": encoded_size}
    return {"kind": type(value).__name__[:48], "bytes": encoded_size}


def _bounded_error_result(value: object, max_output_bytes: int) -> tuple[Any, int] | None:
    if not isinstance(value, dict):
        return None
    try:
        json_value = _to_json_value(value)
        encoded = json.dumps(
            json_value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        ).encode("utf-8")
    except (ToolCallError, TypeError, ValueError):
        return None
    if len(encoded) > max_output_bytes:
        return None
    return json_value, len(encoded)


def _to_json_value(value: Any) -> Any:
    if is_dataclass(value) and not isinstance(value, type):
        return _to_json_value(asdict(value))
    if isinstance(value, dict):
        if any(not isinstance(key, str) for key in value):
            raise ToolCallError("invalid_tool_result", "Tool returned a value that cannot be serialized")
        return {key: _to_json_value(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_to_json_value(item) for item in value]
    if value is None or isinstance(value, (str, int, bool)):
        return value
    if isinstance(value, float) and value == value and abs(value) != float("inf"):
        return value
    model_dump = getattr(value, "model_dump", None)
    if callable(model_dump):
        return _to_json_value(model_dump(mode="json"))
    raise ToolCallError("invalid_tool_result", "Tool returned a value that cannot be serialized")


def _reject_schema_references(node: Any) -> None:
    if isinstance(node, dict):
        for key in ("$ref", "$dynamicRef", "$recursiveRef"):
            reference = node.get(key)
            if reference is not None and (not isinstance(reference, str) or not reference.startswith("#")):
                raise ValueError("external schema references are not supported")
        schema_id = node.get("$id")
        if schema_id is not None and (not isinstance(schema_id, str) or "://" in schema_id):
            raise ValueError("external schema identifiers are not supported")
        for value in node.values():
            _reject_schema_references(value)
    elif isinstance(node, list):
        for value in node:
            _reject_schema_references(value)


def _check_schema_complexity(schema: Any) -> None:
    remaining = 10000

    def visit(node: Any, depth: int) -> None:
        nonlocal remaining
        remaining -= 1
        if remaining < 0 or depth > 32:
            raise ValueError("tool input schema is too complex")
        if isinstance(node, dict):
            if len(node) > 256:
                raise ValueError("tool input schema is too complex")
            pattern = node.get("pattern")
            if pattern is not None and (not isinstance(pattern, str) or len(pattern) > 256):
                raise ValueError("tool schema pattern exceeds the size limit")
            for value in node.values():
                visit(value, depth + 1)
        elif isinstance(node, list):
            if len(node) > 256:
                raise ValueError("tool input schema is too complex")
            for value in node:
                visit(value, depth + 1)

    visit(schema, 0)


def _has_string_object_keys(value: Any, depth: int = 0) -> bool:
    if depth > 32:
        return False
    if isinstance(value, dict):
        return all(isinstance(key, str) and _has_string_object_keys(item, depth + 1) for key, item in value.items())
    if isinstance(value, (list, tuple)):
        return all(_has_string_object_keys(item, depth + 1) for item in value)
    return value is None or isinstance(value, (str, int, float, bool))
