"""Shared text cleaning for terminal and model display boundaries.

The rules are deliberately data-agnostic: escape sequences and Unicode
format/control characters cannot reach a terminal or a model envelope, and the
reserved untrusted-boundary markers are replaced with visible text.
"""
from __future__ import annotations

import re
import unicodedata

UNTRUSTED_START = "[[UNTRUSTED_REMOTE_OUTPUT]]"
UNTRUSTED_END = "[[/UNTRUSTED_REMOTE_OUTPUT]]"

_ESCAPES = re.compile(
    r"(?:\x1b\][^\x07\x1b]*(?:\x07|\x1b\\|$))"
    r"|(?:\x1b[P^_X].*?(?:\x1b\\|$))"
    r"|(?:\x1b\[[0-?]*[ -/]*[@-~])|(?:\x1b[ -/]*[@-Z\\-_])",
    re.DOTALL,
)


def strip_unsafe_text(value: str) -> str:
    value = _ESCAPES.sub("", value)
    return "".join(
        character
        for character in value
        if character in "\n\t" or unicodedata.category(character) not in {"Cc", "Cf", "Cs"}
    )


def clean_text(value: str) -> str:
    return (
        strip_unsafe_text(value)
        .replace(UNTRUSTED_START, "[remote boundary text]")
        .replace(UNTRUSTED_END, "[/remote boundary text]")
    )
