"""SQLite storage and ingestion for standalone knowledge collections."""
from __future__ import annotations

import hashlib
import json
import math
import os
import re
import sqlite3
import tomllib
from dataclasses import dataclass
from datetime import UTC, date, datetime, time
from pathlib import Path
from typing import Any, Iterable, Sequence

from .engine import (
    CHUNKER_VERSION,
    EMBEDDING_VERSION,
    MAX_DOCUMENT_CHUNKS,
    KnowledgeChunk,
    QueryTokenLimitError,
    SearchResult,
    chunk_markdown,
    content_hash,
    decode_vector,
    feature_vector,
    normalize_text,
    query_token_weights,
    search_chunks,
    tokenize,
)

APPLICATION_ID = 0x594B4E4C  # YKNL
SCHEMA_VERSION = 1
# A document must fit the per-document chunk budget at the default chunk size:
# 96 KiB / (1024 - 160) bytes per core span leaves room for section boundaries.
MAX_DOCUMENT_BYTES = 96 * 1024
TEXT_SUFFIXES = frozenset({".md", ".markdown", ".txt"})
_RESERVED_DB_NAMES = frozenset({"yukinal.db", "agent-state.sqlite", "conversation.sqlite", "conversations.sqlite"})
_SECTION_ANCHOR = re.compile(r"\{#([^{}\r\n]+)\}")
_CONTENT_HASH = re.compile(r"[0-9a-f]{64}\Z")
_MAX_DOCUMENT_ID_CHARS = 512
_MAX_INDEXED_TEXT_CHARS = MAX_DOCUMENT_BYTES
_MAX_TERM_COUNT = 4096
_SCHEMA_STATEMENTS = (
    """
    CREATE TABLE knowledge_meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    )
    """,
    """
    CREATE TABLE knowledge_documents (
        doc_id TEXT PRIMARY KEY,
        title TEXT NOT NULL,
        source TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        environment TEXT NOT NULL,
        server_tags TEXT NOT NULL,
        acl TEXT NOT NULL,
        metadata_json TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        indexed_at TEXT NOT NULL
    )
    """,
    """
    CREATE TABLE knowledge_chunks (
        chunk_id TEXT PRIMARY KEY,
        doc_id TEXT NOT NULL REFERENCES knowledge_documents(doc_id) ON DELETE CASCADE,
        ordinal INTEGER NOT NULL,
        heading_path TEXT NOT NULL,
        text TEXT NOT NULL,
        terms TEXT NOT NULL,
        vector BLOB NOT NULL,
        token_count INTEGER NOT NULL,
        UNIQUE(doc_id, ordinal)
    )
    """,
    """
    CREATE INDEX knowledge_chunks_document
        ON knowledge_chunks(doc_id, ordinal)
    """,
)


def _normalized_schema_sql(statement: str) -> str:
    return " ".join(statement.split()).casefold()


_EXPECTED_SCHEMA_SQL = frozenset(
    _normalized_schema_sql(statement) for statement in _SCHEMA_STATEMENTS
)


class KnowledgeError(Exception):
    """Base class for stable CLI/user-facing knowledge errors."""

    code = "knowledge_error"
    exit_code = 1


class KnowledgeInputError(KnowledgeError):
    code = "invalid_input"
    exit_code = 2


class KnowledgeStorageError(KnowledgeError):
    code = "storage_unavailable"
    exit_code = 3


@dataclass(frozen=True)
class CorpusDocument:
    doc_id: str
    title: str
    source: str
    updated_at: str
    environment: str
    server_tags: tuple[str, ...]
    acl: tuple[str, ...]
    metadata: dict[str, Any]
    markdown: str


@dataclass(frozen=True)
class IndexReport:
    scanned: int
    indexed: int
    unchanged: int
    skipped: int
    chunks: int
    errors: tuple[str, ...]
    removed: int = 0


@dataclass(frozen=True)
class SearchFilters:
    principal: str = ""
    environment: str = ""
    server_id: str = ""
    tags: tuple[str, ...] = ()
    global_only: bool = False


def _json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def _jsonable(value: Any) -> Any:
    if value is None or isinstance(value, (str, bool, int)):
        return value
    if isinstance(value, float):
        if not math.isfinite(value):
            raise KnowledgeInputError("front matter contains a non-finite number")
        return value
    if isinstance(value, (datetime, date, time)):
        return value.isoformat()
    if isinstance(value, (list, tuple)):
        return [_jsonable(item) for item in value]
    if isinstance(value, dict):
        if not all(isinstance(key, str) for key in value):
            raise KnowledgeInputError("front matter keys must be strings")
        return {key: _jsonable(item) for key, item in value.items()}
    raise KnowledgeInputError(f"front matter contains unsupported {type(value).__name__} data")


def _storage_value_error(label: str) -> KnowledgeStorageError:
    return KnowledgeStorageError(f"knowledge {label} metadata is unreadable")


def _storage_text(
    value: Any,
    label: str,
    *,
    maximum: int,
    minimum: int = 1,
    stripped: bool = True,
    line_breaks: bool = False,
) -> str:
    if not isinstance(value, str):
        raise _storage_value_error(label)
    if not minimum <= len(value) <= maximum or (stripped and value != value.strip()):
        raise _storage_value_error(label)
    allowed_line_breaks = "\n\t" if line_breaks else ""
    if any(
        ord(character) < 32 or ord(character) == 127
        for character in value
        if character not in allowed_line_breaks
    ):
        raise _storage_value_error(label)
    return value


def _storage_integer(
    value: Any,
    label: str,
    *,
    minimum: int,
    maximum: int,
) -> int:
    if (
        not isinstance(value, int)
        or isinstance(value, bool)
        or not minimum <= value <= maximum
    ):
        raise _storage_value_error(label)
    return value


def _storage_document_id(value: Any) -> str:
    return _storage_text(
        value,
        "document id",
        maximum=_MAX_DOCUMENT_ID_CHARS,
    )


