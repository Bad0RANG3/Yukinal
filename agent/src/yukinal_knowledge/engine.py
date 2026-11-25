"""Deterministic chunking, lexical retrieval, vectors, and reranking.

The implementation is intentionally small and auditable:

* Markdown headings define section boundaries; fenced code blocks never become
  headings. Chunk limits are measured in UTF-8 bytes, not characters.
* Complete identifiers remain primary lexical facets, with safe component
  subtokens added for recall. Chinese text keeps single characters and bigrams.
* A curated operations alias table adds low-weight query expansion.
* Vectors are signed feature hashes, not a model-generated embedding.
* Hybrid ranking uses reciprocal-rank fusion, followed by exact facet bonuses.

No function in this module performs I/O or depends on the Agent runtime.
"""
from __future__ import annotations

import hashlib
import math
import re
import struct
import unicodedata
from collections import Counter
from dataclasses import dataclass
from typing import Any, Iterable, Sequence

CHUNKER_VERSION = "markdown-heading-bytes-v4"
EMBEDDING_VERSION = "signed-feature-hash-v2"
VECTOR_DIMENSIONS = 768
DEFAULT_CHUNK_BYTES = 1024
DEFAULT_OVERLAP_BYTES = 160
MAX_DOCUMENT_CHUNKS = 128
MAX_CHUNK_BYTES = 1200
MAX_OVERLAP_BYTES = 400

# Compatibility aliases for callers that imported the old character constants.
# Their units are now bytes; the old chunk_markdown keyword names are preserved.
DEFAULT_CHUNK_CHARS = DEFAULT_CHUNK_BYTES
DEFAULT_OVERLAP_CHARS = DEFAULT_OVERLAP_BYTES
MAX_CHUNK_CHARS = MAX_CHUNK_BYTES

_CJK_RANGE = "\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff"
_CJK_RUN = re.compile(rf"[{_CJK_RANGE}]+")
_WORD_BASE = rf"[^\W_{_CJK_RANGE}]"
_WORD_RUN = rf"{_WORD_BASE}+"
_TOKEN_RUN = re.compile(
    rf"(?:[{_CJK_RANGE}]+|/?{_WORD_RUN}(?:[._/:@+\-=~%]{_WORD_RUN})*)"
)
_TOKEN_CONNECTOR = re.compile(r"[._/:@+\-=~%]+")
_VERSION = re.compile(r"\d+(?:\.\d+)+")
_HEADING = re.compile(r"^[ \t]{0,3}(#{1,6})[ \t]+(.+?)(?:[ \t]+#+)?[ \t]*$")
_HEADING_ATTRIBUTES = re.compile(r"[ \t]*\{([^{}]*#[^{}]*)\}[ \t]*$")
_HEADING_ID = re.compile(r"(?:^|[ \t])#([^\s{}]+)")
_FENCE = re.compile(r"^[ \t]{0,3}(`{3,}|~{3,})(.*)$")
_SLUG_PART = re.compile(r"[^\W_]+")
_SURROGATE = re.compile("[\ud800-\udfff]")
_CONTROL = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]")
_QUERY_TOKEN_LIMIT = 64
_QUERY_SAFE = re.compile(r"^[\w./:@+\-=~%]+$", re.UNICODE)
_FTS_OPERATORS = frozenset(("and", "or", "not", "near"))
_RRF_K = 60.0

