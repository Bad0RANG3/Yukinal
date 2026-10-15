from __future__ import annotations

import base64
import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer
from unittest.mock import patch

from yukinal_ssh import SshServiceClient, SshServiceError
from yukinal_ssh.client import _SYSTEM_SNAPSHOT_COMMAND_IDS, _parse_system_snapshot_output

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

    def test_system_snapshot_sends_nonce_and_splits_every_fixed_command(self) -> None:
        nonce = "a" * 32
        self.server.response_body["operation"] = "system_snapshot"
        self.server.response_body["stdout"] = "".join(
            f"__YK_SNAPSHOT_{nonce}_{command_id}_BEGIN__\n"
            f"value-{command_id}\n\n"
            f"__YK_SNAPSHOT_{nonce}_{command_id}_END_{index % 3}__\n"
            for index, command_id in enumerate(_SYSTEM_SNAPSHOT_COMMAND_IDS)
        )

        with patch("yukinal_ssh.client.secrets.token_hex", return_value=nonce):
            results = self.client.system_snapshot("staging-web-01", 9)

        self.assertEqual(
            results,
            {
                command_id: (index % 3, f"value-{command_id}\n")
                for index, command_id in enumerate(_SYSTEM_SNAPSHOT_COMMAND_IDS)
            },
        )
        self.assertEqual(
            self.server.last_request["body"],
            {
                "target_id": "staging-web-01",
                "operation": "system_snapshot",
                "parameters": {"nonce": nonce},
                "timeout_seconds": 9,
            },
        )

    def test_system_snapshot_rejects_missing_duplicate_and_invalid_markers(self) -> None:
        nonce = "b" * 32
        valid = "".join(
            f"__YK_SNAPSHOT_{nonce}_{command_id}_BEGIN__\n"
            f"value\n\n__YK_SNAPSHOT_{nonce}_{command_id}_END_0__\n"
            for command_id in _SYSTEM_SNAPSHOT_COMMAND_IDS
        )
        cases = (
            (
                valid.replace(
                    f"__YK_SNAPSHOT_{nonce}_{_SYSTEM_SNAPSHOT_COMMAND_IDS[0]}_BEGIN__\n",
                    "",
                    1,
                ),
                "incomplete system snapshot",
            ),
            (
                valid + f"__YK_SNAPSHOT_{nonce}_{_SYSTEM_SNAPSHOT_COMMAND_IDS[0]}_BEGIN__\n",
                "incomplete system snapshot",
            ),
            (
                valid.replace("_END_0__", "_END_256__", 1),
                "invalid system snapshot status",
            ),
        )
        for output, expected_message in cases:
            with self.subTest(expected_message=expected_message), self.assertRaises(SshServiceError) as caught:
                _parse_system_snapshot_output(output, nonce)
            self.assertIn(expected_message, str(caught.exception))

    def test_read_only_operations_use_only_their_structured_parameters(self) -> None:
        calls = (
            ("connection_check", lambda: self.client.connection_check("staging-web-01", 7), {}),
            ("service_inventory", lambda: self.client.service_inventory("staging-web-01", 7), {}),
            (
                "docker_ps",
                lambda: self.client.docker_ps("staging-web-01", include_all_containers=True, timeout_seconds=7),
                {"all": True},
            ),
            (
                "docker_logs",
                lambda: self.client.docker_logs("staging-web-01", "api_1", tail=40, timeout_seconds=7),
                {"container": "api_1", "tail": 40},
            ),
            (
                "docker_inspect",
                lambda: self.client.docker_inspect("staging-web-01", "api_1", 7),
                {"container": "api_1"},
            ),
            (
                "systemd_inspect",
                lambda: self.client.systemd_inspect("staging-web-01", "nginx.service", 7),
                {"service": "nginx.service"},
            ),
            (
                "package_inspect",
                lambda: self.client.package_inspect("staging-web-01", "apt", "nginx", 7),
                {"manager": "apt", "package": "nginx"},
            ),
        )
        for operation, invoke, parameters in calls:
            with self.subTest(operation=operation):
                self.server.response_body["operation"] = operation
                result = invoke()
                self.assertEqual(result.operation, operation)
                self.assertEqual(
                    self.server.last_request["body"],
                    {
                        "target_id": "staging-web-01",
                        "operation": operation,
                        "parameters": parameters,
                        "timeout_seconds": 7,
                    },
                )

    def test_read_only_operation_arguments_are_rejected_before_network_call(self) -> None:
        invalid_calls = (
            lambda: self.client.docker_ps("staging-web-01", include_all_containers=1),
            lambda: self.client.docker_logs("staging-web-01", "api;id"),
            lambda: self.client.docker_logs("staging-web-01", "api", tail=True),
            lambda: self.client.docker_inspect("staging-web-01", "api/other"),
            lambda: self.client.systemd_inspect("staging-web-01", "nginx"),
            lambda: self.client.package_inspect("staging-web-01", "yum", "nginx"),
            lambda: self.client.package_inspect("staging-web-01", "apt", "nginx;id"),
        )
        for invoke in invalid_calls:
            with self.subTest(invoke=invoke), self.assertRaises(ValueError):
                invoke()
        self.assertIsNone(self.server.last_request)

    def test_systemd_restart_uses_a_structured_bounded_operation(self) -> None:
        self.server.response_body["operation"] = "systemd_restart"
        result = self.client.systemd_restart("staging-web-01", "nginx.service", 7)
        self.assertEqual(result.operation, "systemd_restart")
        self.assertEqual(
            self.server.last_request["body"],
            {
                "target_id": "staging-web-01",
                "operation": "systemd_restart",
                "parameters": {"service": "nginx.service", "timeout_seconds": 7},
                "timeout_seconds": 7,
            },
        )

    def test_systemd_restart_rejects_invalid_units_and_timeouts_before_network_call(self) -> None:
        invalid_calls = (
            lambda: self.client.systemd_restart("staging-web-01", "nginx"),
            lambda: self.client.systemd_restart("staging-web-01", "nginx.service;id"),
            lambda: self.client.systemd_restart("staging-web-01", "nginx.service", True),
            lambda: self.client.systemd_restart("staging-web-01", "nginx.service", 31),
        )
        for invoke in invalid_calls:
            with self.subTest(invoke=invoke), self.assertRaises(ValueError):
                invoke()
        self.assertIsNone(self.server.last_request)

    def test_docker_restart_uses_a_structured_bounded_operation(self) -> None:
        self.server.response_body["operation"] = "docker_restart"
        result = self.client.docker_restart("staging-web-01", "api-1", 20)
        self.assertEqual(result.operation, "docker_restart")
        self.assertEqual(
            self.server.last_request["body"],
            {
                "target_id": "staging-web-01",
                "operation": "docker_restart",
                "parameters": {"container": "api-1", "grace_period_seconds": 20},
                "timeout_seconds": 30,
            },
        )

    def test_docker_restart_rejects_invalid_containers_and_grace_periods(self) -> None:
        invalid_calls = (
            lambda: self.client.docker_restart("staging-web-01", "api;id"),
            lambda: self.client.docker_restart("staging-web-01", "api-1", True),
            lambda: self.client.docker_restart("staging-web-01", "api-1", 0),
            lambda: self.client.docker_restart("staging-web-01", "api-1", 121),
        )
        for invoke in invalid_calls:
            with self.subTest(invoke=invoke), self.assertRaises(ValueError):
                invoke()
        self.assertIsNone(self.server.last_request)

    def test_package_install_uses_a_validated_structured_operation(self) -> None:
        self.server.response_body["operation"] = "package_install"
        result = self.client.package_install(
            "staging-web-01", "apt", "nginx", "1.27.0-1", 180
        )
        self.assertEqual(result.operation, "package_install")
        self.assertEqual(
            self.server.last_request["body"],
            {
                "target_id": "staging-web-01",
                "operation": "package_install",
                "parameters": {
                    "manager": "apt",
                    "package": "nginx",
                    "version": "1.27.0-1",
                    "timeout_seconds": 180,
                },
                "timeout_seconds": 180,
            },
        )

    def test_package_install_rejects_invalid_manager_package_version_and_timeout(self) -> None:
        invalid_calls = (
            lambda: self.client.package_install("staging-web-01", "yum", "nginx"),
            lambda: self.client.package_install("staging-web-01", "apt", "nginx;id"),
            lambda: self.client.package_install("staging-web-01", "apt", "nginx", "1;id"),
            lambda: self.client.package_install("staging-web-01", "apt", "nginx", timeout_seconds=True),
            lambda: self.client.package_install("staging-web-01", "apt", "nginx", timeout_seconds=601),
        )
        for invoke in invalid_calls:
            with self.subTest(invoke=invoke), self.assertRaises(ValueError):
                invoke()
        self.assertIsNone(self.server.last_request)

    def test_server_exec_uses_bounded_one_shot_operation(self) -> None:
        self.server.response_body = {
            "status": "succeeded",
            "target_id": "staging-web-01",
            "operation": "server_exec",
            "exit_code": 0,
            "stdout": "hello\n",
            "stderr": "",
            "stdout_truncated": False,
            "stderr_truncated": False,
            "duration_ms": 12,
            "output_truncated": False,
            "error": None,
        }
        result = self.client.server_exec("staging-web-01", "printf hello", 1_250, 4096)
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(result.stdout, "hello\n")
        self.assertEqual(
            self.server.last_request,
            {
                "authorization": f"Bearer {TOKEN}",
                "body": {
                    "target_id": "staging-web-01",
                    "operation": "server_exec",
                    "parameters": {
                        "command": "printf hello",
                        "max_output_bytes": 4096,
                        "timeout_ms": 1250,
                    },
                    "timeout_seconds": 2,
                },
            },
        )

    def test_server_exec_rejects_unsafe_inputs_before_network_call(self) -> None:
        invalid_calls = (
            lambda: self.client.server_exec("staging-web-01", " ", 1000, 1024),
            lambda: self.client.server_exec("staging-web-01", "echo\x00bad", 1000, 1024),
            lambda: self.client.server_exec("staging-web-01", "echo ok", True, 1024),
            lambda: self.client.server_exec("staging-web-01", "echo ok", 0, 1024),
            lambda: self.client.server_exec("staging-web-01", "echo ok", 1000, True),
            lambda: self.client.server_exec("staging-web-01", "echo ok", 1000, 0),
        )
        for invoke in invalid_calls:
            with self.subTest(invoke=invoke), self.assertRaises(ValueError):
                invoke()
        self.assertIsNone(self.server.last_request)

    def test_server_exec_rejects_malformed_or_oversized_response(self) -> None:
        self.server.response_body = {
            "status": "succeeded",
            "target_id": "staging-web-01",
            "operation": "server_exec",
            "exit_code": 0,
            "stdout": "x" * 20,
            "stderr": "",
            "stdout_truncated": False,
            "stderr_truncated": False,
            "duration_ms": 12,
            "output_truncated": False,
            "error": None,
        }
        with self.assertRaises(SshServiceError) as caught:
            self.client.server_exec("staging-web-01", "printf x", 1000, 4)
        self.assertEqual(caught.exception.code, "invalid_response")

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

    def test_bounded_sftp_reads_decode_binary_and_allow_one_truncation_byte(self) -> None:
        self.server.response_body["operation"] = "sftp_read"
        self.server.response_body["stdout"] = base64.b64encode(b"alpha\x00").decode("ascii")
        result = self.client.sftp_read("staging-web-01", "/srv/app/data.bin", 5, 8)
        self.assertEqual(result, b"alpha\x00")
        self.assertEqual(
            self.server.last_request["body"],
            {
                "target_id": "staging-web-01",
                "operation": "sftp_read",
                "parameters": {"path": "/srv/app/data.bin", "max_bytes": 5},
                "timeout_seconds": 8,
            },
        )

    def test_sftp_read_rejects_invalid_limits_and_responses(self) -> None:
        for max_bytes in (True, -1, 1024 * 1024 + 1):
            with self.subTest(max_bytes=max_bytes), self.assertRaises(ValueError):
                self.client.sftp_read("staging-web-01", "/srv/app/data.bin", max_bytes)
        self.assertIsNone(self.server.last_request)

        self.server.response_body["operation"] = "sftp_read"
        self.server.response_body["stdout"] = base64.b64encode(b"1234567").decode("ascii")
        with self.assertRaises(SshServiceError) as oversize:
            self.client.sftp_read("staging-web-01", "/srv/app/data.bin", 5)
        self.assertEqual(oversize.exception.code, "invalid_response")

        self.server.response_body["stdout"] = "not base64"
        with self.assertRaises(SshServiceError) as invalid:
            self.client.sftp_read("staging-web-01", "/srv/app/data.bin", 5)
        self.assertEqual(invalid.exception.code, "invalid_response")

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
