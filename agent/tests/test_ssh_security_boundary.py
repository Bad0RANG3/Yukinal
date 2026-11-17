from __future__ import annotations

import asyncio
import copy
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

from yukinal_agent.ssh_service_process import SshServiceProcess, SshServiceProcessError
from yukinal_agent.tools import ToolCallError
from yukinal_agent.ssh_tools import (
    desktop_connection_binding, _prepare_desktop_target, _desktop_client,
    _with_credential_cleanup, _request_desktop_target,
)


def desktop_payload() -> dict:
    return {
        "target": {
            "target_id": "srv_example1", "host": "stage.example.test", "port": 22,
            "username": "ops", "host_key_fingerprint": "SHA256:" + "A" * 43,
            "auth": {"type": "password", "password_env": "YK_SSH_PASSWORD"},
            "sftp_allowed_roots": ["/srv/app"], "truecolor": True,
        },
        "credentials": {"YK_SSH_PASSWORD": "test-secret-no-logging"},
    }


class SshSecurityBoundaryTests(unittest.IsolatedAsyncioTestCase):
    async def test_option_host_rejected_and_failure_payload_credentials_cleared(self) -> None:
        for host in ("-oProxyCommand=id", "-oProxyJump=evil", "-w", "-alias"):
            payload = desktop_payload()
            payload["target"]["host"] = host
            with self.subTest(host=host), self.assertRaises(SshServiceProcessError):
                SshServiceProcess.for_desktop_target(Path(sys.executable), payload)
            self.assertEqual(payload["credentials"], {})

    async def test_credentials_are_consumed_into_zeroable_buffers_and_cleared_on_stop(self) -> None:
        payload = desktop_payload()
        process, _, _ = SshServiceProcess.for_desktop_target(Path(sys.executable), payload)
        secret = process._secret_values["YK_SSH_PASSWORD"]
        self.assertIsInstance(secret, bytearray)
        self.assertEqual(payload["credentials"], {})
        self.assertNotIn("test-secret-no-logging", process._config.read_text(encoding="utf-8"))
        await process.stop()
        self.assertEqual(secret, bytearray(len(secret)))
        self.assertIsNone(process._secret_values)

    async def test_launch_failure_zeroes_buffers_and_never_inherits_native_stderr(self) -> None:
        payload = desktop_payload()
        process, _, _ = SshServiceProcess.for_desktop_target(Path(sys.executable), payload)
        secret = process._secret_values["YK_SSH_PASSWORD"]
        with (
            patch("yukinal_agent.ssh_service_process._health_check", AsyncMock(return_value=False)),
            patch("yukinal_agent.ssh_service_process.asyncio.create_subprocess_exec", AsyncMock(side_effect=OSError("test-secret-no-logging"))) as spawn,
        ):
            with self.assertRaises(SshServiceProcessError) as failure:
                await process.start()
            self.assertNotIn("test-secret-no-logging", str(failure.exception))
            self.assertEqual(spawn.await_args.kwargs["stderr"], asyncio.subprocess.DEVNULL)
            self.assertNotIn("YK_SSH_PASSWORD", spawn.await_args.kwargs["env"])
            self.assertNotIn("YUKINAL_SSH_API_TOKEN", spawn.await_args.kwargs["env"])
        self.assertEqual(secret, bytearray(len(secret)))
        await process.stop()

    async def test_allowed_roots_and_security_flags_are_validated_and_forwarded(self) -> None:
        payload = desktop_payload()
        payload["target"].update(confirmation_read_only=True, execution_expires_at_ms=12345)
        process, _, _ = SshServiceProcess.for_desktop_target(Path(sys.executable), payload)
        self.assertEqual(json.loads(process._config.read_text(encoding="utf-8"))["targets"][0], payload["target"])
        await process.stop()
        for roots in (["."], ["/srv/app/"], ["/srv//app"], ["/srv/./app"]):
            payload = desktop_payload()
            payload["target"]["sftp_allowed_roots"] = roots
            process, _, _ = SshServiceProcess.for_desktop_target(Path(sys.executable), payload)
            self.assertEqual(json.loads(process._config.read_text(encoding="utf-8"))["targets"][0]["sftp_allowed_roots"], roots)
            await process.stop()
        for invalid in ({"sftp_allowed_roots": []}, {"sftp_allowed_roots": ["/srv/../etc"]}, {"sftp_allowed_roots": ["relative"]}, {"truecolor": "true"}, {"confirmation_read_only": 1}, {"execution_expires_at_ms": False}):
            payload = desktop_payload()
            payload["target"].update(invalid)
            with self.subTest(invalid=invalid), self.assertRaises(SshServiceProcessError):
                SshServiceProcess.for_desktop_target(Path(sys.executable), payload)

    async def test_stop_revokes_execution_before_terminating_native_child(self) -> None:
        process = SshServiceProcess(Path(sys.executable), Path("unused"))
        events = []
        process._process = SimpleNamespace(returncode=None, terminate=lambda: events.append("terminate"), wait=AsyncMock(return_value=0))
        process.bind_client(SimpleNamespace(cancel_pending=lambda: events.append("cancel")))
        await process.stop()
        self.assertEqual(events, ["cancel", "terminate"])

    async def test_checkpoint_binding_covers_credentials_host_key_roots_and_proxy(self) -> None:
        original = desktop_payload()
        delivered = []

        async def resolve(*args, **kwargs):
            result = copy.deepcopy(original)
            delivered.append(result)
            return result

        host = SimpleNamespace(request=resolve)
        key = b"k" * 32
        baseline = await desktop_connection_binding(host, "srv_example1", key)
        self.assertEqual(baseline, await desktop_connection_binding(host, "srv_example1", key))
        self.assertEqual(len(baseline), 64)
        for target_delta, credentials in (
            ({"host": "other.example.test"}, None),
            ({"host_key_fingerprint": "SHA256:" + "B" * 43}, None),
            ({"sftp_allowed_roots": ["/"]}, None),
            ({"proxy": {"host": "proxy", "port": 1080}}, None),
            ({}, {"YK_SSH_PASSWORD": "rotated-secret"}),
        ):
            original = desktop_payload()
            original["target"].update(target_delta)
            if credentials is not None:
                original["credentials"] = credentials
            self.assertNotEqual(baseline, await desktop_connection_binding(host, "srv_example1", key))
        self.assertTrue(all(item["credentials"] == {} for item in delivered))

    async def test_checkpoint_binding_detects_same_path_certificate_and_trust_file_rotation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            identity_file = Path(directory) / "identity-material"
            key = b"k" * 32
            for owner, field in (("auth", "public_key_file"), ("auth", "private_key_file"), ("target", "known_hosts_file")):
                original = desktop_payload()
                source = original["target"]["auth"] if owner == "auth" else original["target"]
                source[field] = str(identity_file)
                delivered = []
                async def resolve(*args, **kwargs):
                    payload = copy.deepcopy(original)
                    delivered.append(payload)
                    return payload
                host = SimpleNamespace(request=resolve)
                identity_file.write_bytes(b"first identity material")
                baseline = await desktop_connection_binding(host, "srv_example1", key)
                self.assertEqual(baseline, await desktop_connection_binding(host, "srv_example1", key))
                identity_file.write_bytes(b"other identity material")
                rotated = await desktop_connection_binding(host, "srv_example1", key)
                with self.subTest(field=field):
                    self.assertNotEqual(baseline, rotated)
                    self.assertRegex(rotated, r"^[0-9a-f]{64}$")
                    self.assertTrue(all(payload["credentials"] == {} for payload in delivered))

    async def test_checkpoint_identity_file_missing_oversized_or_nonregular_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            identity_file = Path(directory) / "private-name-must-not-leak.pub"
            oversized = Path(directory) / "oversized.pub"
            oversized.write_bytes(b"x" * (1024 * 1024 + 1))
            for path in (str(identity_file), str(oversized), directory, "relative-cert.pub"):
                payload = desktop_payload()
                payload["target"]["auth"]["public_key_file"] = path
                host = SimpleNamespace(request=AsyncMock(return_value=payload))
                with self.subTest(path=path), self.assertRaises(ToolCallError) as rejected:
                    await desktop_connection_binding(host, "srv_example1", b"k" * 32)
                self.assertEqual(rejected.exception.code, "ssh_target_unavailable")
                self.assertNotIn(path, str(rejected.exception))
                self.assertEqual(payload["credentials"], {})

    async def test_identity_file_constructor_failure_does_not_leak_posix_directory_descriptor(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            payload = desktop_payload()
            payload["target"]["auth"]["public_key_file"] = directory
            host = SimpleNamespace(request=AsyncMock(return_value=payload))
            allocated = []
            real_open = os.open
            def tracked_open(*args, **kwargs):
                descriptor = real_open(*args, **kwargs)
                allocated.append(descriptor)
                return descriptor
            with patch("yukinal_agent.ssh_tools.os.open", side_effect=tracked_open):
                with self.assertRaises(ToolCallError):
                    await desktop_connection_binding(host, "srv_example1", b"k" * 32)
            # Windows rejects directory open before allocating; POSIX can open
            # the directory but FileIO then rejects it. Both must leave no FD.
            for descriptor in allocated:
                with self.assertRaises(OSError):
                    os.fstat(descriptor)
            self.assertEqual(payload["credentials"], {})

    async def test_confirmation_and_approval_deadline_reach_transport(self) -> None:
        payload = desktop_payload()
        context = SimpleNamespace(confirmation_read_only=True, approval_expires_at_ms=12345)
        _prepare_desktop_target(payload, context)
        self.assertIs(payload["target"]["confirmation_read_only"], True)
        self.assertEqual(payload["target"]["execution_expires_at_ms"], 12345)
        payload["target"]["execution_expires_at_ms"] = 12300
        _prepare_desktop_target(payload, SimpleNamespace(confirmation_read_only=False, approval_expires_at_ms=20000))
        self.assertEqual(payload["target"]["execution_expires_at_ms"], 12300)
        self.assertIs(payload["target"]["confirmation_read_only"], True)
        process = Mock()
        client = _desktop_client(process, "x" * 40, "http://127.0.0.1:8765", context)
        self.assertEqual(client._execution_deadline_ms, 12345)
        process.bind_client.assert_called_once_with(client)

    async def test_invalid_binding_exception_clears_credentials_before_leaving_tool(self) -> None:
        payload = desktop_payload()
        host = SimpleNamespace(request=AsyncMock(return_value={"targetData": payload}))

        @_with_credential_cleanup
        async def invalid_binding():
            await _request_desktop_target(host, "host.ssh.effectful_action_target", {}, timeout_seconds=1)
            raise ValueError("binding rejected before process creation")

        with self.assertRaises(ValueError):
            await invalid_binding()
        self.assertEqual(payload["credentials"], {})


if __name__ == "__main__":
    unittest.main()
