"""Control-plane API tests.

They run against SQLite so no PostgreSQL server is needed, but they exercise the
real FastAPI app, the real SQLAlchemy models and the real Alembic migration. The
partial unique index that serialises tasks per node, the tenant scoping and the
credential boundary are all asserted as behaviour, not just as schema.
"""

from __future__ import annotations

import unittest
from pathlib import Path

import sqlalchemy
from alembic import command
from alembic.config import Config
from fastapi.testclient import TestClient
from sqlalchemy import inspect
from sqlalchemy.ext.asyncio import create_async_engine
from sqlalchemy.pool import NullPool

from support import AUTH, SERVICE_ROOT, TOKEN, ControlPlaneTestCase, new_key

from yukinal_control_plane import actions
from yukinal_control_plane.app import create_app
from yukinal_control_plane.config import Settings
from yukinal_control_plane.db import Base


class ControlPlaneTests(ControlPlaneTestCase):
    # ── health and auth ──────────────────────────────────────────────────────

    def test_health_is_open(self) -> None:
        response = self.client.get("/healthz")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["status"], "ok")

    def test_mutations_require_the_token(self) -> None:
        self.assertEqual(self.client.get("/v1/tenants").status_code, 401)
        self.assertEqual(
            self.client.get("/v1/tenants", headers={"Authorization": "Bearer wrong"}).status_code,
            401,
        )
        self.assertEqual(self.client.get("/v1/tenants", headers=AUTH).status_code, 200)

    def test_app_refuses_to_start_without_a_token(self) -> None:
        with self.assertRaises(RuntimeError):
            create_app(Settings(database_url=self.url), engine=self.engine)

    def test_startup_verifies_the_schema(self) -> None:
        with TestClient(self.app) as client:
            self.assertEqual(client.get("/healthz").status_code, 200)

    def test_startup_rejects_an_unmigrated_database(self) -> None:
        fresh = Path(self._temporary.name) / "empty.db"
        url = f"sqlite+aiosqlite:///{fresh.as_posix()}"
        engine = create_async_engine(url, poolclass=NullPool)
        app = create_app(Settings(database_url=url, token=TOKEN), engine=engine)
        with self.assertRaises(RuntimeError) as raised:
            with TestClient(app):
                pass
        self.assertIn("alembic upgrade head", str(raised.exception))

    # ── tenants ──────────────────────────────────────────────────────────────

    def test_tenant_lifecycle(self) -> None:
        tenant_id = self.create_tenant("Acme", "ten_acme")
        self.assertEqual(tenant_id, "ten_acme")
        self.assertEqual(
            self.client.get(f"/v1/tenants/{tenant_id}", headers=AUTH).json()["name"], "Acme"
        )
        listing = self.client.get("/v1/tenants", headers=AUTH).json()
        self.assertEqual([tenant["id"] for tenant in listing], ["ten_acme"])
        duplicate = self.client.post(
            "/v1/tenants", json={"id": "ten_acme", "name": "Other"}, headers=AUTH
        )
        self.assertEqual(duplicate.status_code, 409)
        self.assertEqual(
            self.client.get("/v1/tenants/ten_missing", headers=AUTH).status_code, 404
        )

    # ── nodes ────────────────────────────────────────────────────────────────

    def test_nodes_are_tenant_scoped(self) -> None:
        tenant_a = self.create_tenant("A")
        tenant_b = self.create_tenant("B")
        node_id, _key = self.provision_node(tenant_a)
        self.assertEqual(
            [
                node["id"]
                for node in self.client.get(f"/v1/tenants/{tenant_a}/nodes", headers=AUTH).json()
            ],
            [node_id],
        )
        self.assertEqual(
            self.client.get(f"/v1/tenants/{tenant_b}/nodes", headers=AUTH).json(), []
        )
        # A node of another tenant is not found, not forbidden: the id is never
        # confirmed to exist elsewhere.
        self.assertEqual(
            self.client.get(
                f"/v1/tenants/{tenant_b}/nodes/{node_id}", headers=AUTH
            ).status_code,
            404,
        )

    def test_node_view_exposes_a_fingerprint_not_a_bare_key_dump(self) -> None:
        tenant_id = self.create_tenant()
        node_id, _key = self.provision_node(tenant_id)
        node = self.client.get(
            f"/v1/tenants/{tenant_id}/nodes/{node_id}", headers=AUTH
        ).json()
        self.assertTrue(node["key_fingerprint"].startswith("SHA256:"))
        self.assertNotIn("key_fingerprint", node["public_key"])

    def test_duplicate_node_name_is_a_conflict(self) -> None:
        tenant_id = self.create_tenant()
        node_id, key = self.provision_node(tenant_id, "build-01")
        self.assertTrue(node_id)
        duplicate = self.register_node(tenant_id, key, "build-01")
        self.assertEqual(duplicate.status_code, 409)

    def test_registering_a_node_in_an_unknown_tenant_is_not_found(self) -> None:
        response = self.register_node("ten_missing", new_key())
        self.assertEqual(response.status_code, 404)

    # ── tasks ────────────────────────────────────────────────────────────────

    def test_task_lifecycle_and_transitions(self) -> None:
        tenant_id = self.create_tenant()
        node_id, _key = self.provision_node(tenant_id)
        created = self.create_task(tenant_id, node_id)
        self.assertEqual(created["status"], 201, created["body"])
        task_id = created["body"]["id"]
        self.assertEqual(created["body"]["status"], "queued")
        self.assertEqual(created["body"]["attempts"], 0)

        # A queued task cannot jump straight to succeeded.
        self.assertEqual(self.move(tenant_id, task_id, "succeeded"), 409)
        self.assertEqual(self.move(tenant_id, task_id, "running"), 200)
        task = self.get_task(tenant_id, task_id)
        self.assertEqual(task["status"], "running")
        self.assertEqual(task["attempts"], 1)
        self.assertEqual(
            self.move(tenant_id, task_id, "succeeded", outcome="ok", result={"code": 0}), 200
        )
        # A completed task is terminal.
        self.assertEqual(self.move(tenant_id, task_id, "running"), 409)
        self.assertEqual(self.move(tenant_id, task_id, "nonsense"), 409)

    def test_one_active_task_per_node(self) -> None:
        tenant_id = self.create_tenant()
        node_id, _key = self.provision_node(tenant_id)
        first = self.create_task(tenant_id, node_id)
        self.assertEqual(first["status"], 201)
        second = self.create_task(tenant_id, node_id)
        self.assertEqual(second["status"], 409, second["body"])

        # Finishing the first frees the node.
        self.assertEqual(self.move(tenant_id, first["body"]["id"], "running"), 200)
        self.assertEqual(self.move(tenant_id, first["body"]["id"], "succeeded"), 200)
        third = self.create_task(tenant_id, node_id)
        self.assertEqual(third["status"], 201, third["body"])

        # Historical tasks accumulate: the index is partial, so the terminal row
        # coexists with the one active row instead of blocking a new task.
        tasks = self.client.get(f"/v1/tenants/{tenant_id}/tasks", headers=AUTH).json()
        self.assertEqual(sorted(task["status"] for task in tasks), ["queued", "succeeded"])

    def test_tasks_are_tenant_scoped(self) -> None:
        tenant_a = self.create_tenant("A")
        tenant_b = self.create_tenant("B")
        node_id, _key = self.provision_node(tenant_a)
        task_id = self.create_task(tenant_a, node_id)["body"]["id"]
        self.assertEqual(
            self.client.get(
                f"/v1/tenants/{tenant_b}/tasks/{task_id}", headers=AUTH
            ).status_code,
            404,
        )
        cross_node = self.create_task(tenant_b, node_id)
        self.assertEqual(cross_node["status"], 404)

    def test_task_payload_rejects_secret_fields(self) -> None:
        tenant_id = self.create_tenant()
        node_id, _key = self.provision_node(tenant_id)
        response = self.client.post(
            f"/v1/tenants/{tenant_id}/tasks",
            json={
                "nodeId": node_id,
                "kind": "filesystem.read",
                "payload": {"password": "hunter2"},
            },
            headers=AUTH,
        )
        self.assertEqual(response.status_code, 400, response.text)
        self.assertEqual(response.json()["code"], "secret_rejected")
        nested = self.client.post(
            f"/v1/tenants/{tenant_id}/tasks",
            json={
                "nodeId": node_id,
                "kind": "filesystem.read",
                "payload": {"auth": {"privateKey": "-----BEGIN"}},
            },
            headers=AUTH,
        )
        self.assertEqual(nested.status_code, 400, nested.text)

    # ── disconnect recovery ──────────────────────────────────────────────────

    def test_heartbeat_marks_running_tasks_unknown(self) -> None:
        tenant_id = self.create_tenant()
        node_id, _key = self.provision_node(tenant_id)
        task_id = self.create_task(tenant_id, node_id)["body"]["id"]
        self.assertEqual(self.move(tenant_id, task_id, "running"), 200)

        heartbeat = self.client.post(
            f"/v1/tenants/{tenant_id}/nodes/{node_id}/heartbeat",
            json={"agentVersion": "0.1.0", "restarted": True},
            headers=AUTH,
        )
        self.assertEqual(heartbeat.status_code, 200, heartbeat.text)
        self.assertEqual(heartbeat.json()["marked_unknown"], [task_id])

        task = self.get_task(tenant_id, task_id)
        self.assertEqual(task["status"], "unknown")
        self.assertEqual(task["outcome"], "node_restarted")
        self.assertIsNotNone(
            self.client.get(f"/v1/tenants/{tenant_id}/nodes/{node_id}", headers=AUTH).json()[
                "last_seen_at"
            ]
        )

        # The unknown task is listed for a human decision and never replayed.
        recoverable = self.client.get(
            f"/v1/tenants/{tenant_id}/nodes/{node_id}/recoverable", headers=AUTH
        ).json()
        self.assertEqual([task["id"] for task in recoverable], [task_id])

        # A heartbeating node with running work that did not restart keeps it.
        next_task = self.create_task(tenant_id, node_id)["body"]["id"]
        self.assertEqual(self.move(tenant_id, next_task, "running"), 200)
        plain = self.client.post(
            f"/v1/tenants/{tenant_id}/nodes/{node_id}/heartbeat",
            json={"restarted": False},
            headers=AUTH,
        )
        self.assertEqual(plain.json()["marked_unknown"], [])
        self.assertEqual(self.get_task(tenant_id, next_task)["status"], "running")

    # ── model configs and approvals ──────────────────────────────────────────

    def test_model_config_never_stores_a_raw_key(self) -> None:
        tenant_id = self.create_tenant()
        rejected = self.client.post(
            f"/v1/tenants/{tenant_id}/model-configs",
            json={"name": "fast", "provider": "openai", "model": "gpt", "apiKey": "sk-live"},
            headers=AUTH,
        )
        self.assertEqual(rejected.status_code, 422, rejected.text)

        accepted = self.client.post(
            f"/v1/tenants/{tenant_id}/model-configs",
            json={
                "name": "fast",
                "provider": "openai",
                "model": "gpt",
                "apiKeyRef": "keychain://yukinal/openai",
            },
            headers=AUTH,
        )
        self.assertEqual(accepted.status_code, 201, accepted.text)
        self.assertEqual(accepted.json()["api_key_ref"], "keychain://yukinal/openai")
        self.assertNotIn("apiKey", accepted.json())

    def test_approval_flow(self) -> None:
        tenant_id = self.create_tenant()
        node_id, _key = self.provision_node(tenant_id)
        created = self.create_approval(
            tenant_id, node_id, "server.exec", {"command": "systemctl restart nginx"}
        )
        self.assertEqual(created.status_code, 201, created.text)
        approval_id = created.json()["id"]
        self.assertEqual(created.json()["state"], "pending")
        # The server owns the canonical form; the caller never supplies a
        # fingerprint, so it cannot name one that does not match the input.
        self.assertEqual(
            created.json()["fingerprint"],
            actions.action_fingerprint(
                kind="server.exec",
                node_id=node_id,
                payload={"command": "systemctl restart nginx"},
            ),
        )
        self.assertEqual(created.json()["node_id"], node_id)
        self.assertEqual(created.json()["payload"], {"command": "systemctl restart nginx"})

        decided = self.decide(tenant_id, approval_id)
        self.assertEqual(decided.status_code, 200, decided.text)
        self.assertEqual(decided.json()["state"], "approved")
        self.assertEqual(decided.json()["decided_by"], "ada")

        # A decision is one-shot.
        again = self.decide(tenant_id, approval_id, "rejected")
        self.assertEqual(again.status_code, 409, again.text)

    # ── audit and metrics ────────────────────────────────────────────────────

    def test_audit_is_recorded_and_tenant_scoped(self) -> None:
        tenant_a = self.create_tenant("A")
        tenant_b = self.create_tenant("B")
        node_id, _key = self.provision_node(tenant_a)
        self.create_task(tenant_a, node_id)

        audit = self.client.get(f"/v1/tenants/{tenant_a}/audit", headers=AUTH).json()
        actions = {event["action"] for event in audit}
        self.assertIn("tenant.create", actions)
        self.assertIn("node.register", actions)
        self.assertIn("task.create", actions)

        # Newest first.
        self.assertEqual(audit[0]["action"], "task.create")
        other = self.client.get(f"/v1/tenants/{tenant_b}/audit", headers=AUTH).json()
        self.assertEqual({event["action"] for event in other}, {"tenant.create"})

    def test_metrics(self) -> None:
        tenant_id = self.create_tenant()
        node_id, _key = self.provision_node(tenant_id)
        task_id = self.create_task(tenant_id, node_id)["body"]["id"]
        self.assertEqual(self.move(tenant_id, task_id, "running"), 200)
        # Left pending on purpose: the metric counts undecided requests.
        self.create_approval(tenant_id, node_id, "server.exec", {"command": "ls"})
        metrics = self.client.get("/v1/metrics", headers=AUTH).json()
        self.assertEqual(metrics["tenants"], 1)
        self.assertEqual(metrics["nodes"], 1)
        self.assertEqual(metrics["tasks_by_status"], {"running": 1})
        self.assertEqual(metrics["pending_approvals"], 1)

    # ── migration ────────────────────────────────────────────────────────────

    def test_migration_matches_the_models(self) -> None:
        migration_db = Path(self._temporary.name) / "migrated.db"
        url = f"sqlite+aiosqlite:///{migration_db.as_posix()}"
        config = Config(str(SERVICE_ROOT / "alembic.ini"))
        config.set_main_option("sqlalchemy.url", url)

        command.upgrade(config, "head")
        sync_engine = sqlalchemy.create_engine(f"sqlite:///{migration_db.as_posix()}")
        try:
            migrated = set(inspect(sync_engine).get_table_names())
            inspector = inspect(sync_engine)
            indexes = {
                index["name"]
                for table in ("tasks", "node_nonces", "approvals", "tool_policies")
                for index in inspector.get_indexes(table)
            }
            approvals_fks = {
                (fk["referred_table"], tuple(fk["constrained_columns"]))
                for fk in inspector.get_foreign_keys("approvals")
            }
            tool_policy_fks = {
                (fk["referred_table"], tuple(fk["constrained_columns"]))
                for fk in inspector.get_foreign_keys("tool_policies")
            }
            tenant_policy_fks = {
                (fk["referred_table"], tuple(fk["constrained_columns"]))
                for fk in inspector.get_foreign_keys("tenant_policies")
            }
            tool_policy_uniques = {
                tuple(constraint["column_names"])
                for constraint in inspector.get_unique_constraints("tool_policies")
            }
        finally:
            sync_engine.dispose()
        self.assertEqual(migrated, set(Base.metadata.tables) | {"alembic_version"})
        self.assertIn("uq_tasks_active_per_node", indexes)
        self.assertIn("ix_node_nonces_at", indexes)
        self.assertIn("ix_approvals_node_id", indexes)
        self.assertIn("ix_tool_policies_tenant_id", indexes)
        self.assertIn(("nodes", ("node_id",)), approvals_fks)
        self.assertIn(("tenants", ("tenant_id",)), tool_policy_fks)
        self.assertIn(("tenants", ("tenant_id",)), tenant_policy_fks)
        self.assertIn(("tenant_id", "kind"), tool_policy_uniques)

        command.downgrade(config, "base")
        sync_engine = sqlalchemy.create_engine(f"sqlite:///{migration_db.as_posix()}")
        try:
            remaining = set(inspect(sync_engine).get_table_names())
        finally:
            sync_engine.dispose()
        self.assertEqual(remaining, {"alembic_version"})


if __name__ == "__main__":
    unittest.main()
