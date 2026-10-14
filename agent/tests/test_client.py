from __future__ import annotations

import base64
import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer

from yukinal_ssh import SshServiceClient, SshServiceError

TOKEN = "test-token-that-is-long-enough-for-local-service"


class _Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        self._respond(200, {"status": "healthy"})

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", "0"))
        self.server.last_request = {
            "authorization": self.headers.get("Authorization"),
            "body": json.loads(self.rfile.read(length)),
        }
        self._respond(self.server.status_code, self.server.response_body)

    def _respond(self, status: int, body: object) -> None:
        encoded = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, _format: str, *_args: object) -> None:
        return


class ClientTests(unittest.TestCase):
    def setUp(self) -> None:
        self.server = HTTPServer(("127.0.0.1", 0), _Handler)
        self.server.status_code = 200
        self.server.response_body = {
            "status": "succeeded",
            "target_id": "staging-web-01",
            "operation": "service_status",
            "exit_code": 0,
            "stdout": "active\n",
            "stderr": "",
            "duration_ms": 12,
            "output_truncated": False,
            "error": None,
        }
        self.server.last_request = None
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.client = SshServiceClient(TOKEN, f"http://127.0.0.1:{self.server.server_port}")

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def test_health_and_structured_call(self) -> None:
        self.assertTrue(self.client.health())
        result = self.client.service_status("staging-web-01", "nginx.service", 8)
        self.assertEqual(result.stdout, "active\n")
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(
            self.server.last_request,
            {
                "authorization": f"Bearer {TOKEN}",
                "body": {
                    "target_id": "staging-web-01",
                    "operation": "service_status",
                    "parameters": {"service": "nginx.service"},
                    "timeout_seconds": 8,
                },
            },
        )

    def test_nonzero_remote_exit_code_is_a_completed_status_query(self) -> None:
        self.server.response_body["exit_code"] = 3
        self.server.response_body["stdout"] = "inactive\n"
        result = self.client.service_status("staging-web-01", "nginx", 3)
        self.assertEqual(result.exit_code, 3)
        self.assertEqual(result.stdout, "inactive\n")

    def test_service_inventory_is_a_parameterless_structured_operation(self) -> None:
        self.server.response_body["operation"] = "service_inventory"
        self.server.response_body["stdout"] = "nginx.service loaded active running Nginx\n__YUKINAL_SOURCE__=systemd\n"
        result = self.client.service_inventory("staging-web-01", 8)
        self.assertEqual(result.operation, "service_inventory")
        self.assertIn("__YUKINAL_SOURCE__=systemd", result.stdout)
        self.assertEqual(
            self.server.last_request["body"],
            {
                "target_id": "staging-web-01",
                "operation": "service_inventory",
                "parameters": {},
                "timeout_seconds": 8,
            },
        )

    def test_server_logs_exposes_only_bounded_filters(self) -> None:
        self.server.response_body["operation"] = "server_logs"
        self.server.response_body["stdout"] = "__YUKINAL_SOURCE__=journalctl\n"
        result = self.client.server_logs(
            "staging-web-01", since_seconds=3600, unit="nginx.service", timeout_seconds=9
        )
        self.assertEqual(result.operation, "server_logs")
        self.assertEqual(
            self.server.last_request["body"],
            {
                "target_id": "staging-web-01",
                "operation": "server_logs",
                "parameters": {"since_seconds": 3600, "unit": "nginx.service"},
                "timeout_seconds": 9,
            },
        )

    def test_server_logs_rejects_invalid_filters_before_network_call(self) -> None:
        for since_seconds, unit in [
            (0, None),
            (604_801, None),
            (True, None),
            (None, "nginx.service;id"),
        ]:
            with self.subTest(since_seconds=since_seconds, unit=unit):
                with self.assertRaises(ValueError):
                    self.client.server_logs("staging-web-01", since_seconds, unit)
        self.assertIsNone(self.server.last_request)

    def test_bounded_binary_sftp_writes_use_base64_and_structured_paths(self) -> None:
        self.server.response_body["operation"] = "sftp_write"
        self.server.response_body["stdout"] = '{"bytes_written":4}'
        result = self.client.sftp_write("staging-web-01", "/srv/app/data.bin", b"\x00\x01\x02\xff", 8)
        self.assertEqual(result.operation, "sftp_write")
        self.assertEqual(
            self.server.last_request["body"],
            {
                "target_id": "staging-web-01",
                "operation": "sftp_write",
                "parameters": {"path": "/srv/app/data.bin", "content_base64": "AAEC/w=="},
                "timeout_seconds": 8,
            },
        )
        self.assertEqual(base64.b64decode(self.server.last_request["body"]["parameters"]["content_base64"]), b"\x00\x01\x02\xff")

    def test_sftp_mutation_limits_are_checked_before_network_call(self) -> None:
        with self.assertRaises(ValueError):
            self.client.sftp_write("staging-web-01", "/srv/app/data.bin", b"x" * (512 * 1024 + 1))
        with self.assertRaises(ValueError):
            self.client.sftp_create_exclusive("staging-web-01", "/srv/app/data.bin", b"x" * (1024 * 1024 + 1))
        with self.assertRaises(ValueError):
            self.client.sftp_remove_file("staging-web-01", "/srv/app/\x00data")
        self.assertIsNone(self.server.last_request)

    def test_link_count_returns_only_a_non_negative_integer_or_null(self) -> None:
        self.server.response_body["operation"] = "sftp_link_count"
        self.server.response_body["stdout"] = '{"link_count":1}'
        self.assertEqual(self.client.sftp_link_count("staging-web-01", "/srv/app/data.bin", 8), 1)
        self.server.response_body["stdout"] = '{"link_count":null}'
        self.assertIsNone(self.client.sftp_link_count("staging-web-01", "/srv/app/missing", 8))

    def test_structured_service_errors_are_preserved(self) -> None:
        self.server.status_code = 404
        self.server.response_body = {"status": "failed", "error": {"code": "unknown_target", "message": "not allow-listed"}}
        with self.assertRaises(SshServiceError) as caught:
            self.client.service_status("staging-web-01", "nginx")
        self.assertEqual(caught.exception.code, "unknown_target")
        self.assertEqual(caught.exception.http_status, 404)

    def test_rejects_shell_injection_and_invalid_timeout_before_network_call(self) -> None:
        with self.assertRaises(ValueError):
            self.client.service_status("staging-web-01", "nginx; whoami")
        with self.assertRaises(ValueError):
            self.client.service_status("staging-web-01", "nginx", True)
        self.assertIsNone(self.server.last_request)

    def test_endpoint_and_token_are_constrained(self) -> None:
        with self.assertRaises(ValueError):
            SshServiceClient(TOKEN, "http://example.com:8765")
        with self.assertRaises(ValueError):
            SshServiceClient("short", f"http://127.0.0.1:{self.server.server_port}")

    def test_invalid_service_response_is_rejected(self) -> None:
        self.server.response_body["duration_ms"] = "quick"
        with self.assertRaises(SshServiceError) as caught:
            self.client.service_status("staging-web-01", "nginx")
        self.assertEqual(caught.exception.code, "invalid_response")


if __name__ == "__main__":
    unittest.main()
