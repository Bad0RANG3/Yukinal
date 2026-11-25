from __future__ import annotations

import hashlib
import json
import os
import shutil
import sqlite3
import struct
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from yukinal_knowledge import store as store_module
from yukinal_knowledge.engine import (
    CHUNKER_VERSION,
    EMBEDDING_VERSION,
    VECTOR_DIMENSIONS,
    QueryTokenLimitError,
    query_token_weights,
)
from yukinal_knowledge.store import (
    APPLICATION_ID,
    SCHEMA_VERSION,
    KnowledgeInputError,
    KnowledgeStorageError,
    KnowledgeStore,
    SearchFilters,
    evaluate,
    preview_index,
    read_questions,
    validate_database_path,
)

REPO_ROOT = Path(__file__).resolve().parents[2]
SAMPLE_CORPUS = REPO_ROOT / "examples" / "rag" / "corpus"
PRIVATE_TIMEOUT_RUNBOOK = "nginx/private-timeout-runbook.md"


def document(
    path: Path,
    title: str,
    body: str,
    *,
    acl: str = '["public"]',
    environment: str = '"demo"',
    server_tags: str = "[]",
    extra: str = "",
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "+++\n"
        f'title = "{title}"\n'
        'source = "https://example.invalid/reference"\n'
        'updated_at = "2026-11-25"\n'
        f"environment = {environment}\n"
        f"server_tags = {server_tags}\n"
        f"acl = {acl}\n"
        f"{extra}"
        "+++\n\n"
        f"{body.strip()}\n",
        encoding="utf-8",
    )


def mutate_database(path: Path, *operations: tuple[str, tuple[object, ...]]) -> None:
    connection = sqlite3.connect(path)
    try:
        connection.execute("PRAGMA foreign_keys = OFF")
        connection.execute("PRAGMA journal_mode = DELETE").fetchone()
        for statement, parameters in operations:
            connection.execute(statement, parameters)
        connection.commit()
    finally:
        connection.close()


def normalize_database(path: Path) -> None:
    mutate_database(path)


