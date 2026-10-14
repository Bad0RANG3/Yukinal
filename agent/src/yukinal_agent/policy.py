from __future__ import annotations

from dataclasses import dataclass
import re
from typing import Mapping
from types import MappingProxyType

from .tools import AuthorizationPolicy, ExecutionContext, ToolCallError, ToolSpec, approval_fingerprint

_TOOL_NAME = re.compile(r"^[A-Za-z][A-Za-z0-9_.:-]{0,127}$")
_IDENTIFIER = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:@/-]{0,127}$")


@dataclass(frozen=True)
class PrincipalScope:
    """The server-owned maximum permissions for one tenant/user pair."""

    allowed_tools: frozenset[str]
    allowed_target_ids: frozenset[str]
    allowed_risk_levels: frozenset[str] = frozenset({"read"})

    def __post_init__(self) -> None:
        if not isinstance(self.allowed_tools, frozenset) or len(self.allowed_tools) > 512 or any(
            not isinstance(value, str) or not _TOOL_NAME.fullmatch(value) for value in self.allowed_tools
        ):
            raise ValueError("principal tool scope contains an invalid name")
        if not isinstance(self.allowed_target_ids, frozenset) or len(self.allowed_target_ids) > 512 or any(
            not isinstance(value, str) or not _IDENTIFIER.fullmatch(value) for value in self.allowed_target_ids
        ):
            raise ValueError("principal target scope contains an invalid id")
        if not isinstance(self.allowed_risk_levels, frozenset) or not self.allowed_risk_levels or not self.allowed_risk_levels.issubset({"read", "write", "destructive"}):
            raise ValueError("principal risk scope is invalid")


class PolicyEngine(AuthorizationPolicy):
    """Default-deny policy that intersects task permissions with principal scope."""

    def __init__(self, principals: Mapping[tuple[str, str], PrincipalScope]) -> None:
        if len(principals) > 100_000:
            raise ValueError("principal policy contains too many entries")
        for identity, scope in principals.items():
            if (
                not isinstance(identity, tuple)
                or len(identity) != 2
                or any(not isinstance(part, str) or not _IDENTIFIER.fullmatch(part) for part in identity)
                or not isinstance(scope, PrincipalScope)
            ):
                raise ValueError("principal policy entry is invalid")
        self._principals = MappingProxyType(dict(principals))

    def authorize(self, context: ExecutionContext, spec: ToolSpec, arguments: dict[str, object]) -> str:
        scope = self._principals.get((context.tenant_id, context.user_id))
        if scope is None or spec.name not in scope.allowed_tools or spec.name not in context.allowed_tools:
            raise ToolCallError("tool_not_allowed", "Tool is not allowed for this principal")
        if spec.risk_level not in scope.allowed_risk_levels:
            raise ToolCallError("risk_not_allowed", "Tool risk level is not allowed for this principal")

        target_id = context.context_target_id
        if spec.target_id_argument is not None and spec.target_id_argument in arguments:
            target_id = arguments[spec.target_id_argument]
        if spec.target_required and not isinstance(target_id, str):
            raise ToolCallError("target_required", "A target is required for this tool")
        if target_id is not None and (
            not isinstance(target_id, str)
            or target_id not in scope.allowed_target_ids
            or target_id not in context.allowed_target_ids
        ):
            raise ToolCallError("target_not_allowed", "Target is not allowed for this principal")

        if not spec.requires_approval:
            return "not_required"
        fingerprint = approval_fingerprint(
            spec.name,
            target_id,
            arguments,
            task_id=context.task_id,
            tenant_id=context.tenant_id,
            user_id=context.user_id,
        )
        if not context.approved_actions.get(fingerprint):
            raise ToolCallError(
                "approval_required",
                "This tool call requires approval",
                approval_fingerprint=fingerprint,
                target_id=target_id,
            )
        return "approved"
