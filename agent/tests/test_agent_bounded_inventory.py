from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from yukinal_agent.ssh_tools import _parse_server_logs, _parse_service_inventory


class InventoryParserBoundsTests(unittest.TestCase):
    def test_service_row_limit_is_reported_even_after_filtering(self) -> None:
        raw = "".join(f"app-{i}.service loaded active running service {i}\n" for i in range(201))
        result = _parse_service_inventory(raw + "__YUKINAL_SOURCE__=systemd\n", {})
        self.assertEqual(len(result["services"]), 200)
        self.assertIn("部分", result["message"])
        filtered = _parse_service_inventory(
            raw + "__YUKINAL_SOURCE__=systemd\n", {"name": "app-200.service"}
        )
        self.assertEqual(filtered["services"], [])
        self.assertIn("部分", filtered["message"])

    def test_log_limit_and_internal_metadata_are_not_displayed(self) -> None:
        raw = "".join(f"message {i}\n" for i in range(101))
        result = _parse_server_logs(
            raw + "__YUKINAL_SOURCE__=journalctl\n__YUKINAL_TRUNCATED__=1\n", {}
        )
        self.assertEqual(len(result["lines"]), 100)
        self.assertEqual(result["lines"][-1]["text"], "message 99")
        self.assertIn("部分", result["message"])

    def test_oversized_utf8_line_is_omitted_whole(self) -> None:
        text = "正常中文日志"
        result = _parse_server_logs(
            "超" * 30_000 + "\n" + text + "\n__YUKINAL_SOURCE__=journalctl\n", {}
        )
        self.assertEqual(result["lines"], [{"text": text, "level": "info"}])
        self.assertIn("部分", result["message"])

    def test_native_partial_marker_survives_normalization_for_both_sources(self) -> None:
        services = _parse_service_inventory(
            "web\trunning\tUp\t应用\n__YUKINAL_SOURCE__=docker\n__YUKINAL_TRUNCATED__=1\n", {}
        )
        logs = _parse_server_logs(
            "正常\n__YUKINAL_SOURCE__=syslog\n__YUKINAL_TRUNCATED__=1\n", {}
        )
        self.assertEqual(len(services["services"]), 1)
        self.assertEqual(len(logs["lines"]), 1)
        self.assertIn("部分", services["message"])
        self.assertIn("部分", logs["message"])