def read_database(
    path: Path,
    statement: str,
    parameters: tuple[object, ...] = (),
) -> list[sqlite3.Row]:
    connection = sqlite3.connect(f"{path.as_uri()}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    try:
        return connection.execute(statement, parameters).fetchall()
    finally:
        connection.close()


def database_digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def metadata(path: Path) -> dict[str, str]:
    return {
        row["key"]: row["value"]
        for row in read_database(path, "SELECT key, value FROM knowledge_meta")
    }


class KnowledgeStoreTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.corpus = self.root / "corpus"
        self.corpus.mkdir()
        self.database = self.root / "knowledge.sqlite"

    def test_new_database_is_owned_and_legal_reopen_and_reset_work(self) -> None:
        document(
            self.corpus / "disk.md",
            "Disk full",
            "# Disk full\n\n## Triage {#triage}\n\nCheck df, du and deleted open files.",
        )
        with KnowledgeStore(self.database, create=True) as store:
            report = store.index(self.corpus)
            self.assertEqual((report.indexed, report.chunks), (1, 1))
            self.assertTrue(store.search("disk triage", filters=SearchFilters()))
        self.assertEqual(
            read_database(self.database, "PRAGMA application_id")[0][0],
            APPLICATION_ID,
        )
        self.assertEqual(
            read_database(self.database, "PRAGMA user_version")[0][0],
            SCHEMA_VERSION,
        )
        self.assertEqual(
            metadata(self.database),
            {
                "schema_version": str(SCHEMA_VERSION),
                "chunker_version": CHUNKER_VERSION,
                "embedding_version": EMBEDDING_VERSION,
            },
        )

        with KnowledgeStore(self.database) as store:
            self.assertEqual(store.stats()["documents"], 1)
        with KnowledgeStore(self.database, create=True) as store:
            self.assertEqual(store.stats()["chunks"], 1)
            reset = store.index(self.corpus, reset=True)
            self.assertEqual((reset.indexed, reset.unchanged), (1, 0))
        with KnowledgeStore(self.database) as store:
            self.assertEqual(store.stats()["documents"], 1)
            self.assertTrue(store.health()["ok"])

    def test_read_only_store_can_query_but_cannot_index_or_delete(self) -> None:
        document(
            self.corpus / "disk.md",
            "Disk full",
            "# Disk full\n\n## Triage {#triage}\n\nCheck df, du and deleted open files.",
        )
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
        digest = database_digest(self.database)

        with KnowledgeStore(self.database, read_only=True) as store:
            self.assertTrue(store.read_only)
            self.assertEqual(store.stats()["documents"], 1)
            self.assertTrue(store.search("disk triage"))
            with self.assertRaisesRegex(KnowledgeStorageError, "read-only"):
                store.index(self.corpus, reset=True)
            with self.assertRaisesRegex(KnowledgeStorageError, "read-only"):
                store.delete_documents(["disk.md"])

        self.assertEqual(database_digest(self.database), digest)

    def test_existing_empty_plain_and_foreign_files_are_never_claimed(self) -> None:
        empty = self.root / "empty.sqlite"
        empty.write_bytes(b"")
        empty_digest = database_digest(empty)
        for create in (False, True):
            with self.assertRaises(KnowledgeStorageError):
                KnowledgeStore(empty, create=create)
            self.assertEqual(database_digest(empty), empty_digest)
            self.assertTrue(empty.is_file())

        plain = self.root / "plain.sqlite"
        connection = sqlite3.connect(plain)
        try:
            connection.execute("CREATE TABLE other(value TEXT)")
            connection.commit()
        finally:
            connection.close()
        normalize_database(plain)
        plain_digest = database_digest(plain)
        for create in (False, True):
            with self.assertRaises(KnowledgeStorageError):
                KnowledgeStore(plain, create=create)
            self.assertEqual(database_digest(plain), plain_digest)

        foreign = self.root / "foreign.sqlite"
        connection = sqlite3.connect(foreign)
        try:
            connection.execute("PRAGMA application_id = 7")
            connection.execute("CREATE TABLE other(value TEXT)")
            connection.commit()
        finally:
            connection.close()
        normalize_database(foreign)
        foreign_digest = database_digest(foreign)
        for create in (False, True):
            with self.assertRaises(KnowledgeStorageError):
                KnowledgeStore(foreign, create=create)
            self.assertEqual(database_digest(foreign), foreign_digest)

    def test_incompatible_yknl_schemas_are_rejected_without_writes(self) -> None:
        document(self.corpus / "one.md", "One", "# One\n\n## Triage {#triage}\n\nalpha")
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
        normalize_database(self.database)

        cases = {
            "foreign_application_id": (
                ("PRAGMA application_id = 7", ()),
            ),
            "unsupported_user_version": (
                ("PRAGMA user_version = 999", ()),
            ),
            "metadata_schema_mismatch": (
                (
                    "UPDATE knowledge_meta SET value = '999' WHERE key = 'schema_version'",
                    (),
                ),
            ),
            "old_schema_missing_table": (
                ("DROP TABLE knowledge_chunks", ()),
            ),
            "old_schema_missing_column": (
                ("ALTER TABLE knowledge_documents DROP COLUMN indexed_at", ()),
            ),
        }
        for name, operations in cases.items():
            with self.subTest(name=name):
                path = self.root / f"{name}.sqlite"
                shutil.copy2(self.database, path)
                mutate_database(path, *operations)
                normalize_database(path)
                before = database_digest(path)
                with self.assertRaises(KnowledgeStorageError):
                    KnowledgeStore(path)
                self.assertEqual(database_digest(path), before)
                with self.assertRaises(KnowledgeStorageError):
                    KnowledgeStore(path, create=True)
                self.assertEqual(database_digest(path), before)

    def test_schema_requires_constraints_and_the_document_index(self) -> None:
        document(self.corpus / "one.md", "One", "# One\n\n## Triage {#triage}\n\nalpha")
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)

        missing_index = self.root / "missing-index.sqlite"
        shutil.copy2(self.database, missing_index)
        mutate_database(
            missing_index,
            ("DROP INDEX knowledge_chunks_document", ()),
        )
        with self.assertRaisesRegex(KnowledgeStorageError, "schema is incompatible"):
            KnowledgeStore(missing_index)

        forged = self.root / "forged-owned-shell.sqlite"
        connection = sqlite3.connect(forged)
        try:
            connection.execute(f"PRAGMA application_id = {APPLICATION_ID}")
            connection.execute(f"PRAGMA user_version = {SCHEMA_VERSION}")
            connection.execute("CREATE TABLE knowledge_meta (key TEXT, value TEXT)")
            connection.execute(
                "CREATE TABLE knowledge_documents ("
                "doc_id TEXT, title TEXT, source TEXT, updated_at TEXT, environment TEXT, "
                "server_tags TEXT, acl TEXT, metadata_json TEXT, content_hash TEXT, indexed_at TEXT)"
            )
            connection.execute(
                "CREATE TABLE knowledge_chunks ("
                "chunk_id TEXT, doc_id TEXT, ordinal INTEGER, heading_path TEXT, text TEXT, "
                "terms TEXT, vector BLOB, token_count INTEGER)"
            )
            connection.execute(
                "CREATE INDEX knowledge_chunks_document "
                "ON knowledge_chunks(doc_id, ordinal)"
            )
            connection.executemany(
                "INSERT INTO knowledge_meta(key, value) VALUES (?, ?)",
                (
                    ("schema_version", str(SCHEMA_VERSION)),
                    ("chunker_version", CHUNKER_VERSION),
                    ("embedding_version", EMBEDDING_VERSION),
                ),
            )
            connection.commit()
        finally:
            connection.close()
        normalize_database(forged)
        before = database_digest(forged)
        with self.assertRaisesRegex(KnowledgeStorageError, "schema is incompatible"):
            KnowledgeStore(forged)
        self.assertEqual(database_digest(forged), before)

    def test_created_file_raced_by_another_writer_is_not_deleted(self) -> None:
        def claim(connection: sqlite3.Connection) -> None:
            connection.execute("CREATE TABLE foreign_claim(value TEXT)")
            connection.commit()
            raise KnowledgeStorageError("simulated competing writer")

        with mock.patch.object(store_module, "_initialize", side_effect=claim):
            with self.assertRaises(KnowledgeStorageError):
                KnowledgeStore(self.database, create=True)
        self.assertTrue(self.database.is_file())
        objects = {
            row["name"]
            for row in read_database(
                self.database,
                "SELECT name FROM sqlite_master WHERE type = 'table'",
            )
        }
        self.assertIn("foreign_claim", objects)

    def test_pristine_created_file_is_removed_after_initialization_failure(self) -> None:
        with mock.patch.object(
            store_module,
            "_initialize",
            side_effect=KnowledgeStorageError("simulated initialization failure"),
        ):
            with self.assertRaises(KnowledgeStorageError):
                KnowledgeStore(self.database, create=True)
        self.assertFalse(self.database.exists())

    def test_stale_chunker_and_embedding_versions_require_reset(self) -> None:
        document(self.corpus / "one.md", "One", "# One\n\n## Triage {#triage}\n\nalpha")
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
        normalize_database(self.database)

        for key in ("chunker_version", "embedding_version"):
            with self.subTest(key=key):
                path = self.root / f"stale-{key}.sqlite"
                shutil.copy2(self.database, path)
                mutate_database(
                    path,
                    ("UPDATE knowledge_meta SET value = 'stale' WHERE key = ?", (key,)),
                )
                normalize_database(path)
                before = database_digest(path)
                with self.assertRaisesRegex(KnowledgeStorageError, "stale"):
                    KnowledgeStore(path)
                self.assertEqual(database_digest(path), before)
                with self.assertRaisesRegex(KnowledgeStorageError, "index --reset"):
                    KnowledgeStore(path, create=True)
                with KnowledgeStore(path, allow_stale=True) as store:
                    with self.assertRaisesRegex(KnowledgeStorageError, "index --reset"):
                        store.index(self.corpus)
                with KnowledgeStore(path, create=True, allow_stale=True) as store:
                    report = store.index(self.corpus, reset=True)
                    self.assertEqual((report.indexed, report.unchanged), (1, 0))
                self.assertEqual(metadata(path)[key], {
                    "chunker_version": CHUNKER_VERSION,
                    "embedding_version": EMBEDDING_VERSION,
                }[key])
                with KnowledgeStore(path) as store:
                    self.assertEqual(store.stats()["documents"], 1)

    def test_health_reports_orphan_chunks_and_documents_without_chunks(self) -> None:
        document(self.corpus / "one.md", "One", "# One\n\n## Triage {#triage}\n\nalpha")
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
        mutate_database(
            self.database,
            (
                "INSERT INTO knowledge_chunks"
                "(chunk_id, doc_id, ordinal, heading_path, text, terms, vector, token_count) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    "missing.md#chunk-0000",
                    "missing.md",
                    0,
                    "[]",
                    "orphan",
                    "orphan",
                    sqlite3.Binary(b"\x00"),
                    1,
                ),
            ),
            (
                "INSERT INTO knowledge_documents"
                "(doc_id, title, source, updated_at, environment, server_tags, acl, "
                "metadata_json, content_hash, indexed_at) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    "empty.md",
                    "Empty",
                    "source",
                    "2026-11-25",
                    "demo",
                    "[]",
                    '["public"]',
                    "{}",
                    "hash",
                    "2026-11-25",
                ),
            ),
        )
        with KnowledgeStore(self.database) as store:
            health = store.health()
        self.assertFalse(health["ok"])
        self.assertEqual(health["orphanChunks"], 1)
        self.assertEqual(health["documentsWithoutChunks"], 1)
        self.assertGreaterEqual(health["foreignKeyViolations"], 1)
        self.assertEqual(health["invalidDocuments"], 1)
        self.assertEqual(health["invalidChunks"], 1)
        self.assertEqual(health["quickCheck"], "ok")

    def test_corrupt_storage_metadata_fails_closed_without_leaking_content(self) -> None:
        document(
            self.corpus / "secret.md",
            "Secret title",
            "# Secret title\n\n## Secret heading\n\nBODY_SECRET_MARKER fail closed",
        )
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            healthy = store.health()
        self.assertTrue(healthy["ok"])
        self.assertEqual(
            (healthy["invalidDocuments"], healthy["invalidChunks"]),
            (0, 0),
        )

        nan_vector = struct.pack(
            f"<{VECTOR_DIMENSIONS}f",
            *([float("nan")] * VECTOR_DIMENSIONS),
        )
        cases = (
            (
                "acl",
                "UPDATE knowledge_documents SET acl = ?",
                ('["public", "ACL_SECRET_MARKER"',),
                "invalidDocuments",
            ),
            (
                "server-tags",
                "UPDATE knowledge_documents SET server_tags = ?",
                ('["srv_ok", 7]',),
                "invalidDocuments",
            ),
            (
                "metadata",
                "UPDATE knowledge_documents SET metadata_json = ?",
                ('{"tags": [1]}',),
                "invalidDocuments",
            ),
            (
                "heading",
                "UPDATE knowledge_chunks SET heading_path = ?",
                ('[{"secret": "HEADING_SECRET_MARKER"}]',),
                "invalidChunks",
            ),
            (
                "terms",
                "UPDATE knowledge_chunks SET terms = ?, token_count = ?",
                ("alpha beta", 1),
                "invalidChunks",
            ),
            (
                "vector-length",
                "UPDATE knowledge_chunks SET vector = ?",
                (sqlite3.Binary(b"\x00"),),
                "invalidChunks",
            ),
            (
                "vector-finite",
                "UPDATE knowledge_chunks SET vector = ?",
                (sqlite3.Binary(nan_vector),),
                "invalidChunks",
            ),
            (
                "document-title",
                "UPDATE knowledge_documents SET title = ?",
                (sqlite3.Binary(b"BLOB_SECRET_MARKER"),),
                "invalidDocuments",
            ),
            (
                "document-environment",
                "UPDATE knowledge_documents SET environment = ?",
                (sqlite3.Binary(b"BLOB_SECRET_MARKER"),),
                "invalidDocuments",
            ),
            (
                "chunk-text",
                "UPDATE knowledge_chunks SET text = ?",
                (sqlite3.Binary(b"BLOB_SECRET_MARKER"),),
                "invalidChunks",
            ),
            (
                "chunk-ordinal",
                "UPDATE knowledge_chunks SET ordinal = ?",
                (sqlite3.Binary(b"BLOB_SECRET_MARKER"),),
                "invalidChunks",
            ),
        )
        for label, statement, parameters, counter in cases:
            with self.subTest(label=label):
                damaged = self.root / f"{label}.sqlite"
                shutil.copyfile(self.database, damaged)
                mutate_database(damaged, (statement, parameters))
                with KnowledgeStore(damaged) as store:
                    health = store.health()
                    self.assertFalse(health["ok"])
                    self.assertEqual(health[counter], 1)
                    with self.assertRaises(KnowledgeStorageError) as caught:
                        store.search("fail closed", filters=SearchFilters())
                self.assertEqual(caught.exception.code, "storage_unavailable")
                diagnostic = json.dumps(health, ensure_ascii=False)
                for secret in (
                    "BODY_SECRET_MARKER",
                    "ACL_SECRET_MARKER",
                    "HEADING_SECRET_MARKER",
                    "BLOB_SECRET_MARKER",
                ):
                    self.assertNotIn(secret, diagnostic)
                    self.assertNotIn(secret, str(caught.exception))

    def test_server_scope_includes_global_documents_and_excludes_other_servers(self) -> None:
        document(
            self.corpus / "global.md",
            "Global",
            "# Global\n\n## Scope {#scope}\n\nredis cluster maintenance",
            server_tags="[]",
        )
        document(
            self.corpus / "docker.md",
            "Docker",
            "# Docker\n\n## Scope {#scope}\n\ndocker redis cluster maintenance bridge",
            server_tags='["srv_docker01"]',
        )
        document(
            self.corpus / "nginx.md",
            "Nginx",
            "# Nginx\n\n## Scope {#scope}\n\nnginx redis cluster maintenance upstream",
            server_tags='["srv_nginx01"]',
        )
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            docker = store.search(
                "redis cluster maintenance",
                mode="lexical",
                filters=SearchFilters(server_id="srv_docker01"),
            )
            nginx = store.search(
                "redis cluster maintenance",
                mode="lexical",
                filters=SearchFilters(server_id="srv_nginx01"),
            )
            global_only = store.search(
                "redis cluster maintenance",
                mode="lexical",
                filters=SearchFilters(),
            )
        self.assertEqual({result.chunk.doc_id for result in docker}, {"global.md", "docker.md"})
        self.assertEqual({result.chunk.doc_id for result in nginx}, {"global.md", "nginx.md"})
        self.assertEqual(
            {result.chunk.doc_id for result in global_only},
            {"global.md", "docker.md", "nginx.md"},
        )

    def test_search_rejects_a_query_above_the_unique_token_budget(self) -> None:
        document(
            self.corpus / "tokens.md",
            "Tokens",
            "# Tokens\n\n## Budget {#budget}\n\nqx001 qx002",
        )
        accepted = " ".join(f"qx{index:03d}" for index in range(1, 65))
        rejected = accepted + " qx065"
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            self.assertTrue(store.search(accepted, mode="lexical"))
            with self.assertRaises(KnowledgeInputError) as caught:
                store.search(rejected, mode="lexical")
        self.assertEqual(caught.exception.code, "invalid_input")
        self.assertEqual(caught.exception.exit_code, 2)
        self.assertEqual(
            str(caught.exception),
            "query has 65 unique searchable tokens; limit is 64",
        )

    def test_no_answer_questions_do_not_enter_retrieval_averages(self) -> None:
        document(
            self.corpus / "oom.md",
            "Docker OOM",
            "# Docker OOM\n\n## Triage {#triage}\n\nOOMKilled cgroup memory",
        )
        document(
            self.corpus / "redis.md",
            "Redis timeout",
            "# Redis timeout\n\n## Timeouts {#timeouts}\n\nredis timeout upstream",
        )
        questions = [
            {
                "id": "scored",
                "question": "OOMKilled cgroup memory",
                "expected_doc_ids": ["oom.md"],
                "expected_section_ids": ["triage"],
                "forbidden_doc_ids": [],
                "filters": {},
            },
            {
                "id": "no-answer-hit",
                "question": "redis timeout",
                "expected_doc_ids": [],
                "expected_section_ids": [],
                "forbidden_doc_ids": [],
                "filters": {},
                "expect_no_answer": True,
            },
            {
                "id": "no-answer-abstain",
                "question": "quantum flux capacitor",
                "expected_doc_ids": [],
                "expected_section_ids": [],
                "forbidden_doc_ids": [],
                "filters": {},
                "expect_no_answer": True,
            },
        ]
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            report = evaluate(store, questions, modes=["lexical"], k=2)
        metrics = report["modes"]["lexical"]
        self.assertEqual(metrics["scoredQuestions"], 1)
        self.assertEqual(metrics["noAnswerQuestions"], 2)
        self.assertEqual(metrics["recall"], 1.0)
        self.assertEqual(metrics["precision"], 0.5)
        self.assertEqual(metrics["mrr"], 1.0)
        self.assertEqual(metrics["ndcg"], 1.0)
        self.assertEqual(metrics["sectionRecall"], 1.0)
        self.assertEqual(metrics["sectionHitCount"], 1)
        self.assertEqual(metrics["expectedSectionCount"], 1)
        self.assertEqual(metrics["noAnswerCorrect"], 1)
        self.assertEqual(metrics["noAnswerTotal"], 2)
        self.assertEqual(metrics["noAnswerFalsePositiveCount"], 1)
        self.assertEqual(metrics["abstainRate"], 0.5)
        self.assertEqual(metrics["falsePositiveRate"], 0.5)
        per_question = {item["id"]: item for item in metrics["perQuestion"]}
        self.assertIsNone(per_question["no-answer-hit"]["recall"])
        self.assertIsNone(per_question["no-answer-hit"]["precision"])
        self.assertTrue(per_question["no-answer-hit"]["falsePositive"])
        self.assertFalse(per_question["no-answer-hit"]["abstained"])
        self.assertTrue(per_question["no-answer-abstain"]["abstained"])

    def test_front_matter_metadata_and_acl_fail_closed(self) -> None:
        path = self.corpus / "bad.md"
        required = {
            "title": "Required metadata",
            "source": "https://example.invalid/reference",
            "updated_at": "2026-11-25",
            "environment": "demo",
            "server_tags": ["demo-server"],
            "acl": ["public"],
        }
        omitted = object()
        cases = [
            ("missing title", "title", omitted),
            ("empty title", "title", ""),
            ("missing source", "source", omitted),
            ("empty source", "source", ""),
            ("missing updated_at", "updated_at", omitted),
            ("empty updated_at", "updated_at", ""),
            ("missing environment", "environment", omitted),
            ("empty environment", "environment", ""),
            ("empty environment list", "environment", []),
            ("missing server_tags", "server_tags", omitted),
            ("missing acl", "acl", omitted),
            ("empty acl", "acl", []),
            ("empty acl entry", "acl", ["public", ""]),
            ("control character in acl", "acl", ["public\nadmin"]),
        ]
        for name, field, value in cases:
            with self.subTest(name=name):
                values = dict(required)
                if value is omitted:
                    del values[field]
                else:
                    values[field] = value
                path.write_text(
                    "+++\n"
                    + "\n".join(
                        f"{key} = {json.dumps(item, ensure_ascii=False)}"
                        for key, item in values.items()
                    )
                    + "\n+++\n\n# Heading still must not replace metadata\n\nalpha\n",
                    encoding="utf-8",
                )
                with self.assertRaises(KnowledgeInputError):
                    preview_index(self.corpus)

        path.write_text("# Heading\n\nalpha without front matter\n", encoding="utf-8")
        with self.assertRaisesRegex(KnowledgeInputError, "requires field"):
            preview_index(self.corpus)

        path.write_text(
            "+++\n"
            + "\n".join(
                f"{key} = {json.dumps(item, ensure_ascii=False)}"
                for key, item in {**required, "server_tags": []}.items()
            )
            + "\n+++\n\n# Global document\n\nalpha global\n",
            encoding="utf-8",
        )
        with KnowledgeStore(self.database, create=True) as store:
            self.assertEqual(store.index(self.corpus).indexed, 1)
            self.assertTrue(store.search(
                "alpha global",
                mode="lexical",
                filters=SearchFilters(server_id="any-server"),
            ))

    def test_expected_sections_are_anchored_to_their_declared_documents(self) -> None:
        document(
            self.corpus / "one.md",
            "One",
            "# One\n\n## Alpha {#alpha}\n\nalpha only\n\n## Shared {#shared}\n\nshared one",
        )
        document(
            self.corpus / "two.md",
            "Two",
            "# Two\n\n## Beta {#beta}\n\nbeta only\n\n## Shared {#shared}\n\nshared two",
        )
        base = {
            "id": "section-owner",
            "question": "alpha beta shared",
            "expected_doc_ids": ["one.md"],
            "expected_section_ids": ["alpha"],
            "forbidden_doc_ids": [],
            "filters": {"environment": "demo"},
        }
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            wrong_document = {**base, "expected_section_ids": ["beta"]}
            with self.assertRaisesRegex(KnowledgeInputError, "missing sections"):
                evaluate(store, [wrong_document], modes=["lexical"], k=2)

            uncovered_document = {
                **base,
                "expected_doc_ids": ["one.md", "two.md"],
                "expected_section_ids": ["alpha"],
            }
            with self.assertRaisesRegex(KnowledgeInputError, "each expected document"):
                evaluate(store, [uncovered_document], modes=["lexical"], k=2)

            ambiguous_section = {
                **base,
                "expected_doc_ids": ["one.md", "two.md"],
                "expected_section_ids": ["shared"],
            }
            with self.assertRaisesRegex(KnowledgeInputError, "more than one expected document"):
                evaluate(store, [ambiguous_section], modes=["lexical"], k=2)

    def test_section_hits_require_the_expected_section_not_only_the_document(self) -> None:
        document(
            self.corpus / "one.md",
            "One",
            "# One\n\n## Expected {#expected}\n\nalpha material\n\n"
            "## Wrong {#wrong}\n\nunique beta material",
        )
        questions = [
            {
                "id": "wrong-section",
                "question": "unique beta material",
                "expected_doc_ids": ["one.md"],
                "expected_section_ids": ["expected"],
                "forbidden_doc_ids": [],
                "filters": {"environment": "demo"},
            },
            {
                "id": "right-section",
                "question": "unique beta material",
                "expected_doc_ids": ["one.md"],
                "expected_section_ids": ["wrong"],
                "forbidden_doc_ids": [],
                "filters": {"environment": "demo"},
            },
        ]
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            report = evaluate(store, questions, modes=["lexical"], k=1)
        metrics = report["modes"]["lexical"]
        self.assertEqual(metrics["recall"], 1.0)
        self.assertEqual(metrics["sectionRecall"], 0.5)
        self.assertEqual(metrics["sectionHitCount"], 1)
        self.assertEqual(metrics["expectedSectionCount"], 2)
        per_question = {item["id"]: item for item in metrics["perQuestion"]}
        self.assertEqual(per_question["wrong-section"]["recall"], 1.0)
        self.assertEqual(per_question["wrong-section"]["sectionHitCount"], 0)
        self.assertEqual(per_question["wrong-section"]["sectionRecall"], 0.0)
        self.assertEqual(
            per_question["wrong-section"]["rankedSections"],
            [{"documentId": "one.md", "sectionId": "wrong"}],
        )
        self.assertEqual(per_question["right-section"]["sectionHitCount"], 1)

    def test_forbidden_hits_are_reported_separately_from_relevance(self) -> None:
        document(
            self.corpus / "expected.md",
            "Expected",
            "# Expected\n\n## Timeout {#timeout}\n\nredis timeout proxy",
        )
        document(
            self.corpus / "forbidden.md",
            "Forbidden",
            "# Forbidden\n\n## Password {#password}\n\nredis timeout password rotation",
        )
        document(
            self.corpus / "other.md",
            "Other",
            "# Other\n\n## Unrelated {#unrelated}\n\ndisk inode",
        )
        questions = [
            {
                "id": "scored-leak",
                "question": "redis timeout password",
                "expected_doc_ids": ["expected.md"],
                "expected_section_ids": ["timeout"],
                "forbidden_doc_ids": ["forbidden.md"],
                "filters": {},
            },
            {
                "id": "no-answer-leak",
                "question": "redis timeout password",
                "expected_doc_ids": [],
                "expected_section_ids": [],
                "forbidden_doc_ids": ["forbidden.md"],
                "filters": {},
                "expect_no_answer": True,
            },
        ]
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            report = evaluate(store, questions, modes=["lexical"], k=3)
        metrics = report["modes"]["lexical"]
        self.assertEqual(metrics["recall"], 1.0)
        self.assertEqual(metrics["precision"], round(1 / 3, 6))
        self.assertEqual(metrics["forbiddenHitCount"], 2)
        self.assertEqual(metrics["forbiddenLeakageCount"], 2)
        self.assertEqual(metrics["forbiddenQuestionCount"], 2)
        self.assertEqual(metrics["forbiddenHitsOnScored"], 1)
        self.assertEqual(metrics["forbiddenHitsOnNoAnswer"], 1)
        self.assertEqual(metrics["forbiddenDocIdHits"], {"forbidden.md": 2})
        self.assertEqual(metrics["aclLeakageCount"], 2)
        self.assertEqual(metrics["noAnswerCorrect"], 0)
        self.assertEqual(metrics["falsePositiveRate"], 1.0)

    def test_sample_evaluation_questions_fit_the_query_budget(self) -> None:
        questions = read_questions(REPO_ROOT / "examples" / "rag" / "eval" / "questions.jsonl")
        self.assertEqual(27, len(questions))
        oversized: list[str] = []
        for question in questions:
            try:
                query_token_weights(question["question"])
            except QueryTokenLimitError as error:
                oversized.append(f"{question['id']}:{error.count}")
        self.assertEqual([], oversized, "sample questions exceed the 64 unique token budget")

    def test_question_validation_rejects_missing_and_contradictory_references(self) -> None:
        document(
            self.corpus / "one.md",
            "One",
            "# One\n\n## Timeout {#timeout}\n\nalpha",
        )
        base = {
            "id": "valid",
            "question": "alpha",
            "expected_doc_ids": ["one.md"],
            "expected_section_ids": ["timeout"],
            "forbidden_doc_ids": [],
            "filters": {},
        }
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            evaluate(store, [base], modes=["lexical"], k=2)

            expected_missing_doc = {**base, "expected_doc_ids": ["missing.md"]}
            with self.assertRaisesRegex(KnowledgeInputError, "missing documents"):
                evaluate(store, [expected_missing_doc], modes=["lexical"], k=2)

            forbidden_missing_doc = {**base, "forbidden_doc_ids": ["missing.md"]}
            with self.assertRaisesRegex(KnowledgeInputError, "missing documents"):
                evaluate(store, [forbidden_missing_doc], modes=["lexical"], k=2)

            missing_section = {**base, "expected_section_ids": ["missing-section"]}
            with self.assertRaisesRegex(KnowledgeInputError, "missing sections"):
                evaluate(store, [missing_section], modes=["lexical"], k=2)

            overlap = {**base, "forbidden_doc_ids": ["one.md"]}
            with self.assertRaisesRegex(KnowledgeInputError, "both expected and forbidden"):
                evaluate(store, [overlap], modes=["lexical"], k=2)

            no_answer_with_expected = {
                **base,
                "expect_no_answer": True,
            }
            with self.assertRaisesRegex(KnowledgeInputError, "cannot expect documents"):
                evaluate(store, [no_answer_with_expected], modes=["lexical"], k=2)

        questions_path = self.root / "questions.jsonl"
        questions_path.write_text(json.dumps(base) + "\n", encoding="utf-8")
        self.assertEqual(read_questions(questions_path), [base])
        missing_sections = {
            "id": "missing-sections",
            "question": "alpha",
            "expected_doc_ids": ["one.md"],
            "forbidden_doc_ids": [],
        }
        questions_path.write_text(json.dumps(missing_sections) + "\n", encoding="utf-8")
        with self.assertRaisesRegex(KnowledgeInputError, "expected_section_ids"):
            read_questions(questions_path)
        missing_id = {
            "question": "alpha",
            "expected_doc_ids": ["one.md"],
            "expected_section_ids": ["timeout"],
            "forbidden_doc_ids": [],
        }
        questions_path.write_text(json.dumps(missing_id) + "\n", encoding="utf-8")
        with self.assertRaisesRegex(KnowledgeInputError, "requires string id and question"):
            read_questions(questions_path)

    def test_location_and_database_owner_boundaries_are_enforced(self) -> None:
        with self.assertRaises(KnowledgeStorageError):
            validate_database_path(self.root / "agent-state" / "knowledge.sqlite")
        with self.assertRaises(KnowledgeStorageError):
            validate_database_path(self.root / "yukinal.db")
        with self.assertRaises(KnowledgeStorageError):
            KnowledgeStore(self.root / "missing.sqlite")

    def test_index_is_idempotent_and_front_matter_changes_rebuild(self) -> None:
        source = self.corpus / "disk.md"
        document(
            source,
            "Disk full",
            "# Disk full\n\n## Triage {#triage}\n\nCheck df, du and deleted open files.",
        )
        with KnowledgeStore(self.database, create=True) as store:
            first = store.index(self.corpus)
            self.assertEqual((first.indexed, first.unchanged, first.chunks), (1, 0, 1))
            second = store.index(self.corpus)
            self.assertEqual((second.indexed, second.unchanged), (0, 1))
            document(
                source,
                "Disk full",
                "# Disk full\n\n## Triage {#triage}\n\nCheck df, du and deleted open files.",
                acl='["alice"]',
            )
            third = store.index(self.corpus)
            self.assertEqual(third.indexed, 1)
            self.assertEqual(store.search("disk", filters=SearchFilters()), [])
            self.assertTrue(store.search("disk", filters=SearchFilters(principal="alice")))

    def test_environment_lists_and_server_tags_filter_at_read_time(self) -> None:
        document(
            self.corpus / "prod.md",
            "Production disk",
            "# Production disk\n\n## Triage {#triage}\n\nUse the production disk runbook.",
            environment='["prod", "staging"]',
            server_tags='["srv-a"]',
            extra='tags = ["disk", "critical"]\n',
        )
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            self.assertTrue(store.search(
                "production disk",
                filters=SearchFilters(environment="staging", server_id="srv-a", tags=("disk",)),
            ))
            self.assertEqual(store.search(
                "production disk",
                filters=SearchFilters(environment="dev", server_id="srv-a"),
            ), [])
            self.assertEqual(store.search(
                "production disk",
                filters=SearchFilters(environment="prod", server_id="srv-b"),
            ), [])

    def test_acl_filter_never_returns_private_documents_without_principal(self) -> None:
        document(self.corpus / "public.md", "Public", "# Public\n\nredis timeout")
        document(
            self.corpus / "private.md",
            "Private",
            "# Private\n\nredis password rotation",
            acl='["alice"]',
        )
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            public = store.search("redis", filters=SearchFilters())
            self.assertEqual({result.chunk.doc_id for result in public}, {"public.md"})
            allowed = store.search("redis", filters=SearchFilters(principal="alice"))
            self.assertEqual({result.chunk.doc_id for result in allowed}, {"public.md", "private.md"})

    def test_sample_corpus_acl_fixture_stays_private_and_is_scoped_to_its_servers(self) -> None:
        self.assertTrue(SAMPLE_CORPUS.is_dir(), "the checked-in RAG sample corpus must exist")
        query = "Nginx 502 upstream timed out proxy_read_timeout upstream_response_time 重试"
        with KnowledgeStore(self.database, create=True) as store:
            report = store.index(SAMPLE_CORPUS)
            self.assertGreater(report.indexed, 0)
            public = store.search(
                query,
                filters=SearchFilters(
                    environment="demo", server_id="srv_nginx01", principal="public"
                ),
                limit=10,
            )
            self.assertNotIn(PRIVATE_TIMEOUT_RUNBOOK, {result.chunk.doc_id for result in public})
            team = store.search(
                query,
                filters=SearchFilters(
                    environment="demo", server_id="srv_nginx01", principal="team-ops"
                ),
                limit=10,
            )
            self.assertIn(PRIVATE_TIMEOUT_RUNBOOK, {result.chunk.doc_id for result in team})
            other_server = store.search(
                query,
                filters=SearchFilters(
                    environment="demo", server_id="srv_docker01", principal="team-ops"
                ),
                limit=10,
            )
            self.assertNotIn(
                PRIVATE_TIMEOUT_RUNBOOK, {result.chunk.doc_id for result in other_server}
            )
            global_query = "只读证据 时间线 回滚 安全检查"
            scoped_globals = store.search(
                global_query,
                mode="lexical",
                filters=SearchFilters(
                    environment="demo", server_id="srv_nginx01", principal="team-ops"
                ),
                limit=20,
            )
            self.assertTrue(
                {"global/change-safety-checklist.md", "global/incident-evidence-and-log-safety.md"}
                <= {result.chunk.doc_id for result in scoped_globals}
            )
            global_only = store.search(
                global_query,
                mode="lexical",
                filters=SearchFilters(
                    environment="demo", global_only=True, principal="team-ops"
                ),
                limit=20,
            )
            self.assertEqual(
                {result.chunk.doc_id for result in global_only},
                {"global/change-safety-checklist.md", "global/incident-evidence-and-log-safety.md"},
            )

    def test_stale_documents_are_removed_and_reset_is_atomic(self) -> None:
        first = self.corpus / "one.md"
        second = self.corpus / "two.md"
        document(first, "One", "# One\n\nalpha")
        document(second, "Two", "# Two\n\nbeta")
        with KnowledgeStore(self.database, create=True) as store:
            self.assertEqual(store.index(self.corpus).indexed, 2)
            second.unlink()
            report = store.index(self.corpus)
            self.assertEqual((report.indexed, report.removed), (0, 1))
            self.assertEqual(store.stats()["documents"], 1)
            report = store.index(self.corpus, reset=True)
            self.assertEqual((report.indexed, report.unchanged), (1, 0))
            self.assertEqual(store.stats()["documents"], 1)

    def test_delete_removes_chunks_and_reports_missing_ids(self) -> None:
        document(self.corpus / "one.md", "One", "# One\n\nalpha")
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            result = store.delete_documents(["one.md", "missing.md"])
            self.assertEqual(result, {"deleted": ["one.md"], "missing": ["missing.md"]})
            self.assertEqual(store.stats()["chunks"], 0)
            self.assertEqual(store.connection.execute("PRAGMA quick_check").fetchone()[0], "ok")

    def test_corpus_rejects_invalid_utf8_unknown_front_matter_types_and_escape_symlinks(self) -> None:
        (self.corpus / "bad.md").write_bytes(b"\xff\xfe\x00")
        with self.assertRaises(KnowledgeInputError):
            preview_index(self.corpus)
        (self.corpus / "bad.md").unlink()
        (self.corpus / "bad.md").write_text(
            "+++\ntitle = [\"not\", \"a\", \"string\"]\n+++\n\n# Heading\n",
            encoding="utf-8",
        )
        with self.assertRaises(KnowledgeInputError):
            preview_index(self.corpus)
        (self.corpus / "bad.md").unlink()
        outside = self.root / "outside.md"
        document(outside, "Outside", "# Outside\n\nsecret")
        document(self.corpus / "valid.md", "Valid", "# Valid\n\npublic content")
        try:
            os.symlink(outside, self.corpus / "escape.md")
        except (OSError, NotImplementedError):
            self.skipTest("symbolic links are unavailable on this platform")
        with self.assertRaises(KnowledgeInputError):
            preview_index(self.corpus)

    def test_preview_does_not_create_a_database(self) -> None:
        document(self.corpus / "one.md", "One", "# One\n\nalpha")
        report = preview_index(self.corpus)
        self.assertEqual((report.scanned, report.indexed, report.chunks), (1, 1, 1))
        self.assertFalse(self.database.exists())

    def test_metadata_serialization_supports_toml_dates_without_weakening_bounds(self) -> None:
        document(
            self.corpus / "dated.md",
            "Dated",
            "# Dated\n\nalpha",
            extra="reviewed_on = 2026-11-25\n",
        )
        with KnowledgeStore(self.database, create=True) as store:
            store.index(self.corpus)
            payload = store.search("alpha", mode="lexical", limit=1)[0].as_dict()
        self.assertEqual(payload["title"], "Dated")


if __name__ == "__main__":
    unittest.main()
