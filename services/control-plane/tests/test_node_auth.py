"""Node identity tests: registration, Ed25519 signatures and the fail-closed paths.

Every assertion here is about the boundary, not the happy path alone: a key that
cannot verify, a signature over the wrong bytes, a stale timestamp, a replayed
nonce and an unknown node all have to be refused the same way.
"""

from __future__ import annotations

import time
import unittest

import sqlalchemy
from sqlalchemy import text

from support import (
    AUTH,
    ControlPlaneTestCase,
    body_bytes,
    new_key,
    openssh_public_key,
    public_key,
    raw_public_key,
)

from yukinal_control_plane import nodeauth

TASKS_PATH = "/v1/node/tasks"
RECOVERABLE_PATH = "/v1/node/recoverable"


class NodeRegistrationTests(ControlPlaneTestCase):
    def test_registration_rejects_a_key_that_is_not_ed25519(self) -> None:
        tenant_id = self.create_tenant()
        invalid = [
            "",
            "not base64 !!",
            "c2hvcnQ=",  # base64 of "short": the wrong length
            "ab" * 10,  # hex, but 20 bytes
            "ssh-ed25519",  # no key material
            "ssh-ed25519 !!!!",  # not base64
            "ssh-rsa AAAA",  # not an ed25519 key
        ]
        for value in invalid:
            with self.subTest(value=value):
                response = self.client.post(
                    f"/v1/tenants/{tenant_id}/nodes",
                    json={"name": "n", "publicKey": value},
                    headers=AUTH,
                )
                self.assertEqual(response.status_code, 422, response.text)

    def test_registration_accepts_base64_hex_and_openssh_forms(self) -> None:
        tenant_id = self.create_tenant()
        key = new_key()
        raw = raw_public_key(key)
        forms = [public_key(key), raw.hex(), openssh_public_key(key)]
        stored = []
        for index, value in enumerate(forms):
            response = self.client.post(
                f"/v1/tenants/{tenant_id}/nodes",
                json={"name": f"node-{index}", "publicKey": value},
                headers=AUTH,
            )
            self.assertEqual(response.status_code, 201, response.text)
            stored.append(response.json()["public_key"])
        # One canonical stored form regardless of how the operator pasted it.
        self.assertEqual(stored, [nodeauth.encode_public_key(raw)] * 3)
        self.assertEqual(
            self.client.get(f"/v1/tenants/{tenant_id}/nodes", headers=AUTH).json()[0][
                "key_fingerprint"
            ],
            nodeauth.key_fingerprint(raw),
        )