def _storage_json(value: Any, label: str) -> Any:
    if not isinstance(value, str):
        raise _storage_value_error(label)
    try:
        return json.loads(value)
    except (TypeError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise _storage_value_error(label) from error


def _storage_string_list(
    value: Any,
    label: str,
    *,
    allow_empty: bool = True,
) -> tuple[str, ...]:
    decoded = _storage_json(value, label)
    if (
        not isinstance(decoded, list)
        or len(decoded) > 32
        or not all(
            isinstance(item, str)
            and 0 < len(item) <= 120
            and item == item.strip()
            and not any(ord(character) < 32 or ord(character) == 127 for character in item)
            for item in decoded
        )
        or (not decoded and not allow_empty)
    ):
        raise _storage_value_error(label)
    return tuple(decoded)


def _storage_metadata(value: Any) -> dict[str, Any]:
    decoded = _storage_json(value, "document")
    if not isinstance(decoded, dict):
        raise _storage_value_error("document")
    tags = decoded.get("tags")
    if tags is not None and not isinstance(tags, str):
        if not isinstance(tags, list) or not all(isinstance(item, str) for item in tags):
            raise _storage_value_error("document")
    return decoded


def _storage_document_scalars(
    row: sqlite3.Row,
) -> tuple[str, str, str, str, str, str, str]:
    doc_id = _storage_document_id(row["document_doc_id"])
    title = _storage_text(row["title"], "document title", maximum=200)
    source = _storage_text(row["source"], "document source", maximum=200)
    updated_at = _storage_text(row["updated_at"], "document timestamp", maximum=64)
    environment = _storage_text(row["environment"], "document environment", maximum=80)
    if any(not part or part != part.strip() for part in environment.split(",")):
        raise _storage_value_error("document environment")
    content_hash_value = _storage_text(
        row["content_hash"],
        "document content hash",
        maximum=64,
        minimum=64,
        stripped=False,
    )
    if _CONTENT_HASH.fullmatch(content_hash_value) is None:
        raise _storage_value_error("document content hash")
    indexed_at = _storage_text(row["indexed_at"], "document index timestamp", maximum=64)
    return doc_id, title, source, updated_at, environment, content_hash_value, indexed_at


def _storage_document_fields(
    row: sqlite3.Row,
) -> tuple[tuple[str, ...], tuple[str, ...], dict[str, Any]]:
    acl = _storage_string_list(row["acl"], "document ACL", allow_empty=False)
    server_tags = _storage_string_list(row["server_tags"], "document server tags")
    metadata = _storage_metadata(row["metadata_json"])
    return acl, server_tags, metadata


def _storage_chunk_scalars(row: sqlite3.Row) -> tuple[str, str, int, str]:
    chunk_id = _storage_text(
        row["chunk_id"],
        "chunk id",
        maximum=_MAX_DOCUMENT_ID_CHARS + 16,
    )
    doc_id = _storage_document_id(row["doc_id"])
    ordinal = _storage_integer(
        row["ordinal"],
        "chunk ordinal",
        minimum=0,
        maximum=MAX_DOCUMENT_CHUNKS - 1,
    )
    text = _storage_text(
        row["text"],
        "chunk text",
        maximum=_MAX_INDEXED_TEXT_CHARS,
        line_breaks=True,
    )
    if chunk_id != f"{doc_id}#chunk-{ordinal:04d}":
        raise _storage_value_error("chunk id")
    return chunk_id, doc_id, ordinal, text


def _storage_heading_path(row: sqlite3.Row) -> tuple[str, ...]:
    heading = _storage_json(row["heading_path"], "chunk heading")
    if (
        not isinstance(heading, list)
        or len(heading) > 64
        or not all(
            isinstance(item, str)
            and len(item) <= 1000
            and not any(ord(character) < 32 or ord(character) == 127 for character in item)
            for item in heading
        )
    ):
        raise _storage_value_error("chunk heading")
    return tuple(heading)


def _storage_chunk_fields(
    row: sqlite3.Row,
) -> tuple[tuple[str, ...], tuple[str, ...], bytes]:
    _storage_chunk_scalars(row)
    heading = _storage_heading_path(row)
    terms = row["terms"]
    token_count = row["token_count"]
    if (
        not isinstance(terms, str)
        or len(terms) > _MAX_INDEXED_TEXT_CHARS
        or not isinstance(token_count, int)
        or isinstance(token_count, bool)
        or not 0 <= token_count <= _MAX_TERM_COUNT
        or token_count != len(terms.split())
    ):
        raise _storage_value_error("chunk terms")
    vector = row["vector"]
    if not isinstance(vector, bytes):
        raise _storage_value_error("chunk vector")
    try:
        values = decode_vector(vector)
    except (TypeError, ValueError) as error:
        raise _storage_value_error("chunk vector") from error
    if not all(math.isfinite(value) for value in values):
        raise _storage_value_error("chunk vector")
    return tuple(heading), tuple(terms.split()), vector


def validate_database_path(value: str | Path, *, must_exist: bool = False) -> Path:
    path = Path(value).expanduser()
    if not path.is_absolute():
        path = Path.cwd() / path
    if path.is_symlink():
        raise KnowledgeStorageError("knowledge database path must not be a symbolic link")
    path = path.resolve(strict=False)
    lowered_parts = {part.casefold() for part in path.parts}
    if path.name.casefold() in _RESERVED_DB_NAMES or "agent-state" in lowered_parts:
        raise KnowledgeStorageError(
            "refusing to use a Yukinal runtime database; choose a dedicated knowledge database path",
            # Raised with a message that identifies the safe boundary without echoing user data.
        )
    if must_exist and (not path.is_file() or path.is_symlink()):
        raise KnowledgeStorageError("knowledge database does not exist or is not a regular file")
    if path.exists() and (not path.is_file() or path.is_symlink()):
        raise KnowledgeStorageError("knowledge database path must be a regular file")
    return path


def _connect(path: Path, *, read_only: bool = False) -> sqlite3.Connection:
    mode = "ro" if read_only else "rw"
    uri = f"{path.resolve(strict=False).as_uri()}?mode={mode}"
    connection = sqlite3.connect(uri, uri=True)
    connection.row_factory = sqlite3.Row
    connection.execute("PRAGMA foreign_keys = ON")
    connection.execute("PRAGMA busy_timeout = 5000")
    if read_only:
        connection.execute("PRAGMA query_only = ON")
    return connection


def _same_file_identity(left: os.stat_result, right: os.stat_result) -> bool:
    return left.st_dev == right.st_dev and left.st_ino == right.st_ino


def _create_empty_database_file(path: Path) -> os.stat_result | None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    flags = os.O_CREAT | os.O_EXCL | os.O_RDWR
    if hasattr(os, "O_BINARY"):
        flags |= os.O_BINARY
    try:
        descriptor = os.open(path, flags, 0o600)
    except FileExistsError:
        return None
    try:
        identity = os.fstat(descriptor)
        os.close(descriptor)
    except OSError:
        try:
            path.unlink()
        except OSError:
            pass
        raise
    return identity


def _discard_created_database(path: Path, identity: os.stat_result) -> None:
    """Remove only the still-pristine file created by this call.

    A failed initialization may have raced with another writer. Never unlink a
    path that now contains schema objects, pages, or non-empty sidecars.
    """
    try:
        current = path.stat()
    except FileNotFoundError:
        current = None
    except OSError:
        return
    if current is None or path.is_symlink() or not _same_file_identity(identity, current):
        return
    for suffix in ("-wal", "-shm", "-journal"):
        try:
            if Path(f"{path}{suffix}").stat().st_size > 0:
                return
        except FileNotFoundError:
            continue
        except OSError:
            return
    try:
        probe = _connect(path, read_only=True)
    except (OSError, sqlite3.Error):
        return
    try:
        if (
            probe.execute("PRAGMA application_id").fetchone()[0] != 0
            or probe.execute("PRAGMA user_version").fetchone()[0] != 0
            or probe.execute("PRAGMA page_count").fetchone()[0] != 0
            or probe.execute("SELECT count(*) FROM sqlite_master").fetchone()[0] != 0
        ):
            return
    except sqlite3.Error:
        return
    finally:
        probe.close()
    try:
        if (
            path.is_symlink()
            or not path.is_file()
            or not _same_file_identity(identity, path.stat())
        ):
            return
    except OSError:
        return
    for suffix in ("", "-wal", "-shm", "-journal"):
        try:
            Path(f"{path}{suffix}").unlink()
        except FileNotFoundError:
            pass
        except OSError:
            pass


def _write_current_versions(connection: sqlite3.Connection) -> None:
    connection.executemany(
        "INSERT INTO knowledge_meta(key, value) VALUES (?, ?) "
        "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (
            ("schema_version", str(SCHEMA_VERSION)),
            ("chunker_version", CHUNKER_VERSION),
            ("embedding_version", EMBEDDING_VERSION),
        ),
    )
    connection.execute(f"PRAGMA user_version = {SCHEMA_VERSION}")


