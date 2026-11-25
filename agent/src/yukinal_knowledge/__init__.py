"""Local, deterministic knowledge indexing and evaluation.

This package deliberately has no third-party dependency. It is shared by the
desktop Agent's explicit knowledge store and the ``yukinal-knowledge`` CLI. The
vector representation is a local hashed feature vector, not a learned neural
embedding model.
"""

from .engine import (
    CHUNKER_VERSION,
    DEFAULT_CHUNK_BYTES,
    DEFAULT_OVERLAP_BYTES,
    EMBEDDING_VERSION,
    MAX_CHUNK_BYTES,
    MAX_DOCUMENT_CHUNKS,
    MAX_OVERLAP_BYTES,
    VECTOR_DIMENSIONS,
    ChunkLimitError,
    KnowledgeChunk,
    MarkdownChunkDetail,
    QueryTokenLimitError,
    SearchResult,
    chunk_markdown,
    chunk_markdown_details,
    content_hash,
    document_metrics,
    feature_vector,
    normalize_text,
    search_chunks,
    tokenize,
)
from .store import (
    CorpusDocument,
    IndexReport,
    KnowledgeError,
    KnowledgeInputError,
    KnowledgeStorageError,
    KnowledgeStore,
    SearchFilters,
    evaluate,
    read_questions,
    validate_database_path,
)
from .text_safety import UNTRUSTED_END, UNTRUSTED_START, clean_text, strip_unsafe_text

__all__ = [
    "CHUNKER_VERSION",
    "DEFAULT_CHUNK_BYTES",
    "DEFAULT_OVERLAP_BYTES",
    "EMBEDDING_VERSION",
    "MAX_CHUNK_BYTES",
    "MAX_DOCUMENT_CHUNKS",
    "MAX_OVERLAP_BYTES",
    "VECTOR_DIMENSIONS",
    "ChunkLimitError",
    "CorpusDocument",
    "IndexReport",
    "KnowledgeError",
    "KnowledgeInputError",
    "KnowledgeChunk",
    "KnowledgeStorageError",
    "KnowledgeStore",
    "MarkdownChunkDetail",
    "QueryTokenLimitError",
    "SearchResult",
    "SearchFilters",
    "UNTRUSTED_END",
    "UNTRUSTED_START",
    "chunk_markdown",
    "chunk_markdown_details",
    "clean_text",
    "content_hash",
    "document_metrics",
    "evaluate",
    "feature_vector",
    "normalize_text",
    "read_questions",
    "search_chunks",
    "strip_unsafe_text",
    "tokenize",
    "validate_database_path",
]
