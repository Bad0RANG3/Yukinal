from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import AsyncMock, patch

from yukinal_agent.ssh_service_process import SshServiceProcess


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
                    self.assertNotIn("UNRELATED_SECRET", child_environment)
                    await service.stop()


if __name__ == "__main__":
    unittest.main()
