from __future__ import annotations

import json
import os
import socket
import subprocess
import tempfile
import time
import unittest
import urllib.error
import urllib.request
from pathlib import Path

from yukinal_ssh import SshServiceClient, SshServiceError

TOKEN = "integration-test-token-long-enough-for-local-service"


def _unused_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


@unittest.skipUnless(os.environ.get("YUKINAL_SSH_SERVICE_BINARY"), "built C++ service path is not configured")
class ServiceIntegrationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="yukinal-ssh-service-")
        root = Path(self.temp.name)
        (root / "known_hosts").write_text("", encoding="utf-8")
        listen_port = _unused_port()
        ssh_port = _unused_port()
        while ssh_port == listen_port:
            ssh_port = _unused_port()
        config = {
            "listen_port": listen_port,
            "api_token_env": "YUKINAL_TEST_API_TOKEN",
            "targets": [
                {
                    "target_id": "loopback-test",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "username": "test-user",
                    "known_hosts_file": "known_hosts",
                    "auth": {"type": "password", "password_env": "YUKINAL_TEST_SSH_PASSWORD"},
                }
            ],
        }
        config_path = root / "service.json"
        config_path.write_text(json.dumps(config), encoding="utf-8")
        environment = os.environ.copy()
        environment["YUKINAL_TEST_API_TOKEN"] = TOKEN
        environment["YUKINAL_TEST_SSH_PASSWORD"] = "test-only-not-a-real-password"
        self.process = subprocess.Popen(
            [os.environ["YUKINAL_SSH_SERVICE_BINARY"], "--config", str(config_path)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            env=environment,
        )
        self.client = SshServiceClient(TOKEN, f"http://127.0.0.1:{config['listen_port']}")
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                self.fail(f"C++ service exited during startup with {self.process.returncode}")
            try:
                if self.client.health():
                    break
            except SshServiceError:
                time.sleep(0.05)
        else:
            self.fail("C++ service did not become healthy")

    def tearDown(self) -> None:
        if getattr(self, "process", None) and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self.temp.cleanup()

    def test_health_and_allowlist_rejection_through_python_client(self) -> None:
        self.assertTrue(self.client.health())
        with self.assertRaises(SshServiceError) as caught:
            self.client.service_status("not-configured", "nginx")
        self.assertEqual(caught.exception.code, "unknown_target")
        self.assertEqual(caught.exception.http_status, 404)

    def test_known_target_connection_failure_is_classified(self) -> None:
        with self.assertRaises(SshServiceError) as caught:
            self.client.service_status("loopback-test", "nginx", timeout_seconds=2)
        self.assertIn(caught.exception.code, {"connection_failed", "timeout"})

    def test_server_exec_is_disabled_in_persistent_target_config(self) -> None:
        request = urllib.request.Request(
            self.client._base_url + "/v1/ssh/exec",
            data=json.dumps({
                "target_id": "loopback-test",
                "operation": "server_exec",
                "parameters": {
                    "command": "id",
                    "max_output_bytes": 4096,
                    "timeout_ms": 1000,
                },
                "timeout_seconds": 1,
            }).encode("utf-8"),
            headers={
                "Authorization": f"Bearer {TOKEN}",
                "Content-Type": "application/json",
            },
            method="POST",
        )
        with self.assertRaises(urllib.error.HTTPError) as caught:
            urllib.request.urlopen(request, timeout=3)
        self.assertEqual(caught.exception.code, 502)
        body = json.loads(caught.exception.read())
        self.assertEqual(body["error"]["code"], "operation_not_allowed")

    def test_browser_origin_is_rejected_even_with_a_token(self) -> None:
        request = urllib.request.Request(
            self.client._base_url + "/v1/ssh/exec",
            data=json.dumps({
                "target_id": "not-configured",
                "operation": "service_status",
                "parameters": {"service": "nginx"},
                "timeout_seconds": 2,
            }).encode("utf-8"),
            headers={
                "Authorization": f"Bearer {TOKEN}",
                "Content-Type": "application/json",
                "Origin": "https://untrusted.example",
            },
            method="POST",
        )
        with self.assertRaises(urllib.error.HTTPError) as caught:
            urllib.request.urlopen(request, timeout=3)
        self.assertEqual(caught.exception.code, 401)


if __name__ == "__main__":
    unittest.main()
