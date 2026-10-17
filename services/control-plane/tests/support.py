"""Shared test scaffolding: a real app over a real SQLite schema.

Nothing here is mocked: the app is the one FastAPI serves, the schema is the one
the models declare, and node requests carry real Ed25519 signatures.
"""

from __future__ import annotations

import json
import tempfile
import time
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

import sqlalchemy
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from fastapi.testclient import TestClient
from sqlalchemy.ext.asyncio import create_async_engine
from sqlalchemy.pool import NullPool

from yukinal_control_plane import nodeauth
from yukinal_control_plane.app import create_app
from yukinal_control_plane.config import Settings
from yukinal_control_plane.db import Base

SERVICE_ROOT = Path(__file__).resolve().parents[1]
TOKEN = "control-plane-test-token"
AUTH = {"Authorization": f"Bearer {TOKEN}"}
CHANNEL_PATH = "/v1/node/channel"


def new_key() -> Ed25519PrivateKey:
    return Ed25519PrivateKey.generate()


def raw_public_key(key: Ed25519PrivateKey) -> bytes:
    return key.public_key().public_bytes(
        encoding=serialization.Encoding.Raw,
        format=serialization.PublicFormat.Raw,
    )


def public_key(key: Ed25519PrivateKey) -> str:
    """Canonical (base64) form, which is what registration stores."""
    return nodeauth.encode_public_key(raw_public_key(key))


def openssh_public_key(key: Ed25519PrivateKey) -> str:
    return key.public_key().public_bytes(
        encoding=serialization.Encoding.OpenSSH,
        format=serialization.PublicFormat.OpenSSH,
    ).decode("ascii")


def body_bytes(payload: dict) -> bytes:
    """Exactly the bytes that go on the wire, so the signature covers them."""
    return json.dumps(payload).encode("utf-8")


