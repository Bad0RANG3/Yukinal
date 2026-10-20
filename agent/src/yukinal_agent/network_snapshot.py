"""Bounded, read-only Linux network configuration; no reachability inference."""
from __future__ import annotations

import ipaddress
import json
from collections.abc import Mapping
from typing import Any

from yukinal_ssh.toolkit_catalog import NETWORK_SNAPSHOT_COMMAND_IDS


def _text(value: object) -> str:
    if not isinstance(value, str) or not value or len(value) > 256 or any(ord(c) < 32 for c in value):
        raise ValueError("invalid network field")
    return value


def _rows(raw: str, limit: int) -> list[dict[str, Any]]:
    value = json.loads(raw)
    if not isinstance(value, list) or len(value) > limit or any(not isinstance(row, dict) for row in value):
        raise ValueError("invalid network rows")
    return value


def _interfaces(raw: str) -> list[dict[str, Any]]:
    result = []
    for row in _rows(raw, 256):
        entries = row.get("addr_info", [])
        if not isinstance(entries, list) or len(entries) > 128:
            raise ValueError("invalid interface addresses")
        addresses = []
        for address in entries:
            if not isinstance(address, dict):
                raise ValueError("invalid interface address")
            family = address.get("family")
            if family not in ("inet", "inet6"):
                continue
            ip = ipaddress.ip_address(_text(address.get("local")))
            prefix = address.get("prefixlen")
            version = 4 if family == "inet" else 6
            if (ip.version != version or isinstance(prefix, bool) or not isinstance(prefix, int)
                    or not 0 <= prefix <= ip.max_prefixlen):
                raise ValueError("invalid interface prefix")
            addresses.append(f"{ip}/{prefix}")
        result.append({"name": _text(row.get("ifname")),
                       "state": _text(row.get("operstate", "UNKNOWN")), "addresses": addresses})
    return result


def _routes(raw: str, version: int) -> list[dict[str, Any]]:
    result = []
    for row in _rows(raw, 512):
        destination = row.get("dst")
        if destination == "default":
            normalized = "default"
        else:
            network = ipaddress.ip_network(_text(destination), strict=False)
            if network.version != version:
                raise ValueError("invalid route family")
            normalized = str(network)
        route: dict[str, Any] = {"destination": normalized}
        if "gateway" in row:
            gateway = ipaddress.ip_address(_text(row["gateway"]))
            if gateway.version != version:
                raise ValueError("invalid gateway family")
            route["gateway"] = str(gateway)
        if "dev" in row:
            route["interface"] = _text(row["dev"])
        if "type" in row:
            route["type"] = _text(row["type"])
        if "metric" in row:
            metric = row["metric"]
            if isinstance(metric, bool) or not isinstance(metric, int) or not 0 <= metric <= 4_294_967_295:
                raise ValueError("invalid route metric")
            route["metric"] = metric
        result.append(route)
    return result


def _dns(raw: str) -> list[str]:
    result = []
    for line in raw.splitlines():
        fields = line.split("#", 1)[0].split(";", 1)[0].split()
        if not fields or fields[0] != "nameserver":
            continue
        if len(fields) != 2:
            raise ValueError("invalid nameserver")
        value = str(ipaddress.ip_address(fields[1]))
        if value not in result:
            result.append(value)
        if len(result) > 32:
            raise ValueError("too many nameservers")
    return result


def normalize_network_details(outputs: Mapping[str, tuple[int, str]]) -> dict[str, Any] | None:
    if not any(key in outputs for key in NETWORK_SNAPSHOT_COMMAND_IDS):
        return None
    details: dict[str, Any] = {}
    errors = {}
    sections = (
        ("network_addresses", "interfaces", _interfaces),
        ("network_routes4", "ipv4Routes", lambda raw: _routes(raw, 4)),
        ("network_routes6", "ipv6Routes", lambda raw: _routes(raw, 6)),
        ("network_dns", "dnsServers", _dns),
    )
    for command, field, parse in sections:
        if command not in outputs:
            continue
        status, raw = outputs[command]
        if status != 0:
            errors[field] = ("无法读取 /etc/resolv.conf" if field == "dnsServers"
                             else "读取失败，请检查 iproute2 和读取权限")
            continue
        try:
            details[field] = parse(raw)
        except (ValueError, TypeError, AttributeError):
            errors[field] = "返回的数据无法解析"
    if errors:
        details["errors"] = errors
    return details
