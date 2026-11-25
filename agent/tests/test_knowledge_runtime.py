from __future__ import annotations

import contextlib
import io
import json
import os
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.sidecar import _conversation_store, _knowledge_database_path
from yukinal_knowledge import KnowledgeStore

REPO_ROOT = Path(__file__).resolve().parents[2]
SHARED_CONTEXT_FIXTURE = REPO_ROOT / "packages" / "shared" / "fixtures" / "agent" / "context_knowledge_sources.json"


def _write_document(
    path: Path,
    title: str,
    body: str,
    *,
    acl: str = '["public"]',
    server_tags: str = "[]",
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "+++\n"
        f'title = "{title}"\n'
        'source = "https://example.invalid/reference"\n'
        'updated_at = "2026-11-25"\n'
        'environment = "demo"\n'
        f"server_tags = {server_tags}\n"
        f"acl = {acl}\n"
        "+++\n\n"
        f"{body.strip()}\n",
        encoding="utf-8",
    )


def _retrieval_payload(message: object) -> dict[str, object]:
    content = getattr(message, "content")
    start = content.find("{")
    if start < 0:
        raise AssertionError("knowledge context is not a JSON envelope")
    payload, end = json.JSONDecoder().raw_decode(content[start:])
    if content[start + end:].strip():
        raise AssertionError("knowledge context contains data outside its envelope")
    return payload


def _mutate_database(path: Path, statement: str, parameters: tuple[object, ...] = ()) -> None:
    connection = sqlite3.connect(path)
    try:
        connection.execute("PRAGMA journal_mode = DELETE").fetchone()
        connection.execute(statement, parameters)
        connection.commit()
    finally:
        connection.close()


class RuntimeKnowledgeIntegrationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.corpus = self.root / "corpus"
        self.knowledge = self.root / "knowledge.sqlite"
        self.conversation = self.root / "conversations.sqlite"

    def _index(self) -> None:
        with KnowledgeStore(self.knowledge, create=True) as store:
            report = store.index(self.corpus)
            self.assertEqual(report.errors, ())

    def _open(self) -> ConversationStore:
        return ConversationStore(self.conversation, knowledge_database=self.knowledge)

    def test_public_corpus_result_reaches_context_without_body_or_acl_metadata(self) -> None:
        _write_document(
            self.corpus / "nginx" / "timeout.md",
            "Public nginx timeout guide",
            "# Public nginx timeout guide\n\n"
            "## Triage {#triage}\n\n"
            "public corpus needle restart nginx upstream",
        )
        self._index()
        store = self._open()
        try:
            self.assertTrue(store.corpus_enabled)
            store.save_memory(
                scope="global",
                server_id=None,
                title="Explicit memory",
                content="explicit memory needle",
            )
            messages, sources = store.memory_context(
                "srv_alpha",
                "public corpus needle explicit memory needle",
                max_bytes=16 * 1024,
            )
            self.assertEqual(len(messages), 1)
            payload = _retrieval_payload(messages[0])
            items = payload["retrieval"]["items"]  # type: ignore[index]
            self.assertTrue(any("public corpus needle" in item["text"] for item in items))
            self.assertTrue(any(item["title"] == "Explicit memory" for item in items))
            self.assertEqual(
                [source["title"] for source in sources[:2]],
                ["Explicit memory", "Public nginx timeout guide"],
            )

            corpus_source = next(
                source for source in sources if source["title"] == "Public nginx timeout guide"
            )
            self.assertEqual(corpus_source["scope"], "knowledge")
            self.assertEqual(corpus_source["serverId"], "srv_alpha")
            self.assertEqual(corpus_source["section"], "Public nginx timeout guide › Triage")
            self.assertIn("chunkId", corpus_source)
            self.assertNotIn("acl", corpus_source)
            self.assertNotIn("text", corpus_source)
            self.assertNotIn("content", corpus_source)
        finally:
            store.close()

    def test_private_corpus_results_require_a_principal_and_are_never_returned(self) -> None:
        _write_document(
            self.corpus / "private.md",
            "Private runbook",
            "# Private runbook\n\nprivate-only credential marker",
            acl='["team-ops"]',
        )
        self._index()
        store = self._open()
        try:
            self.assertEqual(store.memory_context(None, "private-only credential marker"), ([], []))
        finally:
            store.close()

    def test_corpus_server_tags_are_scoped_for_server_and_global_conversations(self) -> None:
        _write_document(
            self.corpus / "global.md",
            "Global database baseline",
            "# Global database baseline\n\nglobal cluster database marker",
        )
        _write_document(
            self.corpus / "alpha.md",
            "Alpha database node",
            "# Alpha database node\n\nalpha node database marker",
            server_tags='["srv_alpha"]',
        )
        _write_document(
            self.corpus / "beta.md",
            "Beta database node",
            "# Beta database node\n\nbeta node database marker",
            server_tags='["srv_beta"]',
        )
        self._index()
        store = self._open()
        try:
            alpha_messages, alpha_sources = store.memory_context("srv_alpha", "database marker")
            self.assertEqual(
                {source["title"] for source in alpha_sources},
                {"Global database baseline", "Alpha database node"},
            )
            self.assertNotIn("beta node database marker", str(alpha_messages))

            global_messages, global_sources = store.memory_context(None, "database marker")
            self.assertEqual(
                {source["title"] for source in global_sources},
                {"Global database baseline"},
            )
            self.assertNotIn("alpha node database marker", str(global_messages))
            self.assertNotIn("beta node database marker", str(global_messages))
        finally:
            store.close()

    def test_invalid_configured_corpus_disables_only_corpus_retrieval(self) -> None:
        data_dir = self.root / "data"
        invalid = data_dir / "knowledge" / "knowledge.sqlite"
        invalid.parent.mkdir(parents=True)
        invalid.write_bytes(b"not a sqlite database")
        output = io.StringIO()
        with mock.patch.dict(os.environ, {"YUKINAL_KNOWLEDGE_DB": ""}):
            self.assertEqual(_knowledge_database_path(data_dir), invalid)
            with contextlib.redirect_stderr(output):
                store = _conversation_store(data_dir)
        self.assertIsNotNone(store)
        assert store is not None
        try:
            self.assertFalse(store.corpus_enabled)
            store.save_memory(
                scope="global",
                server_id=None,
                title="Explicit fallback",
                content="explicit fallback marker",
            )
            messages, sources = store.memory_context(None, "explicit fallback marker")
            self.assertIn("explicit fallback marker", str(messages))
            self.assertEqual([source["title"] for source in sources], ["Explicit fallback"])
        finally:
            store.close()
        self.assertIn("corpus retrieval is disabled", output.getvalue())

    def test_corrupt_corpus_disables_only_corpus_retrieval(self) -> None:
        _write_document(
            self.corpus / "public.md",
            "Public corpus",
            "# Public corpus\n\nCORPUS_BODY_SECRET public corpus needle",
        )
        self._index()
        _mutate_database(
            self.knowledge,
            "UPDATE knowledge_chunks SET vector = ?",
            (sqlite3.Binary(b"\x00"),),
        )
        store = self._open()
        try:
            self.assertTrue(store.corpus_enabled)
            store.save_memory(
                scope="global",
                server_id=None,
                title="Explicit fallback",
                content="explicit fallback marker",
            )
            with self.assertLogs("yukinal_agent.conversation_store", level="WARNING") as logs:
                messages, sources = store.memory_context(
                    None,
                    "public corpus needle explicit fallback marker",
                )
            self.assertIn("explicit fallback marker", str(messages))
            self.assertEqual([source["title"] for source in sources], ["Explicit fallback"])
            self.assertFalse(store.corpus_enabled)
        finally:
            store.close()
        self.assertIn("Public knowledge corpus disabled", logs.output[0])
        self.assertNotIn("CORPUS_BODY_SECRET", "\n".join(logs.output))

    def test_legacy_oversized_memory_is_skipped_without_blocking_startup(self) -> None:
        seed = ConversationStore(self.conversation)
        seed.close()
        now = "2026-11-25T00:00:00+00:00"
        oversized = "OVERSIZED_BODY_SECRET " + "x" * 200_000
        _mutate_database(
            self.conversation,
            "INSERT INTO saved_memories"
            "(id, scope, server_id, title, content, created_at, updated_at) "
            "VALUES (?, 'global', '', ?, ?, ?, ?)",
            ("mem_legacy_large", "Legacy oversized", oversized, now, now),
        )
        _mutate_database(
            self.conversation,
            "INSERT INTO saved_memories"
            "(id, scope, server_id, title, content, created_at, updated_at) "
            "VALUES (?, 'global', '', ?, ?, ?, ?)",
            ("mem_legacy_normal", "Legacy normal", "normal legacy marker", now, now),
        )
        with self.assertLogs("yukinal_agent.conversation_store", level="WARNING") as logs:
            store = ConversationStore(self.conversation)
        try:
            self.assertEqual(len(store.list_memories()), 2)
            messages, sources = store.memory_context(None, "normal legacy marker")
            self.assertIn("normal legacy marker", str(messages))
            self.assertEqual([source["title"] for source in sources], ["Legacy normal"])
            self.assertNotIn("legacy oversized", str(messages).casefold())
        finally:
            store.close()
        warning = "\n".join(logs.output)
        self.assertIn("not searchable", warning)
        self.assertNotIn("OVERSIZED_BODY_SECRET", warning)

    def test_save_memory_rejects_oversized_input_without_partial_index(self) -> None:
        store = ConversationStore(self.conversation)
        try:
            with self.assertRaisesRegex(ValueError, "at most 12000 characters"):
                store.save_memory(
                    scope="global",
                    server_id=None,
                    title="Too large",
                    content="x" * 12_001,
                )
            with self.assertRaisesRegex(ValueError, "at most 200 characters"):
                store.save_memory(
                    scope="global",
                    server_id=None,
                    title="t" * 201,
                    content="short content",
                )
            self.assertEqual(store.list_memories(), [])
        finally:
            store.close()

    def test_shared_context_fixture_matches_the_retrieval_payload(self) -> None:
        # The TypeScript gates parse this fixture; if the producer's keys or the
        # knowledge-source shape change, the desktop gate would reject the payload and
        # the run would fail to reach `completed`. Keep the two sides on the same bytes.
        _write_document(
            self.corpus / "nginx" / "timeout.md",
            "Nginx timeout guide",
            "# Nginx timeout guide\n\n"
            "## Triage\n\n"
            "raise proxy_read_timeout and restart nginx when upstream latency spikes",
            server_tags='["srv_alpha"]',
        )
        self._index()
        store = self._open()
        try:
            store.save_memory(
                scope="global",
                server_id=None,
                title="Explicit memory",
                content="explicit memory needle",
            )
            _, sources = store.memory_context(
                "srv_alpha",
                "nginx timeout needle explicit memory needle",
                max_bytes=16 * 1024,
            )
        finally:
            store.close()
        fixture = json.loads(SHARED_CONTEXT_FIXTURE.read_text(encoding="utf-8"))
        live_by_scope = {source["scope"]: source for source in sources}
        fixture_by_scope = {source["scope"]: source for source in fixture}
        self.assertEqual(set(live_by_scope), set(fixture_by_scope))
        self.assertEqual(live_by_scope["knowledge"], fixture_by_scope["knowledge"])
        self.assertEqual(
            sorted(live_by_scope["global"]),
            sorted(fixture_by_scope["global"]),
        )

    def test_oversized_query_degrades_the_turn_but_keeps_the_corpus_open(self) -> None:
        _write_document(
            self.corpus / "public.md",
            "Public corpus",
            "# Public corpus\n\n## Scope {#scope}\n\npublic corpus needle",
        )
        self._index()
        store = self._open()
        try:
            self.assertTrue(store.corpus_enabled)
            store.save_memory(
                scope="global",
                server_id=None,
                title="Explicit fallback",
                content="explicit fallback marker",
            )
            oversized = " ".join(f"qx{index:03d}" for index in range(1, 66))
            with self.assertLogs("yukinal_agent.conversation_store", level="WARNING") as logs:
                messages, sources = store.memory_context(None, oversized)
            self.assertEqual([source["title"] for source in sources], ["Explicit fallback"])
            payload = _retrieval_payload(messages[0])
            self.assertEqual(payload["retrieval"]["mode"], "recent-low-confidence")  # type: ignore[index]
            self.assertTrue(store.corpus_enabled)
        finally:
            store.close()
        self.assertIn("degraded to recent explicit memories", logs.output[0])

    def test_corpus_retrieval_treats_an_oversized_query_as_input_not_damage(self) -> None:
        _write_document(
            self.corpus / "public.md",
            "Public corpus",
            "# Public corpus\n\n## Scope {#scope}\n\npublic corpus needle",
        )
        self._index()
        store = self._open()
        oversized = " ".join(f"qx{index:03d}" for index in range(1, 66))
        try:
            self.assertTrue(store.corpus_enabled)
            self.assertEqual(store._search_corpus(None, oversized), [])
            self.assertTrue(store.corpus_enabled)
            self.assertTrue(store._search_corpus(None, "public corpus needle"))
        finally:
            store.close()

    def test_list_memories_rejects_an_oversized_query_as_invalid_input(self) -> None:
        store = ConversationStore(self.conversation)
        try:
            store.save_memory(
                scope="global",
                server_id=None,
                title="Explicit fallback",
                content="explicit fallback marker",
            )
            accepted = " ".join(f"qx{index:03d}" for index in range(1, 65))
            self.assertEqual(store.list_memories(query=accepted), [])
            with self.assertRaises(ValueError) as caught:
                store.list_memories(query=accepted + " qx065")
        finally:
            store.close()
        self.assertEqual(
            str(caught.exception),
            "query has 65 unique searchable tokens; limit is 64",
        )

    def test_explicit_corpus_path_overrides_the_default_location(self) -> None:
        explicit = self.root / "custom" / "knowledge.sqlite"
        with mock.patch.dict(os.environ, {"YUKINAL_KNOWLEDGE_DB": str(explicit)}):
            self.assertEqual(_knowledge_database_path(self.root), explicit)


if __name__ == "__main__":
    unittest.main()
