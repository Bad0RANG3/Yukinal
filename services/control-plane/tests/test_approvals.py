"""Approval binding and atomic assignment.

Two properties are under test. First, an effectful task only exists and only
starts when an approved decision matches its *exact* action — same tenant, same
kind, same node, same payload — so an approval for one input can never dispatch
another. Second, assigning queued work is a conditional update, so two workers
cannot both hand the same task to a node.
"""

from __future__ import annotations

import asyncio
import unittest

from sqlalchemy import text
from sqlalchemy.ext.asyncio import create_async_engine
from sqlalchemy.pool import NullPool
from support import AUTH, ControlPlaneTestCase

from yukinal_control_plane import actions, models, service
from yukinal_control_plane.db import create_session_factory

EXEC = "server.exec"
EXEC_PAYLOAD = {"command": "systemctl restart nginx"}


class ApprovalBindingTests(ControlPlaneTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.tenant_id = self.create_tenant()
        self.node_id, _key = self.provision_node(self.tenant_id)

    def moved_to_running(self, task_id: str) -> int:
        return self.move(self.tenant_id, task_id, "running")

    # ── the gate ─────────────────────────────────────────────────────────────

    def test_a_read_only_kind_needs_no_approval(self) -> None:
        created = self.create_task(
            self.tenant_id, self.node_id, "filesystem.read", {"path": "/etc/hosts"}
        )
        self.assertEqual(created["status"], 201, created["body"])
        # Its fingerprint is still published, so it can be referenced.
        self.assertEqual(
            created["body"]["fingerprint"],
            actions.action_fingerprint(
                kind="filesystem.read", node_id=self.node_id, payload={"path": "/etc/hosts"}
            ),
        )
        self.assertEqual(self.moved_to_running(created["body"]["id"]), 200)

    def test_an_effectful_kind_is_refused_without_an_approval(self) -> None:
        refused = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(refused["status"], 409, refused["body"])
        self.assertEqual(refused["body"]["code"], "approval_required")
        # The refusal names the fingerprint to approve, so the operator is not
        # left guessing what the control plane wants.
        self.assertIn(
            actions.action_fingerprint(kind=EXEC, node_id=self.node_id, payload=EXEC_PAYLOAD),
            refused["body"]["message"],
        )
        # Nothing was queued, so the node is still free.
        self.assertEqual(self.client.get(f"/v1/tenants/{self.tenant_id}/tasks", headers=AUTH).json(), [])

    def test_an_unknown_kind_is_treated_as_effectful(self) -> None:
        refused = self.create_task(self.tenant_id, self.node_id, "custom.mutate", {})
        self.assertEqual(refused["status"], 409, refused["body"])
        self.assertEqual(refused["body"]["code"], "approval_required")

    def test_a_pending_approval_does_not_clear_a_task(self) -> None:
        self.assertEqual(
            self.create_approval(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD).status_code,
            201,
        )
        self.assertEqual(
            self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)["status"], 409
        )

    def test_a_rejected_approval_does_not_clear_a_task(self) -> None:
        created = self.create_approval(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.decide(self.tenant_id, created.json()["id"], "rejected")
        self.assertEqual(
            self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)["status"], 409
        )

    # ── the binding is exact ─────────────────────────────────────────────────

    def test_an_approved_action_clears_the_matching_task(self) -> None:
        self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        created = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(created["status"], 201, created["body"])
        task = created["body"]
        fingerprint = actions.action_fingerprint(
            kind=EXEC, node_id=self.node_id, payload=EXEC_PAYLOAD
        )
        self.assertEqual(task["fingerprint"], fingerprint)
        # The approval carries the same fingerprint and the same visible input,
        # so an approver can see exactly what they authorised.
        approval = self.client.get(
            f"/v1/tenants/{self.tenant_id}/approvals", headers=AUTH
        ).json()[0]
        self.assertEqual(approval["fingerprint"], fingerprint)
        self.assertEqual(approval["node_id"], self.node_id)
        self.assertEqual(approval["payload"], EXEC_PAYLOAD)
        self.assertEqual(self.moved_to_running(task["id"]), 200)

    def test_a_different_payload_is_not_covered(self) -> None:
        self.approve(self.tenant_id, self.node_id, EXEC, {"command": "ls"})
        refused = self.create_task(self.tenant_id, self.node_id, EXEC, {"command": "rm -rf /"})
        self.assertEqual(refused["status"], 409, refused["body"])

    def test_a_different_node_is_not_covered(self) -> None:
        other_id, _key = self.provision_node(self.tenant_id, "build-02")
        self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        refused = self.create_task(self.tenant_id, other_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(refused["status"], 409, refused["body"])

    def test_a_different_action_is_not_covered(self) -> None:
        self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        refused = self.create_task(self.tenant_id, self.node_id, "docker.restart", EXEC_PAYLOAD)
        self.assertEqual(refused["status"], 409, refused["body"])

    def test_another_tenants_approval_does_not_clear_a_task(self) -> None:
        other_tenant = self.create_tenant("Other")
        other_node, _key = self.provision_node(other_tenant, "other-01")
        # Same action, same input text, different tenant: not this tenant's
        # decision to spend.
        self.approve(other_tenant, other_node, EXEC, EXEC_PAYLOAD)
        refused = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(refused["status"], 409, refused["body"])

    # ── the request itself ───────────────────────────────────────────────────

    def test_an_approval_for_a_foreign_node_is_not_found(self) -> None:
        other_tenant = self.create_tenant("Other")
        other_node, _key = self.provision_node(other_tenant)
        response = self.create_approval(self.tenant_id, other_node, EXEC, EXEC_PAYLOAD)
        self.assertEqual(response.status_code, 404, response.text)

    def test_an_approval_cannot_be_requested_for_a_foreign_task(self) -> None:
        other_tenant = self.create_tenant("Other")
        other_node, _key = self.provision_node(other_tenant)
        foreign_task = self.create_task(other_tenant, other_node)["body"]["id"]
        response = self.client.post(
            f"/v1/tenants/{self.tenant_id}/approvals",
            json={
                "action": EXEC,
                "nodeId": self.node_id,
                "payload": EXEC_PAYLOAD,
                "taskId": foreign_task,
                "requestedBy": "agent",
            },
            headers=AUTH,
        )
        self.assertEqual(response.status_code, 404, response.text)

    def test_a_caller_cannot_supply_a_fingerprint(self) -> None:
        # The field is gone from the schema: a caller that tries to name the
        # fingerprint is refused rather than quietly ignored.
        response = self.client.post(
            f"/v1/tenants/{self.tenant_id}/approvals",
            json={
                "action": EXEC,
                "nodeId": self.node_id,
                "payload": EXEC_PAYLOAD,
                "fingerprint": "0" * 64,
                "requestedBy": "agent",
            },
            headers=AUTH,
        )
        self.assertEqual(response.status_code, 422, response.text)

    def test_an_expired_approval_does_not_dispatch(self) -> None:
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        task_id = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)["body"]["id"]

        # The approval lapses between creation and dispatch.
        self.backdate_approval(approval_id)

        refused = self.client.post(
            f"/v1/tenants/{self.tenant_id}/tasks/{task_id}/status",
            json={"status": "running"},
            headers=AUTH,
        )
        self.assertEqual(refused.status_code, 409, refused.text)
        self.assertEqual(refused.json()["code"], "approval_required")
        self.assertIn("approval_expired", refused.json()["message"])
        self.assertEqual(self.get_task(self.tenant_id, task_id)["status"], "queued")


