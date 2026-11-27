"""Model-facing tools that record investigation evidence products.

The host already validates and persists findings, decision briefs and artifacts
(`host.investigation.finding.record`, `.brief.record`, `.artifact.record`), and
the desktop UI renders a decision brief plus its options as the only way a user
can approve a plan.  Until this module existed the Agent had no way to call
those routes, so `decisionBrief` never appeared in `investigation_task_get` and
the UI's approval surface was dead code.

Every id and timestamp below is derived from the durable call identity, exactly
like `investigation.plan`, so a retried call after a lost reply lands on the
same rows instead of duplicating them.
"""

from __future__ import annotations

import asyncio
import hashlib
import re
from collections.abc import Mapping
from datetime import UTC, datetime
from typing import Any

from jsonschema import Draft202012Validator, ValidationError

from .audit import redact_audit
from .host_rpc import HostRpcClient
from .tools import ExecutionContext, ToolCallError

_TASK_ID_SCHEMA = {
    "type": "string",
    "minLength": 1,
    "maxLength": 256,
    "pattern": "^[A-Za-z0-9][A-Za-z0-9_.:@/-]{0,255}$",
}
_EVIDENCE_IDS_SCHEMA = {
    "type": "array",
    "maxItems": 128,
    "uniqueItems": True,
    "items": _TASK_ID_SCHEMA,
}
_TEXT = lambda limit: {"type": "string", "minLength": 1, "maxLength": limit}  # noqa: E731

_FINDING_INPUT_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["title", "kind", "statement", "confidence"],
    "properties": {
        "title": _TEXT(512),
        "kind": {"type": "string", "enum": ["fact", "inference", "unknown"]},
        "statement": _TEXT(16384),
        "confidence": {"type": "string", "enum": ["high", "medium", "low"]},
        "nextVerification": {"type": "string", "maxLength": 4096},
        "evidenceIds": _EVIDENCE_IDS_SCHEMA,
    },
}
_BRIEF_OPTION_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["title", "summary", "impact", "riskLevel", "verification"],
    "properties": {
        "id": _TASK_ID_SCHEMA,
        "title": _TEXT(512),
        "summary": _TEXT(8192),
        "impact": _TEXT(8192),
        "riskLevel": {"type": "string", "enum": ["read", "low", "medium", "high", "critical"]},
        "verification": _TEXT(8192),
        "preview": {"type": "string", "maxLength": 16384},
        "rollback": {"type": "string", "maxLength": 8192},
        "evidenceIds": _EVIDENCE_IDS_SCHEMA,
        "findingIds": _EVIDENCE_IDS_SCHEMA,
        "requiresApproval": {"type": "boolean"},
        "continuation": {
            "type": "string",
            "enum": ["continue_readonly", "start_plan", "wait_user", "stop"],
        },
    },
}
_BRIEF_INPUT_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["options"],
    "properties": {
        "planId": _TASK_ID_SCHEMA,
        "findingIds": _EVIDENCE_IDS_SCHEMA,
        "options": {
            "type": "array",
            "minItems": 1,
            "maxItems": 4,
            "items": _BRIEF_OPTION_SCHEMA,
        },
    },
}
_ARTIFACT_INPUT_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["kind", "title", "summary"],
    "properties": {
        "phase": {
            "type": "string",
            "enum": ["investigating", "decision", "execution", "verification", "recovery", "completed"],
        },
        "kind": {
            "type": "string",
            "enum": [
                "investigation_plan",
                "baseline",
                "evidence_set",
                "decision_brief",
                "change_plan",
                "execution",
                "verification",
                "failure",
            ],
        },
        "status": {
            "type": "string",
            "enum": ["draft", "ready", "succeeded", "failed", "superseded"],
        },
        "title": _TEXT(512),
        "summary": _TEXT(8192),
        "content": {"type": ["object", "array", "string", "number", "boolean", "null"]},
        "evidenceIds": _EVIDENCE_IDS_SCHEMA,
        "planId": _TASK_ID_SCHEMA,
        "planStepId": _TASK_ID_SCHEMA,
    },
}


