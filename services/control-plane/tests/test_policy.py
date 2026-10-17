"""The tenant's action policy: what a kind *is*, and what happens to its tier.

Two questions, deliberately separate. The first is a fact — the risk of a kind,
which comes from this tenant's declaration, the host's own declaration for its
built-in tools, or the closed default. The second is a decision — the mode of that
risk's permission tier, which is per tenant. These tests hold both, and hold the
seam where a policy change has to reach work that is already queued.
"""

from __future__ import annotations

import unittest
from datetime import datetime, timezone

from support import AUTH, ControlPlaneTestCase

from yukinal_control_plane import actions, service

EXEC = "server.exec"
EXEC_PAYLOAD = {"command": "systemctl restart nginx"}
MCP_KIND = "mcp.docker.get"
MCP_PAYLOAD = {"container": "web", "tail": 100}


class ActionPolicyTableTests(unittest.TestCase):
    """The table itself, and its agreement with the shared risk layer."""

    def test_the_shared_vocabulary_is_mirrored(self) -> None:
        """`packages/shared/src/types/risk.ts` is the source of truth."""
        self.assertEqual(actions.RISK_LEVELS, ("read", "low", "medium", "high", "critical"))
        self.assertEqual(actions.PERMISSION_TIERS, ("read", "write", "dangerous"))
        self.assertEqual(actions.TIER_MODES, ("auto", "ask", "deny"))
        self.assertEqual(actions.ENVIRONMENTS, ("local", "development", "staging", "production", "unknown"))

    def test_levels_collapse_into_tiers_the_same_way(self) -> None:
        self.assertEqual(actions.tier_of("read"), "read")
        self.assertEqual(actions.tier_of("low"), "read")
        self.assertEqual(actions.tier_of("medium"), "write")
        self.assertEqual(actions.tier_of("high"), "dangerous")
        self.assertEqual(actions.tier_of("critical"), "dangerous")
        self.assertEqual(actions.risk_rank("read"), 0)
        self.assertEqual(actions.risk_rank("critical"), 4)

    def test_the_builtin_sets_are_the_hosts_two_sets(self) -> None:
        """`host_tool_is_plan_bound_observation` and `host_tool_is_effectful`."""
        self.assertEqual(
            sorted(actions.READ_ONLY_TASK_KINDS),
            sorted(
                [
                    "server.info", "server.logs", "server.services",
                    "docker.ps", "docker.logs", "docker.inspect",
                    "systemd.inspect", "package.inspect", "filesystem.read",
                    "filesystem.backup.list", "filesystem.backup.retention",
                ]
            ),
        )
        self.assertEqual(
            sorted(actions.ALWAYS_EFFECTFUL_TASK_KINDS),
            sorted(
                [
                    "docker.restart", "server.exec", "systemd.restart",
                    "package.install", "filesystem.write", "filesystem.edit",
                    "filesystem.backup", "filesystem.backup.cleanup",
                    "filesystem.restore",
                ]
            ),
        )
        # The host decides these, so nothing can resolve them to `auto`.
        for kind in actions.ALWAYS_EFFECTFUL_TASK_KINDS:
            with self.subTest(kind=kind):
                self.assertTrue(actions.requires_approval(kind))
                self.assertEqual(actions.effective_risk(kind), actions.EFFECTFUL_RISK)

    def test_an_unknown_kind_needs_an_approval_by_default(self) -> None:
        self.assertEqual(actions.effective_risk("unknown.tool"), actions.UNKNOWN_RISK)
        self.assertTrue(actions.requires_approval("unknown.tool"))
        self.assertTrue(actions.requires_approval("mcp.anything"))

    def test_a_declaration_is_what_makes_a_kind_safe(self) -> None:
        declared = {"mcp.docker.get": "read"}
        self.assertEqual(actions.effective_risk("mcp.docker.get", declared), "read")
        self.assertFalse(actions.requires_approval("mcp.docker.get", declared=declared))
        self.assertFalse(actions.requires_approval("mcp.who.knows", declared={"mcp.who.knows": "low"}))

    def test_a_declaration_never_overrides_the_host(self) -> None:
        """The requirement of an always-effectful kind is not the tenant's to set."""
        declared = {"server.exec": "read"}
        # Even a stored row naming it `read` cannot lower it: the host effectful
        # set is checked first, in the decision itself rather than only in the
        # API that refuses to store such a row.
        self.assertEqual(actions.effective_risk("server.exec", declared), actions.EFFECTFUL_RISK)
        self.assertNotEqual(actions.action_mode("server.exec", declared=declared), "auto")

    def test_an_environment_selects_the_tier_modes(self) -> None:
        self.assertEqual(
            actions.default_tier_modes("development"),
            {"read": "auto", "write": "auto", "dangerous": "ask"},
        )
        self.assertEqual(
            actions.default_tier_modes("production"),
            {"read": "auto", "write": "ask", "dangerous": "ask"},
        )
        # An unknown environment must never be more permissive than production.
        self.assertEqual(
            actions.default_tier_modes("unknown"),
            actions.default_tier_modes("production"),
        )
        # And the caller cannot mutate the table by mutating the copy it got.
        modes = actions.default_tier_modes("local")
        modes["read"] = "deny"
        self.assertEqual(actions.default_tier_modes("local")["read"], "auto")

    def test_the_development_table_auto_approves_a_write(self) -> None:
        development = actions.default_tier_modes("development")
        # A medium kind is a write, and a dev box may do it without asking.
        self.assertFalse(
            actions.requires_approval("some.medium.tool", declared={"some.medium.tool": "medium"}, tier_modes=development)
        )
        self.assertTrue(
            actions.requires_approval("some.medium.tool", declared={"some.medium.tool": "medium"})
        )

    def test_tier_modes_are_validated(self) -> None:
        for bad in [
            {"read": "auto", "write": "ask"},  # missing tier
            {"read": "auto", "write": "ask", "dangerous": "ask", "extra": "ask"},
            {"read": "auto", "write": "sometimes", "dangerous": "ask"},
            {"read": "auto", "write": "ask", "dangerous": "auto"},  # the floor
        ]:
            with self.subTest(modes=bad):
                with self.assertRaises(ValueError):
                    actions.validate_tier_modes(bad)
        actions.validate_tier_modes({"read": "auto", "write": "deny", "dangerous": "ask"})

    def test_the_effective_view_marks_where_a_risk_came_from(self) -> None:
        entries = {entry["kind"]: entry for entry in service.effective_policy_view({}, {})}
        self.assertEqual(entries["server.info"]["source"], "host")
        self.assertEqual(entries["server.info"]["mode"], "auto")
        self.assertEqual(entries["server.exec"]["source"], "host")
        self.assertEqual(entries["server.exec"]["mode"], "ask")
        self.assertNotIn("mcp.docker.get", entries)
        declared_view = {
            entry["kind"]: entry
            for entry in service.effective_policy_view({"mcp.docker.get": "read"}, {})
        }
        self.assertEqual(declared_view["mcp.docker.get"]["source"], "declared")
        self.assertEqual(declared_view["mcp.docker.get"]["risk"], "read")
        self.assertEqual(declared_view["mcp.docker.get"]["tier"], "read")
        self.assertEqual(declared_view["mcp.docker.get"]["mode"], "auto")