def _initialize(connection: sqlite3.Connection) -> None:
    try:
        # BEGIN IMMEDIATE materializes page 1 even for an otherwise empty file.
        # Record emptiness before taking the lock, then require that acquiring
        # the lock did not reveal pages from an earlier writer.
        if connection.execute("PRAGMA page_count").fetchone()[0] != 0:
            raise KnowledgeStorageError(
                "refusing to initialize an existing SQLite file; only an empty database created by this call may be claimed"
            )
        connection.execute("BEGIN IMMEDIATE")
        # Re-check while holding the write lock. Checking before BEGIN leaves a
        # window for another process to claim the freshly created path.
        application_id = connection.execute("PRAGMA application_id").fetchone()[0]
        user_version = connection.execute("PRAGMA user_version").fetchone()[0]
        page_count = connection.execute("PRAGMA page_count").fetchone()[0]
        object_count = connection.execute("SELECT count(*) FROM sqlite_master").fetchone()[0]
        if application_id != 0 or user_version != 0 or page_count > 1 or object_count != 0:
            raise KnowledgeStorageError(
                "refusing to initialize an existing SQLite file; only an empty database created by this call may be claimed"
            )
        for statement in _SCHEMA_STATEMENTS:
            connection.execute(statement)
        _write_current_versions(connection)
        # Ownership is written last so an interrupted initialization is never
        # mistaken for a complete Yukinal database.
        connection.execute(f"PRAGMA application_id = {APPLICATION_ID}")
        connection.commit()
    except Exception:
        connection.rollback()
        raise


def _check_schema(connection: sqlite3.Connection) -> None:
    stored = {
        _normalized_schema_sql(row["sql"])
        for row in connection.execute(
            "SELECT sql FROM sqlite_master "
            "WHERE sql IS NOT NULL "
            "AND name NOT LIKE 'sqlite_autoindex_%' "
            "AND name NOT LIKE 'sqlite_%'"
        )
    }
    if stored != _EXPECTED_SCHEMA_SQL:
        raise KnowledgeStorageError(
            "knowledge database schema is incompatible (table and index definitions do not match)"
        )


def _check_database(connection: sqlite3.Connection, *, allow_stale: bool = False) -> bool:
    application_id = connection.execute("PRAGMA application_id").fetchone()[0]
    if application_id != APPLICATION_ID:
        raise KnowledgeStorageError("not a Yukinal knowledge database")
    version = connection.execute("PRAGMA user_version").fetchone()[0]
    if version != SCHEMA_VERSION:
        raise KnowledgeStorageError(f"unsupported knowledge schema version {version}")
    _check_schema(connection)
    rows = connection.execute("SELECT key, value FROM knowledge_meta").fetchall()
    metadata = {row["key"]: row["value"] for row in rows}
    if metadata.get("schema_version") != str(SCHEMA_VERSION):
        raise KnowledgeStorageError("knowledge database metadata is incompatible (schema_version mismatch)")
    chunker_version = metadata.get("chunker_version")
    embedding_version = metadata.get("embedding_version")
    stale = chunker_version != CHUNKER_VERSION or embedding_version != EMBEDDING_VERSION
    if stale and not allow_stale:
        details = []
        if chunker_version != CHUNKER_VERSION:
            details.append(f"chunker {chunker_version or 'missing'} != {CHUNKER_VERSION}")
        if embedding_version != EMBEDDING_VERSION:
            details.append(f"embedding {embedding_version or 'missing'} != {EMBEDDING_VERSION}")
        raise KnowledgeStorageError(
            "knowledge index is stale (" + "; ".join(details) + "); run index --reset"
        )
    return stale


def _database_identity(path: Path) -> os.stat_result:
    try:
        if path.is_symlink():
            raise KnowledgeStorageError("knowledge database path must not be a symbolic link")
        identity = path.stat()
    except OSError as error:
        raise KnowledgeStorageError("knowledge database disappeared during validation") from error
    if not path.is_file():
        raise KnowledgeStorageError("knowledge database path must be a regular file")
    return identity


def _open_existing(
    path: Path, *, allow_stale: bool, read_only: bool = False,
) -> tuple[sqlite3.Connection, bool]:
    expected = _database_identity(path)
    probe = _connect(path, read_only=True)
    try:
        if not _same_file_identity(expected, _database_identity(path)):
            raise KnowledgeStorageError("knowledge database changed during validation")
        stale = _check_database(probe, allow_stale=allow_stale)
    finally:
        probe.close()

    connection = _connect(path, read_only=read_only)
    try:
        if not _same_file_identity(expected, _database_identity(path)):
            raise KnowledgeStorageError("knowledge database changed during validation")
        stale = _check_database(connection, allow_stale=allow_stale)
        if not _same_file_identity(expected, _database_identity(path)):
            raise KnowledgeStorageError("knowledge database changed during validation")
        return connection, stale
    except Exception:
        connection.close()
        raise


def _front_matter(text: str) -> tuple[dict[str, Any], str]:
    stripped = text.lstrip("\ufeff")
    if not stripped.startswith(("+++\n", "---\n", "+++\r\n", "---\r\n")):
        return {}, stripped
    marker = stripped[:3]
    lines = stripped.splitlines(keepends=True)
    closing = next((index for index, line in enumerate(lines[1:], 1) if line.rstrip() == marker), None)
    if closing is None:
        raise KnowledgeInputError("unterminated TOML front matter")
    raw = "".join(lines[1:closing])
    try:
        metadata = tomllib.loads(raw)
    except tomllib.TOMLDecodeError as error:
        raise KnowledgeInputError(f"invalid TOML front matter: {error}") from error
    return metadata, "".join(lines[closing + 1:])


def _string_list(value: Any, field: str) -> tuple[str, ...]:
    if value is None:
        return ()
    if isinstance(value, str):
        values = [item.strip() for item in value.split(",")]
    elif isinstance(value, list) and all(isinstance(item, str) for item in value):
        values = [item.strip() for item in value]
    else:
        raise KnowledgeInputError(f"front matter field {field!r} must be a string or string list")
    if any(not item for item in values):
        raise KnowledgeInputError(f"front matter field {field!r} must not contain empty values")
    if any(
        ord(character) < 32 or ord(character) == 127
        for item in values
        for character in item
    ):
        raise KnowledgeInputError(f"front matter field {field!r} contains control characters")
    result = tuple(dict.fromkeys(values))
    if len(result) > 32 or any(len(item) > 120 for item in result):
        raise KnowledgeInputError(f"front matter field {field!r} exceeds its bound")
    return result


def _required_string(metadata: dict[str, Any], field: str, *, maximum: int) -> str:
    if field not in metadata:
        raise KnowledgeInputError(f"front matter requires field {field!r}")
    value = metadata[field]
    if not isinstance(value, str):
        raise KnowledgeInputError(f"front matter field {field!r} must be a string")
    result = value.strip()
    if not result:
        raise KnowledgeInputError(f"front matter field {field!r} must not be empty")
    if len(result) > maximum or any(
        ord(character) < 32 or ord(character) == 127 for character in result
    ):
        raise KnowledgeInputError(f"front matter field {field!r} exceeds its bound")
    return result


def _required_string_list(metadata: dict[str, Any], field: str, *, allow_empty: bool = False) -> tuple[str, ...]:
    if field not in metadata:
        raise KnowledgeInputError(f"front matter requires field {field!r}")
    result = _string_list(metadata[field], field)
    if not result and not allow_empty:
        raise KnowledgeInputError(f"front matter field {field!r} must not be empty")
    return result


def _current_section_id(heading_path: Sequence[str]) -> str:
    if not heading_path:
        return ""
    anchors = _SECTION_ANCHOR.findall(heading_path[-1])
    return anchors[-1].strip() if anchors else ""