class ControlPlaneTestCase(unittest.TestCase):
    """A running app over a throwaway SQLite file, with operator and node helpers."""

    def setUp(self) -> None:
        # ignore_cleanup_errors: on Windows a SQLite file can still be held by a
        # shutting-down app task for a moment; that must not fail an assertion.
        self._temporary = tempfile.TemporaryDirectory(ignore_cleanup_errors=True)
        self.db_path = Path(self._temporary.name) / "control.db"
        self.url = f"sqlite+aiosqlite:///{self.db_path.as_posix()}"
        sync_engine = sqlalchemy.create_engine(f"sqlite:///{self.db_path.as_posix()}")
        Base.metadata.create_all(sync_engine)
        sync_engine.dispose()
        self.engine = create_async_engine(self.url, poolclass=NullPool)
        self.app = create_app(Settings(database_url=self.url, token=TOKEN), engine=self.engine)
        self.client = TestClient(self.app)

    def tearDown(self) -> None:
        self.client.close()
        self._temporary.cleanup()

    # ── operator helpers ─────────────────────────────────────────────────────

    def create_tenant(self, name: str = "Acme", tenant_id: str | None = None) -> str:
        body: dict = {"name": name}
        if tenant_id is not None:
            body["id"] = tenant_id
        response = self.client.post("/v1/tenants", json=body, headers=AUTH)
        self.assertEqual(response.status_code, 201, response.text)
        return response.json()["id"]

    def register_node(self, tenant_id: str, key: Ed25519PrivateKey, name: str = "build-01"):
        return self.client.post(
            f"/v1/tenants/{tenant_id}/nodes",
            json={"name": name, "publicKey": public_key(key)},
            headers=AUTH,
        )

    def provision_node(self, tenant_id: str, name: str = "build-01"):
        """Register a node with a fresh Ed25519 identity. Returns ``(id, key)``."""
        key = new_key()
        response = self.register_node(tenant_id, key, name)
        self.assertEqual(response.status_code, 201, response.text)
        return response.json()["id"], key

    def create_task(
        self, tenant_id: str, node_id: str, kind: str = "filesystem.read", payload: dict | None = None
    ) -> dict:
        response = self.client.post(
            f"/v1/tenants/{tenant_id}/tasks",
            json={
                "nodeId": node_id,
                "kind": kind,
                "payload": payload if payload is not None else {"path": "/etc/hosts"},
            },
            headers=AUTH,
        )
        return {"status": response.status_code, "body": response.json()}

    def move(self, tenant_id: str, task_id: str, status: str, **extra) -> int:
        response = self.client.post(
            f"/v1/tenants/{tenant_id}/tasks/{task_id}/status",
            json={"status": status, **extra},
            headers=AUTH,
        )
        return response.status_code

    def get_task(self, tenant_id: str, task_id: str) -> dict:
        response = self.client.get(
            f"/v1/tenants/{tenant_id}/tasks/{task_id}", headers=AUTH
        )
        self.assertEqual(response.status_code, 200, response.text)
        return response.json()

    # ── approval helpers ─────────────────────────────────────────────────────

    def create_approval(
        self,
        tenant_id: str,
        node_id: str,
        action: str,
        payload: dict | None = None,
        requested_by: str = "agent",
        max_uses: int | None = None,
    ):
        body = {
            "action": action,
            "nodeId": node_id,
            "payload": payload if payload is not None else {},
            "requestedBy": requested_by,
        }
        if max_uses is not None:
            body["maxUses"] = max_uses
        return self.client.post(
            f"/v1/tenants/{tenant_id}/approvals", json=body, headers=AUTH
        )

    def decide(
        self,
        tenant_id: str,
        approval_id: str,
        decision: str = "approved",
        decided_by: str = "ada",
        max_uses: int | None = None,
    ):
        body = {"decision": decision, "decidedBy": decided_by}
        if max_uses is not None:
            body["maxUses"] = max_uses
        return self.client.post(
            f"/v1/tenants/{tenant_id}/approvals/{approval_id}/decide",
            json=body,
            headers=AUTH,
        )

    def approve(
        self,
        tenant_id: str,
        node_id: str,
        action: str,
        payload: dict | None = None,
        max_uses: int | None = None,
    ) -> str:
        """Request and approve one action, returning the approval id."""
        created = self.create_approval(tenant_id, node_id, action, payload, max_uses=max_uses)
        self.assertEqual(created.status_code, 201, created.text)
        decided = self.decide(tenant_id, created.json()["id"], max_uses=max_uses)
        self.assertEqual(decided.status_code, 200, decided.text)
        return created.json()["id"]

    def get_approval(self, tenant_id: str, approval_id: str) -> dict:
        listed = self.list_approvals(tenant_id)
        for approval in listed:
            if approval["id"] == approval_id:
                return approval
        raise AssertionError(f"approval `{approval_id}` not in {listed!r}")

    def list_approvals(self, tenant_id: str) -> list:
        response = self.client.get(f"/v1/tenants/{tenant_id}/approvals", headers=AUTH)
        self.assertEqual(response.status_code, 200, response.text)
        return response.json()

    def revoke(self, tenant_id: str, approval_id: str, revoked_by: str = "ada"):
        return self.client.post(
            f"/v1/tenants/{tenant_id}/approvals/{approval_id}/revoke",
            json={"revokedBy": revoked_by},
            headers=AUTH,
        )

    def expire_approvals(self, tenant_id: str):
        return self.client.post(f"/v1/tenants/{tenant_id}/approvals/expire", headers=AUTH)

    def backdate_approval(self, approval_id: str, *, seconds: int = 1) -> None:
        """Push an approval's expiry into the past, the way time would.

        The API has no way to set an expiry in the past, and waiting for a real
        TTL is not a test. This writes the same column SQLAlchemy writes.
        """
        import sqlite3

        moment = datetime.now(timezone.utc) - timedelta(seconds=seconds)
        stored = moment.strftime("%Y-%m-%d %H:%M:%S.%f")
        connection = sqlite3.connect(self.db_path)
        try:
            changed = connection.execute(
                "UPDATE approvals SET expires_at = ? WHERE id = ?", (stored, approval_id)
            ).rowcount
            connection.commit()
        finally:
            connection.close()
        self.assertEqual(changed, 1, f"approval `{approval_id}` was not updated")

    def stored_approval_state(self, approval_id: str) -> str:
        """Read an approval's stored state without going through a listing.

        Listing sweeps that tenant's lapsed decisions, so a test about the sweep
        itself has to look at the row directly.
        """
        import sqlite3

        connection = sqlite3.connect(self.db_path)
        try:
            row = connection.execute(
                "SELECT state FROM approvals WHERE id = ?", (approval_id,)
            ).fetchone()
        finally:
            connection.close()
        self.assertIsNotNone(row, f"approval `{approval_id}` was not found")
        return row[0]

    def get_policy(self, tenant_id: str) -> dict:
        response = self.client.get(f"/v1/tenants/{tenant_id}/policy", headers=AUTH)
        self.assertEqual(response.status_code, 200, response.text)
        return response.json()

    def put_policy(self, tenant_id: str, **body):
        body.setdefault("updatedBy", "ada")
        return self.client.put(
            f"/v1/tenants/{tenant_id}/policy", json=body, headers=AUTH
        )

    def delete_policy(self, tenant_id: str):
        return self.client.delete(f"/v1/tenants/{tenant_id}/policy", headers=AUTH)

    def list_tool_policies(self, tenant_id: str) -> list:
        response = self.client.get(
            f"/v1/tenants/{tenant_id}/tool-policies", headers=AUTH
        )
        self.assertEqual(response.status_code, 200, response.text)
        return response.json()

    def tool_policy(self, tenant_id: str, kind: str) -> dict | None:
        for entry in self.list_tool_policies(tenant_id):
            if entry["kind"] == kind:
                return entry
        return None

    def put_tool_policy(self, tenant_id: str, kind: str, risk: str):
        return self.client.put(
            f"/v1/tenants/{tenant_id}/tool-policies/{kind}",
            json={"risk": risk, "updatedBy": "ada"},
            headers=AUTH,
        )

    def delete_tool_policy(self, tenant_id: str, kind: str):
        return self.client.delete(
            f"/v1/tenants/{tenant_id}/tool-policies/{kind}", headers=AUTH
        )

    def wait_for_task_status(
        self, tenant_id: str, task_id: str, status: str, timeout: float = 5.0
    ) -> dict:
        """Poll until a task reaches ``status``.

        Channel teardown happens in the app's ``finally``, which the test client
        does not synchronously join; polling keeps the assertion about the real
        behaviour without assuming a scheduling order.
        """
        deadline = time.monotonic() + timeout
        task = self.get_task(tenant_id, task_id)
        while task["status"] != status and time.monotonic() < deadline:
            time.sleep(0.05)
            task = self.get_task(tenant_id, task_id)
        self.assertEqual(task["status"], status, task)
        return task

    # ── node helpers ─────────────────────────────────────────────────────────

    def node_headers(
        self,
        key: Ed25519PrivateKey,
        method: str,
        path: str,
        body: bytes = b"",
        *,
        node_id: str,
        **kwargs,
    ) -> dict:
        """Sign one node request: the contract a node driver implements.

        The node id is the caller's identity and is not derived from the key, so
        it is sent alongside the signature as a separate header.
        """
        headers = nodeauth.sign_headers(key, method=method, path=path, body=body, **kwargs)
        headers[nodeauth.HEADER_NODE] = node_id
        return headers

    def node_get(self, path: str, key: Ed25519PrivateKey, node_id: str):
        return self.client.get(path, headers=self.node_headers(key, "GET", path, node_id=node_id))

    def node_post(self, path: str, key: Ed25519PrivateKey, node_id: str, payload: dict):
        body = body_bytes(payload)
        headers = self.node_headers(key, "POST", path, body, node_id=node_id)
        headers["Content-Type"] = "application/json"
        return self.client.post(path, content=body, headers=headers)

    def node_socket(
        self, key: Ed25519PrivateKey, node_id: str, path: str = CHANNEL_PATH, **kwargs
    ):
        headers = self.node_headers(key, "GET", path, node_id=node_id, **kwargs)
        return self.client.websocket_connect(path, headers=headers)