class NativeInventoryCommandTests(unittest.TestCase):
    """Run the actual native command builders with local fake providers, never SSH."""

    @classmethod
    def setUpClass(cls) -> None:
        compiler = os.environ.get("CXX") or shutil.which("c++") or shutil.which("g++")
        shell = os.environ.get("YUKINAL_TEST_POSIX_SHELL") or shutil.which("sh")
        if not shell and os.name == "nt":
            candidate = Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "Git/bin/bash.exe"
            if candidate.is_file():
                shell = str(candidate)
        if not compiler or not shell:
            raise unittest.SkipTest("native command regression requires a C++20 compiler and POSIX shell with awk")
        probe = subprocess.run([shell, "-c", "command -v awk"], capture_output=True, timeout=10)
        if probe.returncode:
            raise unittest.SkipTest("POSIX awk is unavailable")
        cls.shell = shell
        cls.temporary = tempfile.TemporaryDirectory(prefix="yukinal-inventory-test-")
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.directory = Path(cls.temporary.name)
        native = Path(__file__).resolve().parents[2] / "native/src/ssh.cpp"
        source = native.read_text(encoding="utf-8")
        # Compile the production builders unchanged with an SSH transport stub;
        # tests execute their resulting shell locally against fixture files.
        start = source.index("static std::string bounded_inventory_source_command(")
        end = source.index("ExecResult host_key_probe(", start)
        fixture = cls.directory / "inventory_commands.cpp"
        fixture.write_text(
            "#include <cstdint>\n#include <iostream>\n#include <optional>\n"
            "#include <string>\n#include <utility>\n"
            "struct TargetConfig {};\nstruct ExecResult { std::string command; };\n"
            "void fail(ExecResult*, const char*, const char*) {}\n"
            "bool valid_service_name(const std::string&) { return true; }\n"
            "ExecResult run_ssh_command(const TargetConfig&, const std::string& c, int) { return {c}; }\n"
            + source[start:end]
            + '\nint main(int argc, char** argv) {\n'
            '  const bool services = std::string(argv[1]) == "services";\n'
            '  const bool filtered = argc > 2;\n'
            '  const auto result = services ? service_inventory(TargetConfig{}, 20) :\n'
            '    server_logs(TargetConfig{}, filtered ? std::optional<std::uint32_t>{60} : std::nullopt,\n'
            '      filtered ? "app.service" : "", 20);\n'
            '  std::cout << result.command;\n}\n',
            encoding="utf-8",
        )
        cls.executable = cls.directory / ("inventory_commands.exe" if os.name == "nt" else "inventory_commands")
        flags = ["-static-libgcc", "-static-libstdc++"] if os.name == "nt" else []
        built = subprocess.run(
            [compiler, "-std=c++20", *flags, str(fixture), "-o", str(cls.executable)],
            capture_output=True, timeout=60,
        )
        if built.returncode:
            raise AssertionError(built.stderr.decode("utf-8", errors="replace"))

    def run_command(
        self, kind: str, *, systemd: str = "", systemd_status: int = 1,
        docker: str = "", docker_status: int = 1, journal: str = "",
        journal_status: int = 1, syslog: str | None = None, filtered: bool = False,
    ) -> str:
        command = subprocess.check_output(
            [str(self.executable), kind, *(["filtered"] if filtered else [])], timeout=10
        ).decode("utf-8")
        environment = os.environ.copy()
        for name, text in (("SYSTEMD", systemd), ("DOCKER", docker), ("JOURNAL", journal)):
            path = self.directory / (name.lower() + ".txt")
            path.write_bytes(text.encode("utf-8"))
            environment[name + "_FILE"] = path.as_posix()
        environment.update({
            "SYSTEMD_STATUS": str(systemd_status), "DOCKER_STATUS": str(docker_status),
            "JOURNAL_STATUS": str(journal_status),
        })
        syslog_path = self.directory / "syslog.txt"
        if syslog is not None:
            syslog_path.write_bytes(syslog.encode("utf-8"))
        elif syslog_path.exists():
            syslog_path.unlink()
        # Point only the two fixed fallback file paths at isolated local fixtures.
        command = command.replace("/var/log/syslog", '"' + syslog_path.as_posix() + '"')
        command = command.replace("/var/log/messages", '"' + (self.directory / "absent.txt").as_posix() + '"')
        providers = (
            'systemctl() { cat "$SYSTEMD_FILE"; return "$SYSTEMD_STATUS"; }; '
            'docker() { cat "$DOCKER_FILE"; return "$DOCKER_STATUS"; }; '
            'journalctl() { cat "$JOURNAL_FILE"; return "$JOURNAL_STATUS"; }; '
        )
        result = subprocess.run(
            [self.shell, "-c", providers + command], env=environment,
            capture_output=True, check=True, timeout=10,
        )
        self.assertEqual(result.stderr, b"")
        self.assertLess(len(result.stdout), 64 * 1024)
        return result.stdout.decode("utf-8", errors="strict")

    def test_service_count_limit_and_complete_utf8_lines(self) -> None:
        raw = self.run_command(
            "services", systemd="".join(
                f"app-{i}.service loaded active running 中文应用\n" for i in range(201)
            ), systemd_status=0,
        )
        self.assertEqual(len(raw.splitlines()), 202)
        self.assertIn("__YUKINAL_TRUNCATED__=1\n", raw)
        result = _parse_service_inventory(raw, {})
        self.assertEqual(len(result["services"]), 200)
        self.assertEqual(result["services"][-1]["description"], "中文应用")
        self.assertIn("部分", result["message"])

    def test_byte_budget_omits_whole_oversized_service_lines(self) -> None:
        oversized = "large.service loaded active running " + "超" * 30_000 + "\n"
        normal = "".join(
            f"app-{i}.service loaded active running " + "中" * 2_000 + "\n" for i in range(12)
        )
        raw = self.run_command("services", systemd=oversized + normal, systemd_status=0)
        self.assertNotIn("large.service", raw)
        self.assertNotIn("app-10.service", raw)
        self.assertLess(len(raw.encode("utf-8")), 60 * 1024 + 128)
        result = _parse_service_inventory(raw, {})
        self.assertEqual(len(result["services"]), 10)
        self.assertEqual(result["services"][0]["description"], "中" * 2_000)
        self.assertIn("部分", result["message"])

    def test_failed_primary_output_is_discarded_before_fallback(self) -> None:
        raw = self.run_command(
            "services", systemd="failed.service loaded active running wrong\n__YUKINAL_STATUS__=0\n",
            systemd_status=3, docker="web\trunning\tUp\t应用\n", docker_status=0,
        )
        self.assertNotIn("failed.service", raw)
        result = _parse_service_inventory(raw, {})
        self.assertEqual(result["source"], "docker")
        self.assertEqual([row["name"] for row in result["services"]], ["web"])
        self.assertNotIn("message", result)

    def test_both_service_failures_return_only_unavailable(self) -> None:
        raw = self.run_command("services", systemd="partial primary\n", docker="partial fallback\n")
        self.assertEqual(raw, "__YUKINAL_SOURCE__=unavailable\n")

    def test_log_limits_omit_oversized_utf8_and_reserved_records(self) -> None:
        raw = self.run_command(
            "logs", journal="超" * 30_000 + "\n__YUKINAL_SOURCE__=syslog\n"
            + "".join(f"正常中文日志 {i}\n" for i in range(101)), journal_status=0,
        )
        self.assertEqual(len(raw.splitlines()), 102)
        self.assertNotIn("__YUKINAL_SOURCE__=syslog", raw)
        result = _parse_server_logs(raw, {})
        self.assertEqual(result["source"], "journalctl")
        self.assertEqual(len(result["lines"]), 100)
        self.assertEqual(result["lines"][0]["text"], "正常中文日志 0")
        self.assertIn("部分", result["message"])

    def test_journal_failure_falls_back_without_leaking_partial_output(self) -> None:
        raw = self.run_command("logs", journal="failed partial journal\n", syslog="正常文件日志\n")
        self.assertNotIn("failed partial", raw)
        result = _parse_server_logs(raw, {})
        self.assertEqual(result["source"], "syslog")
        self.assertEqual(result["lines"], [{"text": "正常文件日志", "level": "info"}])
        self.assertNotIn("message", result)
        filtered = self.run_command("logs", journal="failed filtered\n", syslog="unfiltered\n", filtered=True)
        self.assertEqual(filtered, "__YUKINAL_SOURCE__=unavailable\n")


if __name__ == "__main__":
    unittest.main()
