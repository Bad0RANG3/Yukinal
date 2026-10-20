"""Local, deterministic inference routing. No extra model request or credentials."""
from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

from .models import supports_reasoning_effort, with_reasoning_effort

_DEEP = re.compile(r"深度|深入|根因|架构|审计|证明|推导|权衡|全面|彻底|详细分析|复杂|漏洞|deep\s+research|root\s+cause|architect|audit|prove|trade.?offs?", re.I)
_ACTION = re.compile(r"修复|优化|落实|部署|迁移|实现|重构|删除|写入|执行|故障|报错|排查|分析|检查|fix|debug|deploy|migrat|implement|refactor|investigat", re.I)
_SIMPLE = re.compile(r"^(?:你好|您好|谢谢|hello|hi|thanks)[!！。,.，\s]*$", re.I)
_TASK_CONTEXT = re.compile(r"\b(task_|plan_|srv_)|服务器|日志|故障|修复|排查|部署|任务", re.I)


@dataclass(frozen=True)
class InferencePlan:
    model: Any
    effort: str | None
    output_tokens: int
    source: str


def select_inference(model: Any, request: dict[str, Any]) -> InferencePlan:
    """Manual effort wins. Unverified endpoints keep their own parameters."""
    explicit = request.get("reasoningEffort")
    if explicit is not None:
        selected = with_reasoning_effort(model, explicit)
        return InferencePlan(selected, explicit, _output_budget(explicit), "manual")
    if not supports_reasoning_effort(model):
        bound = getattr(model, "max_tokens", None)
        return InferencePlan(model, None, bound if type(bound) is int and 1 <= bound <= 32768 else 2048, "provider")
    text = request.get("promptText", request.get("prompt", ""))
    # Attachments and follow-ups may depend on substantial existing work. Never
    # disable thinking just because a continuation is a few characters long.
    contextual = bool(request.get("taskId") or request.get("planBinding") or request.get("parts"))
    if _DEEP.search(text) or len(text) > 1800:
        effort = "high"
    elif contextual or _ACTION.search(text) or request.get("coordination") or _TASK_CONTEXT.search(text):
        effort = "low"
    elif _SIMPLE.fullmatch(text.strip()):
        effort = "none"
    else:
        effort = "low"
    return InferencePlan(with_reasoning_effort(model, effort), effort, _output_budget(effort), "auto")


def _output_budget(effort: str) -> int:
    # Completion limits include private reasoning. These bounds are distinct
    # from the short public answer contract; never truncate reasoning to be terse.
    return {"none": 2048, "low": 8192, "high": 16384, "max": 32768}[effort]