_STOPWORDS = frozenset(
    """
    a an and are as at be been before but by can could did do does for from had has
    have how i if in into is it its may might more most no not of on or our should
    so than that the their them then there these they this to was we were what when
    where which who why will with would you your
    """.split()
)
# One canonical token joins common Chinese/English operational phrases. It is
# low weight and never replaces either side's literal terms.
_ALIAS_GROUPS: tuple[tuple[str, ...], ...] = (
    ("oom", "out of memory", "memory exhausted", "内存不足", "内存耗尽", "137", "oomkilled", "oom killer"),
    ("502", "bad gateway", "网关错误", "upstream", "上游"),
    ("504", "gateway timeout", "网关超时", "upstream timeout", "上游超时"),
    ("413", "request entity too large", "payload too large", "请求体过大", "上传过大"),
    ("disk full", "no space left on device", "space left", "磁盘满", "磁盘已满", "空间不足"),
    ("inode", "no space left inode", "inode exhausted", "inode 用尽", "inode耗尽"),
    ("restart loop", "restarting", "crash loop", "反复重启", "重启循环", "不断重启"),
    ("permission denied", "eacces", "权限不足", "权限拒绝"),
    ("dns", "name resolution", "resolve", "域名解析", "解析失败", "nxdomain"),
    ("tcp retransmission", "retransmit", "retrans", "tcp 重传", "网络重传"),
    ("time sync", "ntp", "chrony", "system clock", "时间同步", "时钟偏移"),
    ("tls certificate", "certificate expired", "x509", "证书过期", "tls 证书"),
    ("journald", "journal", "systemd journal", "日志服务"),
    ("overlay", "overlay2", "docker storage", "容器存储"),
    ("docker logs", "container logs", "容器日志"),
    ("systemd failed", "unit failed", "failed state", "服务失败", "启动失败"),
)

_ALIAS_LOOKUP: dict[str, tuple[str, float]] = {}


@dataclass(frozen=True)
class KnowledgeChunk:
    doc_id: str
    title: str
    scope: str
    server_id: str
    ordinal: int
    heading_path: tuple[str, ...]
    text: str
    tokens: tuple[str, ...]
    vector: bytes
    updated_at: str = ""
    metadata: dict[str, Any] | None = None

    @property
    def chunk_id(self) -> str:
        return f"{self.doc_id}#chunk-{self.ordinal:04d}"

    @property
    def section(self) -> str:
        headings = [
            display
            for heading in self.heading_path
            if (display := _display_heading(heading))
        ]
        return " › ".join(headings) if headings else self.title

    @property
    def index_text(self) -> str:
        return " ".join((self.title, *self.heading_path, self.text))


@dataclass(frozen=True)
class SearchResult:
    chunk: KnowledgeChunk
    score: float
    lexical_score: float
    vector_score: float
    coverage: float
    mode: str

    def as_dict(self, *, snippet_chars: int = 360) -> dict[str, Any]:
        text = self.chunk.text.strip()
        if len(text) > snippet_chars:
            text = text[:snippet_chars].rstrip() + "…"
        metadata = dict(self.chunk.metadata or {})
        return {
            "chunkId": self.chunk.chunk_id,
            "documentId": self.chunk.doc_id,
            "title": self.chunk.title,
            "section": self.chunk.section,
            "ordinal": self.chunk.ordinal,
            "updatedAt": self.chunk.updated_at,
            "text": text,
            "score": round(self.score, 6),
            "lexicalScore": round(self.lexical_score, 6),
            "vectorScore": round(self.vector_score, 6),
            "coverage": round(self.coverage, 6),
            "mode": self.mode,
            "trust": "untrusted-reference",
            "metadata": metadata,
        }


@dataclass(frozen=True)
class MarkdownChunkDetail:
    """A normalized-source chunk with stable section and source offsets."""

    heading_path: tuple[str, ...]
    text: str
    section_id: str
    start_offset: int
    end_offset: int
    byte_count: int


class ChunkLimitError(ValueError):
    """Raised instead of silently truncating an oversized Markdown document."""


class QueryTokenLimitError(ValueError):
    """Raised when a query exceeds the stable searchable-token limit."""

    def __init__(self, count: int, limit: int) -> None:
        self.count = count
        self.limit = limit
        super().__init__(f"query has {count} unique searchable tokens; limit is {limit}")