class ApprovalChannelTests(ControlPlaneTestCase):
    def test_the_channel_reports_work_waiting_for_an_approval(self) -> None:
        tenant_id = self.create_tenant()
        node_id, key = self.provision_node(tenant_id)
        approval_id = self.approve(tenant_id, node_id, EXEC, EXEC_PAYLOAD)
        task_id = self.create_task(tenant_id, node_id, EXEC, EXEC_PAYLOAD)["body"]["id"]

        # The decision lapses after the task was authorised by it.
        self.backdate_approval(approval_id)

        with self.node_socket(key, node_id) as socket:
            ready = socket.receive_json()
            # Held back, not handed over and not silently dropped.
            self.assertEqual(ready["assigned"], 0)
            self.assertEqual(
                ready["waiting"],
                [
                    {
                        "taskId": task_id,
                        "reason": "approval_expired",
                        "fingerprint": actions.action_fingerprint(
                            kind=EXEC, node_id=node_id, payload=EXEC_PAYLOAD
                        ),
                    }
                ],
            )
            # And the task did not move.
            self.assertEqual(self.get_task(tenant_id, task_id)["status"], "queued")

    def test_an_approved_task_is_dispatched_over_the_channel(self) -> None:
        tenant_id = self.create_tenant()
        node_id, key = self.provision_node(tenant_id)
        self.approve(tenant_id, node_id, EXEC, EXEC_PAYLOAD)
        task_id = self.create_task(tenant_id, node_id, EXEC, EXEC_PAYLOAD)["body"]["id"]

        with self.node_socket(key, node_id) as socket:
            ready = socket.receive_json()
            self.assertEqual(ready["assigned"], 1)
            self.assertEqual(ready["waiting"], [])
            assigned = socket.receive_json()
            self.assertEqual(assigned["task"]["id"], task_id)
            self.assertEqual(assigned["task"]["status"], "running")


