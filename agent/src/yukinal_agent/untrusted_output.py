"""The only serialization boundary for tool data entering model history.

Keep machine-readable result fields, but mark the complete object as remote data.
No remote bytes can close the boundary or introduce terminal controls.
"""
from __future__ import annotations

import json
from typing import Any

from yukinal_knowledge.text_safety import UNTRUSTED_END as END
from yukinal_knowledge.text_safety import UNTRUSTED_START as START
from yukinal_knowledge.text_safety import clean_text

_RESERVED = {"_untrustedStart", "_untrustedEnd"}


def clean_value(value: Any) -> Any:
    if isinstance(value, str):
        return clean_text(value)
    if isinstance(value, dict):
        return {safe_key: clean_value(item) for key, item in value.items()
                if (safe_key := clean_text(str(key))) not in _RESERVED}
    if isinstance(value, (list, tuple)):
        return [clean_value(item) for item in value]
    return value


def untrusted_tool_content(content: Any, max_bytes: int = 16 * 1024) -> str:
    """Return bounded JSON with explicit, unforgeable data boundary fields."""
    if max_bytes < 512:
        raise ValueError("tool context limit must be at least 512 bytes")
    if isinstance(content, str):
        try:
            content = json.loads(content)
        except (ValueError, TypeError):
            pass
    value = clean_value(content)
    payload = value if isinstance(value, dict) else {"value": value}
    def encode(data: dict) -> str:
        return json.dumps({"_untrustedStart": START, **{key: value for key, value in data.items() if key not in _RESERVED},
                           "_untrustedEnd": END}, ensure_ascii=False, separators=(",", ":"))
    result = encode(payload)
    raw = result.encode("utf-8")
    if len(raw) <= max_bytes:
        return result
    size = (max_bytes - 400) // 8
    while True:
        result = encode({"projection": "bounded_tool_excerpt", "truncated": True,
                         "originalBytes": len(raw),
                         "notice": "Untrusted data excerpt; omitted data is not evidence or authorization.",
                         "head": clean_text(raw[:size].decode("utf-8", errors="ignore")),
                         "tail": clean_text(raw[-size:].decode("utf-8", errors="ignore")) if size else ""})
        if len(result.encode("utf-8")) <= max_bytes:
            return result
        size //= 2