def load_document(path: Path, source_root: Path) -> CorpusDocument:
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise KnowledgeInputError(f"cannot read {path.name}: {error.__class__.__name__}") from error
    try:
        resolved.relative_to(source_root)
    except ValueError as error:
        raise KnowledgeInputError(f"source file escapes the corpus root: {path.name}") from error
    if path.is_symlink() or not resolved.is_file():
        raise KnowledgeInputError(f"source must be a regular file: {path.name}")
    size = resolved.stat().st_size
    if size <= 0 or size > MAX_DOCUMENT_BYTES:
        raise KnowledgeInputError(f"{path.name} must be between 1 and {MAX_DOCUMENT_BYTES} bytes")
    try:
        text = resolved.read_text(encoding="utf-8")
    except UnicodeDecodeError as error:
        raise KnowledgeInputError(f"{path.name} is not valid UTF-8") from error
    except OSError as error:
        raise KnowledgeInputError(f"cannot read {path.name}: {error.__class__.__name__}") from error
    metadata, markdown = _front_matter(text)
    metadata = _jsonable(metadata)
    try:
        metadata_json = _json(metadata)
    except (TypeError, ValueError) as error:
        raise KnowledgeInputError(f"{path.name} has front matter that cannot be serialized") from error
    if len(metadata_json.encode("utf-8")) > 16 * 1024:
        raise KnowledgeInputError(f"{path.name} has front matter that exceeds 16 KiB")
    relative = resolved.relative_to(source_root).as_posix()
    title = _required_string(metadata, "title", maximum=200)
    source = _required_string(metadata, "source", maximum=200)
    updated_at = _required_string(metadata, "updated_at", maximum=64)
    environment_values = _required_string_list(metadata, "environment")
    environment = ",".join(environment_values)
    if len(environment) > 80:
        raise KnowledgeInputError(f"{path.name} has an invalid environment")
    server_tags = _required_string_list(metadata, "server_tags", allow_empty=True)
    acl = _required_string_list(metadata, "acl")
    return CorpusDocument(
        doc_id=relative,
        title=title,
        source=source,
        updated_at=updated_at,
        environment=environment,
        server_tags=server_tags,
        acl=acl,
        metadata=metadata,
        markdown=markdown.strip(),
    )


def discover_documents(source: str | Path) -> tuple[Path, list[Path]]:
    root = Path(source).expanduser()
    if root.is_symlink():
        raise KnowledgeInputError("corpus source must not be a symbolic link")
    try:
        root = root.resolve(strict=True)
    except OSError as error:
        raise KnowledgeInputError("corpus directory does not exist") from error
    if not root.is_dir() or root.is_symlink():
        raise KnowledgeInputError("corpus source must be a regular directory")
    files: list[Path] = []
    for path in sorted(root.rglob("*")):
        if any(part.startswith(".") for part in path.relative_to(root).parts):
            continue
        if path.suffix.casefold() not in TEXT_SUFFIXES:
            continue
        if path.is_symlink():
            relative = path.relative_to(root).as_posix()
            raise KnowledgeInputError(f"corpus contains a symbolic link: {relative}")
        if not path.is_file():
            continue
        files.append(path)
    return root, files


def _load_corpus(source: str | Path) -> tuple[Path, list[Path], list[CorpusDocument]]:
    root, paths = discover_documents(source)
    if not paths:
        raise KnowledgeInputError("corpus contains no Markdown or text documents")
    errors: list[str] = []
    documents: list[CorpusDocument] = []
    for path in paths:
        try:
            documents.append(load_document(path, root))
        except KnowledgeInputError as error:
            errors.append(f"{path.relative_to(root).as_posix()}: {error}")
    if errors:
        raise KnowledgeInputError("; ".join(errors[:8]))
    return root, paths, documents


def preview_index(source: str | Path) -> IndexReport:
    _, paths, documents = _load_corpus(source)
    chunks = sum(len(_document_chunks(document)) for document in documents)
    return IndexReport(len(paths), len(documents), 0, 0, chunks, ())


def _document_chunks(document: CorpusDocument) -> list[tuple[tuple[str, ...], str]]:
    try:
        chunks = chunk_markdown(document.markdown)
    except ValueError as error:
        raise KnowledgeInputError(f"{document.doc_id} cannot be indexed: {error}") from error
    if not chunks:
        raise KnowledgeInputError(f"{document.doc_id} has no indexable content")
    return chunks


