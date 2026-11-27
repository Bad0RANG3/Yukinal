"""Explicit memory lifecycle: expiry, version chain, origins and the model tools.

The store and the model-facing tools are tested together because the interesting
failure is a disagreement between them: an expired memory that still reaches
retrieval, a `supersedes` link that dangles, or a proposal that writes without the
operator's approval.
"""

from __future__ import annotations

import asyncio
import sqlite3
import tempfile
import unittest
from datetime import UTC, datetime, timedelta
from pathlib import Path

from yukinal_agent import MemoryAuditSink, ToolCallError, ToolRegistry
from yukinal_agent.conversation_store import ConversationStore
from yukinal_agent.memory_tools import register_memory_tools
from yukinal_agent.tools import ExecutionContext


def _context(*, tools: tuple[str, ...] = ("memory.search", "memory.propose"), server: str | None = None,
             approvals: dict[str, str] | None = None) -> ExecutionContext:
    return ExecutionContext(
        task_id="task-memory",
        tenant_id="local",
        user_id="user-memory",
        allowed_tools=frozenset(tools),
        allowed_target_ids=frozenset({server}) if server else frozenset(),
        approved_actions=approvals or {},
        host_target={"host": "remote", "serverId": server} if server else {"host": "local"},
        host_task_id=None,
        context_target_id=server,
        run_id="run-memory",
        trace_id="trace-memory",
    )


def _instant(offset_seconds: int) -> str:
    return (datetime.now(UTC) + timedelta(seconds=offset_seconds)).isoformat()


class MemoryLifecycleTest(unittest.TestCase):
    def setUp(self) -> None:
        self.store = ConversationStore()

    def tearDown(self) -> None:
        self.store.close()

    def test_expired_memory_leaves_retrieval_but_stays_on_disk(self) -> None:
        self.store.save_memory(scope="global", server_id=None, title="旧约定", content="旧的数据库约定",
                               expires_at=_instant(-60))
        self.store.save_memory(scope="global", server_id=None, title="新约定", content="新的数据库约定",
                               expires_at=_instant(600))
        self.store.save_memory(scope="global", server_id=None, title="永久约定", content="永久有效的数据库约定")

        self.assertEqual([memory["title"] for memory in self.store.list_memories()], ["永久约定", "新约定"])
        # The search path filters too: `list_memories(query=...)` goes through the
        # knowledge index, which must agree with the plain listing.
        searched = self.store.list_memories(query="数据库约定")
        self.assertNotIn("旧约定", [memory["title"] for memory in searched])
        memories, sources = self.store.memory_context(None, "数据库约定")
        self.assertNotIn("旧的数据库约定", str(memories))
        self.assertNotIn("旧约定", str(sources))
        # The audit trail keeps the expired row; only retrieval hides it.
        remaining = self.store._connection.execute(
            "SELECT title FROM saved_memories WHERE expires_at IS NOT NULL ORDER BY title").fetchall()
        self.assertEqual([row["title"] for row in remaining], ["新约定", "旧约定"])

    def test_expiry_is_validated_and_normalized(self) -> None:
        for value in ("2030-01-01", "2030-01-01T00:00:00", "not-a-date", ""):
            with self.assertRaises(ValueError):
                self.store.save_memory(scope="global", server_id=None, title="t", content="c", expires_at=value)
        zone = self.store.save_memory(scope="global", server_id=None, title="t", content="c",
                                      expires_at="2030-01-01T08:00:00+08:00")
        self.assertEqual(zone["expiresAt"], "2030-01-01T00:00:00+00:00")
        naive = self.store.save_memory(scope="global", server_id=None, title="t2", content="c2",
                                       expires_at="2030-01-01T00:00:00Z")
        self.assertEqual(naive["expiresAt"], "2030-01-01T00:00:00+00:00")

    def test_version_chain_requires_a_live_predecessor_in_the_same_scope(self) -> None:
        original = self.store.save_memory(scope="global", server_id=None, title="连接池", content="上限 32")
        revised = self.store.save_memory(scope="global", server_id=None, title="连接池", content="上限 48",
                                          supersedes=original["id"], origin="model")
        self.assertEqual(revised["supersedes"], original["id"])
        self.assertEqual(revised["origin"], "model")
        self.assertEqual(self.store.list_memories()[0]["supersedes"], original["id"])
        with self.assertRaises(ValueError):
            self.store.save_memory(scope="global", server_id=None, title="bad", content="bad", supersedes="mem_missing")
        with self.assertRaises(ValueError) as self_link:
            self.store.save_memory(scope="global", server_id=None, title="bad", content="bad",
                                    memory_id=original["id"], supersedes=original["id"])
        self.assertEqual(str(self_link.exception), "a memory cannot supersede itself")
        self.store.save_memory(scope="server", server_id="srv_alpha", title="连接池", content="上限 24")
        with self.assertRaises(ValueError) as error:
            self.store.save_memory(scope="server", server_id="srv_alpha", title="连接池", content="上限 64",
                                    supersedes=original["id"])
        self.assertEqual(str(error.exception), "superseded memory must exist in the same scope")

    def test_origin_is_provenance_only(self) -> None:
        with self.assertRaises(ValueError):
            self.store.save_memory(scope="global", server_id=None, title="t", content="c", origin="system")
        memory = self.store.save_memory(scope="global", server_id=None, title="t", content="c")
        self.assertEqual(memory["origin"], "user")
        self.assertEqual(self.store.list_memories()[0]["origin"], "user")

    def test_legacy_database_gains_the_new_columns_in_place(self) -> None:
        legacy = Path(tempfile.mkdtemp()) / "legacy.sqlite"
        raw = sqlite3.connect(legacy)
        raw.execute(
            "CREATE TABLE saved_memories (id TEXT PRIMARY KEY, scope TEXT NOT NULL, server_id TEXT NOT NULL,"
            " title TEXT NOT NULL, content TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)")
        raw.execute("INSERT INTO saved_memories VALUES ('mem_legacy','global','','Legacy','Old row','2026-01-01','2026-01-01')")
        raw.commit()
        raw.close()

        upgraded = ConversationStore(legacy)
        columns = {row["name"] for row in upgraded._connection.execute("PRAGMA table_info(saved_memories)")}
        self.assertTrue({"expires_at", "supersedes", "origin"} <= columns)
        kept = upgraded.list_memories()
        self.assertEqual([(memory["id"], memory["origin"]) for memory in kept], [("mem_legacy", "user")])
        upgraded.save_memory(scope="global", server_id=None, title="修订", content="新值", supersedes="mem_legacy")
        upgraded.close()