class AtomicAssignmentTests(ControlPlaneTestCase):
    """The claim predicate, driven through the service layer.

    The test client is synchronous and the app engine closes every connection
    between requests, so these open their own engine over the same SQLite file.
    """

    def setUp(self) -> None:
        super().setUp()
        self.tenant_id = self.create_tenant()
        self.node_id, _key = self.provision_node(self.tenant_id)
        self.task_id = self.create_task(self.tenant_id, self.node_id)["body"]["id"]

    def run_service(self, coroutine):
        async def with_engine():
            engine = create_async_engine(self.url, poolclass=NullPool)
            try:
                return await coroutine(engine)
            finally:
                await engine.dispose()

        return asyncio.run(with_engine())

    def test_a_queued_task_can_only_be_claimed_once(self) -> None:
        async def scenario(engine):
            factory = create_session_factory(engine)
            async with factory() as session:
                async with session.begin():
                    first = await service.claim_queued_task(session, self.task_id)
            async with factory() as session:
                async with session.begin():
                    second = await service.claim_queued_task(session, self.task_id)
            return first, second

        first, second = self.run_service(scenario)
        self.assertTrue(first)
        self.assertFalse(second)
        task = self.get_task(self.tenant_id, self.task_id)
        self.assertEqual(task["status"], "running")
        self.assertEqual(task["attempts"], 1)

    def test_the_claim_predicate_reads_the_database_not_python_state(self) -> None:
        """A worker holding a stale object still cannot steal the task."""

        async def scenario(engine):
            factory = create_session_factory(engine)
            async with factory() as session:
                async with session.begin():
                    stale = await session.get(models.Task, self.task_id)
                    self.assertEqual(stale.status, "queued")
                    # Another worker moves the task on behind this object's back.
                    await session.execute(
                        text("UPDATE tasks SET status = 'running' WHERE id = :id"),
                        {"id": self.task_id},
                    )
                    status = await session.scalar(
                        text("SELECT status FROM tasks WHERE id = :id"), {"id": self.task_id}
                    )
                    self.assertEqual(status, "running")
                    return await service.claim_queued_task(session, self.task_id)

        self.assertFalse(self.run_service(scenario))
        self.assertEqual(self.get_task(self.tenant_id, self.task_id)["status"], "running")

    def test_assignment_hands_a_task_out_once(self) -> None:
        async def scenario(engine):
            factory = create_session_factory(engine)
            async with factory() as session:
                async with session.begin():
                    node = await service.require_node_by_id(session, self.node_id)
                    first, first_waiting = await service.assign_queued_tasks(session, node)
                    second, second_waiting = await service.assign_queued_tasks(session, node)
                    return (
                        [task.id for task in first],
                        first_waiting,
                        [task.id for task in second],
                        second_waiting,
                    )

        first, first_waiting, second, second_waiting = self.run_service(scenario)
        self.assertEqual(first, [self.task_id])
        self.assertEqual(second, [])
        self.assertEqual(first_waiting, [])
        self.assertEqual(second_waiting, [])
        self.assertEqual(self.get_task(self.tenant_id, self.task_id)["attempts"], 1)

        # Exactly one queued-to-running audit event, so a retry cannot inflate
        # the ledger either.
        audit = self.client.get(f"/v1/tenants/{self.tenant_id}/audit", headers=AUTH).json()
        transitions = [
            event
            for event in audit
            if event["action"] == "task.transition" and event["subject_id"] == self.task_id
        ]
        self.assertEqual(len(transitions), 1)
        self.assertEqual(
            transitions[0]["detail"], {"from": "queued", "to": "running", "outcome": None}
        )


class ActionPolicyTests(unittest.TestCase):
    """The policy table itself, without the HTTP layer."""

    def test_only_the_hosts_observations_skip_approval(self) -> None:
        for kind in sorted(actions.READ_ONLY_TASK_KINDS):
            with self.subTest(kind=kind):
                self.assertFalse(actions.requires_approval(kind))
        for kind in ["server.exec", "docker.restart", "filesystem.write", "filesystem.edit",
                     "filesystem.backup", "filesystem.backup.cleanup", "filesystem.restore",
                     "systemd.restart", "package.install", "mcp.docker.get", "unknown.tool"]:
            with self.subTest(kind=kind):
                self.assertTrue(actions.requires_approval(kind))

    def test_the_fingerprint_is_key_order_independent(self) -> None:
        first = actions.action_fingerprint(
            kind="server.exec", node_id="nod_1", payload={"b": 1, "a": {"d": 2, "c": 3}}
        )
        second = actions.action_fingerprint(
            kind="server.exec", node_id="nod_1", payload={"a": {"c": 3, "d": 2}, "b": 1}
        )
        self.assertEqual(first, second)
        self.assertEqual(len(first), 64)
        self.assertEqual(first, first.lower())

    def test_the_fingerprint_separates_kind_node_and_payload(self) -> None:
        base = actions.action_fingerprint(kind="server.exec", node_id="nod_1", payload={"a": 1})
        self.assertNotEqual(
            base, actions.action_fingerprint(kind="server.exec", node_id="nod_2", payload={"a": 1})
        )
        self.assertNotEqual(
            base, actions.action_fingerprint(kind="docker.restart", node_id="nod_1", payload={"a": 1})
        )
        self.assertNotEqual(
            base, actions.action_fingerprint(kind="server.exec", node_id="nod_1", payload={"a": 2})
        )

    def test_an_empty_payload_has_a_stable_fingerprint(self) -> None:
        self.assertEqual(
            actions.action_fingerprint(kind="server.exec", node_id="nod_1", payload={}),
            actions.action_fingerprint(kind="server.exec", node_id="nod_1", payload={}),
        )


if __name__ == "__main__":
    unittest.main()
