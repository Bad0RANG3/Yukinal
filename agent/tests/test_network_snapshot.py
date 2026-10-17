from __future__ import annotations
import json
import unittest

from yukinal_agent.network_snapshot import NETWORK_SNAPSHOT_COMMAND_IDS, normalize_network_details
from yukinal_ssh.client import _NETWORK_SNAPSHOT_COMMAND_IDS
from yukinal_agent.system_snapshot import normalize_system_snapshot
from test_system_snapshot import _outputs


class NetworkSnapshotTests(unittest.TestCase):
    def outputs(self):
        return {
            "network_addresses": (0, json.dumps([{"ifname": "eth0", "operstate": "UP", "addr_info": [
                {"family": "inet", "local": "10.24.1.10", "prefixlen": 24},
                {"family": "inet6", "local": "2001:db8::10", "prefixlen": 64}]}])),
            "network_routes4": (0, '[{"dst":"default","gateway":"10.24.1.1","dev":"eth0","metric":100}]'),
            "network_routes6": (0, '[{"dst":"2001:db8::/64","dev":"eth0"}]'),
            "network_dns": (0, '# generated\nnameserver 127.0.0.53\nnameserver 2001:db8::53 # example\nsearch internal\n'),
        }

    def test_full_snapshot_preserves_legacy_fields_and_network_configuration(self):
        snapshot = normalize_system_snapshot({**_outputs(), **self.outputs()})
        details = snapshot["networkDetails"]
        self.assertEqual(snapshot["network"][0]["rxBytes"], 100)
        self.assertEqual(details["interfaces"][0]["addresses"], ["10.24.1.10/24", "2001:db8::10/64"])
        self.assertEqual(details["ipv4Routes"][0], {"destination": "default", "gateway": "10.24.1.1", "interface": "eth0", "metric": 100})
        self.assertEqual(details["ipv6Routes"][0]["destination"], "2001:db8::/64")
        self.assertEqual(details["dnsServers"], ["127.0.0.53", "2001:db8::53"])
        self.assertNotIn("errors", details)
        self.assertNotIn("networkDetails", normalize_system_snapshot(_outputs()))

    def test_probe_failures_and_empty_configuration_are_distinct(self):
        outputs = self.outputs()
        outputs["network_routes4"] = (127, "ip: command not found")
        outputs["network_routes6"] = (0, '[]')
        outputs["network_dns"] = (0, '# no resolver configured')
        details = normalize_system_snapshot({**_outputs(), **outputs})["networkDetails"]
        self.assertNotIn("ipv4Routes", details)
        self.assertIn("ipv4Routes", details["errors"])
        self.assertEqual(details["ipv6Routes"], [])
        self.assertEqual(details["dnsServers"], [])
        self.assertEqual(len(details["interfaces"]), 1)

    def test_malformed_or_oversized_sections_fail_locally(self):
        cases = [
            ("network_addresses", '[{"ifname":"eth0","addr_info":[{"family":"inet","local":"10.0.0.1","prefixlen":33}]}]', "interfaces"),
            ("network_addresses", '[{"ifname":"eth0","addr_info":[null]}]', "interfaces"),
            ("network_addresses", '[{"ifname":"eth0","addr_info":[{"family":"inet","local":true,"prefixlen":24}]}]', "interfaces"),
            ("network_addresses", json.dumps([{}] * 257), "interfaces"),
            ("network_routes4", '[{"dst":"default","gateway":"::1"}]', "ipv4Routes"),
            ("network_routes4", '[{"dst":"default","metric":true}]', "ipv4Routes"),
            ("network_routes6", '{"error":"not a list"}', "ipv6Routes"),
            ("network_dns", 'nameserver not-an-ip', "dnsServers"),
        ]
        for command, raw, field in cases:
            with self.subTest(command=command, raw=raw[:60]):
                details = normalize_network_details({command: (0, raw)})
                self.assertNotIn(field, details)
                self.assertIn(field, details["errors"])

    def test_transport_and_normalizer_recognize_the_same_optional_probes(self):
        self.assertEqual(NETWORK_SNAPSHOT_COMMAND_IDS, _NETWORK_SNAPSHOT_COMMAND_IDS)

    def test_optional_commands_obey_the_shared_output_limit(self):
        outputs = {**_outputs(), "network_dns": (0, 'x' * (64 * 1024))}
        with self.assertRaises(ValueError):
            normalize_system_snapshot(outputs)
        with self.assertRaises(ValueError):
            normalize_system_snapshot({**_outputs(), "unlisted_probe": (0, '')})


if __name__ == "__main__":
    unittest.main()
