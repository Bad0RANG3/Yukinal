"""Node channel tests: assignment, status reporting and disconnect recovery.

These exercise the real WebSocket endpoint through Starlette's test client, so
the handshake signature, the message protocol and the effect on the task rows are
all observed rather than simulated.
"""

from __future__ import annotations

import unittest

from starlette.websockets import WebSocketDisconnect

from support import AUTH, CHANNEL_PATH, ControlPlaneTestCase, new_key

from yukinal_control_plane import channel


class NodeChannelTests(ControlPlaneTestCase):
    def test_channel_assigns_queued_work_and_records_the_outcome(self) -> None:
        tenant_id = self.create_tenant()
        node_id, key = self.provision_node(tenant_id)
        task_id = self.create_task(tenant_id, node_id)["body"]["id"]

        with self.node_socket(key, node_id) as socket:
            self.assertEqual(
                socket.receive_json(),
                {"type": channel.MESSAGE_READY, "nodeId": node_id, "assigned": 1},
            )
            assigned = socket.receive_json()
            self.assertEqual(assigned["type"], channel.MESSAGE_ASSIGNED)
            self.assertEqual(assigned["task"]["id"], task_id)
            # Assignment is the moment work leaves the queue.
            self.assertEqual(assigned["task"]["status"], "running")
            self.assertEqual(assigned["task"]["attempts"], 1)

            socket.send_json(
                {
                    "type": "status",
                    "taskId": task_id,
                    "status": "succeeded",
                    "outcome": "ok",
                    "result": {"code": 0},
                }
            )
            self.assertEqual(
                socket.receive_json(),
                {"type": channel.MESSAGE_ACK, "taskId": task_id, "status": "succeeded"},
            )

        task = self.wait_for_task_status(tenant_id, task_id, "succeeded")
        self.assertEqual(task["outcome"], "ok")
        self.assertEqual(task["result"], {"code": 0})
        actions = [
            event["action"]
            for event in self.client.get(f"/v1/tenants/{tenant_id}/audit", headers=AUTH).json()
        ]
        self.assertIn("task.transition", actions)

    def test_channel_disconnect_marks_running_work_unknown(self) -> None:
        tenant_id = self.create_tenant()
        node_id, key = self.provision_node(tenant_id)
        task_id = self.create_task(tenant_id, node_id)["body"]["id"]

        with self.node_socket(key, node_id) as socket:
            self.assertEqual(socket.receive_json()["assigned"], 1)
            self.assertEqual(socket.receive_json()["task"]["id"], task_id)
            # Drop the socket without reporting anything.

        task = self.wait_for_task_status(tenant_id, task_id, "unknown")
        self.assertEqual(task["outcome"], "node_disconnected")

        # Listed for a human decision through the node's own endpoint.
        recoverable = self.node_get("/v1/node/recoverable", key, node_id)
        self.assertEqual([entry["id"] for entry in recoverable.json()], [task_id])

        # Reconnecting does not replay it; the node states what actually happened.
        with self.node_socket(key, node_id) as socket:
            self.assertEqual(socket.receive_json()["assigned"], 0)
            socket.send_json(
                {
                    "type": "status",
                    "taskId": task_id,
                    "status": "succeeded",
                    "outcome": "late",
                }
            )
            self.assertEqual(socket.receive_json()["status"], "succeeded")

        self.assertEqual(
            self.wait_for_task_status(tenant_id, task_id, "succeeded")["outcome"], "late"
        )

    def test_channel_graceful_close_reports_the_marked_tasks(self) -> None:
        tenant_id = self.create_tenant()
        node_id, key = self.provision_node(tenant_id)
        task_id = self.create_task(tenant_id, node_id)["body"]["id"]

        with self.node_socket(key, node_id) as socket:
            self.assertEqual(socket.receive_json()["assigned"], 1)
            self.assertEqual(socket.receive_json()["task"]["id"], task_id)
            socket.send_json({"type": "close"})
            bye = socket.receive_json()
            self.assertEqual(bye["type"], channel.MESSAGE_BYE)
            self.assertEqual(bye["marked"], [task_id])

        task = self.wait_for_task_status(tenant_id, task_id, "unknown")
        self.assertEqual(task["outcome"], "node_closed")

    def test_channel_sync_pulls_newly_queued_work(self) -> None:
        tenant_id = self.create_tenant()
        node_id, key = self.provision_node(tenant_id)

        with self.node_socket(key, node_id) as socket:
            self.assertEqual(socket.receive_json()["assigned"], 0)
            task_id = self.create_task(tenant_id, node_id)["body"]["id"]
            socket.send_json({"type": "sync"})
            self.assertEqual(
                socket.receive_json(), {"type": channel.MESSAGE_SYNCED, "assigned": 1}
            )
            self.assertEqual(socket.receive_json()["task"]["id"], task_id)
            socket.send_json(
                {"type": "status", "taskId": task_id, "status": "failed", "outcome": "boom"}
            )
            self.assertEqual(socket.receive_json()["status"], "failed")

        self.assertEqual(self.wait_for_task_status(tenant_id, task_id, "failed")["outcome"], "boom")

    def test_channel_reports_a_bad_message_without_dropping_the_socket(self) -> None:
        tenant_id = self.create_tenant()
        node_id, key = self.provision_node(tenant_id)

        with self.node_socket(key, node_id) as socket:
            self.assertEqual(socket.receive_json()["type"], channel.MESSAGE_READY)
            for raw, code in (
                ("not json", "invalid_json"),
                ('{"type": "nonsense"}', "unknown_message"),
                ('{"type": "status", "status": "running"}', "invalid_status"),
                ('{"type": "status", "taskId": "tsk_missing", "status": "running"}', "not_found"),
            ):
                socket.send_text(raw)
                error = socket.receive_json()
                self.assertEqual(error["type"], channel.MESSAGE_ERROR)
                self.assertEqual(error["code"], code, error)
            # Still alive after all of that.
            socket.send_json({"type": "sync"})
            self.assertEqual(socket.receive_json()["type"], channel.MESSAGE_SYNCED)

    def test_a_node_may_not_report_another_nodes_task_over_the_channel(self) -> None:
        tenant_id = self.create_tenant()
        node_id, key = self.provision_node(tenant_id, "build-01")
        other_id, _other_key = self.provision_node(tenant_id, "build-02")
        foreign_task = self.create_task(tenant_id, other_id)["body"]["id"]

        with self.node_socket(key, node_id) as socket:
            self.assertEqual(socket.receive_json()["assigned"], 0)
            socket.send_json({"type": "status", "taskId": foreign_task, "status": "succeeded"})
            self.assertEqual(socket.receive_json()["code"], "not_found")

        self.assertEqual(self.get_task(tenant_id, foreign_task)["status"], "queued")


