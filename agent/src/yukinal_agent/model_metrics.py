"""Bounded public metrics from provider metadata, never private reasoning text."""
from __future__ import annotations

from typing import Any


def _count(value: Any) -> bool:
    return type(value) is int and 0 <= value <= 2**53 - 1


def accumulate_usage(previous: dict[str, Any], response: Any) -> dict[str, Any]:
    reported = getattr(response, "usage_metadata", None) or {}
    valid = all(_count(reported.get(key)) for key in ("input_tokens", "output_tokens"))
    usage = {**previous,
        "inputTokens": previous.get("inputTokens", 0) + (reported["input_tokens"] if valid else 0),
        "outputTokens": previous.get("outputTokens", 0) + (reported["output_tokens"] if valid else 0),
        "reportedRequests": previous.get("reportedRequests", 0) + int(valid),
        "unreportedRequests": previous.get("unreportedRequests", 0) + int(not valid)}
    metadata = getattr(response, "response_metadata", {}) or {}
    raw = metadata.get("deepseek_usage") or metadata.get("token_usage") or {}
    details = reported.get("output_token_details") or {}
    reasoning = details.get("reasoning")
    if _count(reasoning):
        usage["reasoningTokens"] = previous.get("reasoningTokens", 0) + reasoning
        usage["reasoningReportedRequests"] = previous.get("reasoningReportedRequests", 0) + 1
    cached = raw.get("prompt_cache_hit_tokens", (reported.get("input_token_details") or {}).get("cache_read"))
    if _count(cached):
        usage["cacheReadTokens"] = previous.get("cacheReadTokens", 0) + cached
        usage["cacheReportedRequests"] = previous.get("cacheReportedRequests", 0) + 1
    missed = raw.get("prompt_cache_miss_tokens")
    if _count(missed):
        usage["cacheMissTokens"] = previous.get("cacheMissTokens", 0) + missed
    return usage


def request_metrics(response: Any, timing: dict[str, Any], inference: dict[str, Any]) -> dict[str, Any]:
    metadata = getattr(response, "response_metadata", {}) or {}
    result = {key: value for key, value in timing.items() if key != "started"}
    for key, value in (("model", metadata.get("model_name") or metadata.get("model") or inference.get("model")),
                       ("systemFingerprint", metadata.get("system_fingerprint")),
                       ("finishReason", metadata.get("finish_reason") or metadata.get("stop_reason"))):
        if isinstance(value, str) and value:
            result[key] = value[:256]
    if inference.get("effort") in {"none", "low", "high", "max"}:
        result["effort"] = inference["effort"]
    if inference.get("source") in {"auto", "manual", "provider"}:
        result["routing"] = inference["source"]
    return result
