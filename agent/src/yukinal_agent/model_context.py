"""Project large tool payloads without changing durable messages or private thought."""
from __future__ import annotations

import json
from typing import Any

from langchain_core.messages import ToolMessage


def model_messages(messages: list[Any], max_tool_bytes: int = 16 * 1024) -> list[Any]:
    result = []
    for message in messages:
        if not isinstance(message, ToolMessage):
            result.append(message)
            continue
        content = message.content
        raw = content if isinstance(content, str) else json.dumps(content, ensure_ascii=False, separators=(",", ":"))
        encoded = raw.encode("utf-8")
        if len(encoded) <= max_tool_bytes:
            result.append(message)
            continue
        half = (max_tool_bytes - 1024) // 4  # JSON escaping can double the text size.
        projection = {
            "projection": "bounded_tool_excerpt", "originalBytes": len(encoded), "truncated": True,
            "notice": "This is an excerpt, not the complete tool result. Full data remains in the local run record. Query a narrower range if omitted content is needed; do not infer it.",
            "head": encoded[:half].decode("utf-8", errors="ignore"),
            "tail": encoded[-half:].decode("utf-8", errors="ignore"),
        }
        projected = json.dumps(projection, ensure_ascii=False, separators=(",", ":"))
        while len(projected.encode("utf-8")) > max_tool_bytes:
            half //= 2
            projection.update(head=encoded[:half].decode("utf-8", errors="ignore"),
                              tail=encoded[-half:].decode("utf-8", errors="ignore") if half else "")
            projected = json.dumps(projection, ensure_ascii=False, separators=(",", ":"))
        result.append(message.model_copy(update={"content": projected}))
    return result
