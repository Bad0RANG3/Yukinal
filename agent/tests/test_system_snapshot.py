from __future__ import annotations

import json
import unittest
from pathlib import Path

from yukinal_agent.system_snapshot import (
    SYSTEM_SNAPSHOT_COMMAND_IDS,
    normalize_system_snapshot,
)


def _outputs() -> dict[str, tuple[int, str]]:
    result = {command_id: (0, "") for command_id in SYSTEM_SNAPSHOT_COMMAND_IDS}
    result.update(
        {
            "os_detect": (0, "Linux\n"),
            "os_release": (0, 'NAME="Test Linux"\nVERSION_ID="1"\n'),
            "os_kernel": (0, "6.1.0\n"),
            "os_arch": (0, "x86_64\n"),
            "os_hostname": (0, "mock-host\n"),
            "cpu_detect": (0, "ok\n"),
            "cpu_ticks_a": (0, "cpu 100 0 100 800\n"),
            "cpu_ticks_b": (0, "cpu 120 0 110 830\n"),
            "cpu_cores": (0, "4\n"),
            "cpu_model": (0, " Mock CPU\n"),
            "cpu_load": (0, "0.10 0.20 0.30 1/20 99\n"),
            "memory_detect": (0, "ok\n"),
            "memory_info": (0, "MemTotal: 1024 kB\nMemAvailable: 512 kB\n"),
            "uptime_detect": (0, "ok\n"),
            "uptime_info": (0, "120.0 100.0\n"),
            "disk_detect": (0, "ok\n"),
            "disk_usage": (
                0,
                "Filesystem 1B-blocks Used Available Use% Mounted on\n"
                "/dev/vda1 1000 250 750 25% /\n",
            ),
            "network_detect": (0, "ok\n"),
            "network_info": (
                0,
                "Inter-| Receive | Transmit\n"
                " face |bytes packets errs drop fifo frame compressed multicast|"
                "bytes packets errs drop fifo colls carrier compressed\n"
                " eth0: 100 1 0 0 0 0 0 0 200 2 0 0 0 0 0 0\n",
            ),
            "docker_detect": (0, "0\n"),
            "docker_info": (0, ""),
            "docker_ps": (
                0,
                '{"Names":"api","Image":"example/api","State":"running",'
                '"Status":"Up","RestartCount":0}\n',
            ),
        }
    )
    return result


class SystemSnapshotTests(unittest.TestCase):
    def test_normalizes_snapshot_using_existing_ui_contract(self) -> None:
        snapshot = normalize_system_snapshot(_outputs())
        self.assertEqual(snapshot["health"], "healthy")
        self.assertEqual(snapshot["os"], {
            "distribution": "Test Linux", "version": "1", "hostname": "mock-host",
            "kernel": "6.1.0", "arch": "x86_64",
        })
        self.assertEqual(snapshot["cpu"], {
            "model": "Mock CPU", "cores": 4, "usagePercent": 50.0,
            "loadAverage": [0.1, 0.2, 0.3],
        })
        self.assertEqual(snapshot["memory"], {
            "totalBytes": 1_048_576, "usedBytes": 524_288,
            "availableBytes": 524_288, "usagePercent": 50.0,
        })
        self.assertEqual(snapshot["disks"][0], {
            "device": "/dev/vda1", "mountPoint": "/", "totalBytes": 1000,
            "usedBytes": 250, "usagePercent": 25.0,
        })
        self.assertEqual(snapshot["uptimeSeconds"], 120)
        self.assertEqual(snapshot["network"], [{"name": "eth0", "rxBytes": 100, "txBytes": 200}])
        self.assertEqual(snapshot["docker"]["containers"][0]["restartCount"], 0)
        self.assertEqual(snapshot["capabilities"], {"linux": True, "docker": True})
        self.assertEqual([sample["collectorId"] for sample in snapshot["collectors"]],
                         ["os", "cpu", "memory", "uptime", "disk", "network", "docker"])
        self.assertTrue(all(sample["ok"] for sample in snapshot["collectors"]))
        self.assertNotIn("id", snapshot)
        self.assertNotIn("serverId", snapshot)

    def test_collector_parse_failures_are_reported_without_losing_other_sections(self) -> None:
        outputs = _outputs()
        outputs["os_release"] = (0, "ID=broken\n")
        outputs["memory_info"] = (0, "MemFree: 10 kB\n")
        outputs["uptime_info"] = (0, "not-uptime\n")
        snapshot = normalize_system_snapshot(outputs)
        self.assertNotIn("os", snapshot)
        self.assertNotIn("memory", snapshot)
        self.assertNotIn("uptimeSeconds", snapshot)
        self.assertEqual(snapshot["collectors"][0]["error"],
                         "collector os failed: failed to parse /etc/os-release")
        self.assertEqual(
            snapshot["collectors"][2]["error"], "collector memory failed: MemTotal missing"
        )
        self.assertEqual(snapshot["collectors"][3]["error"],
                         "collector uptime failed: unexpected /proc/uptime output")
        self.assertEqual(snapshot["health"], "healthy")

    def test_health_uses_shared_threshold_fixture_and_worst_usage(self) -> None:
        root = Path(__file__).resolve().parents[2]
        thresholds = json.loads(
            (root / "packages/shared/fixtures/health_thresholds.json").read_text()
        )
        from yukinal_agent import system_snapshot

        self.assertEqual(system_snapshot._HEALTH_THRESHOLDS, thresholds)
        outputs = _outputs()
        outputs["cpu_ticks_b"] = (0, "cpu 190 0 200 810\n")
        self.assertEqual(normalize_system_snapshot(outputs)["health"], "critical")

    def test_docker_unavailable_is_a_valid_sample(self) -> None:
        outputs = _outputs()
        outputs["docker_info"] = (1, "")
        snapshot = normalize_system_snapshot(outputs)
        self.assertEqual(snapshot["docker"], {"available": False, "containers": []})
        self.assertTrue(snapshot["collectors"][-1]["ok"])

    def test_rejects_incomplete_invalid_and_oversized_command_results(self) -> None:
        outputs = _outputs()
        del outputs["os_detect"]
        with self.assertRaises(ValueError):
            normalize_system_snapshot(outputs)
        outputs = _outputs()
        outputs["os_detect"] = (256, "Linux")
        with self.assertRaises(ValueError):
            normalize_system_snapshot(outputs)
        outputs = _outputs()
        outputs["os_detect"] = (0, "x" * (64 * 1024 + 1))
        with self.assertRaises(ValueError):
            normalize_system_snapshot(outputs)


if __name__ == "__main__":
    unittest.main()
