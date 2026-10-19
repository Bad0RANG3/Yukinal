#!/usr/bin/env python3
"""Exercise the Python Agent tool loop against an explicitly configured live model."""

from __future__ import annotations

import asyncio
import os
import secrets
import sys
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "agent" / "src"))

from yukinal_agent import (
    AgentBudget,
    ExecutionContext,
    MemoryAuditSink,
    ModelConfig,
    ModelDrivenAgent,
    ModelFactory,
    ModelFactoryError,
    ToolRegistry,
    ToolSpec,
)

_PROVIDERS = {"openai", "openai-compatible", "anthropic", "gemini"}


def _live_config() -> tuple[str, ModelConfig]:
    if os.environ.get("YUKINAL_LIVE_PROVIDER_TESTS") != "1":
        raise ValueError("set YUKINAL_LIVE_PROVIDER_TESTS=1 to explicitly enable a billable live request")

    provider = os.environ.get("YUKINAL_LIVE_PROVIDER", "").strip()
    if provider not in _PROVIDERS:
        raise ValueError("YUKINAL_LIVE_PROVIDER must be openai, openai-compatible, anthropic, or gemini")
    model = os.environ.get("YUKINAL_LIVE_MODEL", "").strip()
    if not model:
        raise ValueError("YUKINAL_LIVE_MODEL is required")

    base_url = os.environ.get("YUKINAL_LIVE_BASE_URL") or None
    api_key = os.environ.get("YUKINAL_LIVE_API_KEY")
    if provider == "openai-compatible" and base_url is None:
        raise ValueError("YUKINAL_LIVE_BASE_URL is required for openai-compatible providers")
    if base_url is not None:
        try:
            scheme = urlsplit(base_url).scheme.lower()
        except ValueError:
            raise ValueError("YUKINAL_LIVE_BASE_URL must be a valid HTTPS endpoint") from None
        if scheme != "https" and not _is_loopback(base_url):
            raise ValueError("remote YUKINAL_LIVE_BASE_URL endpoints must use HTTPS")
    if not api_key:
        if provider != "openai-compatible" or not _is_loopback(base_url):
            raise ValueError("YUKINAL_LIVE_API_KEY is required for a remote provider")

    wire_api = os.environ.get("YUKINAL_LIVE_WIRE_API") or None
    api_version = os.environ.get("YUKINAL_LIVE_API_VERSION") or None
    try:
        config = ModelConfig(
            provider=provider,  # type: ignore[arg-type]
            model=model,
            api_key_env="YUKINAL_LIVE_API_KEY",
            temperature=0,
            timeout_seconds=30,
            max_retries=0,
            max_output_tokens=128,
            base_url=base_url,
            api_version=api_version,
            wire_api=wire_api,  # type: ignore[arg-type]
        )
    except (TypeError, ValueError):
        raise ValueError("the live model configuration is invalid; check provider, endpoint, API version, and wire API") from None
    return provider, config


def _is_loopback(base_url: str | None) -> bool:
    if not base_url:
        return False
    try:
        hostname = (urlsplit(base_url).hostname or "").lower()
    except ValueError:
        return False
    return hostname in {"localhost", "127.0.0.1", "::1"}


async def _run() -> int:
    try:
        provider, config = _live_config()
    except ValueError as error:
        print(f"Live provider acceptance not started: {error}", file=sys.stderr)
        return 2

    token = secrets.token_urlsafe(18)
    executions: list[bool] = []

    async def probe(_arguments: dict[str, Any], _context: ExecutionContext) -> dict[str, str]:
        executions.append(True)
        return {"verificationToken": token}

    registry = ToolRegistry(MemoryAuditSink())
    registry.register(ToolSpec(
        name="acceptance.probe",
        description="Return the unique verification token for this read-only provider acceptance check.",
        input_schema={
            "type": "object",
            "properties": {},
            "additionalProperties": False,
        },
        risk_level="read",
        requires_approval=False,
        handler=probe,
        target_id_argument=None,
    ))

    try:
        model = ModelFactory().create(config)
        agent = ModelDrivenAgent(
            registry,
            model,
            AgentBudget(
                max_tool_calls=1,
                max_model_rounds=3,
                max_total_seconds=75,
                max_tool_seconds=10,
                max_output_bytes=4096,
                max_model_output_tokens=128,
            ),
        )
        result = await agent.run(
            task_id="live-provider-acceptance",
            tenant_id="local-acceptance",
            user_id="operator",
            prompt=(
                "Call acceptance.probe exactly once. Then reply with the exact verificationToken "
                "returned by that tool and no other token."
            ),
            allowed_tools={"acceptance.probe"},
            allowed_target_ids=set(),
        )
    except ModelFactoryError as error:
        print(f"Live provider acceptance failed during model setup: {error.code}", file=sys.stderr)
        return 1
    except Exception:
        print("Live provider acceptance failed during model setup; diagnostic details were suppressed.", file=sys.stderr)
        return 1

    if (
        result.status != "succeeded"
        or result.error_code is not None
        or result.tool_calls != 1
        or len(executions) != 1
        or result.answer.strip() != token
    ):
        code = result.error_code or "tool_call_or_verification_failed"
        print(f"Live provider acceptance failed: {code}", file=sys.stderr)
        return 1

    print(f"PASS live Python Agent tool loop: provider={provider}, model={config.model}, tools=1")
    return 0


def main() -> int:
    return asyncio.run(_run())


if __name__ == "__main__":
    raise SystemExit(main())
