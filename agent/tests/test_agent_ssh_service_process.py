from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import AsyncMock, patch

from yukinal_agent.ssh_service_process import SshServiceProcess, SshServiceProcessError


class _ChildProcess:
    returncode: int | None = None

    def terminate(self) -> None:
        self.returncode = 0

    async def wait(self) -> int:
        return self.returncode or 0


class SshServiceProcessTests(unittest.IsolatedAsyncioTestCase):
    async def test_private_key_environment_reference_reaches_only_cpp_child(self) -> None:
        with tempfile.TemporaryDirectory(prefix="yukinal-ssh-process-") as temporary:
            config = Path(temporary) / "service.json"
            config.write_text(json.dumps({
                "listen_port": 8765,
                "api_token_env": "YUKINAL_SSH_API_TOKEN",
                "targets": [{
                    "auth": {"type": "private_key", "private_key_env": "YK_SSH_KEY_0"},
                    "proxy": {
                        "type": "http_connect",
                        "host": "127.0.0.1",
                        "port": 8080,
                        "auth_env": "YK_PROXY_CREDENTIAL",
                    },
                }],
            }), encoding="utf-8")
            environment = {
                "YUKINAL_SSH_SERVICE_BINARY": sys.executable,
                "YUKINAL_SSH_SERVICE_CONFIG": str(config),
                "YUKINAL_SSH_API_TOKEN": "test-token",
                "YK_SSH_KEY_0": "private-key-material",
                "YK_PROXY_CREDENTIAL": "proxy-user:proxy-password",
                "SSH_AUTH_SOCK": "/tmp/agent.sock",
                "UNRELATED_SECRET": "must-not-be-forwarded",
            }
            with patch.dict(os.environ, environment, clear=True):
                service = SshServiceProcess.from_environment()
                self.assertIsNotNone(service)
                assert service is not None

                child = _ChildProcess()
                with (
                    patch(
                        "yukinal_agent.ssh_service_process.asyncio.create_subprocess_exec",
                        new_callable=AsyncMock,
                        return_value=child,
                    ) as spawn,
                    patch(
                        "yukinal_agent.ssh_service_process._health_check",
                        new_callable=AsyncMock,
                        side_effect=[False, True],
                    ),
                ):
                    await service.start()
                    child_environment = spawn.await_args.kwargs["env"]
                    self.assertEqual(child_environment["YK_SSH_KEY_0"], "private-key-material")
                    self.assertEqual(child_environment["YK_PROXY_CREDENTIAL"], "proxy-user:proxy-password")
                    self.assertNotIn("SSH_AUTH_SOCK", child_environment)
                    self.assertNotIn("UNRELATED_SECRET", child_environment)
                    await service.stop()

    async def test_agent_socket_is_forwarded_only_for_agent_target(self) -> None:
        with tempfile.TemporaryDirectory(prefix="yukinal-ssh-process-") as temporary:
            config = Path(temporary) / "service.json"
            config.write_text(json.dumps({
                "listen_port": 8765,
                "api_token_env": "YUKINAL_SSH_API_TOKEN",
                "targets": [{"auth": {"type": "agent"}}],
            }), encoding="utf-8")
            environment = {
                "YUKINAL_SSH_SERVICE_BINARY": sys.executable,
                "YUKINAL_SSH_SERVICE_CONFIG": str(config),
                "YUKINAL_SSH_API_TOKEN": "test-token",
                "SSH_AUTH_SOCK": "/tmp/yukinal-agent.sock",
                "UNRELATED_SECRET": "must-not-be-forwarded",
            }
            with patch.dict(os.environ, environment, clear=True):
                service = SshServiceProcess.from_environment()
                self.assertIsNotNone(service)
                assert service is not None
                child = _ChildProcess()
                with (
                    patch(
                        "yukinal_agent.ssh_service_process.asyncio.create_subprocess_exec",
                        new_callable=AsyncMock,
                        return_value=child,
                    ) as spawn,
                    patch(
                        "yukinal_agent.ssh_service_process._health_check",
                        new_callable=AsyncMock,
                        side_effect=[False, True],
                    ),
                ):
                    await service.start()
                    child_environment = spawn.await_args.kwargs["env"]
                    self.assertEqual(child_environment["SSH_AUTH_SOCK"], "/tmp/yukinal-agent.sock")
                    self.assertNotIn("UNRELATED_SECRET", child_environment)
                    await service.stop()

    async def test_desktop_agent_target_accepts_only_a_valid_agent_socket(self) -> None:
        target = {
            "target_id": "srv_example1",
            "host": "stage.example.test",
            "port": 22,
            "username": "ops",
            "host_key_fingerprint": "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "auth": {"type": "agent"},
        }
        with tempfile.TemporaryDirectory(prefix="yukinal-ssh-process-") as temporary:
            binary = Path(temporary) / "service.exe"
            binary.write_text("service", encoding="utf-8")
            service, _, _ = SshServiceProcess.for_desktop_target(
                binary,
                {"target": target, "credentials": {"SSH_AUTH_SOCK": "/tmp/agent.sock"}},
            )
            self.assertEqual(service._secret_values, {"SSH_AUTH_SOCK": "/tmp/agent.sock"})
            await service.stop()

            with self.assertRaises(SshServiceProcessError):
                SshServiceProcess.for_desktop_target(
                    binary,
                    {"target": target, "credentials": {"SSH_AUTH_SOCK": "bad\x00path"}},
                )

            with self.assertRaises(SshServiceProcessError):
                SshServiceProcess.for_desktop_target(
                    binary,
                    {"target": target, "credentials": {"UNRELATED": "value"}},
                )

    async def test_desktop_target_forwards_http_proxy_credentials_to_cpp_child(self) -> None:
        target = {
            "target_id": "srv_example1",
            "host": "stage.example.test",
            "port": 22,
            "username": "ops",
            "host_key_fingerprint": "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "auth": {"type": "agent"},
            "proxy": {
                "type": "http_connect",
                "host": "proxy.example.test",
                "port": 8080,
                "auth_env": "YK_SSH_PROXY_CREDENTIAL",
                "no_proxy": ["*.corp", "10.*", "localhost"],
            },
        }
        credentials = {
            "SSH_AUTH_SOCK": "/tmp/agent.sock",
            "YK_SSH_PROXY_CREDENTIAL": "proxy-user:proxy-password",
        }
        with tempfile.TemporaryDirectory(prefix="yukinal-ssh-process-") as temporary:
            binary = Path(temporary) / "service.exe"
            binary.write_text("service", encoding="utf-8")
            service, _, _ = SshServiceProcess.for_desktop_target(
                binary, {"target": target, "credentials": credentials}
            )
            self.assertEqual(service._secret_values, credentials)
            configured_target = json.loads(service._config.read_text(encoding="utf-8"))["targets"][0]
            self.assertEqual(configured_target["proxy"], target["proxy"])
            await service.stop()

            invalid_target = dict(target)
            invalid_target["proxy"] = {**target["proxy"], "auth_env": "PATH"}
            with self.assertRaises(SshServiceProcessError):
                SshServiceProcess.for_desktop_target(
                    binary, {"target": invalid_target, "credentials": credentials}
                )

            invalid_target = dict(target)
            invalid_target["proxy"] = {**target["proxy"], "no_proxy": ["10.0.0.0/8"]}
            with self.assertRaises(SshServiceProcessError):
                SshServiceProcess.for_desktop_target(
                    binary, {"target": invalid_target, "credentials": credentials}
                )


if __name__ == "__main__":
    unittest.main()