class MemoryToolTest(unittest.TestCase):
    def setUp(self) -> None:
        self.store = ConversationStore()
        self.registry = ToolRegistry(MemoryAuditSink())
        self.names = register_memory_tools(self.registry, self.store)

    def tearDown(self) -> None:
        self.store.close()

    def test_registration_is_idempotent_and_write_needs_approval(self) -> None:
        handlers = {spec.name: spec.handler for spec in self.registry.list()}
        self.assertEqual(self.names, frozenset({"memory.search", "memory.propose"}))
        self.assertEqual(register_memory_tools(self.registry, self.store), frozenset())
        self.assertEqual({spec.name: spec.handler for spec in self.registry.list()}, handlers)
        propose = self.registry.get("memory.propose")
        search = self.registry.get("memory.search")
        self.assertEqual((propose.risk_level, propose.requires_approval, propose.parallel_safe), ("write", True, False))
        self.assertEqual((search.risk_level, search.requires_approval, search.parallel_safe), ("read", False, True))
        # Neither tool accepts a model-supplied target: scope comes from the run.
        self.assertIsNone(propose.target_id_argument)
        self.assertIsNone(search.target_id_argument)

    def test_search_returns_provenance_without_content(self) -> None:
        stored = self.store.save_memory(scope="global", server_id=None, title="约定", content="先读证据")
        result = asyncio.run(self.registry.execute("memory.search", {"scope": "global", "query": "约定"},
                                                  _context()))
        self.assertEqual(result.value["returned"], 1)
        entry = result.value["memories"][0]
        self.assertEqual(entry["id"], stored["id"])
        self.assertNotIn("content", entry)

    def test_server_scope_requires_the_run_target(self) -> None:
        self.store.save_memory(scope="server", server_id="srv_alpha", title="路径", content="/srv/app")
        with self.assertRaises(ToolCallError) as error:
            asyncio.run(self.registry.execute("memory.search", {"scope": "server"}, _context()))
        self.assertEqual(error.exception.code, "target_required")
        allowed = asyncio.run(self.registry.execute("memory.search", {"scope": "server"}, _context(server="srv_alpha")))
        self.assertEqual(allowed.value["memories"][0]["serverId"], "srv_alpha")
        # A different run target cannot read the other server's memory.
        other = asyncio.run(self.registry.execute("memory.search", {"scope": "server"}, _context(server="srv_beta")))
        self.assertEqual(other.value, {"memories": [], "returned": 0})

    def test_propose_is_blocked_until_the_operator_approves_this_call(self) -> None:
        payload = {"scope": "global", "title": "模型提议", "content": "候选结论"}
        with self.assertRaises(ToolCallError) as error:
            asyncio.run(self.registry.execute("memory.propose", payload, _context()))
        self.assertEqual(error.exception.code, "approval_required")
        self.assertEqual(self.store.list_memories(), [])
        approved = _context(approvals={error.exception.approval_fingerprint: "grant-1"})
        result = asyncio.run(self.registry.execute("memory.propose", payload, approved))
        self.assertEqual(result.value["origin"], "model")
        self.assertEqual(self.store.list_memories()[0]["origin"], "model")
        # The grant is single-use: the same fingerprint may not run twice.
        with self.assertRaises(ToolCallError) as replay:
            asyncio.run(self.registry.execute("memory.propose", payload, approved))
        self.assertEqual(replay.exception.code, "approval_required")

    def test_propose_rejects_invalid_expiry_and_dangling_supersedes(self) -> None:
        for index, payload in enumerate((
            {"scope": "global", "title": "t", "content": "c", "expiresAt": "tomorrow"},
            {"scope": "global", "title": "t", "content": "c", "supersedes": "mem_missing"},
            {"scope": "server", "title": "t", "content": "c"},
        )):
            # Approval is checked before the handler runs, so a rejected proposal is
            # proven by approving this exact call and watching the handler refuse it.
            with self.assertRaises(ToolCallError) as pending:
                asyncio.run(self.registry.execute("memory.propose", payload, _context()))
            self.assertEqual(pending.exception.code, "approval_required")
            # A grant reference is single-use per run, so each case needs its own.
            approved = _context(approvals={pending.exception.approval_fingerprint: f"grant-invalid-{index}"})
            with self.assertRaises(ToolCallError) as error:
                asyncio.run(self.registry.execute("memory.propose", payload, approved))
            self.assertIn(error.exception.code, ("invalid_arguments", "target_required"))
        self.assertEqual(self.store.list_memories(), [])


if __name__ == "__main__":
    unittest.main()
