from __future__ import annotations

import unittest
from pathlib import Path

from yukinal_agent.audit import MemoryAuditSink
from yukinal_agent.host_tools import register_desktop_backup_metadata_tools, register_host_tools
from yukinal_agent.sidecar import PythonSidecar
from yukinal_agent.ssh_tools import register_desktop_ssh_tools
from yukinal_agent.tools import ToolRegistry


_DIRECT_ACTIONS = (
    "server.exec",
    "systemd.restart",
    "docker.restart",
    "package.install",
    "filesystem.write",
    "filesystem.edit",
    "filesystem.backup",
    "filesystem.restore",
    "filesystem.backup.cleanup",
)


class _NoRpcHost:
    async def request(self, *_args: object, **_kwargs: object) -> object:
        raise AssertionError("tool registration must not issue host RPCs")


class SidecarRegistrationTests(unittest.TestCase):
    def test_direct_and_host_tools_keep_direct_handlers_and_approval(self) -> None:
        registry = ToolRegistry(MemoryAuditSink())
        host = _NoRpcHost()
        direct_names = set(register_desktop_backup_metadata_tools(registry, host))  # type: ignore[arg-type]
        direct_names.update(register_desktop_ssh_tools(
            registry, host, Path("offline-registration-only.exe")  # type: ignore[arg-type]
        ))
        direct_handlers = {spec.name: spec.handler for spec in registry.list()}

        register_host_tools(registry, host, excluded_names=frozenset(direct_names))  # type: ignore[arg-type]

        names = [spec.name for spec in registry.list()]
        self.assertEqual(len(names), len(set(names)))
        self.assertIsNotNone(registry.get("investigation.plan"))
        for name in _DIRECT_ACTIONS:
            with self.subTest(tool=name):
                spec = registry.get(name)
                self.assertIsNotNone(spec)
                self.assertIs(spec.handler, direct_handlers[name])
                self.assertTrue(spec.requires_approval)

    def test_managed_sidecar_registration_is_unique_and_repeatable(self) -> None:
        registry = ToolRegistry(MemoryAuditSink())
        sidecar = PythonSidecar(
            registry, managed_ssh_service_binary=Path("offline-registration-only.exe")
        )
        host = _NoRpcHost()

        sidecar.set_host_rpc(host)  # type: ignore[arg-type]
        original_handlers = {spec.name: spec.handler for spec in registry.list()}
        names = [spec.name for spec in registry.list()]
        self.assertEqual(len(names), len(set(names)))
        self.assertTrue(set(_DIRECT_ACTIONS).issubset(original_handlers))

        sidecar.set_host_rpc(host)  # type: ignore[arg-type]

        self.assertEqual(names, [spec.name for spec in registry.list()])
        for spec in registry.list():
            with self.subTest(tool=spec.name):
                self.assertIs(spec.handler, original_handlers[spec.name])
        for name in _DIRECT_ACTIONS:
            with self.subTest(approved_tool=name):
                self.assertTrue(registry.get(name).requires_approval)


if __name__ == "__main__":
    unittest.main()