_EVIDENCE_INPUT_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": ["kind", "sourceTool", "contentType", "content"],
    "properties": {
        "kind": {
            "type": "string",
            "enum": ["snapshot", "log", "service", "container", "file", "tool_result", "failure"],
        },
        "sourceTool": _TEXT(256),
        "inputSummary": {"type": "string", "minLength": 1, "maxLength": 4096},
        # The evidence content kind is derived here, never demanded of the model: it
        # used to be a two-value enum, and a model that wrote a readable description
        # instead ("server.info full snapshot on …") lost the whole run to a schema
        # rejection over a field the product can answer itself.
        "contentType": _TEXT(64),
        "content": {"type": ["object", "array", "string", "number", "boolean", "null"]},
        "truncated": {"type": "boolean"},
    },
}


def _record_output_validator(result_key: str) -> Draft202012Validator:
    return Draft202012Validator(
        {
            "type": "object",
            "properties": {
                result_key: {
                    "type": "object",
                    "properties": {"id": _TASK_ID_SCHEMA, "taskId": _TASK_ID_SCHEMA},
                    "required": ["id", "taskId"],
                }
            },
            "required": [result_key],
            "additionalProperties": False,
        }
    )


_EVIDENCE_OUTPUT_VALIDATOR = Draft202012Validator(
    {
        "type": "object",
        "properties": {
            "evidenceId": _TASK_ID_SCHEMA,
            "reused": {"type": "boolean"},
        },
        "required": ["evidenceId"],
        "additionalProperties": False,
    }
)


def derived_id(prefix: str, task_id: str, call_id: str) -> str:
    """Stable id for a durable record, so a retried call cannot duplicate rows."""
    digest = hashlib.sha256(f"{task_id}\x1f{call_id}".encode("utf-8")).hexdigest()[:32]
    return f"{prefix}_{digest}"


def _now() -> str:
    return datetime.now(UTC).isoformat()


def _content_type(claimed: object, content: object) -> str:
    """Normalize the caller's content kind onto the two values the host stores.

    The model may describe what it collected ("server.info snapshot on host x")
    instead of naming a stored kind. That description is useful context but not a
    valid kind, so the value is derived from the payload the host will actually
    keep: objects and arrays are JSON, everything else is text.
    """
    if isinstance(claimed, str) and claimed in {"json", "text"}:
        return claimed
    return "json" if isinstance(content, (dict, list)) else "text"


def _call_identity(tool_name: str, context: ExecutionContext) -> tuple[str, str]:
    task_id = context.host_task_id
    if not isinstance(task_id, str) or not task_id:
        raise ToolCallError("durable_task_required", f"{tool_name} requires a durable host task")
    if not context.call_id:
        raise ToolCallError("invalid_call_identity", f"{tool_name} has no stable tool-call identity")
    return task_id, context.call_id


def _error_code(response: dict[str, Any], fallback: str) -> tuple[str, str]:
    error = response.get("error")
    code = error.get("code") if isinstance(error, dict) else None
    message = error.get("message") if isinstance(error, dict) else None
    if not isinstance(code, str) or not re.fullmatch(r"[a-z][a-z0-9_]{0,63}", code):
        code = fallback
    if not isinstance(message, str) or not message:
        message = "The desktop host rejected the investigation record"
    return code, message


async def _send(host: HostRpcClient, tool_name: str, method: str, body: dict[str, Any]) -> dict[str, Any]:
    try:
        response = await host.request(method, body, timeout_seconds=15.0)
    except asyncio.CancelledError:
        raise
    except Exception:
        raise ToolCallError("host_unavailable", f"The desktop host could not record `{tool_name}`") from None
    if not isinstance(response, dict):
        raise ToolCallError("invalid_host_response", "The desktop host returned an invalid record response")
    if response.get("recorded") is not True:
        code, message = _error_code(response, "record_rejected")
        raise ToolCallError(code, message)
    return response


