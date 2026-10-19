"""The life of an approval: it lapses, it can be withdrawn, and it runs out.

An approval bound to an exact action is still only as good as its lifetime. These
tests hold the three ways a decision stops being usable — time passing, an
operator withdrawing it, and its uses being spent — and check that each one stops
both a *new* task and a *queued* one, because correctness cannot depend on the
gate it happened to pass earlier.
"""

from __future__ import annotations

import asyncio
import unittest
from datetime import datetime, timezone

from sqlalchemy import update
from sqlalchemy.ext.asyncio import create_async_engine
from sqlalchemy.pool import NullPool
from support import AUTH, ControlPlaneTestCase, new_key, public_key

from yukinal_control_plane import actions, models, service
from yukinal_control_plane.db import create_session_factory

EXEC = "server.exec"
EXEC_PAYLOAD = {"command": "systemctl restart nginx"}


class ApprovalLifetimeTests(ControlPlaneTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.tenant_id = self.create_tenant()
        self.node_id, _key = self.provision_node(self.tenant_id)

    def disable_tenant(self, tenant_id: str) -> None:
        """Take a tenant out of service the way an operator would.

        There is no API for it, so this writes the same column the app reads.
        """
        import sqlite3

        connection = sqlite3.connect(self.db_path)
        try:
            changed = connection.execute(
                "UPDATE tenants SET disabled = 1 WHERE id = ?", (tenant_id,)
            ).rowcount
            connection.commit()
        finally:
            connection.close()
        self.assertEqual(changed, 1, f"tenant `{tenant_id}` was not disabled")

    def audit_actions(self, tenant_id: str) -> list[str]:
        """Read a tenant's audit trail straight from the table.

        Used where the API cannot be asked — a disabled tenant answers 404 on
        ``/audit`` too — so an assertion about "nothing was written" is still an
        assertion about the database and not about the endpoint.
        """
        import sqlite3

        connection = sqlite3.connect(self.db_path)
        try:
            rows = connection.execute(
                "SELECT action FROM audit_events WHERE tenant_id = ? ORDER BY id",
                (tenant_id,),
            ).fetchall()
        finally:
            connection.close()
        return [row[0] for row in rows]

    def finish(self, task_id: str) -> None:
        """Run a task to a terminal state, freeing the node's active slot.

        Dispatch is checked on the move to ``running``, so this also proves the
        approval still covers a task it already authorised.
        """
        self.assertEqual(self.move(self.tenant_id, task_id, "running"), 200)
        self.assertEqual(self.move(self.tenant_id, task_id, "succeeded"), 200)

    # ── expiry ───────────────────────────────────────────────────────────────

    def test_approving_sets_a_window(self) -> None:
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        approval = self.get_approval(self.tenant_id, approval_id)
        self.assertEqual(approval["state"], "approved")
        self.assertEqual(approval["uses"], 0)
        self.assertEqual(approval["max_uses"], 1)
        self.assertIsNotNone(approval["expires_at"], approval)
        # The window starts when the decision is made and lasts the default TTL.
        # Both instants are read back from the database on this path, so this also
        # pins down the offset the API attaches to a value SQLite stored naive.
        expires = datetime.fromisoformat(approval["expires_at"])
        decided = datetime.fromisoformat(approval["decided_at"])
        self.assertEqual(expires.tzinfo, timezone.utc)
        self.assertEqual(decided.tzinfo, timezone.utc)
        self.assertAlmostEqual(
            (expires - decided).total_seconds(),
            actions.DEFAULT_APPROVAL_TTL_SECONDS,
            delta=5,
        )
        # And the window is still open, so the task it is meant for can be made.
        self.assertEqual(
            self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)["status"], 201
        )

    def test_timestamps_always_carry_an_offset(self) -> None:
        """A client must not have to guess whether a timestamp is UTC."""
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        approval = self.get_approval(self.tenant_id, approval_id)
        for field in ("requested_at", "decided_at", "expires_at", "revoked_at"):
            value = approval[field]
            if value is None:
                continue
            with self.subTest(field=field):
                self.assertRegex(value, r"(Z|\+00:00)$", f"{field} = {value!r}")

    def test_a_rejection_has_no_window(self) -> None:
        created = self.create_approval(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.decide(self.tenant_id, created.json()["id"], "rejected")
        approval = self.get_approval(self.tenant_id, created.json()["id"])
        self.assertEqual(approval["state"], "rejected")
        self.assertIsNone(approval["expires_at"])

    def test_a_lapsed_decision_authorises_nothing(self) -> None:
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.backdate_approval(approval_id)
        refused = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(refused["status"], 409, refused["body"])
        self.assertEqual(refused["body"]["code"], "approval_required")

    def test_listing_sweeps_a_lapsed_decision(self) -> None:
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.backdate_approval(approval_id)
        # The stored state is a convenience; the refusal above does not depend on
        # it. Listing is what makes it honest.
        self.assertEqual(self.get_approval(self.tenant_id, approval_id)["state"], "expired")
        audit = self.client.get(f"/v1/tenants/{self.tenant_id}/audit", headers=AUTH).json()
        expired = [event for event in audit if event["action"] == "approval.expired"]
        self.assertEqual([event["subject_id"] for event in expired], [approval_id])

    def test_the_sweep_endpoint_is_explicit_and_idempotent(self) -> None:
        first = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        second = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.backdate_approval(first)
        self.backdate_approval(second)
        swept = self.expire_approvals(self.tenant_id)
        self.assertEqual(swept.status_code, 200, swept.text)
        self.assertEqual(swept.json(), {"expired": 2})
        # Nothing is left to sweep, and a live decision is never touched.
        again = self.expire_approvals(self.tenant_id)
        self.assertEqual(again.json(), {"expired": 0})

    def test_the_sweep_leaves_a_live_decision_alone(self) -> None:
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(self.expire_approvals(self.tenant_id).json(), {"expired": 0})
        self.assertEqual(self.get_approval(self.tenant_id, approval_id)["state"], "approved")

    def test_the_sweep_is_tenant_scoped(self) -> None:
        other_tenant = self.create_tenant("Other")
        other_node, _key = self.provision_node(other_tenant, "other-01")
        mine = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        theirs = self.approve(other_tenant, other_node, EXEC, EXEC_PAYLOAD)
        self.backdate_approval(mine)
        self.backdate_approval(theirs)
        # Sweeping one tenant must not reach into another's rows. The stored state
        # is read directly, because listing the other tenant would sweep it too.
        self.assertEqual(self.expire_approvals(self.tenant_id).json(), {"expired": 1})
        self.assertEqual(self.stored_approval_state(mine), "expired")
        self.assertEqual(self.stored_approval_state(theirs), "approved")

    def test_the_sweep_still_runs_for_an_enabled_tenant(self) -> None:
        """The gate must not become an off switch.

        Pins the behaviour the fix could have broken: an enabled tenant's lapsed
        decision is still moved to ``expired`` and the move is still audited.
        """
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.backdate_approval(approval_id)
        before = self.audit_actions(self.tenant_id)

        swept = self.expire_approvals(self.tenant_id)
        self.assertEqual(swept.status_code, 200, swept.text)
        self.assertEqual(swept.json(), {"expired": 1})
        self.assertEqual(self.stored_approval_state(approval_id), "expired")
        after = self.audit_actions(self.tenant_id)
        self.assertEqual(after[: len(before)], before)
        self.assertEqual(after[len(before) :], ["approval.expired"])
        # And it is still idempotent: nothing is left to sweep.
        self.assertEqual(self.expire_approvals(self.tenant_id).json(), {"expired": 0})

    # ── the sweep is tenant-gated ────────────────────────────────────────────
    # Sweeping is a write. Every other route under ``/v1/tenants/{id}`` answers
    # 404 for a tenant that does not exist or has been disabled; this one has to
    # answer the same way rather than report a sweep of nothing, and it must not
    # reach the disabled tenant's rows while doing so.

    def test_the_sweep_of_a_missing_tenant_is_not_found(self) -> None:
        refused = self.expire_approvals("ten_does_not_exist")
        self.assertEqual(refused.status_code, 404, refused.text)
        self.assertEqual(refused.json()["code"], "not_found")
        # The same answer the rest of the tenant surface gives, so the sweep is
        # not the one endpoint that confirms a tenant id does or does not exist.
        sibling = self.client.get("/v1/tenants/ten_does_not_exist", headers=AUTH)
        self.assertEqual(refused.json(), sibling.json())

    def test_the_sweep_of_a_disabled_tenant_is_refused_and_touches_nothing(self) -> None:
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.backdate_approval(approval_id)
        before = self.audit_actions(self.tenant_id)
        self.disable_tenant(self.tenant_id)

        refused = self.expire_approvals(self.tenant_id)
        self.assertEqual(refused.status_code, 404, refused.text)
        self.assertEqual(refused.json()["code"], "not_found")

        # A 404 that still swept would be worse than a 200: the caller is told
        # the tenant is not there while its rows change.
        self.assertEqual(self.stored_approval_state(approval_id), "approved")
        self.assertEqual(self.audit_actions(self.tenant_id), before)
        self.assertNotIn("approval.expired", self.audit_actions(self.tenant_id))

    def test_a_disabled_tenant_is_refused_by_every_tenant_route(self) -> None:
        """The gate is the surface's, not one endpoint's: every route, one by one.

        The set is derived from the application's own route table and compared
        with the table below, so a route added later cannot silently escape the
        sweep; each one must answer 404 ``not_found``, the same answer the rest
        of the surface gives for a tenant that is not there. A 422 would also be
        "not less than 400" while proving nothing about the gate, which is why
        the assertion is on the exact refusal rather than on the status class.
        """
        task_id = self.create_task(self.tenant_id, self.node_id)["body"]["id"]
        approval_id = self.create_approval(
            self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD
        ).json()["id"]
        self.disable_tenant(self.tenant_id)

        body = {
            "nodes": {"name": "n2", "publicKey": public_key(new_key())},
            "heartbeat": {"restarted": False},
            "tasks": {"nodeId": self.node_id, "kind": "filesystem.read", "payload": {}},
            "status": {"status": "cancelled"},
            "model-configs": {"name": "m", "provider": "openai", "model": "gpt"},
            "policy-put": {"updatedBy": "ada"},
            "tool-policy": {"risk": "read", "updatedBy": "ada"},
            "approvals": {
                "action": EXEC,
                "nodeId": self.node_id,
                "payload": EXEC_PAYLOAD,
                "requestedBy": "ada",
            },
            "decide": {"decision": "approved", "decidedBy": "ada"},
            "revoke": {"revokedBy": "ada"},
        }
        suffix = "/nodes/{node_id}"
        paths = [
            ("GET", f"/v1/tenants/{{tenant_id}}", None),
            ("GET", f"/v1/tenants/{{tenant_id}}/nodes", None),
            ("POST", f"/v1/tenants/{{tenant_id}}/nodes", body["nodes"]),
            ("GET", f"/v1/tenants/{{tenant_id}}{suffix}", None),
            ("POST", f"/v1/tenants/{{tenant_id}}{suffix}/heartbeat", body["heartbeat"]),
            ("GET", f"/v1/tenants/{{tenant_id}}{suffix}/recoverable", None),
            ("GET", f"/v1/tenants/{{tenant_id}}/tasks", None),
            ("POST", f"/v1/tenants/{{tenant_id}}/tasks", body["tasks"]),
            ("GET", f"/v1/tenants/{{tenant_id}}/tasks/{{task_id}}", None),
            ("POST", f"/v1/tenants/{{tenant_id}}/tasks/{{task_id}}/status", body["status"]),
            ("GET", f"/v1/tenants/{{tenant_id}}/model-configs", None),
            ("POST", f"/v1/tenants/{{tenant_id}}/model-configs", body["model-configs"]),
            ("GET", f"/v1/tenants/{{tenant_id}}/policy", None),
            ("PUT", f"/v1/tenants/{{tenant_id}}/policy", body["policy-put"]),
            ("DELETE", f"/v1/tenants/{{tenant_id}}/policy", None),
            ("GET", f"/v1/tenants/{{tenant_id}}/tool-policies", None),
            ("PUT", f"/v1/tenants/{{tenant_id}}/tool-policies/{{kind}}", body["tool-policy"]),
            ("DELETE", f"/v1/tenants/{{tenant_id}}/tool-policies/{{kind}}", None),
            ("GET", f"/v1/tenants/{{tenant_id}}/approvals", None),
            ("POST", f"/v1/tenants/{{tenant_id}}/approvals", body["approvals"]),
            (
                "POST",
                f"/v1/tenants/{{tenant_id}}/approvals/{{approval_id}}/decide",
                body["decide"],
            ),
            (
                "POST",
                f"/v1/tenants/{{tenant_id}}/approvals/{{approval_id}}/revoke",
                body["revoke"],
            ),
            ("POST", f"/v1/tenants/{{tenant_id}}/approvals/expire", None),
            ("GET", f"/v1/tenants/{{tenant_id}}/audit", None),
        ]

        declared = {(method, path) for method, path, _ in paths}
        implemented = set()
        for route in self.app.routes:
            path = getattr(route, "path", "")
            methods = getattr(route, "methods", None) or set()
            if "{tenant_id}" not in path:
                continue
            for method in methods:
                if method not in ("HEAD", "OPTIONS"):
                    implemented.add((method, path))
        self.assertEqual(
            implemented,
            declared,
            "a `{tenant_id}` route is missing from this sweep",
        )

        substitutions = {
            "node_id": self.node_id,
            "task_id": task_id,
            "approval_id": approval_id,
            "kind": "mcp.docker.get",
        }
        for method, path, payload in paths:
            concrete = path
            for name, value in substitutions.items():
                concrete = concrete.replace(f"{{{name}}}", value)
            concrete = concrete.replace("{tenant_id}", self.tenant_id)
            with self.subTest(method=method, path=path):
                response = self.client.request(
                    method, concrete, json=payload, headers=AUTH
                )
                self.assertEqual(response.status_code, 404, f"{method} {concrete}: {response.text}")
                self.assertEqual(response.json()["code"], "not_found")

    # ── revocation ───────────────────────────────────────────────────────────

    def test_a_pending_request_can_be_withdrawn(self) -> None:
        created = self.create_approval(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        approval_id = created.json()["id"]
        revoked = self.revoke(self.tenant_id, approval_id, revoked_by="ada")
        self.assertEqual(revoked.status_code, 200, revoked.text)
        self.assertEqual(revoked.json()["state"], "revoked")
        self.assertEqual(revoked.json()["revoked_by"], "ada")
        self.assertIsNotNone(revoked.json()["revoked_at"])
        # A revoked request never counted as a decision, so it cannot be decided.
        self.assertEqual(self.decide(self.tenant_id, approval_id).status_code, 409)

    def test_an_approved_decision_can_be_withdrawn(self) -> None:
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        revoked = self.revoke(self.tenant_id, approval_id)
        self.assertEqual(revoked.status_code, 200, revoked.text)
        self.assertEqual(revoked.json()["state"], "revoked")
        refused = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(refused["status"], 409, refused["body"])
        # The reason names the withdrawal rather than pretending nothing matched.
        audit = self.client.get(f"/v1/tenants/{self.tenant_id}/audit", headers=AUTH).json()
        revoke_events = [event for event in audit if event["action"] == "approval.revoke"]
        self.assertEqual(revoke_events[0]["detail"]["from"], "approved")

    def test_withdrawing_stops_a_task_that_was_already_authorised(self) -> None:
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        task_id = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)["body"]["id"]
        self.revoke(self.tenant_id, approval_id)
        refused = self.client.post(
            f"/v1/tenants/{self.tenant_id}/tasks/{task_id}/status",
            json={"status": "running"},
            headers=AUTH,
        )
        self.assertEqual(refused.status_code, 409, refused.text)
        self.assertIn("approval_revoked", refused.json()["message"])
        # Withdrawal is not a rewrite of history: the task still records what
        # authorised it, and the task itself is untouched.
        task = self.get_task(self.tenant_id, task_id)
        self.assertEqual(task["status"], "queued")
        self.assertEqual(task["approval_id"], approval_id)

    def test_withdrawal_is_reported_on_the_channel(self) -> None:
        # A fresh node, so the channel handshake owns its own socket.
        node_id, key = self.provision_node(self.tenant_id, "build-02")
        approval_id = self.approve(self.tenant_id, node_id, EXEC, EXEC_PAYLOAD)
        task_id = self.create_task(self.tenant_id, node_id, EXEC, EXEC_PAYLOAD)["body"]["id"]
        self.revoke(self.tenant_id, approval_id)
        with self.node_socket(key, node_id) as socket:
            ready = socket.receive_json()
            self.assertEqual(ready["assigned"], 0)
            self.assertEqual(ready["waiting"][0]["reason"], "approval_revoked")
            self.assertEqual(ready["waiting"][0]["taskId"], task_id)

    def test_a_finished_decision_cannot_be_withdrawn(self) -> None:
        created = self.create_approval(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        approval_id = created.json()["id"]
        self.decide(self.tenant_id, approval_id, "rejected")
        self.assertEqual(self.revoke(self.tenant_id, approval_id).status_code, 409)
        # And a decision is withdrawn once, not twice.
        approved = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(self.revoke(self.tenant_id, approved).status_code, 200)
        self.assertEqual(self.revoke(self.tenant_id, approved).status_code, 409)

    def test_a_decision_cannot_name_a_lifecycle_state(self) -> None:
        created = self.create_approval(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        response = self.decide(self.tenant_id, created.json()["id"], "expired")
        self.assertEqual(response.status_code, 422, response.text)

    # ── uses ─────────────────────────────────────────────────────────────────

    def test_a_decision_runs_out_after_its_uses(self) -> None:
        approval_id = self.approve(
            self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD, max_uses=2
        )
        self.assertEqual(self.get_approval(self.tenant_id, approval_id)["max_uses"], 2)

        first = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(first["status"], 201, first["body"])
        self.assertEqual(self.get_approval(self.tenant_id, approval_id)["uses"], 1)
        # One active task per node, so the first has to finish before the second.
        self.finish(first["body"]["id"])

        second = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(second["status"], 201, second["body"])
        self.assertEqual(self.get_approval(self.tenant_id, approval_id)["uses"], 2)
        self.finish(second["body"]["id"])

        refused = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(refused["status"], 409, refused["body"])
        self.assertEqual(refused["body"]["code"], "approval_required")

    def test_a_spent_decision_still_covers_the_task_it_authorised(self) -> None:
        """Uses gate new work; they do not invalidate work already authorised."""
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        task_id = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)["body"]["id"]
        self.assertEqual(self.get_approval(self.tenant_id, approval_id)["uses"], 1)
        self.assertEqual(self.move(self.tenant_id, task_id, "running"), 200)

    def test_a_decision_can_narrow_the_requested_uses(self) -> None:
        created = self.create_approval(
            self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD, max_uses=5
        )
        approval_id = created.json()["id"]
        narrowed = self.decide(self.tenant_id, approval_id, max_uses=1)
        self.assertEqual(narrowed.status_code, 200, narrowed.text)
        self.assertEqual(narrowed.json()["max_uses"], 1)
        first = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(first["status"], 201, first["body"])
        self.finish(first["body"]["id"])
        second = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(second["status"], 409, second["body"])

    def test_a_decision_cannot_widen_the_requested_uses(self) -> None:
        created = self.create_approval(
            self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD, max_uses=2
        )
        widened = self.decide(self.tenant_id, created.json()["id"], max_uses=3)
        self.assertEqual(widened.status_code, 409, widened.text)

    def test_the_requested_uses_are_bounded(self) -> None:
        too_many = self.create_approval(
            self.tenant_id,
            self.node_id,
            EXEC,
            EXEC_PAYLOAD,
            max_uses=actions.MAX_APPROVAL_USES + 1,
        )
        self.assertEqual(too_many.status_code, 422, too_many.text)
        zero = self.create_approval(
            self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD, max_uses=0
        )
        self.assertEqual(zero.status_code, 422, zero.text)

    def test_the_default_is_one_use(self) -> None:
        created = self.create_approval(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(created.json()["max_uses"], 1)

    # ── the task records its authorisation ───────────────────────────────────

    def test_a_task_records_the_approval_that_authorised_it(self) -> None:
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        task = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)["body"]
        self.assertEqual(task["approval_id"], approval_id)

    def test_a_read_only_task_records_no_approval(self) -> None:
        task = self.create_task(
            self.tenant_id, self.node_id, "filesystem.read", {"path": "/etc/hosts"}
        )["body"]
        self.assertIsNone(task["approval_id"])


class ApprovalSweepRaceTests(ControlPlaneTestCase):
    """A decision withdrawn while the sweep is running must stay withdrawn.

    The sweep used to select the due ids and then update ``id IN (due)`` without
    re-reading state or deadline, so a row revoked in that window was overwritten
    and audited as ``approval.expired`` even though the operator's withdrawal had
    won. The revocation here is committed by another connection immediately
    before the sweep's own UPDATE.
    """

    def setUp(self) -> None:
        super().setUp()
        self.tenant_id = self.create_tenant()
        self.node_id, _key = self.provision_node(self.tenant_id)

    def run_service(self, coroutine):
        async def with_engine():
            engine = create_async_engine(self.url, poolclass=NullPool)
            try:
                return await coroutine(engine)
            finally:
                await engine.dispose()

        return asyncio.run(with_engine())

    def audit_actions(self) -> list[str]:
        import sqlite3

        connection = sqlite3.connect(self.db_path)
        try:
            rows = connection.execute(
                "SELECT action FROM audit_events WHERE tenant_id = ? ORDER BY id",
                (self.tenant_id,),
            ).fetchall()
        finally:
            connection.close()
        return [row[0] for row in rows]

    def test_a_revocation_committed_before_the_sweep_update_survives(self) -> None:
        approval_id = self.approve(
            self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD
        )
        self.backdate_approval(approval_id)

        async def scenario(engine):
            factory = create_session_factory(engine)
            async with factory() as session:
                async with session.begin():
                    real_execute = session.execute
                    revoked = False

                    async def revoke_before_update(statement, *args, **kwargs):
                        nonlocal revoked
                        table = getattr(statement, "table", None)
                        if (
                            not revoked
                            and isinstance(statement, type(update(models.Approval)))
                            and getattr(table, "name", None) == "approvals"
                        ):
                            revoked = True
                            async with factory() as other:
                                async with other.begin():
                                    await service.revoke_approval(
                                        other,
                                        tenant_id=self.tenant_id,
                                        approval_id=approval_id,
                                        revoked_by="ada",
                                    )
                        return await real_execute(statement, *args, **kwargs)

                    session.execute = revoke_before_update  # type: ignore[method-assign]
                    expired = await service.expire_stale_approvals(
                        session, tenant_id=self.tenant_id
                    )
                    return expired, revoked

        expired, revoked = self.run_service(scenario)
        # The ordering the test claims must really have happened: if the sweep
        # never reached its UPDATE the assertion below would prove nothing.
        self.assertTrue(revoked, "the withdrawal was never interposed")
        self.assertEqual(expired, 0)
        self.assertEqual(self.stored_approval_state(approval_id), "revoked")
        actions_seen = self.audit_actions()
        self.assertNotIn("approval.expired", actions_seen)
        self.assertIn("approval.revoke", actions_seen)


class ApprovalMetricsTests(ControlPlaneTestCase):
    def test_metrics_count_approvals_by_state(self) -> None:
        tenant_id = self.create_tenant()
        node_id, _key = self.provision_node(tenant_id)
        payload = {"command": "ls"}
        self.create_approval(tenant_id, node_id, EXEC, payload)  # stays pending
        approved = self.approve(tenant_id, node_id, EXEC, payload)
        created = self.create_approval(tenant_id, node_id, EXEC, payload)
        self.revoke(tenant_id, created.json()["id"])
        self.backdate_approval(approved)
        self.list_approvals(tenant_id)  # sweeps the lapsed one
        metrics = self.client.get("/v1/metrics", headers=AUTH).json()
        self.assertEqual(
            metrics["approvals_by_state"], {"pending": 1, "expired": 1, "revoked": 1}
        )
        self.assertEqual(metrics["pending_approvals"], 1)


if __name__ == "__main__":
    unittest.main()
