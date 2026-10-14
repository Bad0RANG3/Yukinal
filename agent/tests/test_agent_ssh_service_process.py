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
                }],
            }), encoding="utf-8")
            environment = {
                "YUKINAL_SSH_SERVICE_BINARY": sys.executable,
                "YUKINAL_SSH_SERVICE_CONFIG": str(config),
                "YUKINAL_SSH_API_TOKEN": "test-token",
                "YK_SSH_KEY_0": "private-key-material",
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


if __name__ == "__main__":
    unittest.main()