class NodeAuthTests(ControlPlaneTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.tenant_id = self.create_tenant()
        self.node_id, self.key = self.provision_node(self.tenant_id)

    def assertUnauthorized(self, response) -> None:
        self.assertEqual(response.status_code, 401, response.text)
        self.assertEqual(response.json()["code"], "unauthorized")

    # ── the happy path, so the negative cases mean something ─────────────────

    def test_a_signed_request_authenticates_the_node(self) -> None:
        task_id = self.create_task(self.tenant_id, self.node_id)["body"]["id"]
        listed = self.node_get(TASKS_PATH, self.key, self.node_id)
        self.assertEqual(listed.status_code, 200, listed.text)
        self.assertEqual([task["id"] for task in listed.json()], [task_id])

        heartbeat = self.node_post(
            "/v1/node/heartbeat",
            self.key,
            self.node_id,
            {"agentVersion": "0.3.0", "restarted": False},
        )
        self.assertEqual(heartbeat.status_code, 200, heartbeat.text)
        self.assertEqual(heartbeat.json()["marked_unknown"], [])

        moved = self.node_post(
            f"/v1/node/tasks/{task_id}/status", self.key, self.node_id, {"status": "running"}
        )
        self.assertEqual(moved.status_code, 200, moved.text)
        self.assertEqual(moved.json()["status"], "running")

    # ── refusals ─────────────────────────────────────────────────────────────

    def test_a_request_without_signature_headers_is_refused(self) -> None:
        self.assertUnauthorized(self.client.get(TASKS_PATH))
        self.assertUnauthorized(
            self.client.get(TASKS_PATH, headers={nodeauth.HEADER_NODE: self.node_id})
        )

    def test_a_signature_from_another_key_is_refused(self) -> None:
        self.assertUnauthorized(self.node_get(TASKS_PATH, new_key(), self.node_id))

    def test_a_signature_that_does_not_cover_the_body_is_refused(self) -> None:
        task_id = self.create_task(self.tenant_id, self.node_id)["body"]["id"]
        path = f"/v1/node/tasks/{task_id}/status"
        signed = self.node_headers(
            self.key,
            "POST",
            path,
            body_bytes({"status": "running"}),
            node_id=self.node_id,
        )
        signed["Content-Type"] = "application/json"
        tampered = self.client.post(
            path, content=body_bytes({"status": "succeeded"}), headers=signed
        )
        self.assertUnauthorized(tampered)

    def test_a_signature_for_another_path_is_refused(self) -> None:
        headers = self.node_headers(self.key, "GET", TASKS_PATH, node_id=self.node_id)
        self.assertUnauthorized(self.client.get(RECOVERABLE_PATH, headers=headers))

    def test_a_stale_timestamp_is_refused(self) -> None:
        stale = int(time.time()) - nodeauth.SIGNATURE_WINDOW_SECONDS - 60
        headers = self.node_headers(
            self.key, "GET", TASKS_PATH, node_id=self.node_id, timestamp=stale
        )
        self.assertUnauthorized(self.client.get(TASKS_PATH, headers=headers))

        future = int(time.time()) + nodeauth.SIGNATURE_WINDOW_SECONDS + 60
        headers = self.node_headers(
            self.key, "GET", TASKS_PATH, node_id=self.node_id, timestamp=future
        )
        self.assertUnauthorized(self.client.get(TASKS_PATH, headers=headers))

    def test_a_nonce_cannot_be_reused(self) -> None:
        headers = self.node_headers(
            self.key, "GET", TASKS_PATH, node_id=self.node_id, nonce="fixed-nonce"
        )
        self.assertEqual(self.client.get(TASKS_PATH, headers=headers).status_code, 200)
        replayed = self.client.get(TASKS_PATH, headers=headers)
        self.assertUnauthorized(replayed)
        self.assertIn("nonce", replayed.json()["message"])

    def test_an_unknown_node_is_unauthorized_not_not_found(self) -> None:
        # Never confirm which node ids exist: an unknown node and a bad
        # signature are indistinguishable to the caller.
        headers = self.node_headers(self.key, "GET", TASKS_PATH, node_id=self.node_id)
        headers[nodeauth.HEADER_NODE] = "nod_does_not_exist"
        self.assertUnauthorized(self.client.get(TASKS_PATH, headers=headers))

    def test_the_operator_token_does_not_authenticate_a_node(self) -> None:
        self.assertEqual(self.client.get(TASKS_PATH, headers=AUTH).status_code, 401)

    def test_a_node_cannot_touch_another_nodes_task(self) -> None:
        other_id, other_key = self.provision_node(self.tenant_id, "build-02")
        foreign_task = self.create_task(self.tenant_id, other_id)["body"]["id"]
        # Same tenant, different node: the task is not this node's to report.
        refused = self.node_post(
            f"/v1/node/tasks/{foreign_task}/status",
            self.key,
            self.node_id,
            {"status": "running"},
        )
        self.assertEqual(refused.status_code, 404, refused.text)
        # And the owner can.
        allowed = self.node_post(
            f"/v1/node/tasks/{foreign_task}/status",
            other_key,
            other_id,
            {"status": "running"},
        )
        self.assertEqual(allowed.status_code, 200, allowed.text)

    def test_disabling_a_tenant_revokes_its_nodes(self) -> None:
        self.assertEqual(self.node_get(TASKS_PATH, self.key, self.node_id).status_code, 200)
        engine = sqlalchemy.create_engine(f"sqlite:///{self.db_path.as_posix()}")
        try:
            with engine.begin() as connection:
                connection.execute(
                    text("UPDATE tenants SET disabled = 1 WHERE id = :id"),
                    {"id": self.tenant_id},
                )
        finally:
            engine.dispose()
        self.assertUnauthorized(self.node_get(TASKS_PATH, self.key, self.node_id))


class NodeAuthPrimitiveTests(unittest.TestCase):
    """The canonical-string contract, independent of the HTTP layer."""

    def test_canonical_request_is_unambiguous(self) -> None:
        one = nodeauth.canonical_request(
            timestamp="1", nonce="a\nb", method="GET", path="/p", body=b""
        )
        two = nodeauth.canonical_request(
            timestamp="1", nonce="a", method="GET", path="b\n/p", body=b""
        )
        self.assertNotEqual(one, two)

    def test_verify_signature_rejects_garbage_without_raising(self) -> None:
        key = new_key()
        canonical = b"payload"
        self.assertFalse(nodeauth.verify_signature(public_key(key), "!!!", canonical))
        self.assertFalse(nodeauth.verify_signature("!!!", "AAAA", canonical))
        self.assertFalse(nodeauth.verify_signature(public_key(key), "", canonical))

    def test_parse_public_key_round_trips(self) -> None:
        raw = raw_public_key(new_key())
        self.assertEqual(nodeauth.parse_public_key(nodeauth.encode_public_key(raw)), raw)


if __name__ == "__main__":
    unittest.main()
