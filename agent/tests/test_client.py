from __future__ import annotations

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
