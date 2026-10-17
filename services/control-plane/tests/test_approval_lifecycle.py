"""The life of an approval: it lapses, it can be withdrawn, and it runs out.

An approval bound to an exact action is still only as good as its lifetime. These
tests hold the three ways a decision stops being usable — time passing, an
operator withdrawing it, and its uses being spent — and check that each one stops
both a *new* task and a *queued* one, because correctness cannot depend on the
gate it happened to pass earlier.
"""

from __future__ import annotations

import unittest
from datetime import datetime, timezone

from support import AUTH, ControlPlaneTestCase

from yukinal_control_plane import actions

EXEC = "server.exec"
EXEC_PAYLOAD = {"command": "systemctl restart nginx"}


class ApprovalLifetimeTests(ControlPlaneTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.tenant_id = self.create_tenant()
        self.node_id, _key = self.provision_node(self.tenant_id)

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
