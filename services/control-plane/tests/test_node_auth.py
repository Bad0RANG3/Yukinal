"""Node identity tests: registration, Ed25519 signatures and the fail-closed paths.

Every assertion here is about the boundary, not the happy path alone: a key that
cannot verify, a signature over the wrong bytes, a stale timestamp, a replayed
nonce and an unknown node all have to be refused the same way.
"""

from __future__ import annotations

import base64
import sqlite3
import threading
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

from yukinal_control_plane import nodeauth, service

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

    def test_a_public_key_cannot_back_a_node_in_two_tenants(self) -> None:
        """One key, one node — globally, not one per tenant.

        The pair this refuses was the setup for a cross-tenant read: the node id
        header is unsigned routing input, so with the same key on a row in each
        tenant a signature made for one id verified against the other's key, and
        swapping the header moved the caller into the other tenant. Refusing the
        second registration means the pair cannot exist to be exploited.
        """
        first = self.create_tenant("First")
        second = self.create_tenant("Second")
        key = new_key()
        registered = self.register_node(first, key, "build-01")
        self.assertEqual(registered.status_code, 201, registered.text)

        # The same name, in a tenant where that name is free, so the conflict can
        # only be the key. `uq_nodes_tenant_name` would not have caught this.
        duplicate = self.register_node(second, key, "build-01")
        self.assertEqual(duplicate.status_code, 409, duplicate.text)
        self.assertEqual(duplicate.json()["code"], "conflict")
        # The refusal does not name the holder: that would cross the tenant line.
        message = duplicate.json()["message"]
        self.assertNotIn(first, message)
        self.assertNotIn(second, message)
        self.assertIn("public key", message)
        # And the second tenant really has no node: the row was never written.
        self.assertEqual(
            self.client.get(f"/v1/tenants/{second}/nodes", headers=AUTH).json(), []
        )

    def test_registration_accepts_base64_hex_and_openssh_forms(self) -> None:
        tenant_id = self.create_tenant()
        # One key per form: a public key may only back one node, so pasting the
        # same key three times is now a conflict (asserted separately below).
        # What is being checked here is the *stored* form, which is canonical
        # whichever spelling the operator used.
        keys = [new_key(), new_key(), new_key()]
        forms = [
            public_key(keys[0]),
            raw_public_key(keys[1]).hex(),
            openssh_public_key(keys[2]),
        ]
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
        self.assertEqual(
            stored, [nodeauth.encode_public_key(raw_public_key(key)) for key in keys]
        )
        self.assertEqual(
            self.client.get(f"/v1/tenants/{tenant_id}/nodes", headers=AUTH).json()[0][
                "key_fingerprint"
            ],
            nodeauth.key_fingerprint(raw_public_key(keys[0])),
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
        # Burned exactly once: the successful request leaves one row, and the
        # replay does not add a second.
        self.assertEqual(self.stored_nonce_count("fixed-nonce"), 1)

    # ── the nonce is burned by the attempt, not by the outcome ───────────────
    # One signed request is one attempt with one nonce, whatever the control
    # plane then answers. Consuming the nonce only on success would leave two
    # replay classes open: a captured signature retried after a refusal, and a
    # captured request retried once the state it tripped over has moved on.

    def stored_nonce_count(self, nonce: str | None = None) -> int:
        sql = "SELECT COUNT(*) FROM node_nonces"
        params: tuple = ()
        if nonce is not None:
            sql += " WHERE nonce = ?"
            params = (nonce,)
        connection = sqlite3.connect(self.db_path)
        try:
            row = connection.execute(sql, params).fetchone()
        finally:
            connection.close()
        return row[0]

    def nonces_for(self, node_id: str) -> int:
        connection = sqlite3.connect(self.db_path)
        try:
            row = connection.execute(
                "SELECT COUNT(*) FROM node_nonces WHERE node_id = ?", (node_id,)
            ).fetchone()
        finally:
            connection.close()
        return row[0]

    def bad_signature_headers(self, nonce: str) -> dict:
        headers = self.node_headers(
            self.key, "GET", TASKS_PATH, node_id=self.node_id, nonce=nonce
        )
        headers[nodeauth.HEADER_SIGNATURE] = base64.b64encode(b"\x00" * 64).decode("ascii")
        return headers

    def test_a_nonce_refused_with_a_bad_signature_is_still_spent(self) -> None:
        nonce = "burned-by-a-bad-signature"
        self.assertUnauthorized(self.client.get(TASKS_PATH, headers=self.bad_signature_headers(nonce)))
        # The very same nonce, now correctly signed: the attempt already cost it.
        retried = self.client.get(
            TASKS_PATH,
            headers=self.node_headers(
                self.key, "GET", TASKS_PATH, node_id=self.node_id, nonce=nonce
            ),
        )
        self.assertUnauthorized(retried)
        self.assertIn("nonce", retried.json()["message"])
        self.assertEqual(self.stored_nonce_count(nonce), 1)

    def test_a_nonce_is_not_released_by_a_request_the_route_refused(self) -> None:
        """The signature verifies, the route refuses, the nonce stays spent.

        The task is still queued, so reporting it as ``succeeded`` is an invalid
        transition. Once an operator moves it to ``running`` the identical bytes
        would be accepted — that is the moment the nonce has to be gone.
        """
        task_id = self.create_task(self.tenant_id, self.node_id)["body"]["id"]
        path = f"/v1/node/tasks/{task_id}/status"
        payload = {"status": "succeeded", "outcome": "done"}
        body = body_bytes(payload)
        nonce = "burned-by-a-refused-route"
        headers = self.node_headers(
            self.key, "POST", path, body, node_id=self.node_id, nonce=nonce
        )
        headers["Content-Type"] = "application/json"

        refused = self.client.post(path, content=body, headers=headers)
        self.assertEqual(refused.status_code, 409, refused.text)
        self.assertEqual(refused.json()["code"], "invalid_transition")
        self.assertEqual(self.stored_nonce_count(nonce), 1)

        self.assertEqual(self.move(self.tenant_id, task_id, "running"), 200)
        replayed = self.client.post(path, content=body, headers=headers)
        self.assertUnauthorized(replayed)
        self.assertIn("nonce", replayed.json()["message"])
        # And nothing happened: the task is running, not succeeded.
        self.assertEqual(self.get_task(self.tenant_id, task_id)["status"], "running")

    def test_a_nonce_is_burned_before_the_signature_is_checked(self) -> None:
        """The burn cannot be conditional on a check it precedes.

        Proves the ordering, not just the outcome: after a refused attempt the
        row exists even though the request never reached a route. This is the
        property that makes both replay classes above hold at once.
        """
        nonce = "burned-before-verification"
        self.assertUnauthorized(self.client.get(TASKS_PATH, headers=self.bad_signature_headers(nonce)))
        self.assertEqual(self.stored_nonce_count(nonce), 1)

    def test_a_request_without_headers_spends_no_nonce(self) -> None:
        """Nothing to attribute: a request that names no live node costs nothing.

        Keeps the fix from turning into an unauthenticated table-fill: only a
        request that resolves to a registered, enabled node can burn a nonce.
        """
        self.assertUnauthorized(self.client.get(TASKS_PATH))
        self.assertEqual(self.stored_nonce_count(), 0)

    def test_an_unknown_node_spends_no_nonce(self) -> None:
        headers = self.node_headers(
            self.key, "GET", TASKS_PATH, node_id="nod_does_not_exist", nonce="unattributable"
        )
        self.assertUnauthorized(self.client.get(TASKS_PATH, headers=headers))
        # The whole table, not just this nonce: nothing at all may be written.
        self.assertEqual(self.stored_nonce_count(), 0)

    def test_an_unknown_node_is_unauthorized_not_not_found(self) -> None:
        # Never confirm which node ids exist: an unknown node and a bad
        # signature are indistinguishable to the caller.
        headers = self.node_headers(self.key, "GET", TASKS_PATH, node_id=self.node_id)
        headers[nodeauth.HEADER_NODE] = "nod_does_not_exist"
        self.assertUnauthorized(self.client.get(TASKS_PATH, headers=headers))

    def test_a_signature_cannot_be_moved_to_another_node_by_its_header(self) -> None:
        """End to end: the header is routing, the signature is identity.

        The header is overwritten *after* signing, which is exactly what an
        attacker on the wire controls. The verifier rebuilds the canonical bytes
        with the id the header names, so the signature made for the first id no
        longer verifies — and that holds even for a pair of rows that share one
        key, which the schema refuses to create in the first place (asserted
        below at the database, not only at the API).
        """
        other_id, _other_key = self.provision_node(self.tenant_id, "build-02")
        headers = self.node_headers(self.key, "GET", TASKS_PATH, node_id=self.node_id)
        headers[nodeauth.HEADER_NODE] = other_id

        refused = self.client.get(TASKS_PATH, headers=headers)
        self.assertUnauthorized(refused)
        self.assertEqual(
            refused.json()["message"], "the request signature is not valid for this node"
        )
        # The untouched header still works, so the refusal is the swap and not
        # something about the request itself.
        allowed = self.client.get(
            TASKS_PATH,
            headers=self.node_headers(
                self.key, "GET", TASKS_PATH, node_id=self.node_id, nonce="unswapped"
            ),
        )
        self.assertEqual(allowed.status_code, 200, allowed.text)

        # The pair the swap needs cannot be built by any writer: the table itself
        # refuses a second row with the same key, so a raw insert (a migration, a
        # fixture, an operator with psql) is held to the same rule as the API.
        engine = sqlalchemy.create_engine(f"sqlite:///{self.db_path.as_posix()}")
        try:
            with self.assertRaises(sqlalchemy.exc.IntegrityError):
                with engine.begin() as connection:
                    connection.execute(
                        text(
                            "INSERT INTO nodes (id, tenant_id, name, public_key, created_at) "
                            "VALUES ('nod_planted', :tenant, 'planted', :key, "
                            "'2026-01-01 00:00:00')"
                        ),
                        {"tenant": self.tenant_id, "key": public_key(self.key)},
                    )
        finally:
            engine.dispose()

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

    # ── the two nonce bounds keep a valid signature from being a write permit ─
    # Burning the nonce before verification made one authenticated request one
    # unbounded row. A node's key is the only credential needed, so without a
    # bound a single enrolled node could hand the control plane as much storage
    # as it could send. The length is refused before any database work; the
    # outstanding count is capped per node inside the claiming transaction.

    def test_an_oversized_nonce_is_refused_like_a_bad_signature(self) -> None:
        """A 100000-character nonce costs no row and reveals nothing.

        The column is ``varchar(128)``: SQLite stores whatever it is handed, so
        before this check the request wrote 100 kB and answered as if the
        signature had been fine. The refusal must be byte-identical to the
        signature failure, otherwise the response tells an attacker which
        check refused them.
        """
        oversized = "z" * 100000
        response = self.client.get(
            TASKS_PATH,
            headers=self.node_headers(
                self.key, "GET", TASKS_PATH, node_id=self.node_id, nonce=oversized
            ),
        )
        self.assertUnauthorized(response)
        self.assertEqual(self.stored_nonce_count(), 0)

        # Byte-identical to a genuine signature failure.
        signature_failure = self.client.get(
            TASKS_PATH, headers=self.bad_signature_headers("a-different-nonce")
        )
        self.assertUnauthorized(signature_failure)
        self.assertEqual(response.json(), signature_failure.json())

        # And at the boundary the request still works: the check is >128, not >=128.
        allowed = self.client.get(
            TASKS_PATH,
            headers=self.node_headers(
                self.key, "GET", TASKS_PATH, node_id=self.node_id, nonce="y" * 128
            ),
        )
        self.assertEqual(allowed.status_code, 200, allowed.text)

    def test_the_cap_holds_when_claims_arrive_at_the_same_instant(self) -> None:
        """Concurrent claims cannot overshoot the cap.

        The cap used to be a `COUNT(*)` followed by an insert, with an `await`
        between them: every request that passed the count before the first commit
        read the same stale value, so N concurrent callers wrote `cap + N` rows.
        Because the claim happens *before* signature verification, that overshoot
        was reachable without a valid key — a caller only needed N connections.

        This drives the real app through its HTTP surface with a barrier so the
        claims really do collide, and asserts the table stops exactly at the cap.
        """
        cap = service.MAX_OUTSTANDING_NONCES_PER_NODE
        headroom = 4
        concurrency = 32
        for index in range(cap - headroom):
            headers = self.node_headers(
                self.key, "GET", TASKS_PATH, node_id=self.node_id, nonce=f"warm-{index}"
            )
            self.assertEqual(self.client.get(TASKS_PATH, headers=headers).status_code, 200)
        self.assertEqual(self.nonces_for(self.node_id), cap - headroom)

        barrier = threading.Barrier(concurrency)
        codes: list[int] = []
        lock = threading.Lock()

        def claim(index: int) -> None:
            barrier.wait()
            headers = self.node_headers(
                self.key, "GET", TASKS_PATH, node_id=self.node_id, nonce=f"burst-{index}"
            )
            response = self.client.get(TASKS_PATH, headers=headers)
            with lock:
                codes.append(response.status_code)

        workers = [threading.Thread(target=claim, args=(i,)) for i in range(concurrency)]
        for worker in workers:
            worker.start()
        for worker in workers:
            worker.join()

        self.assertEqual(self.nonces_for(self.node_id), cap, "the cap was overshot")
        self.assertEqual(codes.count(200), headroom, "more claims got in than there was room for")
        self.assertEqual(codes.count(401), concurrency - headroom)

    def test_a_node_stops_burning_nonces_at_the_outstanding_cap(self) -> None:
        """The 129th live nonce is refused, and the table stops growing.

        Only nonces still inside the signature window count, so an honest node
        polling every few seconds never approaches the cap while a caller holding
        one key cannot write without bound.
        """
        other_id, other_key = self.provision_node(self.tenant_id, "build-02")
        cap = service.MAX_OUTSTANDING_NONCES_PER_NODE

        for index in range(cap):
            headers = self.node_headers(
                self.key, "GET", TASKS_PATH, node_id=self.node_id, nonce=f"bulk-{index}"
            )
            self.assertEqual(self.client.get(TASKS_PATH, headers=headers).status_code, 200)
        self.assertEqual(self.nonces_for(self.node_id), cap)

        refused = self.client.get(
            TASKS_PATH,
            headers=self.node_headers(
                self.key, "GET", TASKS_PATH, node_id=self.node_id, nonce="bulk-over"
            ),
        )
        self.assertUnauthorized(refused)
        self.assertEqual(self.stored_nonce_count("bulk-over"), 0)
        self.assertEqual(self.nonces_for(self.node_id), cap)

        # Byte-identical to the signature refusal: no cap oracle either.
        signature_failure = self.client.get(
            TASKS_PATH, headers=self.bad_signature_headers("a-different-nonce")
        )
        self.assertEqual(refused.json(), signature_failure.json())

        # The cap is per node, not global: a second node is untouched.
        self.assertEqual(
            self.node_get(TASKS_PATH, other_key, other_id).status_code, 200
        )


class NodeAuthPrimitiveTests(unittest.TestCase):
    """The canonical-string contract, independent of the HTTP layer."""

    def test_canonical_request_is_unambiguous(self) -> None:
        one = nodeauth.canonical_request(
            node_id="nod_1", timestamp="1", nonce="a\nb", method="GET", path="/p", body=b""
        )
        two = nodeauth.canonical_request(
            node_id="nod_1", timestamp="1", nonce="a", method="GET", path="b\n/p", body=b""
        )
        self.assertNotEqual(one, two)

    def test_canonical_request_binds_the_node_id(self) -> None:
        """The signed bytes name the node, so a header swap changes the bytes.

        ``X-Yukinal-Node`` is unsigned routing data on the wire; putting the id
        in the canonical string is what makes it part of the identity the key
        signs. Without it one key registered under two node ids could be used
        for either of them.
        """
        arguments = dict(timestamp="1", nonce="n", method="GET", path="/p", body=b"")
        first = nodeauth.canonical_request(node_id="nod_1", **arguments)
        second = nodeauth.canonical_request(node_id="nod_2", **arguments)
        self.assertNotEqual(first, second)
        self.assertTrue(first.startswith(b"nod_1\n"), first)
        self.assertEqual(first.split(b"\n")[0], b"nod_1")

    def test_a_signature_for_one_node_id_does_not_verify_for_another(self) -> None:
        """The primitive half of the header-swap defence."""
        key = new_key()
        signed = nodeauth.sign_headers(
            key, node_id="nod_1", method="GET", path="/p", body=b"", nonce="n", timestamp=1
        )
        canonical_for_other = nodeauth.canonical_request(
            node_id="nod_2",
            timestamp=signed[nodeauth.HEADER_TIMESTAMP],
            nonce=signed[nodeauth.HEADER_NONCE],
            method="GET",
            path="/p",
            body=b"",
        )
        self.assertFalse(
            nodeauth.verify_signature(
                public_key(key), signed[nodeauth.HEADER_SIGNATURE], canonical_for_other
            )
        )
        canonical_for_self = nodeauth.canonical_request(
            node_id="nod_1",
            timestamp=signed[nodeauth.HEADER_TIMESTAMP],
            nonce=signed[nodeauth.HEADER_NONCE],
            method="GET",
            path="/p",
            body=b"",
        )
        self.assertTrue(
            nodeauth.verify_signature(
                public_key(key), signed[nodeauth.HEADER_SIGNATURE], canonical_for_self
            )
        )

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