class KnowledgeStore:
    def __init__(
        self,
        database: str | Path,
        *,
        create: bool = False,
        allow_stale: bool = False,
        read_only: bool = False,
    ) -> None:
        if create and read_only:
            raise KnowledgeInputError("a read-only knowledge store cannot be created")
        self.read_only = read_only
        self.connection: sqlite3.Connection | None = None
        created_identity: os.stat_result | None = None
        self._stale = False
        try:
            self.path = validate_database_path(database, must_exist=not create)
            if create and not self.path.exists():
                created_identity = _create_empty_database_file(self.path)
            if not create and not self.path.exists():
                raise KnowledgeStorageError("knowledge database does not exist; run index first")
            if created_identity is not None:
                current = _database_identity(self.path)
                if not _same_file_identity(created_identity, current):
                    raise KnowledgeStorageError("knowledge database changed during initialization")
                self.connection = _connect(self.path)
                current = _database_identity(self.path)
                if not _same_file_identity(created_identity, current):
                    raise KnowledgeStorageError("knowledge database changed during initialization")
                _initialize(self.connection)
                current = _database_identity(self.path)
                if not _same_file_identity(created_identity, current):
                    raise KnowledgeStorageError("knowledge database changed during initialization")
                self._stale = False
            else:
                self.connection, self._stale = _open_existing(
                    self.path,
                    allow_stale=allow_stale,
                    read_only=read_only,
                )
            if os.name != "nt" and not read_only:
                self.path.parent.chmod(0o700)
                self.path.chmod(0o600)
        except OSError as error:
            if self.connection is not None:
                self.connection.close()
            if created_identity is not None:
                _discard_created_database(self.path, created_identity)
            raise KnowledgeStorageError(f"knowledge database is unavailable: {error.__class__.__name__}") from error
        except sqlite3.Error as error:
            if self.connection is not None:
                self.connection.close()
            if created_identity is not None:
                _discard_created_database(self.path, created_identity)
            raise KnowledgeStorageError(f"SQLite error: {error}") from error
        except Exception:
            if self.connection is not None:
                self.connection.close()
            if created_identity is not None:
                _discard_created_database(self.path, created_identity)
            raise

    def close(self) -> None:
        self.connection.close()

    def __enter__(self) -> KnowledgeStore:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def _enable_write_ahead_logging(self) -> None:
        if self.read_only:
            raise KnowledgeStorageError("knowledge store is read-only")
        self.connection.execute("PRAGMA journal_mode = WAL").fetchone()

    def _delete_document(self, doc_id: str) -> None:
        self.connection.execute("DELETE FROM knowledge_chunks WHERE doc_id = ?", (doc_id,))
        self.connection.execute("DELETE FROM knowledge_documents WHERE doc_id = ?", (doc_id,))

    @staticmethod
    def _document_digest(document: CorpusDocument) -> str:
        payload = {
            "markdown": normalize_text(document.markdown),
            "title": document.title,
            "source": document.source,
            "updatedAt": document.updated_at,
            "environment": document.environment,
            "serverTags": list(document.server_tags),
            "acl": list(document.acl),
            "metadata": document.metadata,
        }
        return hashlib.sha256(_json(payload).encode("utf-8")).hexdigest()

    @staticmethod
    def _prepared_chunks(
        document: CorpusDocument,
    ) -> list[tuple[str, int, str, str, str, int, bytes]]:
        prepared: list[tuple[str, int, str, str, str, int, bytes]] = []
        for ordinal, (heading_path, text) in enumerate(_document_chunks(document)):
            chunk_id = f"{document.doc_id}#chunk-{ordinal:04d}"
            index_text = "\n\n".join((document.title, *heading_path, text))
            tokens = tokenize(index_text)
            prepared.append((
                chunk_id,
                ordinal,
                _json(list(heading_path)),
                text,
                " ".join(tokens),
                len(tokens),
                feature_vector(index_text),
            ))
        return prepared

    def _stored_index_matches(
        self,
        document: CorpusDocument,
        digest: str,
        existing: sqlite3.Row,
        prepared: Sequence[tuple[str, int, str, str, str, int, bytes]],
    ) -> bool:
        if existing["content_hash"] != digest:
            return False
        expected_document = (
            document.title,
            document.source,
            document.updated_at,
            document.environment,
            _json(document.server_tags),
            _json(document.acl),
            _json(document.metadata),
        )
        actual_document = (
            existing["title"],
            existing["source"],
            existing["updated_at"],
            existing["environment"],
            existing["server_tags"],
            existing["acl"],
            existing["metadata_json"],
        )
        if actual_document != expected_document:
            return False
        try:
            _storage_text(
                existing["indexed_at"],
                "document index timestamp",
                maximum=64,
            )
        except KnowledgeStorageError:
            return False
        rows = self.connection.execute(
            "SELECT doc_id, chunk_id, ordinal, heading_path, text, terms, token_count, vector "
            "FROM knowledge_chunks WHERE doc_id = ? ORDER BY ordinal, chunk_id",
            (document.doc_id,),
        ).fetchall()
        if len(rows) != len(prepared):
            return False
        for row, expected in zip(rows, prepared, strict=True):
            actual = (
                row["doc_id"],
                row["chunk_id"],
                row["ordinal"],
                row["heading_path"],
                row["text"],
                row["terms"],
                row["token_count"],
                row["vector"],
            )
            if actual != (document.doc_id, *expected):
                return False
        return True

    def _index_document(self, document: CorpusDocument) -> tuple[bool, int]:
        digest = self._document_digest(document)
        prepared = self._prepared_chunks(document)
        existing = self.connection.execute(
            "SELECT * FROM knowledge_documents WHERE doc_id = ?", (document.doc_id,)
        ).fetchone()
        if (
            existing is not None
            and self._stored_index_matches(document, digest, existing, prepared)
        ):
            return False, 0
        self._delete_document(document.doc_id)
        now = datetime.now(UTC).isoformat()
        self.connection.execute(
            "INSERT INTO knowledge_documents(doc_id, title, source, updated_at, environment, server_tags, acl, metadata_json, content_hash, indexed_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                document.doc_id, document.title, document.source, document.updated_at,
                document.environment, _json(document.server_tags), _json(document.acl),
                _json(document.metadata), digest, now,
            ),
        )
        for chunk_id, ordinal, heading_json, text, terms, token_count, vector in prepared:
            self.connection.execute(
                "INSERT INTO knowledge_chunks(chunk_id, doc_id, ordinal, heading_path, text, terms, token_count, vector) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    chunk_id, document.doc_id, ordinal, heading_json, text,
                    terms, token_count, vector,
                ),
            )
        return True, len(prepared)

    def index(self, source: str | Path, *, reset: bool = False, dry_run: bool = False) -> IndexReport:
        if self.read_only and not dry_run:
            raise KnowledgeStorageError("knowledge store is read-only")
        _, paths, documents = _load_corpus(source)
        if dry_run:
            chunks = sum(len(_document_chunks(document)) for document in documents)
            return IndexReport(len(paths), len(documents), 0, 0, chunks, ())
        if self._stale and not reset:
            raise KnowledgeStorageError("knowledge index is stale; run index --reset")
        indexed = unchanged = chunks = 0
        removed = 0
        try:
            self._enable_write_ahead_logging()
            with self.connection:
                if reset:
                    self.connection.execute("DELETE FROM knowledge_chunks")
                    self.connection.execute("DELETE FROM knowledge_documents")
                seen: set[str] = set()
                for document in documents:
                    seen.add(document.doc_id)
                    changed, count = self._index_document(document)
                    indexed += int(changed)
                    unchanged += int(not changed)
                    chunks += count
                if not reset:
                    stale = [
                        row["doc_id"] for row in self.connection.execute("SELECT doc_id FROM knowledge_documents")
                        if row["doc_id"] not in seen
                    ]
                    for doc_id in stale:
                        self._delete_document(doc_id)
                        removed += 1
                if reset:
                    _write_current_versions(self.connection)
            if reset:
                self._stale = False
            return IndexReport(len(paths), indexed, unchanged, 0, chunks, (), removed)
        except sqlite3.Error as error:
            raise KnowledgeStorageError(f"index transaction failed: {error}") from error

    def delete_documents(self, doc_ids: Iterable[str]) -> dict[str, list[str]]:
        if self.read_only:
            raise KnowledgeStorageError("knowledge store is read-only")
        requested = sorted(set(
            value.strip() for value in doc_ids
            if isinstance(value, str) and value.strip()
        ))
        if not requested or len(requested) > 100 or any(len(value) > 512 for value in requested):
            raise KnowledgeInputError("delete requires between 1 and 100 valid document ids")
        placeholders = ",".join("?" for _ in requested)
        existing = [
            row[0] for row in self.connection.execute(
                f"SELECT doc_id FROM knowledge_documents WHERE doc_id IN ({placeholders})", requested
            )
        ]
        found = set(existing)
        try:
            self._enable_write_ahead_logging()
            with self.connection:
                for doc_id in existing:
                    self._delete_document(doc_id)
        except sqlite3.Error as error:
            raise KnowledgeStorageError(f"delete transaction failed: {error}") from error
        return {
            "deleted": sorted(found),
            "missing": [doc_id for doc_id in requested if doc_id not in found],
        }

    def stats(self) -> dict[str, Any]:
        try:
            documents = self.connection.execute(
                "SELECT count(*) FROM knowledge_documents"
            ).fetchone()[0]
            chunks = self.connection.execute(
                "SELECT count(*) FROM knowledge_chunks"
            ).fetchone()[0]
            updated_values: list[str] = []
            environments: set[str] = set()
            for row in self.connection.execute(
                "SELECT doc_id AS document_doc_id, title, source, updated_at, environment, "
                "server_tags, acl, metadata_json, content_hash, indexed_at "
                "FROM knowledge_documents"
            ):
                *_, updated_at, environment, _, _ = _storage_document_scalars(row)
                _storage_document_fields(row)
                updated_values.append(updated_at)
                environments.add(environment)
            for row in self.connection.execute("SELECT * FROM knowledge_chunks"):
                _storage_chunk_fields(row)
        except sqlite3.Error as error:
            raise KnowledgeStorageError(f"knowledge statistics are unreadable: {error}") from error
        return {
            "documents": int(documents),
            "chunks": int(chunks),
            "lastDocumentUpdatedAt": max(updated_values, default=None),
            "environments": sorted(environments),
            "chunkerVersion": CHUNKER_VERSION,
            "embeddingVersion": EMBEDDING_VERSION,
            "schemaVersion": SCHEMA_VERSION,
            "databaseBytes": self.path.stat().st_size if self.path.is_file() else 0,
        }

    def health(self) -> dict[str, Any]:
        try:
            quick = [row[0] for row in self.connection.execute("PRAGMA quick_check")]
            foreign_keys = self.connection.execute("PRAGMA foreign_key_check").fetchall()
            orphan_chunks = self.connection.execute(
                "SELECT count(*) FROM knowledge_chunks c "
                "LEFT JOIN knowledge_documents d ON d.doc_id = c.doc_id "
                "WHERE d.doc_id IS NULL"
            ).fetchone()[0]
            empty_documents = self.connection.execute(
                "SELECT count(*) FROM knowledge_documents d "
                "LEFT JOIN knowledge_chunks c ON c.doc_id = d.doc_id "
                "WHERE c.chunk_id IS NULL"
            ).fetchone()[0]
            invalid_documents = 0
            for row in self.connection.execute(
                "SELECT doc_id AS document_doc_id, title, source, updated_at, environment, "
                "server_tags, acl, metadata_json, content_hash, indexed_at "
                "FROM knowledge_documents"
            ):
                try:
                    _storage_document_scalars(row)
                    _storage_document_fields(row)
                except KnowledgeStorageError:
                    invalid_documents += 1
            invalid_chunks = 0
            for row in self.connection.execute("SELECT * FROM knowledge_chunks"):
                try:
                    _storage_chunk_fields(row)
                except KnowledgeStorageError:
                    invalid_chunks += 1
        except sqlite3.Error as error:
            raise KnowledgeStorageError(f"knowledge database check failed: {error}") from error
        ok = (
            quick == ["ok"]
            and not foreign_keys
            and not orphan_chunks
            and not empty_documents
            and not invalid_documents
            and not invalid_chunks
        )
        return {
            "ok": ok,
            "quickCheck": quick[0] if len(quick) == 1 else "multiple results",
            "foreignKeyViolations": len(foreign_keys),
            "orphanChunks": int(orphan_chunks),
            "documentsWithoutChunks": int(empty_documents),
            "invalidDocuments": invalid_documents,
            "invalidChunks": invalid_chunks,
        }

    def _records(self, filters: SearchFilters) -> list[KnowledgeChunk]:
        rows = self.connection.execute(
            "SELECT c.*, d.doc_id AS document_doc_id, d.title, d.source, d.updated_at, "
            "d.environment, d.server_tags, d.acl, d.metadata_json, d.content_hash, d.indexed_at "
            "FROM knowledge_chunks c JOIN knowledge_documents d ON d.doc_id = c.doc_id "
            "ORDER BY d.doc_id, c.ordinal"
        )
        records: list[KnowledgeChunk] = []
        tags = {tag.casefold() for tag in filters.tags}
        for row in rows:
            (
                document_doc_id,
                title,
                source,
                updated_at,
                environment,
                _,
                _,
            ) = _storage_document_scalars(row)
            acl_values, raw_server_tags, metadata = _storage_document_fields(row)
            _, chunk_doc_id, ordinal, text = _storage_chunk_scalars(row)
            heading_path, tokens, vector = _storage_chunk_fields(row)
            if chunk_doc_id != document_doc_id:
                raise _storage_value_error("chunk document")
            acl = {value.casefold() for value in acl_values}
            principal = filters.principal.casefold()
            if principal and principal not in acl and "public" not in acl:
                continue
            if not principal and "public" not in acl:
                continue
            environments = {
                value.strip().casefold()
                for value in environment.split(",")
                if value.strip()
            }
            if filters.environment and "any" not in environments and "*" not in environments \
                    and filters.environment.casefold() not in environments:
                continue
            server_tags = {value.casefold() for value in raw_server_tags}
            if filters.global_only and server_tags:
                continue
            if filters.server_id and server_tags and filters.server_id.casefold() not in server_tags:
                continue
            raw_tags = metadata.get("tags", [])
            if isinstance(raw_tags, str):
                raw_tags = [raw_tags]
            document_tags = {
                str(value).strip().casefold() for value in raw_tags
                if isinstance(value, str) and value.strip()
            } if isinstance(raw_tags, list) else set()
            if tags and not tags.issubset(document_tags):
                continue
            records.append(KnowledgeChunk(
                doc_id=document_doc_id,
                title=title,
                scope="knowledge",
                server_id=filters.server_id,
                ordinal=ordinal,
                heading_path=heading_path,
                text=text,
                tokens=tokens,
                vector=vector,
                updated_at=updated_at,
                metadata={
                    "source": source,
                    "environment": environment,
                    "serverTags": list(raw_server_tags),
                    "acl": list(acl_values),
                },
            ))
        return records

    def search(self, query: str, *, mode: str = "hybrid", limit: int = 8,
               filters: SearchFilters | None = None) -> list[SearchResult]:
        filters = filters or SearchFilters()
        try:
            # Validate the caller's budget before decoding the corpus: an oversized
            # query must fail fast instead of loading every chunk first.
            query_token_weights(query)
            records = self._records(filters)
            return search_chunks(query, records, mode=mode, limit=max(1, min(50, limit)))
        except QueryTokenLimitError as error:
            # The token budget is a property of the caller's query, not of the store.
            # Converting here keeps every caller (CLI, sidecar, conversation store) on
            # the stable input-error path instead of a generic internal failure.
            raise KnowledgeInputError(
                f"query has {error.count} unique searchable tokens; limit is {error.limit}"
            ) from error

    def documents_exist(self, doc_ids: Iterable[str]) -> list[str]:
        expected = sorted(set(doc_ids))
        if not expected:
            return []
        placeholders = ",".join("?" for _ in expected)
        existing = {
            row[0] for row in self.connection.execute(
                f"SELECT doc_id FROM knowledge_documents WHERE doc_id IN ({placeholders})", expected
            )
        }
        return [doc_id for doc_id in expected if doc_id not in existing]

    def document_section_ids(self, doc_ids: Iterable[str]) -> dict[str, set[str]]:
        requested = sorted(set(doc_ids))
        sections = {doc_id: set() for doc_id in requested}
        if not requested:
            return sections
        placeholders = ",".join("?" for _ in requested)
        try:
            rows = self.connection.execute(
                f"SELECT DISTINCT doc_id, heading_path FROM knowledge_chunks "
                f"WHERE doc_id IN ({placeholders})",
                requested,
            )
            for row in rows:
                doc_id = _storage_document_id(row["doc_id"])
                heading_path = _storage_heading_path(row)
                section_id = _current_section_id(heading_path)
                if section_id:
                    sections[doc_id].add(section_id)
        except sqlite3.Error as error:
            raise KnowledgeStorageError(f"knowledge section metadata is unreadable: {error}") from error
        return sections