@dataclass(frozen=True)
class _Section:
    heading_path: tuple[str, ...]
    section_id: str
    body_start: int
    body_end: int


def normalize_text(value: str) -> str:
    value = unicodedata.normalize("NFKC", value).replace("\r\n", "\n").replace("\r", "\n")
    value = _CONTROL.sub("", value)
    value = _SURROGATE.sub("\ufffd", value)
    return value.replace("\x00", "")


for _group_index, _group in enumerate(_ALIAS_GROUPS):
    _canonical = f"syn_ops{_group_index:02d}"
    for _phrase in _group:
        _normalized_phrase = " ".join(normalize_text(_phrase).casefold().split())
        _ALIAS_LOOKUP[_normalized_phrase] = (_canonical, 0.45)


def content_hash(value: str) -> str:
    return hashlib.sha256(normalize_text(value).encode("utf-8")).hexdigest()


def _token_text(value: str) -> str:
    text = normalize_text(value).casefold()
    # Combining marks are folded away for matching only. Latin text is already
    # composed by NFKC; this also keeps Arabic and Hebrew vocalized text stable.
    return "".join(
        character for character in text
        if not unicodedata.category(character).startswith("M")
    )


def _raw_tokens(value: str) -> list[str]:
    text = _token_text(value)
    tokens: list[str] = []
    for match in _TOKEN_RUN.finditer(text):
        surface = match.group(0)
        if _CJK_RUN.fullmatch(surface):
            if len(surface) == 1:
                tokens.append(surface)
                continue
            tokens.extend(surface)
            tokens.extend(
                surface[index:index + 2]
                for index in range(len(surface) - 1)
            )
            if len(surface) <= 8:
                tokens.append(surface)
            continue
        if surface not in _STOPWORDS:
            tokens.append(surface)
        tokens.extend(_facet_subtokens(surface))
    return tokens


def _facet_subtokens(facet: str) -> list[str]:
    # Versions, IP addresses and numeric identifiers stay atomic. Identifiers
    # with meaningful word/path components get lower-noise secondary facets.
    if _VERSION.fullmatch(facet) or re.search(r"[a-z]", facet) is None:
        return []
    parts = [
        part for part in _TOKEN_CONNECTOR.split(facet)
        if len(part) > 1 and part != facet and part not in _STOPWORDS
    ]
    return parts if len(parts) > 1 else []


def _expand_tokens(tokens: Iterable[str], surface: str) -> list[str]:
    expanded = list(tokens)
    normalized_surface = " ".join(normalize_text(surface).casefold().split())
    for phrase, (canonical, _) in _ALIAS_LOOKUP.items():
        if _contains_phrase(normalized_surface, phrase):
            expanded.append(canonical)
    return expanded


def _contains_phrase(text: str, phrase: str) -> bool:
    if not phrase:
        return False
    if re.fullmatch(r"[a-z0-9_.:+-]+", phrase):
        return re.search(rf"(?<![a-z0-9_.:+-]){re.escape(phrase)}(?![a-z0-9_.:+-])", text) is not None
    return phrase in text


def tokenize(value: str) -> tuple[str, ...]:
    """Return document/query features including low-weight alias tokens."""
    tokens = _raw_tokens(value)
    return tuple(_expand_tokens(tokens, value))


def query_token_weights(value: str) -> dict[str, float]:
    """Return safe, unique query facets in stable input order.

    Queries above :data:`_QUERY_TOKEN_LIMIT` raise instead of silently
    ignoring trailing terms in lexical, vector, and hybrid retrieval.
    """
    weighted: dict[str, float] = {}
    for token in tokenize(value):
        if not token or _QUERY_SAFE.fullmatch(token) is None or token in _FTS_OPERATORS:
            continue
        weight = 0.45 if token.startswith("syn_ops") else 1.0
        if token in weighted:
            weighted[token] = max(weighted[token], weight)
            continue
        weighted[token] = weight
    if len(weighted) > _QUERY_TOKEN_LIMIT:
        raise QueryTokenLimitError(len(weighted), _QUERY_TOKEN_LIMIT)
    return weighted