class NodeChannelHandshakeTests(ControlPlaneTestCase):
    def test_an_unauthenticated_handshake_is_closed(self) -> None:
        tenant_id = self.create_tenant()
        node_id, _key = self.provision_node(tenant_id)
        # A signature that is not from the registered key.
        with self.assertRaises(WebSocketDisconnect) as raised:
            with self.node_socket(new_key(), node_id) as socket:
                socket.receive_json()
        self.assertEqual(raised.exception.code, channel.CLOSE_UNAUTHORIZED)

    def test_a_handshake_without_headers_is_closed(self) -> None:
        with self.assertRaises(WebSocketDisconnect) as raised:
            with self.client.websocket_connect(CHANNEL_PATH) as socket:
                socket.receive_json()
        self.assertEqual(raised.exception.code, channel.CLOSE_UNAUTHORIZED)

    def test_a_handshake_with_a_replayed_nonce_is_closed(self) -> None:
        tenant_id = self.create_tenant()
        node_id, key = self.provision_node(tenant_id)
        headers = self.node_headers(
            key, "GET", CHANNEL_PATH, node_id=node_id, nonce="channel-nonce"
        )
        with self.client.websocket_connect(CHANNEL_PATH, headers=headers) as socket:
            self.assertEqual(socket.receive_json()["type"], channel.MESSAGE_READY)
        # The same handshake again is a replay.
        with self.assertRaises(WebSocketDisconnect) as raised:
            with self.client.websocket_connect(CHANNEL_PATH, headers=headers) as socket:
                socket.receive_json()
        self.assertEqual(raised.exception.code, channel.CLOSE_UNAUTHORIZED)


if __name__ == "__main__":
    unittest.main()
