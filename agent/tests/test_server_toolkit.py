import json
from pathlib import Path
import unittest

from test_system_snapshot import _outputs
from yukinal_agent.system_snapshot import normalize_system_snapshot
from yukinal_ssh.client import _parse_system_snapshot_output
from yukinal_ssh.errors import SshServiceTransportError
from yukinal_ssh.toolkit_catalog import CATALOG, SYSTEM_SNAPSHOT_COMMAND_IDS, OPTIONAL_SNAPSHOT_COMMAND_IDS


class ServerToolkitTests(unittest.TestCase):
    def test_catalog_has_unique_bounded_fixed_probes(self):
        probes = CATALOG["probes"]
        self.assertEqual(len({p["id"] for p in probes}), len(probes))
        self.assertEqual(len(SYSTEM_SNAPSHOT_COMMAND_IDS), 22)
        self.assertEqual(sum("docker info" in p["command"] for p in probes), 1)
        self.assertTrue(all(p["timeoutSeconds"] in (0, 2) for p in probes))
        for p in probes:
            if p["module"] in {"gpu", "security", "docker"} and p["id"] not in {"security_identity", "docker_detect"}:
                self.assertEqual(p["timeoutSeconds"], 2)

    def test_legacy_snapshots_remain_valid_without_new_modules(self):
        snapshot = normalize_system_snapshot(_outputs())
        self.assertNotIn("toolkit", snapshot)
        self.assertNotIn("gpu", snapshot)

    def test_gpu_metrics_and_unsupported_fields(self):
        outputs = _outputs() | {"gpu_nvidia": (0, '0, "NVIDIA A, B", 87, 2048, 8192, 64\n1, T4, N/A, 0, 16384, [Not Supported]\n')}
        snapshot = normalize_system_snapshot(outputs)
        self.assertEqual(snapshot["gpu"]["status"], "available")
        self.assertEqual(snapshot["gpu"]["devices"][0]["name"], "NVIDIA A, B")
        self.assertEqual(snapshot["gpu"]["devices"][0]["memoryUsedBytes"], 2 * 1024**3)
        self.assertNotIn("usagePercent", snapshot["gpu"]["devices"][1])
        self.assertEqual(snapshot["toolkit"], {"version": "1", "modules": ["gpu"]})

    def test_missing_gpu_is_not_zero_usage_or_no_gpu_claim(self):
        snapshot = normalize_system_snapshot(_outputs() | {"gpu_nvidia": (127, "")})
        self.assertEqual(snapshot["gpu"]["status"], "unavailable")
        self.assertEqual(snapshot["gpu"]["devices"], [])
        self.assertTrue(snapshot["collectors"][-1]["ok"])

    def test_bad_gpu_is_isolated_and_security_still_parses(self):
        for raw in ['0, GPU, nan, 1, 2, 30', '0, GPU, 101, 1, 2, 30', '0, GPU, 1, 3, 2, 30', 'bad']:
            with self.subTest(raw=raw):
                snapshot = normalize_system_snapshot(_outputs() | {"gpu_nvidia": (0, raw), "security_identity": (0, "1000")})
                self.assertNotIn("gpu", snapshot)
                self.assertFalse(next(c for c in snapshot["collectors"] if c["collectorId"] == "gpu")["ok"])
                self.assertEqual(snapshot["cpu"]["usagePercent"], 50)
                self.assertEqual(snapshot["security"]["checks"][0]["status"], "info")

    def test_timeout_keeps_other_resources_available(self):
        snapshot = normalize_system_snapshot(_outputs() | {"gpu_nvidia": (124, ""), "docker_info": (124, "")})
        self.assertEqual(snapshot["gpu"]["message"], "检测超时")
        self.assertNotIn("docker", snapshot)
        self.assertEqual(snapshot["memory"]["usagePercent"], 50)
        self.assertFalse(snapshot["capabilities"]["docker"])

    def test_security_unknown_and_conditional_config_are_not_secure_claims(self):
        outputs = _outputs() | {
            "security_identity": (0, "0\n"),
            "security_sshd": (0, "permitrootlogin yes\npasswordauthentication yes\npubkeyauthentication yes\n"),
            "security_firewall": (1, "ERROR: You need to be root"),
            "security_listeners": (0, "tcp LISTEN 0 128 0.0.0.0:22 0.0.0.0:*\ntcp LISTEN 0 128 [::]:22 [::]:*\nudp UNCONN 0 0 127.0.0.53:53 0.0.0.0:*\n"),
        }
        checks = normalize_system_snapshot(outputs)["security"]["checks"]
        self.assertEqual([c["status"] for c in checks], ["warning", "warning", "unknown", "info"])
        self.assertIn("默认配置", checks[1]["detail"])
        self.assertEqual(checks[3]["detail"], "1 个通配地址监听 · tcp/22")
        outputs["security_firewall"] = (0, "ufw\nStatus: inactive\n")
        self.assertEqual(normalize_system_snapshot(outputs)["security"]["checks"][2]["status"], "warning")

    def test_cpu_counts_irq_and_iowait_without_double_counting_guest(self):
        outputs = _outputs() | {"cpu_ticks_a": (0, "cpu 100 0 100 800 0 0 0 0 20 0"),
                                "cpu_ticks_b": (0, "cpu 120 0 110 830 10 10 20 0 30 0")}
        # Delta total=100, idle+iowait=40 -> busy=60%.
        self.assertEqual(normalize_system_snapshot(outputs)["cpu"]["usagePercent"], 60)
        outputs["cpu_ticks_b"] = outputs["cpu_ticks_a"]
        snapshot = normalize_system_snapshot(outputs)
        self.assertNotIn("cpu", snapshot)
        self.assertFalse(snapshot["collectors"][1]["ok"])

    def test_optional_framing_is_accepted_but_malformed_present_probe_is_rejected(self):
        nonce = "a" * 32
        def frame(name, status="0", raw=""):
            return f"__YK_SNAPSHOT_{nonce}_{name}_BEGIN__\n{raw}\n__YK_SNAPSHOT_{nonce}_{name}_END_{status}__\n"
        base = "".join(frame(name) for name in SYSTEM_SNAPSHOT_COMMAND_IDS)
        self.assertEqual(len(_parse_system_snapshot_output(base, nonce)), 22)
        self.assertIn("gpu_nvidia", _parse_system_snapshot_output(base + frame("gpu_nvidia", "127"), nonce))
        for suffix in (frame("gpu_nvidia", "0x"), frame("gpu_nvidia") * 2, f"__YK_SNAPSHOT_{nonce}_gpu_nvidia_END_0__\n"):
            with self.assertRaises(SshServiceTransportError):
                _parse_system_snapshot_output(base + suffix, nonce)

    def test_missing_core_count_is_unknown_and_old_memory_fallback_uses_free_bytes(self):
        snapshot = normalize_system_snapshot(_outputs() | {"cpu_cores": (1, ""), "memory_info": (0, "MemTotal: 1024 kB\nMemFree: 256 kB")})
        self.assertNotIn("cpu", snapshot)
        self.assertEqual(snapshot["memory"]["usagePercent"], 75)
        self.assertEqual(snapshot["memory"]["availableBytes"], 256 * 1024)

    def test_shared_extension_fixture_is_a_real_normalizer_result(self):
        outputs = _outputs() | {
            "gpu_nvidia": (0, "0, NVIDIA T4, 73, 2048, 16384, 62"),
            "security_identity": (0, "0"),
            "security_sshd": (0, "permitrootlogin prohibit-password\npasswordauthentication no\npubkeyauthentication yes"),
            "security_firewall": (127, ""), "security_listeners": (0, ""),
        }
        actual = normalize_system_snapshot(outputs)
        fixture = json.loads((Path(__file__).resolve().parents[2] / "packages/shared/fixtures/server_toolkit.json").read_text(encoding="utf-8"))
        for field in ("gpu", "security", "toolkit"):
            self.assertEqual(actual[field], fixture[field])


if __name__ == "__main__":
    unittest.main()