def _question_id_list(
    item: dict[str, Any],
    field: str,
    number: int,
    *,
    maximum_length: int,
) -> list[str]:
    value = item.get(field, [])
    if not isinstance(value, list):
        raise KnowledgeInputError(f"questions line {number} has invalid {field}")
    if len(value) > 50:
        raise KnowledgeInputError(f"questions line {number} references too many {field} values")
    result: list[str] = []
    seen: set[str] = set()
    for entry in value:
        if not isinstance(entry, str) or not entry or entry != entry.strip():
            raise KnowledgeInputError(f"questions line {number} has an empty or invalid {field} value")
        if len(entry) > maximum_length or any(
            ord(character) < 32 or ord(character) == 127 for character in entry
        ):
            raise KnowledgeInputError(f"questions line {number} has an overlong or invalid {field} value")
        if entry in seen:
            raise KnowledgeInputError(f"questions line {number} has a duplicate {field} value")
        seen.add(entry)
        result.append(entry)
    return result


def _validate_question(item: dict[str, Any], number: int) -> None:
    if not isinstance(item.get("id"), str) or not isinstance(item.get("question"), str):
        raise KnowledgeInputError(f"questions line {number} requires string id and question")
    question_id = item["id"]
    question = item["question"]
    if (
        not question_id.strip()
        or question_id != question_id.strip()
        or len(question_id) > 120
        or any(ord(character) < 32 or ord(character) == 127 for character in question_id)
    ):
        raise KnowledgeInputError(f"questions line {number} has an empty or overlong id")
    if not question.strip() or len(question) > 4000:
        raise KnowledgeInputError(f"questions line {number} has an empty or overlong question")
    if "expect_no_answer" in item and not isinstance(item["expect_no_answer"], bool):
        raise KnowledgeInputError(f"questions line {number} has invalid expect_no_answer")
    expect_no_answer = bool(item.get("expect_no_answer", False))
    expected = _question_id_list(item, "expected_doc_ids", number, maximum_length=512)
    expected_sections = _question_id_list(item, "expected_section_ids", number, maximum_length=200)
    forbidden = _question_id_list(item, "forbidden_doc_ids", number, maximum_length=512)
    overlap = sorted(set(expected).intersection(forbidden))
    if overlap:
        raise KnowledgeInputError(
            f"questions line {number} lists documents as both expected and forbidden: " + ", ".join(overlap[:8])
        )
    if expect_no_answer:
        if expected or expected_sections:
            raise KnowledgeInputError(
                f"questions line {number} cannot expect documents or sections when expect_no_answer is true"
            )
    elif not expected or not expected_sections:
        raise KnowledgeInputError(
            f"questions line {number} requires expected_doc_ids and expected_section_ids"
        )
    filters = item.get("filters", {})
    if filters is not None and not isinstance(filters, dict):
        raise KnowledgeInputError(f"questions line {number} has invalid filters")
    if filters:
        unknown = set(filters) - {"principal", "environment", "server", "server_id", "tags"}
        if unknown:
            raise KnowledgeInputError(f"questions line {number} has unknown filter fields")
        for key in ("principal", "environment", "server", "server_id"):
            value = filters.get(key)
            if value is not None and (
                not isinstance(value, str)
                or len(value) > 120
                or value != value.strip()
                or any(ord(character) < 32 or ord(character) == 127 for character in value)
            ):
                raise KnowledgeInputError(f"questions line {number} has an invalid {key} filter")
        tags = filters.get("tags", [])
        if not isinstance(tags, list) or len(tags) > 32 \
                or not all(
                    isinstance(value, str)
                    and 0 < len(value) <= 120
                    and value == value.strip()
                    and not any(ord(character) < 32 or ord(character) == 127 for character in value)
                    for value in tags
                ):
            raise KnowledgeInputError(f"questions line {number} has invalid tag filters")


