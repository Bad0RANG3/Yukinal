"""Conservative admission for optional background work, independent of a model.

Having a configured roster does not make every turn a delegation. Ordinary work
stays in the foreground with its normal tools; the user can still inspect and
cancel existing jobs. These are scope signals, never an authorization decision.
"""
from __future__ import annotations

import re
from typing import Any


def user_request_text(request: dict[str, Any]) -> str:
    """Use the current instruction, never attached documents or old task results."""
    parts = request.get("parts")
    texts = [part["text"] for part in parts or []
             if isinstance(part, dict) and part.get("type") == "text" and isinstance(part.get("text"), str)]
    text = "\n".join(texts).strip() or request.get("prompt", "")
    # Code is data; examples containing delegation commands cannot open a route.
    return re.sub(r"```[^\n]*\n[\s\S]*?(?:```|$)", "", text).strip().lower()


_NO_DELEGATION = re.compile(r"(?:不要|不用|无需|别|禁止).{0,12}(?:团队|子线程|子代理|并行|分派)|"
                            r"\b(?:do not|don't|without|no)\s+(?:use\s+)?(?:teams?|delegat\w*|subagents?|parallel\w*)\b")
_EXPLICIT_DELEGATION = re.compile(
    r"(?:使用|用|调用|启动|安排|交给|分配给|让|派出).{0,12}(?:团队|子线程|子代理|多个.{0,4}(?:代理|角色))|"
    r"(?:分派|派发|分工|并行|并发).{0,20}(?:任务|工作|检查|测试|分析|处理|执行)|"
    r"(?:团队|子线程|子代理).{0,8}(?:执行|处理|分工|完成)|"
    r"^(?:please\s+)?(?:dispatch|delegate|coordinate|parallel(?:ize)?)(?:\b|$)|"
    r"\b(?:use|launch|assign|ask)\s+(?:a\s+|the\s+)?(?:team|subagents?|multiple agents)\b|"
    r"\b(?:in parallel|independent workstreams|separate agents)\b")
_CONTINUE = re.compile(r"^(?:请|帮我)?(?:继续|接着|接下来|重试|重新安排)(?:\b|一下|执行|处理|任务|下一|后续|吧|[，。！？\s]|$)|"
                       r"^(?:please\s+)?(?:continue|resume|retry|next phase|next stage)\b")
_BROAD_SCOPE = re.compile(r"完整|全面|整体|端到端|全流程|大型|系统性|所有|全套|comprehensive|end.to.end|multi.stage|full.workflow|all components")
_STAGES = (
    r"规划|设计|plan\b|design\b", r"实现|编程|开发|implement\w*|develop\w*",
    r"安装|部署|install\w*|deploy\w*", r"测试|回归|验收|test\w*|regression|acceptance",
    r"修复|整改|优化|fix\w*|remediat\w*|optimi[sz]\w*", r"审计|审查|排查|audit\w*|review\w*|investigat\w*",
)
_DOMAINS = (
    r"防火墙|firewall", r"漏洞|vulnerabilit", r"日志|logs?\b", r"服务|services?\b",
    r"网络|network", r"容器|docker|containers?\b", r"权限|permissions?\b", r"代码|source code",
)


def tools_prohibited(request: dict[str, Any]) -> bool:
    """A direct current-turn no-tools instruction also closes continuation loops."""
    text = user_request_text(request)
    return bool(re.search(
        r"(?:^|[。！？\n；;])\s*(?:本轮|这轮|本次|此次)?\s*(?:不用|不要|无需|禁止)"
        r"(?:再|任何|一切)?(?:调用|使用)?(?:任何)?工具|"
        r"\b(?:do not|don't|never)\s+(?:use|call)\s+(?:any\s+)?tools\b|"
        r"(?:^|[.!?\n;])\s*(?:this turn[:,]?\s*)?no tools\b", text))


def delegation_allowed(request: dict[str, Any], *, has_prior_work: bool = False) -> bool:
    """Admit explicit delegation or a broad, multi-part workflow; default direct.

    Length, role preference, server focus, and number of metrics are deliberately
    irrelevant. A long health-check request still uses server.info directly.
    A short continuation may inherit only this conversation's actual job scope.
    """
    if not request.get("coordination") or request.get("toolAccess") == "none" or tools_prohibited(request):
        return False
    text = user_request_text(request)
    if _NO_DELEGATION.search(text):
        return False
    stages = sum(bool(re.search(pattern, text)) for pattern in _STAGES)
    domains = sum(bool(re.search(pattern, text)) for pattern in _DOMAINS)
    if _BROAD_SCOPE.search(text) and (stages >= 3 or (domains >= 3 and re.search(r"安全|安防|摸底|审计|测试|security|assessment|audit|test", text))):
        return True
    # Arithmetic does not become parallel work merely because roles are saved
    # or the model suggests them. A real arithmetic-engine project passes above.
    calculation = re.sub(r"请|帮我|用团队|使用团队|计算|算一下|算一算|等于多少|等于|是多少|多少|结果|是什么|"
                         r"\b(?:please|calculate|compute|what is|equals?)\b", "", text)
    if re.search(r"\d\s*[-+*/×÷=]\s*\d", calculation) and re.fullmatch(r"[\d\s()+*/×÷=?.？!！,，。%\-]+", calculation):
        return False
    if re.match(r"(?:请)?(?:为什么|为何|什么是|解释|介绍)|(?:why|what is|how does)\b", text):
        return False
    if _EXPLICIT_DELEGATION.search(text):
        return True
    return has_prior_work and bool(_CONTINUE.search(text))