def _trimmed_span(text: str, start: int, end: int) -> tuple[int, int] | None:
    while start < end and text[start].isspace():
        start += 1
    while end > start and text[end - 1].isspace():
        end -= 1
    return (start, end) if start < end else None


def _byte_length(text: str, start: int = 0, end: int | None = None) -> int:
    return len(text[start:end].encode("utf-8"))


def _prefix_end(text: str, start: int, end: int, byte_limit: int) -> int:
    used = 0
    cursor = start
    while cursor < end:
        size = len(text[cursor].encode("utf-8"))
        if used + size > byte_limit:
            break
        used += size
        cursor += 1
    return cursor


def _suffix_start(text: str, start: int, end: int, byte_limit: int) -> int:
    used = 0
    cursor = end
    while cursor > start:
        size = len(text[cursor - 1].encode("utf-8"))
        if used + size > byte_limit:
            break
        used += size
        cursor -= 1
    return cursor


def _preferred_split_end(text: str, start: int, end: int, hard_end: int) -> int:
    # Only prefer a natural boundary in the latter half of the byte window.
    # Otherwise a single early newline would create tiny, inefficient chunks.
    minimum_end = start + max(1, (hard_end - start) // 2)
    preferred_end = start
    for marker in ("\n\n", ". ", "。", "！", "？", "；", "; ", "，", ", ", "\n", " "):
        position = text.rfind(marker, minimum_end, hard_end)
        if position >= 0:
            preferred_end = max(preferred_end, position + len(marker))
    return preferred_end if preferred_end > start else hard_end


def _split_byte_span(text: str, start: int, end: int, byte_limit: int) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    while start < end:
        hard_end = _prefix_end(text, start, end, byte_limit)
        if hard_end <= start:
            raise ValueError("chunk byte limit is too small for one character")
        split_end = end if hard_end == end else _preferred_split_end(text, start, end, hard_end)
        if split_end <= start:
            split_end = hard_end
        spans.append((start, split_end))
        start = split_end
    return spans


def _overlap_start(text: str, start: int, previous_end: int, byte_limit: int) -> int:
    if byte_limit <= 0 or previous_end <= start:
        return previous_end
    candidate = _suffix_start(text, start, previous_end, byte_limit)
    boundaries: list[int] = []
    for marker in ("\n", "。", "！", "？", ". ", "；", ";"):
        position = text.find(marker, candidate, previous_end)
        if position >= 0:
            boundaries.append(position + len(marker))
    if boundaries:
        candidate = min(boundaries)
    while candidate < previous_end and text[candidate].isspace():
        candidate += 1
    return candidate


def _display_heading(heading: str) -> str:
    """Return the human-facing heading text without a trailing anchor block."""
    return _HEADING_ATTRIBUTES.sub("", heading).strip()


def _heading_explicit_id(heading: str) -> str | None:
    match = _HEADING_ATTRIBUTES.search(normalize_text(heading))
    if match is None:
        return None
    identifiers = _HEADING_ID.findall(match.group(1))
    return identifiers[-1] if identifiers else None


def _slug_heading(heading: str) -> str:
    normalized = normalize_text(_display_heading(heading)).casefold()
    slug = "-".join(_SLUG_PART.findall(normalized)).strip("-")
    slug = re.sub(r"-+", "-", slug)
    if not slug:
        digest = hashlib.sha256(normalized.encode("utf-8")).hexdigest()[:8]
        slug = f"section-{digest}"
    if len(slug) > 64:
        digest = hashlib.sha256(normalized.encode("utf-8")).hexdigest()[:8]
        slug = f"{slug[:55].rstrip('-')}-{digest}"
    return slug


def _section_id(heading_path: tuple[str, ...]) -> str:
    if not heading_path:
        return "document"
    parts: list[str] = []
    last = len(heading_path) - 1
    for index, heading in enumerate(heading_path):
        explicit = _heading_explicit_id(heading)
        if explicit is not None:
            if index == last:
                return explicit
            parts.append(explicit)
        else:
            parts.append(_slug_heading(heading))
    return "/".join(parts)


def _unique_section_id(base: str, used: set[str]) -> str:
    candidate = base
    suffix = 2
    while candidate in used:
        candidate = f"{base}-{suffix}"
        suffix += 1
    used.add(candidate)
    return candidate


def _assign_section_ids(heading_paths: Sequence[tuple[str, ...]]) -> list[str]:
    identifiers = [""] * len(heading_paths)
    explicit = [
        bool(path and _heading_explicit_id(path[-1]) is not None)
        for path in heading_paths
    ]
    used: set[str] = set()
    # Explicit anchors are allocated first so a later explicit ID wins over an
    # earlier automatic slug with the same text. Document order remains the
    # stable tie-breaker among duplicate explicit IDs.
    for index, path in enumerate(heading_paths):
        if explicit[index]:
            identifiers[index] = _unique_section_id(_section_id(path), used)
    for index, path in enumerate(heading_paths):
        if not explicit[index]:
            identifiers[index] = _unique_section_id(_section_id(path), used)
    return identifiers


def _sections(markdown: str) -> list[_Section]:
    text = normalize_text(markdown)
    raw_sections: list[tuple[tuple[str, ...], int, int]] = []
    # A deterministic line scan is easier to audit than trying to interpret
    # heading-like lines inside code fences with a single regex.
    offset = 0
    title_stack: list[tuple[int, str]] = []
    section_body_start = 0
    current_path: tuple[str, ...] = ()
    fence_char = ""
    fence_length = 0
    in_fence = False
    for raw_line in text.splitlines(keepends=True):
        line = raw_line.rstrip("\n")
        fence_match = _FENCE.match(line)
        if in_fence:
            if (
                fence_match is not None
                and fence_match.group(1)[0] == fence_char
                and len(fence_match.group(1)) >= fence_length
                and not fence_match.group(2).strip()
            ):
                in_fence = False
                fence_char = ""
                fence_length = 0
        elif fence_match is not None:
            in_fence = True
            fence_char = fence_match.group(1)[0]
            fence_length = len(fence_match.group(1))
        else:
            match = _HEADING.match(line)
            if match is not None:
                body = _trimmed_span(text, section_body_start, offset)
                if body is not None:
                    raw_sections.append((current_path, *body))
                level = len(match.group(1))
                label = match.group(2).strip()
                while title_stack and title_stack[-1][0] >= level:
                    title_stack.pop()
                title_stack.append((level, label))
                current_path = tuple(item[1] for item in title_stack)
                section_body_start = offset + len(raw_line)
        offset += len(raw_line)
    body = _trimmed_span(text, section_body_start, len(text))
    if body is not None:
        raw_sections.append((current_path, *body))
    identifiers = _assign_section_ids([path for path, _, _ in raw_sections])
    return [
        _Section(path, identifiers[index], start, end)
        for index, (path, start, end) in enumerate(raw_sections)
    ]


def chunk_markdown_details(
    markdown: str,
    *,
    target_bytes: int = DEFAULT_CHUNK_BYTES,
    overlap_bytes: int = DEFAULT_OVERLAP_BYTES,
    max_chunks: int = MAX_DOCUMENT_CHUNKS,
) -> list[MarkdownChunkDetail]:
    """Split normalized Markdown without losing content or exceeding byte caps.

    Offsets refer to ``normalize_text(markdown)``. Fenced code is scanned only
    for fence boundaries, so heading-like code lines do not alter sections.
    """
    target = max(64, min(MAX_CHUNK_BYTES, int(target_bytes)))
    overlap = max(0, min(MAX_OVERLAP_BYTES, target // 2, int(overlap_bytes)))
    if int(max_chunks) < 1:
        raise ValueError("max_chunks must be positive")
    hard_chunk_limit = min(int(max_chunks), MAX_DOCUMENT_CHUNKS)
    text = normalize_text(markdown)
    details: list[MarkdownChunkDetail] = []
    for section in _sections(text):
        length = _byte_length(text, section.body_start, section.body_end)
        if length <= target:
            core_spans = [(section.body_start, section.body_end)]
        else:
            core_limit = max(1, target - overlap)
            core_spans = _split_byte_span(text, section.body_start, section.body_end, core_limit)
        previous_end: int | None = None
        for raw_start, raw_end in core_spans:
            core_span = _trimmed_span(text, raw_start, raw_end)
            if core_span is None:
                continue
            core_start, core_end = core_span
            core_bytes = _byte_length(text, core_start, core_end)
            start = core_start
            if previous_end is not None and overlap > 0:
                gap_bytes = _byte_length(text, previous_end, core_start)
                budget = min(
                    overlap,
                    target - core_bytes,
                    MAX_CHUNK_BYTES - core_bytes,
                    target - core_bytes - gap_bytes,
                )
                if budget > 0:
                    start = _overlap_start(text, section.body_start, previous_end, budget)
            chunk_bytes = _byte_length(text, start, core_end)
            if chunk_bytes > target or chunk_bytes > MAX_CHUNK_BYTES:
                start = core_start
                chunk_bytes = _byte_length(text, start, core_end)
            chunk_text = text[start:core_end]
            details.append(
                MarkdownChunkDetail(
                    heading_path=section.heading_path,
                    text=chunk_text,
                    section_id=section.section_id,
                    start_offset=start,
                    end_offset=core_end,
                    byte_count=chunk_bytes,
                )
            )
            if len(details) > hard_chunk_limit:
                raise ChunkLimitError(
                    f"markdown requires more than {hard_chunk_limit} chunks; refusing to truncate"
                )
            previous_end = core_end
    return details


def chunk_markdown(
    markdown: str,
    *,
    target_chars: int = DEFAULT_CHUNK_CHARS,
    overlap_chars: int = DEFAULT_OVERLAP_CHARS,
    max_chunks: int = MAX_DOCUMENT_CHUNKS,
) -> list[tuple[tuple[str, ...], str]]:
    """Legacy tuple view of :func:`chunk_markdown_details`.

    ``target_chars`` and ``overlap_chars`` retain their historic names but now
    bound UTF-8 bytes. Oversized documents raise instead of being truncated.
    """
    details = chunk_markdown_details(
        markdown,
        target_bytes=target_chars,
        overlap_bytes=overlap_chars,
        max_chunks=max_chunks,
    )
    return [(detail.heading_path, detail.text) for detail in details]


def _hash_feature(token: str) -> tuple[int, float]:
    digest = hashlib.blake2b(token.encode("utf-8"), digest_size=8).digest()
    value = int.from_bytes(digest, "big")
    index = (value >> 1) % VECTOR_DIMENSIONS
    sign = 1.0 if value & 1 else -1.0
    return index, sign


def feature_vector(value: str) -> bytes:
    """Encode text as a normalized signed feature-hash vector.

    This is a deterministic local similarity feature, not a learned semantic
    embedding. It makes vector/hybrid evaluation reproducible without network
    access or a model download.
    """
    tokens = _raw_tokens(value)
    tokens = _expand_tokens(tokens, value)
    counts = Counter(tokens)
    if not counts:
        return struct.pack(f"<{VECTOR_DIMENSIONS}f", *([0.0] * VECTOR_DIMENSIONS))
    values = [0.0] * VECTOR_DIMENSIONS
    for token, count in counts.items():
        weight = (0.45 if token.startswith("syn_ops") else 1.0) * (1.0 + math.log(count))
        index, sign = _hash_feature(f"tok:{token}")
        values[index] += sign * weight
    raw_tokens = [token for token in _raw_tokens(value) if not token.startswith("syn_ops")]
    for left, right in zip(raw_tokens, raw_tokens[1:]):
        index, sign = _hash_feature(f"bi:{left}\x1f{right}")
        values[index] += sign * 0.35
    norm = math.sqrt(sum(number * number for number in values))
    if norm:
        values = [number / norm for number in values]
    return struct.pack(f"<{VECTOR_DIMENSIONS}f", *values)


def decode_vector(value: bytes) -> tuple[float, ...]:
    if len(value) != VECTOR_DIMENSIONS * 4:
        raise ValueError("knowledge vector has an unsupported dimension")
    return struct.unpack(f"<{VECTOR_DIMENSIONS}f", value)


def cosine(left: Sequence[float], right: Sequence[float]) -> float:
    if len(left) != len(right):
        return 0.0
    return max(0.0, min(1.0, sum(a * b for a, b in zip(left, right))))


def _bm25(
    records: Sequence[KnowledgeChunk],
    frequencies: Sequence[Counter[str]],
    query_weights: dict[str, float],
) -> list[float]:
    if not records:
        return []
    lengths = [max(1, len(record.tokens)) for record in records]
    average = sum(lengths) / len(lengths)
    document_frequency = Counter()
    for frequency in frequencies:
        document_frequency.update(set(frequency))
    count = len(records)
    scores: list[float] = []
    for frequency, length in zip(frequencies, lengths):
        score = 0.0
        for token, query_weight in query_weights.items():
            term_frequency = frequency.get(token, 0)
            if not term_frequency:
                continue
            doc_frequency = document_frequency.get(token, 0)
            inverse = math.log(1.0 + (count - doc_frequency + 0.5) / (doc_frequency + 0.5))
            denominator = term_frequency + 1.2 * (1.0 - 0.75 + 0.75 * length / average)
            score += query_weight * inverse * (term_frequency * 2.2) / denominator
        scores.append(score)
    return scores


def _rank_map(
    values: Sequence[float],
    records: Sequence[KnowledgeChunk],
    eligible: set[int],
) -> dict[int, int]:
    ordered = sorted(
        eligible,
        key=lambda index: (
            -values[index],
            records[index].doc_id,
            records[index].ordinal,
            records[index].chunk_id,
        ),
    )
    return {index: rank for rank, index in enumerate(ordered, 1)}


def _weighted_coverage(tokens: set[str], query_weights: dict[str, float]) -> float:
    total = sum(query_weights.values())
    if total <= 0:
        return 0.0
    present = sum(weight for token, weight in query_weights.items() if token in tokens)
    return present / total


def search_chunks(
    query: str,
    records: Sequence[KnowledgeChunk],
    *,
    mode: str = "hybrid",
    limit: int = 8,
) -> list[SearchResult]:
    """Rank eligible chunks with deterministic reciprocal-rank fusion.

    BM25 and signed-feature-vector scores only determine their respective
    ranks. Exact token/facet coverage adds a bounded post-RRF rerank bonus.
    Eligibility and ACL filtering still belong to the caller.
    """
    if mode not in {"lexical", "vector", "hybrid"}:
        raise ValueError("mode must be lexical, vector, or hybrid")
    if not query.strip():
        return []
    query_weights = query_token_weights(query)
    if limit <= 0 or not records or not query_weights:
        return []
    frequencies = [Counter(record.tokens) for record in records]
    lexical = _bm25(records, frequencies, query_weights)
    query_vector = decode_vector(feature_vector(query))
    vector = [cosine(query_vector, decode_vector(record.vector)) for record in records]
    total_weight = sum(query_weights.values())
    coverage = [
        sum(weight for token, weight in query_weights.items() if frequencies[index].get(token))
        / total_weight if total_weight else 0.0
        for index in range(len(records))
    ]
    lexical_eligible = {index for index, value in enumerate(lexical) if value > 0}
    vector_eligible = {
        index for index, value in enumerate(vector)
        if value > 0 and coverage[index] > 0
    }
    lexical_ranks = _rank_map(lexical, records, lexical_eligible)
    vector_ranks = _rank_map(vector, records, vector_eligible)
    if mode == "lexical":
        lexical_weight, vector_weight = 1.0, 0.0
        candidates = lexical_eligible
    elif mode == "vector":
        lexical_weight, vector_weight = 0.0, 1.0
        candidates = vector_eligible
    else:
        lexical_weight, vector_weight = 1.0, 1.0
        candidates = {
            index for index in range(len(records))
            if coverage[index] > 0 and (index in lexical_eligible or index in vector_eligible)
        }

    normalized_query = " ".join(normalize_text(query).casefold().split())
    results: list[SearchResult] = []
    for index in candidates:
        record = records[index]
        surface = " ".join(normalize_text(record.index_text).casefold().split())
        heading = " ".join(normalize_text(record.section).casefold().split())
        title = normalize_text(record.title).casefold()
        heading_coverage = _weighted_coverage(set(_raw_tokens(record.section)), query_weights)
        title_coverage = _weighted_coverage(set(_raw_tokens(record.title)), query_weights)
        phrase_match = 0.0
        if len(normalized_query) >= 3:
            if normalized_query in surface:
                phrase_match = 1.0
            if normalized_query in heading or normalized_query in title:
                phrase_match = 1.0
        fusion = 0.0
        if index in lexical_ranks and lexical_weight:
            fusion += lexical_weight / (_RRF_K + lexical_ranks[index])
        if index in vector_ranks and vector_weight:
            fusion += vector_weight / (_RRF_K + vector_ranks[index])
        exact_bonus = min(
            1.0,
            0.50 * coverage[index]
            + 0.20 * heading_coverage
            + 0.10 * title_coverage
            + 0.20 * phrase_match,
        ) / (_RRF_K + 1.0)
        results.append(
            SearchResult(
                record,
                min(1.0, fusion + exact_bonus),
                lexical[index],
                vector[index],
                coverage[index],
                mode,
            )
        )
    results.sort(
        key=lambda item: (
            -item.score,
            -item.coverage,
            -item.lexical_score,
            -item.vector_score,
            item.chunk.doc_id,
            item.chunk.ordinal,
            item.chunk.chunk_id,
        )
    )
    return results[:limit]


def document_metrics(
    results: Sequence[SearchResult],
    relevant_documents: Sequence[str],
    *,
    k: int = 5,
) -> dict[str, float]:
    """Compute binary document-level Recall, Precision, MRR and nDCG."""
    relevant = set(relevant_documents)
    ranked: list[str] = []
    for result in results:
        if result.chunk.doc_id not in ranked:
            ranked.append(result.chunk.doc_id)
        if len(ranked) >= k:
            break
    top = ranked[:k]
    hits = sum(1 for doc_id in top if doc_id in relevant)
    recall = hits / len(relevant) if relevant else (1.0 if not top else 0.0)
    precision = hits / k if k else 0.0
    reciprocal = next((1.0 / (index + 1) for index, doc_id in enumerate(top) if doc_id in relevant), 0.0)
    dcg = sum(1.0 / math.log2(index + 2) for index, doc_id in enumerate(top) if doc_id in relevant)
    ideal = sum(1.0 / math.log2(index + 2) for index in range(min(len(relevant), k)))
    ndcg = dcg / ideal if ideal else (1.0 if not relevant else 0.0)
    return {
        "recall": recall,
        "precision": precision,
        "mrr": reciprocal,
        "ndcg": ndcg,
        "hits": float(hits),
        "returned": float(len(top)),
    }