def read_questions(path: str | Path) -> list[dict[str, Any]]:
    source = Path(path)
    try:
        if not source.is_file() or source.is_symlink():
            raise KnowledgeInputError("questions file must be a regular JSONL file")
        size = source.stat().st_size
    except OSError as error:
        raise KnowledgeInputError(
            f"questions file is unavailable: {error.__class__.__name__}"
        ) from error
    if size > 4 * 1024 * 1024:
        raise KnowledgeInputError("questions file exceeds 4 MiB")
    questions: list[dict[str, Any]] = []
    try:
        lines = source.read_text(encoding="utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise KnowledgeInputError("questions file is not valid UTF-8") from error
    except OSError as error:
        raise KnowledgeInputError(
            f"questions file cannot be read: {error.__class__.__name__}"
        ) from error
    for number, line in enumerate(lines, 1):
        if not line.strip():
            continue
        try:
            item = json.loads(line)
        except json.JSONDecodeError as error:
            raise KnowledgeInputError(f"questions line {number} is not valid JSON") from error
        if not isinstance(item, dict):
            raise KnowledgeInputError(f"questions line {number} requires a JSON object")
        _validate_question(item, number)
        questions.append(item)
    if not questions:
        raise KnowledgeInputError("questions file is empty")
    identifiers = [item["id"] for item in questions]
    if len(identifiers) != len(set(identifiers)):
        raise KnowledgeInputError("questions file contains duplicate ids")
    return questions


def evaluate(store: KnowledgeStore, questions: Sequence[dict[str, Any]], *,
             modes: Sequence[str], k: int) -> dict[str, Any]:
    if not questions:
        raise KnowledgeInputError("at least one evaluation question is required")
    if k <= 0 or k > 50:
        raise KnowledgeInputError("k must be between 1 and 50")
    modes = list(dict.fromkeys(modes))
    if not modes:
        raise KnowledgeInputError("at least one retrieval mode is required")
    for number, question in enumerate(questions, 1):
        if not isinstance(question, dict):
            raise KnowledgeInputError(f"questions line {number} requires a JSON object")
        _validate_question(question, number)
    identifiers = [question["id"] for question in questions]
    if len(identifiers) != len(set(identifiers)):
        raise KnowledgeInputError("questions file contains duplicate ids")
    missing = store.documents_exist(
        doc_id
        for question in questions
        for doc_id in [*question.get("expected_doc_ids", []), *question.get("forbidden_doc_ids", [])]
    )
    if missing:
        raise KnowledgeInputError("questions reference missing documents: " + ", ".join(missing[:8]))
    expected_sections = store.document_section_ids(
        doc_id
        for question in questions
        for doc_id in question.get("expected_doc_ids", [])
    )
    missing_sections: list[str] = []
    ambiguous_sections: list[str] = []
    uncovered_documents: list[str] = []
    section_owners: dict[str, dict[str, str]] = {}
    for question in questions:
        expected_doc_ids = question.get("expected_doc_ids", [])
        owners: dict[str, str] = {}
        for section_id in question.get("expected_section_ids", []):
            matching_documents = [
                doc_id for doc_id in expected_doc_ids
                if section_id in expected_sections.get(doc_id, set())
            ]
            if not matching_documents:
                missing_sections.append(f"{question['id']}:{section_id}")
            elif len(matching_documents) > 1:
                ambiguous_sections.append(f"{question['id']}:{section_id}")
            else:
                owners[section_id] = matching_documents[0]
        covered_documents = set(owners.values())
        for doc_id in expected_doc_ids:
            if doc_id not in covered_documents:
                uncovered_documents.append(f"{question['id']}:{doc_id}")
        section_owners[question["id"]] = owners
    if missing_sections:
        raise KnowledgeInputError(
            "questions reference missing sections: " + ", ".join(missing_sections[:8])
        )
    if ambiguous_sections:
        raise KnowledgeInputError(
            "questions reference sections present in more than one expected document: "
            + ", ".join(ambiguous_sections[:8])
        )
    if uncovered_documents:
        raise KnowledgeInputError(
            "each expected document requires at least one of that document's sections: "
            + ", ".join(uncovered_documents[:8])
        )
    reports: dict[str, Any] = {}
    scored_questions = [question for question in questions if not question.get("expect_no_answer")]
    no_answer_questions = [question for question in questions if question.get("expect_no_answer")]
    for mode in modes:
        if mode not in {"lexical", "vector", "hybrid"}:
            raise KnowledgeInputError(f"unsupported retrieval mode: {mode}")
        per_question: list[dict[str, Any]] = []
        totals = {
            "recall": 0.0,
            "precision": 0.0,
            "mrr": 0.0,
            "ndcg": 0.0,
            "sectionRecall": 0.0,
        }
        section_hit_count = 0
        expected_section_count = 0
        leakage = 0
        leaked_questions = 0
        leaked_doc_ids: dict[str, int] = {}
        scored_leakage = 0
        no_answer_leakage = 0
        no_answer_correct = 0
        no_answer_false_positive = 0
        for question in questions:
            raw_filters = question.get("filters") or {}
            filters = SearchFilters(
                principal=str(raw_filters.get("principal") or ""),
                environment=str(raw_filters.get("environment") or ""),
                server_id=str(raw_filters.get("server") or raw_filters.get("server_id") or ""),
                tags=tuple(str(value) for value in raw_filters.get("tags", [])),
            )
            results = store.search(question["question"], mode=mode, limit=k, filters=filters)
            expected = question.get("expected_doc_ids", [])
            forbidden = set(question.get("forbidden_doc_ids", []))
            ranked_docs: list[str] = []
            for result in results:
                if result.chunk.doc_id not in ranked_docs:
                    ranked_docs.append(result.chunk.doc_id)
            leaked = sorted(forbidden.intersection(ranked_docs))
            leakage += len(leaked)
            leaked_questions += int(bool(leaked))
            for doc_id in leaked:
                leaked_doc_ids[doc_id] = leaked_doc_ids.get(doc_id, 0) + 1
            expected_set = set(expected)
            owners = section_owners.get(question["id"], {})
            if question.get("expect_no_answer"):
                hits = 0
                recalled = precision = reciprocal = ndcg = section_recalled = None
                section_hits = 0
                ranked_sections: list[dict[str, str]] = []
                no_answer_leakage += len(leaked)
                if ranked_docs:
                    no_answer_false_positive += 1
                else:
                    no_answer_correct += 1
            else:
                hits = sum(1 for doc_id in ranked_docs[:k] if doc_id in expected_set)
                recalled = len(expected_set.intersection(ranked_docs[:k])) / len(expected_set)
                precision = hits / k
                reciprocal = next(
                    (1.0 / (index + 1) for index, doc_id in enumerate(ranked_docs) if doc_id in expected_set),
                    0.0,
                )
                dcg = sum(1.0 / math.log2(index + 2)
                          for index, doc_id in enumerate(ranked_docs[:k]) if doc_id in expected_set)
                ideal = sum(1.0 / math.log2(index + 2) for index in range(min(len(expected_set), k)))
                ndcg = dcg / ideal if ideal else 0.0
                ranked_sections = []
                seen_sections: set[tuple[str, str]] = set()
                for result in results:
                    section_id = _current_section_id(result.chunk.heading_path)
                    pair = (result.chunk.doc_id, section_id)
                    if not section_id or pair in seen_sections:
                        continue
                    seen_sections.add(pair)
                    ranked_sections.append({
                        "documentId": result.chunk.doc_id,
                        "sectionId": section_id,
                    })
                retrieved_sections = {
                    (item["documentId"], item["sectionId"]) for item in ranked_sections
                }
                section_hits = sum(
                    1 for section_id, doc_id in owners.items()
                    if (doc_id, section_id) in retrieved_sections
                )
                expected_section_count += len(owners)
                section_hit_count += section_hits
                section_recalled = section_hits / len(owners) if owners else 0.0
                scored_leakage += len(leaked)
                totals["recall"] += recalled
                totals["precision"] += precision
                totals["mrr"] += reciprocal
                totals["ndcg"] += ndcg
                totals["sectionRecall"] += section_recalled
            per_question.append({
                "id": question["id"],
                "question": question["question"],
                "expectedDocIds": expected,
                "expectedSectionIds": list(question.get("expected_section_ids", [])),
                "expectedSectionDocumentIds": dict(sorted(owners.items())),
                "forbiddenDocIds": sorted(forbidden),
                "expectNoAnswer": bool(question.get("expect_no_answer")),
                "rankedDocIds": ranked_docs,
                "rankedSections": ranked_sections,
                "relevantRanked": hits,
                "returnedDocumentCount": len(ranked_docs),
                "abstained": (not ranked_docs) if question.get("expect_no_answer") else None,
                "falsePositive": bool(ranked_docs) if question.get("expect_no_answer") else None,
                "recall": round(recalled, 6) if recalled is not None else None,
                "precision": round(precision, 6) if precision is not None else None,
                "mrr": round(reciprocal, 6) if reciprocal is not None else None,
                "ndcg": round(ndcg, 6) if ndcg is not None else None,
                "sectionHitCount": section_hits,
                "sectionRecall": (
                    round(section_recalled, 6) if section_recalled is not None else None
                ),
                "forbiddenLeakage": leaked,
            })
        scored_count = len(scored_questions)
        no_answer_total = len(no_answer_questions)
        reports[mode] = {
            "questions": len(questions),
            "scoredQuestions": scored_count,
            "noAnswerQuestions": no_answer_total,
            "k": k,
            "recall": round(totals["recall"] / scored_count, 6) if scored_count else None,
            "precision": round(totals["precision"] / scored_count, 6) if scored_count else None,
            "mrr": round(totals["mrr"] / scored_count, 6) if scored_count else None,
            "ndcg": round(totals["ndcg"] / scored_count, 6) if scored_count else None,
            "sectionRecall": (
                round(totals["sectionRecall"] / scored_count, 6) if scored_count else None
            ),
            "sectionHitCount": section_hit_count,
            "expectedSectionCount": expected_section_count,
            "aclLeakageCount": leakage,
            "forbiddenHitCount": leakage,
            "forbiddenLeakageCount": leakage,
            "forbiddenQuestionCount": leaked_questions,
            "forbiddenHitsOnScored": scored_leakage,
            "forbiddenHitsOnNoAnswer": no_answer_leakage,
            "forbiddenDocIdHits": dict(sorted(leaked_doc_ids.items())),
            "noAnswerCorrect": no_answer_correct,
            "noAnswerTotal": no_answer_total,
            "noAnswerFalsePositiveCount": no_answer_false_positive,
            "abstainRate": (
                round(no_answer_correct / no_answer_total, 6) if no_answer_total else None
            ),
            "falsePositiveRate": (
                round(no_answer_false_positive / no_answer_total, 6) if no_answer_total else None
            ),
            "perQuestion": per_question,
        }
    return {
        "schemaVersion": 1,
        "k": k,
        "questionCount": len(questions),
        "scoredQuestionCount": len(scored_questions),
        "noAnswerQuestionCount": len(no_answer_questions),
        "modes": reports,
        "retrievalDefinition": {
            "precision": "relevant unique documents divided by k",
            "recall": "unique expected documents retrieved divided by expected documents",
            "mrr": "reciprocal rank of the first relevant document",
            "ndcg": "binary-relevance nDCG at k",
            "sectionRecall": "expected anchored sections retrieved from their declared document, averaged over scored questions",
            "averages": "Recall, Precision, MRR, nDCG and sectionRecall exclude expect_no_answer questions",
            "noAnswerAbstainRate": "fraction of expect_no_answer questions for which no document was returned; this is retrieval abstention, not model-confidence abstention",
            "falsePositive": "any returned document for an expect_no_answer question",
            "forbidden": "forbidden_doc_ids hits are reported separately and never count as relevant",
            "aclLeakageCount": "deprecated compatibility alias for forbiddenHitCount; this is not an ACL enforcement or authorization metric",
        },
    }