def _finding_handler(host: HostRpcClient, tool_name: str):
    async def execute(arguments: dict[str, Any], context: ExecutionContext) -> dict[str, Any]:
        task_id, call_id = _call_identity(tool_name, context)
        finding: dict[str, Any] = {
            "id": derived_id("finding", task_id, call_id),
            "taskId": task_id,
            "title": arguments["title"],
            "kind": arguments["kind"],
            "statement": arguments["statement"],
            "confidence": arguments["confidence"],
            "evidenceIds": list(arguments.get("evidenceIds") or context.evidence_ids or ()),
            "createdAt": _now(),
        }
        if isinstance(arguments.get("nextVerification"), str):
            finding["nextVerification"] = arguments["nextVerification"]
        response = await _send(host, tool_name, "host.investigation.finding.record", {"finding": finding})
        return {"finding": response.get("finding")}

    return execute


def _brief_handler(host: HostRpcClient, tool_name: str):
    async def execute(arguments: dict[str, Any], context: ExecutionContext) -> dict[str, Any]:
        task_id, call_id = _call_identity(tool_name, context)
        brief_id = derived_id("brief", task_id, call_id)
        options = []
        for index, raw in enumerate(arguments["options"]):
            option = {
                "id": raw.get("id") or f"{brief_id}-option-{index + 1}",
                "title": raw["title"],
                "summary": raw["summary"],
                "impact": raw["impact"],
                "riskLevel": raw["riskLevel"],
                "verification": raw["verification"],
                "evidenceIds": list(raw.get("evidenceIds") or context.evidence_ids or ()),
                "findingIds": list(raw.get("findingIds") or ()),
                "requiresApproval": bool(raw.get("requiresApproval", True)),
                "status": "available",
            }
            for key in ("preview", "rollback", "continuation"):
                if isinstance(raw.get(key), str):
                    option[key] = raw[key]
            options.append(option)
        # The host only accepts a brief that describes the current plan revision.
        plan_id = arguments.get("planId") or context.plan_id
        brief: dict[str, Any] = {
            "id": brief_id,
            "taskId": task_id,
            "generatedAt": _now(),
            "status": "presented",
            "findingIds": list(arguments.get("findingIds") or ()),
            "options": options,
        }
        if isinstance(plan_id, str) and plan_id:
            brief["planId"] = plan_id
        response = await _send(host, tool_name, "host.investigation.brief.record", {"brief": brief})
        return {"brief": response.get("brief")}

    return execute


def _artifact_handler(host: HostRpcClient, tool_name: str):
    async def execute(arguments: dict[str, Any], context: ExecutionContext) -> dict[str, Any]:
        task_id, call_id = _call_identity(tool_name, context)
        now = _now()
        artifact: dict[str, Any] = {
            "id": derived_id("artifact", task_id, call_id),
            "taskId": task_id,
            "phase": arguments.get("phase") or "investigating",
            "kind": arguments["kind"],
            "status": arguments.get("status") or "ready",
            "title": arguments["title"],
            "summary": arguments["summary"],
            "content": arguments.get("content"),
            "evidenceIds": list(arguments.get("evidenceIds") or context.evidence_ids or ()),
            "createdAt": now,
            "updatedAt": now,
        }
        body: dict[str, Any] = {"artifact": artifact}
        for key, fallback in (("planId", context.plan_id), ("planStepId", context.plan_step_id)):
            value = arguments.get(key) or fallback
            if isinstance(value, str) and value:
                body[key] = value
        if context.run_id:
            artifact["runId"] = context.run_id
        response = await _send(host, tool_name, "host.investigation.artifact.record", body)
        return {"artifact": response.get("artifact")}

    return execute