class TenantPolicyEndpointTests(ControlPlaneTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.tenant_id = self.create_tenant()
        self.node_id, _key = self.provision_node(self.tenant_id)

    def approvals(self) -> list:
        return self.list_approvals(self.tenant_id)

    # ── defaults and storage ─────────────────────────────────────────────────

    def test_a_tenant_with_no_policy_gets_the_closed_defaults(self) -> None:
        policy = self.get_policy(self.tenant_id)
        self.assertFalse(policy["stored"])
        self.assertEqual(policy["environment"], actions.DEFAULT_ENVIRONMENT)
        self.assertEqual(policy["tier_modes"], actions.default_tier_modes("production"))
        self.assertEqual(policy["approval_ttl_seconds"], actions.DEFAULT_APPROVAL_TTL_SECONDS)
        self.assertIsNone(policy["updated_at"])
        # A read-only kind is still free and an effectful one still is not.
        self.assertEqual(
            self.create_task(self.tenant_id, self.node_id, "filesystem.read", {"path": "/x"})["status"],
            201,
        )
        refused = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(refused["status"], 409)
        self.assertEqual(refused["body"]["code"], "approval_required")

    def test_setting_a_policy_stores_it_and_is_audited(self) -> None:
        response = self.put_policy(self.tenant_id, environment="staging", approvalTtlSeconds=120)
        self.assertEqual(response.status_code, 200, response.text)
        policy = response.json()
        self.assertTrue(policy["stored"])
        self.assertEqual(policy["environment"], "staging")
        self.assertEqual(policy["tier_modes"], actions.default_tier_modes("staging"))
        self.assertEqual(policy["approval_ttl_seconds"], 120)
        self.assertEqual(policy["updated_by"], "ada")
        self.assertIsNotNone(policy["updated_at"])
        # Stored, not derived: a read agrees with the write.
        self.assertEqual(self.get_policy(self.tenant_id), policy)
        audit = self.client.get(f"/v1/tenants/{self.tenant_id}/audit", headers=AUTH).json()
        events = [event for event in audit if event["action"] == "tenant_policy.set"]
        self.assertEqual(events[0]["detail"]["approvalTtlSeconds"], 120)

    def test_naming_an_environment_derives_its_table(self) -> None:
        policy = self.put_policy(self.tenant_id, environment="development").json()
        self.assertEqual(
            policy["tier_modes"], {"read": "auto", "write": "auto", "dangerous": "ask"}
        )

    def test_an_explicit_table_wins_over_the_environment(self) -> None:
        policy = self.put_policy(
            self.tenant_id,
            environment="development",
            tierModes={"read": "auto", "write": "deny", "dangerous": "ask"},
        ).json()
        self.assertEqual(policy["environment"], "development")
        self.assertEqual(policy["tier_modes"]["write"], "deny")

    def test_changing_one_field_keeps_the_others(self) -> None:
        self.put_policy(self.tenant_id, environment="development", approvalTtlSeconds=200)
        policy = self.put_policy(self.tenant_id, approvalTtlSeconds=300).json()
        self.assertEqual(policy["environment"], "development")
        self.assertEqual(policy["tier_modes"], actions.default_tier_modes("development"))
        self.assertEqual(policy["approval_ttl_seconds"], 300)

    def test_the_stored_ttl_is_the_window_an_approval_gets(self) -> None:
        self.put_policy(self.tenant_id, approvalTtlSeconds=120)
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        approval = self.get_approval(self.tenant_id, approval_id)
        decided = datetime.fromisoformat(approval["decided_at"])
        expires = datetime.fromisoformat(approval["expires_at"])
        self.assertEqual(decided.tzinfo, timezone.utc)
        self.assertAlmostEqual((expires - decided).total_seconds(), 120, delta=5)

    def test_a_policy_can_be_dropped(self) -> None:
        self.put_policy(self.tenant_id, approvalTtlSeconds=120)
        self.assertEqual(self.delete_policy(self.tenant_id).status_code, 204)
        self.assertFalse(self.get_policy(self.tenant_id)["stored"])
        self.assertEqual(self.delete_policy(self.tenant_id).status_code, 404)

    # ── validation ───────────────────────────────────────────────────────────

    def test_a_ttl_outside_the_window_is_rejected(self) -> None:
        for ttl in (actions.MIN_APPROVAL_TTL_SECONDS - 1, actions.MAX_APPROVAL_TTL_SECONDS + 1):
            with self.subTest(ttl=ttl):
                self.assertEqual(
                    self.put_policy(self.tenant_id, approvalTtlSeconds=ttl).status_code, 422
                )

    def test_an_unknown_environment_is_rejected(self) -> None:
        self.assertEqual(self.put_policy(self.tenant_id, environment="prod").status_code, 422)

    def test_an_incomplete_or_open_tier_table_is_rejected(self) -> None:
        cases = [
            {"read": "auto", "write": "ask"},  # missing `dangerous`
            {"read": "auto", "write": "ask", "dangerous": "auto"},  # the floor
            {"read": "auto", "write": "ask", "dangerous": "ask", "extra": "ask"},
            {"read": "auto", "write": "sometimes", "dangerous": "ask"},
        ]
        for tier_modes in cases:
            with self.subTest(tier_modes=tier_modes):
                response = self.put_policy(self.tenant_id, tierModes=tier_modes)
                self.assertEqual(response.status_code, 422, response.text)
        # Nothing was stored by any of those attempts.
        self.assertFalse(self.get_policy(self.tenant_id)["stored"])

    def test_an_unknown_field_is_rejected(self) -> None:
        response = self.client.put(
            f"/v1/tenants/{self.tenant_id}/policy",
            json={"updatedBy": "ada", "approvalTtl": 100},
            headers=AUTH,
        )
        self.assertEqual(response.status_code, 422, response.text)

    # ── scoping ──────────────────────────────────────────────────────────────

    def test_a_policy_is_per_tenant(self) -> None:
        other = self.create_tenant("Other")
        self.put_policy(self.tenant_id, environment="development", approvalTtlSeconds=120)
        self.assertFalse(self.get_policy(other)["stored"])
        self.assertEqual(self.get_policy(other)["approval_ttl_seconds"], actions.DEFAULT_APPROVAL_TTL_SECONDS)

    def test_a_policy_for_an_unknown_tenant_is_not_found(self) -> None:
        response = self.client.get("/v1/tenants/ten_missing/policy", headers=AUTH)
        self.assertEqual(response.status_code, 404, response.text)
        self.assertEqual(self.put_policy("ten_missing", approvalTtlSeconds=120).status_code, 404)
        self.assertEqual(self.delete_policy("ten_missing").status_code, 404)


class ToolPolicyEndpointTests(ControlPlaneTestCase):
    def setUp(self) -> None:
        super().setUp()
        self.tenant_id = self.create_tenant()
        self.node_id, _key = self.provision_node(self.tenant_id)

    def test_the_listing_shows_the_builtins_and_the_closed_default(self) -> None:
        entries = {entry["kind"]: entry for entry in self.list_tool_policies(self.tenant_id)}
        self.assertEqual(entries["filesystem.read"]["source"], "host")
        self.assertEqual(entries["filesystem.read"]["tier"], "read")
        self.assertEqual(entries["filesystem.read"]["mode"], "auto")
        self.assertEqual(entries["docker.restart"]["source"], "host")
        self.assertEqual(entries["docker.restart"]["tier"], "dangerous")
        self.assertEqual(entries["docker.restart"]["mode"], "ask")
        # A kind the control plane does not ship is not listed as known until the
        # tenant says something about it.
        self.assertNotIn(MCP_KIND, entries)

    def test_declaring_a_trusted_kind_lets_it_run(self) -> None:
        # First: the closed default refuses it.
        self.assertEqual(
            self.create_task(self.tenant_id, self.node_id, MCP_KIND, MCP_PAYLOAD)["status"], 409
        )
        response = self.put_tool_policy(self.tenant_id, MCP_KIND, "read")
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(response.json()["source"], "declared")
        self.assertEqual(response.json()["mode"], "auto")
        created = self.create_task(self.tenant_id, self.node_id, MCP_KIND, MCP_PAYLOAD)
        self.assertEqual(created["status"], 201, created["body"])
        # A kind needing no decision records none.
        self.assertIsNone(created["body"]["approval_id"])

    def test_declaring_a_write_still_asks(self) -> None:
        self.put_tool_policy(self.tenant_id, MCP_KIND, "medium")
        entry = self.tool_policy(self.tenant_id, MCP_KIND)
        self.assertEqual(entry["tier"], "write")
        self.assertEqual(entry["mode"], "ask")
        self.assertEqual(
            self.create_task(self.tenant_id, self.node_id, MCP_KIND, MCP_PAYLOAD)["status"], 409
        )

    def test_a_builtin_observation_can_be_tightened(self) -> None:
        """A tenant may decide its own readers are not all safe readers."""
        self.put_tool_policy(self.tenant_id, "filesystem.read", "critical")
        entry = self.tool_policy(self.tenant_id, "filesystem.read")
        self.assertEqual(entry["source"], "declared")
        self.assertEqual(entry["mode"], "ask")
        self.assertEqual(
            self.create_task(self.tenant_id, self.node_id, "filesystem.read", {"path": "/x"})["status"],
            409,
        )

    def test_the_hosts_own_effectful_set_cannot_be_redeclared(self) -> None:
        for kind in sorted(actions.ALWAYS_EFFECTFUL_TASK_KINDS):
            with self.subTest(kind=kind):
                response = self.put_tool_policy(self.tenant_id, kind, "read")
                self.assertEqual(response.status_code, 409, response.text)
                self.assertEqual(response.json()["code"], "conflict")
        # And nothing was stored by the attempt.
        entry = self.tool_policy(self.tenant_id, "server.exec")
        self.assertEqual(entry["source"], "host")

    def test_an_unknown_risk_is_rejected(self) -> None:
        self.assertEqual(self.put_tool_policy(self.tenant_id, MCP_KIND, "harmless").status_code, 422)
        self.assertEqual(self.put_tool_policy(self.tenant_id, MCP_KIND, "READ").status_code, 422)

    def test_a_declaration_can_be_cleared(self) -> None:
        self.put_tool_policy(self.tenant_id, MCP_KIND, "read")
        self.assertEqual(self.delete_tool_policy(self.tenant_id, MCP_KIND).status_code, 204)
        self.assertIsNone(self.tool_policy(self.tenant_id, MCP_KIND))
        self.assertEqual(
            self.create_task(self.tenant_id, self.node_id, MCP_KIND, MCP_PAYLOAD)["status"], 409
        )
        self.assertEqual(self.delete_tool_policy(self.tenant_id, MCP_KIND).status_code, 404)
        # Clearing a host-declared kind restores the host's declaration.
        self.put_tool_policy(self.tenant_id, "filesystem.read", "critical")
        self.delete_tool_policy(self.tenant_id, "filesystem.read")
        self.assertEqual(self.tool_policy(self.tenant_id, "filesystem.read")["source"], "host")
        self.assertEqual(self.tool_policy(self.tenant_id, "filesystem.read")["mode"], "auto")

    def test_declarations_are_per_tenant(self) -> None:
        other = self.create_tenant("Other")
        other_node, _key = self.provision_node(other, "other-01")
        self.put_tool_policy(self.tenant_id, MCP_KIND, "read")
        self.assertIsNone(self.tool_policy(other, MCP_KIND))
        self.assertEqual(
            self.create_task(other, other_node, MCP_KIND, MCP_PAYLOAD)["status"], 409
        )

    def test_a_declaration_is_audited(self) -> None:
        self.put_tool_policy(self.tenant_id, MCP_KIND, "low")
        audit = self.client.get(f"/v1/tenants/{self.tenant_id}/audit", headers=AUTH).json()
        events = [event for event in audit if event["action"] == "tool_policy.set"]
        self.assertEqual(events[0]["detail"], {"kind": MCP_KIND, "risk": "low"})


class PolicyReachesQueuedWorkTests(ControlPlaneTestCase):
    """A policy change has to reach work that is already queued, not only new work."""

    def setUp(self) -> None:
        super().setUp()
        self.tenant_id = self.create_tenant()
        self.node_id, _key = self.provision_node(self.tenant_id)

    def test_tightening_an_exempt_kind_stops_a_queued_task(self) -> None:
        self.put_tool_policy(self.tenant_id, MCP_KIND, "read")
        task_id = self.create_task(self.tenant_id, self.node_id, MCP_KIND, MCP_PAYLOAD)["body"]["id"]
        self.assertEqual(self.get_task(self.tenant_id, task_id)["status"], "queued")
        # The task was created with no decision because the kind was exempt. The
        # tenant changes its mind before anybody dispatches it.
        self.put_tool_policy(self.tenant_id, MCP_KIND, "medium")
        self.assertEqual(self.move(self.tenant_id, task_id, "running"), 409)
        self.assertEqual(self.get_task(self.tenant_id, task_id)["status"], "queued")
        self.assertIsNone(self.get_task(self.tenant_id, task_id)["approval_id"])
        # Approving the action now does not retroactively clear the old task: its
        # authorisation is the one it recorded, and it recorded none. (Letting a
        # later decision clear it would also let it be spent without its use being
        # counted.) The path forward is a fresh task.
        approval_id = self.approve(self.tenant_id, self.node_id, MCP_KIND, MCP_PAYLOAD)
        self.assertEqual(self.move(self.tenant_id, task_id, "running"), 409)
        # Free the node's single active slot, then take the path forward.
        self.assertEqual(self.move(self.tenant_id, task_id, "cancelled"), 200)
        second = self.create_task(self.tenant_id, self.node_id, MCP_KIND, MCP_PAYLOAD)
        self.assertEqual(second["status"], 201, second["body"])
        self.assertEqual(second["body"]["approval_id"], approval_id)

    def test_deny_refuses_creation_outright(self) -> None:
        self.put_policy(
            self.tenant_id, tierModes={"read": "auto", "write": "deny", "dangerous": "ask"}
        )
        self.put_tool_policy(self.tenant_id, MCP_KIND, "medium")
        refused = self.create_task(self.tenant_id, self.node_id, MCP_KIND, MCP_PAYLOAD)
        self.assertEqual(refused["status"], 403, refused["body"])
        self.assertEqual(refused["body"]["code"], "action_denied")
        # No task was created at all, so nothing holds the node's active slot.
        listed = self.client.get(f"/v1/tenants/{self.tenant_id}/tasks", headers=AUTH).json()
        self.assertEqual(listed, [])

    def test_deny_beats_an_approval_that_already_exists(self) -> None:
        """A denial is absolute; no decision can clear it."""
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.put_policy(
            self.tenant_id, tierModes={"read": "auto", "write": "ask", "dangerous": "deny"}
        )
        refused = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        self.assertEqual(refused["status"], 403, refused["body"])
        self.assertEqual(refused["body"]["code"], "action_denied")
        # The approval is untouched — it was a decision about an action, and the
        # policy now refuses that action rather than the decision being wrong.
        self.assertEqual(self.get_approval(self.tenant_id, approval_id)["state"], "approved")

    def test_deny_stops_a_task_that_was_already_authorised(self) -> None:
        approval_id = self.approve(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)
        task_id = self.create_task(self.tenant_id, self.node_id, EXEC, EXEC_PAYLOAD)["body"]["id"]
        self.put_policy(
            self.tenant_id, tierModes={"read": "auto", "write": "ask", "dangerous": "deny"}
        )
        refused = self.client.post(
            f"/v1/tenants/{self.tenant_id}/tasks/{task_id}/status",
            json={"status": "running"},
            headers=AUTH,
        )
        self.assertEqual(refused.status_code, 403, refused.text)
        self.assertEqual(refused.json()["code"], "action_denied")
        self.assertEqual(self.get_task(self.tenant_id, task_id)["status"], "queued")
        self.assertEqual(self.get_approval(self.tenant_id, approval_id)["state"], "approved")

    def test_a_denial_is_reported_on_the_channel(self) -> None:
        node_id, key = self.provision_node(self.tenant_id, "build-02")
        approval_id = self.approve(self.tenant_id, node_id, EXEC, EXEC_PAYLOAD)
        task_id = self.create_task(self.tenant_id, node_id, EXEC, EXEC_PAYLOAD)["body"]["id"]
        self.put_policy(
            self.tenant_id, tierModes={"read": "auto", "write": "ask", "dangerous": "deny"}
        )
        with self.node_socket(key, node_id) as socket:
            ready = socket.receive_json()
            self.assertEqual(ready["assigned"], 0)
            self.assertEqual(ready["waiting"][0]["reason"], "policy_denied")
            self.assertEqual(ready["waiting"][0]["taskId"], task_id)
        # The decision still exists; the queue simply may not advance.
        self.assertEqual(self.get_approval(self.tenant_id, approval_id)["state"], "approved")

    def test_deny_leaves_other_tiers_alone(self) -> None:
        self.put_policy(
            self.tenant_id, tierModes={"read": "auto", "write": "deny", "dangerous": "ask"}
        )
        # A read still runs, and a dangerous action still asks rather than being
        # refused: `deny` is per tier, not a mode for the whole tenant.
        self.assertEqual(
            self.create_task(self.tenant_id, self.node_id, "filesystem.read", {"path": "/x"})["status"],
            201,
        )


if __name__ == "__main__":
    unittest.main()