def _evidence_handler(host: HostRpcClient, tool_name: str):
    async def execute(arguments: dict[str, Any], context: ExecutionContext) -> dict[str, Any]:
        task_id, call_id = _call_identity(tool_name, context)
        target = context.host_target if isinstance(context.host_target, Mapping) else {}
        # The host refuses evidence whose scope does not match the task scope, so the
        # scope comes from the run's own target instead of model text.
        scope = {
            key: target[key]
            for key in ("host", "serverId", "workspaceId", "environment")
            if isinstance(target.get(key), str) and target[key]
        }
        if not scope.get("host"):
            raise ToolCallError("target_required", f"{tool_name} needs the run target scope")
        content = arguments.get("content")
        # Redacting before persistence keeps `redactionStatus` honest and stops a
        # credential from becoming evidence. The host recomputes `contentHash` over
        # exactly what it stores, so no digest is sent: a Python-side digest could
        # never match nlohmann's dump() escaping byte for byte, and a wrong claim
        # would only make a good observation fail.
        redacted = redact_audit(content)
        redaction_status = "clean" if redacted == content else "redacted"
        evidence: dict[str, Any] = {
            "id": derived_id("ev", task_id, call_id),
            "taskId": task_id,
            "scope": scope,
            "kind": arguments["kind"],
            "sourceTool": arguments["sourceTool"],
            "collectedAt": _now(),
            "contentType": _content_type(arguments["contentType"], redacted),
            "content": redacted,
            "truncated": bool(arguments.get("truncated", False)),
            "redactionStatus": redaction_status,
        }
        summary = arguments.get("inputSummary")
        if isinstance(summary, str) and summary:
            evidence["inputSummary"] = summary
        if context.run_id:
            evidence["runId"] = context.run_id
        response = await _send(host, tool_name, "host.investigation.evidence.record", {"evidence": evidence})
        return {"evidenceId": response.get("evidenceId"), "reused": response.get("reused")}

    return execute


RECORD_TOOLS: tuple[tuple[str, str, dict[str, Any], Draft202012Validator, str], ...] = (
    (
        "investigation.evidence",
        "Persist one bounded observation as evidence for the current investigation. A finding "
        "cannot cite anything else, so record the tool result you actually read before drawing a "
        "conclusion. The host owns the run association and recomputes the content hash; recording "
        "evidence never touches the target.",
        _EVIDENCE_INPUT_SCHEMA,
        _EVIDENCE_OUTPUT_VALIDATOR,
        "evidenceId",
    ),
    (
        "investigation.finding",
        "Record one durable finding for the current investigation. A fact or inference must cite the "
        "evidence ids it rests on; use kind `unknown` only when no evidence exists yet. Recording a "
        "finding never changes the target.",
        _FINDING_INPUT_SCHEMA,
        _record_output_validator("finding"),
        "finding",
    ),
    (
        "investigation.brief",
        "Publish the decision brief for the current investigation so the user can choose. Options are "
        "the only way the user approves a plan: an option with continuation `start_plan` approves the "
        "current plan revision, `wait_user` keeps waiting, `stop` ends the task. Cite findings, not "
        "raw evidence, and keep each option independently verifiable.",
        _BRIEF_INPUT_SCHEMA,
        _record_output_validator("brief"),
        "brief",
    ),
    (
        "investigation.artifact",
        "Attach a bounded structured artifact (baseline, change plan, verification, execution or "
        "failure record) to the current investigation at this plan step. Artifacts are exempt from "
        "per-step tool allowlists but still bound to the task, plan revision and evidence set.",
        _ARTIFACT_INPUT_SCHEMA,
        _record_output_validator("artifact"),
        "artifact",
    ),
)


def record_handler(host: HostRpcClient, tool_name: str):
    builders = {
        "investigation.evidence": _evidence_handler,
        "investigation.finding": _finding_handler,
        "investigation.brief": _brief_handler,
        "investigation.artifact": _artifact_handler,
    }
    return builders[tool_name](host, tool_name)


def validate_record_output(result_key: str, validator: Draft202012Validator, response: Any) -> None:
    try:
        validator.validate(response)
    except ValidationError:
        raise ToolCallError(
            "invalid_host_response", f"The desktop host returned an invalid {result_key} record"
        ) from None
