"""Normalize the bounded Linux snapshot returned by the desktop SSH service."""

from __future__ import annotations

import json
import math
from collections.abc import Mapping
from typing import Any

from yukinal_ssh.toolkit_catalog import SYSTEM_SNAPSHOT_COMMAND_IDS, OPTIONAL_SNAPSHOT_COMMAND_IDS

from .network_snapshot import normalize_network_details
from .server_toolkit import extend_snapshot

_MAX_OUTPUT_BYTES = 64 * 1024
_U64_MAX = (1 << 64) - 1
_U32_MAX = (1 << 32) - 1
_HEALTH_THRESHOLDS = {
    "cpu": {"warning": 70.0, "critical": 90.0},
    "memory": {"warning": 70.0, "critical": 90.0},
    "disk": {"warning": 75.0, "critical": 90.0},
}


class _CollectorParseError(ValueError):
    pass


def normalize_system_snapshot(
    outputs: Mapping[str, tuple[int, str]],
) -> dict[str, Any]:
    """Return UI-shaped snapshot data, leaving host identity and timestamps to the desktop host."""
    if (not isinstance(outputs, Mapping)
            or not set(SYSTEM_SNAPSHOT_COMMAND_IDS) <= set(outputs)
            or not set(outputs) <= set(SYSTEM_SNAPSHOT_COMMAND_IDS + OPTIONAL_SNAPSHOT_COMMAND_IDS)):
        raise ValueError("the desktop SSH service returned an invalid snapshot command set")

    checked: dict[str, tuple[int, str]] = {}
    total_bytes = 0
    for command_id in outputs:
        result = outputs[command_id]
        if (
            not isinstance(result, tuple)
            or len(result) != 2
            or isinstance(result[0], bool)
            or not isinstance(result[0], int)
            or not 0 <= result[0] <= 255
            or not isinstance(result[1], str)
        ):
            raise ValueError("the desktop SSH service returned an invalid snapshot result")
        total_bytes += len(result[1].encode("utf-8"))
        checked[command_id] = result
    if total_bytes > _MAX_OUTPUT_BYTES:
        raise ValueError("the desktop SSH service returned an oversized snapshot")

    snapshot: dict[str, Any] = {
        "capabilities": {
            "linux": bool(checked["os_detect"][1].strip()),
            "docker": checked["docker_info"][0] == 0,
        },
        "collectors": [],
    }
    parsed: dict[str, Any] = {}
    parsers = (
        ("os", _parse_os),
        ("cpu", _parse_cpu),
        ("memory", _parse_memory),
        ("uptime", _parse_uptime),
        ("disk", _parse_disks),
        ("network", _parse_network),
        ("docker", _parse_docker),
    )
    for collector_id, parser in parsers:
        try:
            parsed[collector_id] = parser(checked)
        except _CollectorParseError as error:
            snapshot["collectors"].append(
                {
                    "collectorId": collector_id,
                    "ok": False,
                    "error": f"collector {collector_id} failed: {error}",
                }
            )
        else:
            snapshot["collectors"].append({"collectorId": collector_id, "ok": True})

    field_names = {
        "os": "os",
        "cpu": "cpu",
        "memory": "memory",
        "uptime": "uptimeSeconds",
        "disk": "disks",
        "network": "network",
        "docker": "docker",
    }
    for collector_id, field_name in field_names.items():
        if collector_id in parsed:
            snapshot[field_name] = parsed[collector_id]
    network_details = normalize_network_details(checked)
    if network_details is not None:
        snapshot["networkDetails"] = network_details
    snapshot["health"] = _overall_health(
        _usage(parsed.get("cpu")),
        _usage(parsed.get("memory")),
        max((_usage(row) or 0.0 for row in parsed.get("disk", [])), default=None),
    )
    extend_snapshot(snapshot, checked)
    return snapshot


def _parse_os(outputs: Mapping[str, tuple[int, str]]) -> dict[str, str]:
    distribution = ""
    version = ""
    for line in outputs["os_release"][1].splitlines():
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        value = value.strip().strip('"')
        if key == "NAME":
            distribution = value
        elif key in ("VERSION_ID", "VERSION") and not version:
            version = value
    if not distribution:
        raise _CollectorParseError("failed to parse /etc/os-release")
    return {
        "distribution": distribution,
        "version": version,
        "hostname": outputs["os_hostname"][1].strip(),
        "kernel": outputs["os_kernel"][1].strip(),
        "arch": outputs["os_arch"][1].strip(),
    }


def _parse_cpu(outputs: Mapping[str, tuple[int, str]]) -> dict[str, Any]:
    ticks_a = _cpu_ticks(outputs["cpu_ticks_a"][1])
    ticks_b = _cpu_ticks(outputs["cpu_ticks_b"][1])
    total_a, total_b = sum(ticks_a), sum(ticks_b)
    delta_total = total_b - total_a
    if delta_total <= 0 or any(b < a for a, b in zip(ticks_a, ticks_b)):
        raise _CollectorParseError("CPU counters did not advance")
    delta_idle = ticks_b[3] + ticks_b[4] - ticks_a[3] - ticks_a[4]
    usage = (
        0.0
        if delta_total == 0
        else min(
            100.0,
            max(
                0.0,
                (delta_total - min(delta_total, delta_idle)) / delta_total * 100.0,
            ),
        )
    )

    core_text = outputs["cpu_cores"][1].strip()
    try:
        cores = int(core_text)
        if not 1 <= cores <= _U32_MAX:
            raise ValueError
    except ValueError as error:
        raise _CollectorParseError("CPU core count unavailable") from error
    load = [0.0, 0.0, 0.0]
    for index, field in enumerate(outputs["cpu_load"][1].split()[:3]):
        try:
            value = float(field)
            load[index] = value if math.isfinite(value) else 0.0
        except ValueError:
            pass
    return {
        "model": outputs["cpu_model"][1].strip(),
        "cores": cores,
        "usagePercent": usage,
        "loadAverage": load,
    }


def _cpu_ticks(line: str) -> tuple[int, ...]:
    # First eight counters include iowait/irq/softirq/steal. Guest is already
    # included in user/nice and must not be counted a second time.
    fields = line.split()[1:9]
    if len(fields) < 4:
        raise _CollectorParseError(f"unexpected /proc/stat line: {line}")
    values: list[int] = []
    for field in fields:
        try:
            value = int(field)
            if not 0 <= value <= _U64_MAX:
                raise ValueError
            values.append(value)
        except ValueError as error:
            raise _CollectorParseError("invalid CPU counter") from error
    return tuple(values + [0] * (8 - len(values)))


def _parse_memory(outputs: Mapping[str, tuple[int, str]]) -> dict[str, Any]:
    raw = outputs["memory_info"][1]
    total = _meminfo_kb(raw, "MemTotal")
    if total is None:
        raise _CollectorParseError("MemTotal missing")
    available = _meminfo_kb(raw, "MemAvailable")
    if available is None:
        available = _meminfo_kb(raw, "MemFree") or 0
    used = max(0, total - available)
    if max(total, used, available) > _U64_MAX // 1024:
        raise _CollectorParseError("memory value exceeds supported byte range")
    usage = 0.0 if total == 0 else used / total * 100.0
    return {
        "totalBytes": total * 1024,
        "usedBytes": used * 1024,
        "availableBytes": available * 1024,
        "usagePercent": usage,
    }


def _meminfo_kb(raw: str, key: str) -> int | None:
    for line in raw.splitlines():
        if ":" not in line:
            continue
        name, rest = line.split(":", 1)
        if name != key or not rest.split():
            continue
        try:
            value = int(rest.split()[0])
        except ValueError:
            continue
        return value if 0 <= value <= _U64_MAX else None
    return None


def _parse_uptime(outputs: Mapping[str, tuple[int, str]]) -> int:
    fields = outputs["uptime_info"][1].split()
    seconds = fields[0].split(".", 1)[0] if fields else ""
    try:
        value = int(seconds)
    except ValueError as error:
        raise _CollectorParseError("unexpected /proc/uptime output") from error
    if not 0 <= value <= _U64_MAX:
        raise _CollectorParseError("unexpected /proc/uptime output")
    return value


def _parse_disks(outputs: Mapping[str, tuple[int, str]]) -> list[dict[str, Any]]:
    disks: list[dict[str, Any]] = []
    for line in outputs["disk_usage"][1].splitlines()[1:]:
        fields = line.split()
        if not fields:
            continue
        if len(fields) < 4:
            raise _CollectorParseError("unexpected df column")
        try:
            size, used, _available = (int(fields[index]) for index in (1, 2, 3))
        except ValueError as error:
            raise _CollectorParseError("unexpected df column") from error
        if any(value < 0 or value > _U64_MAX for value in (size, used, _available)):
            raise _CollectorParseError("unexpected df column")
        device = fields[0]
        if device.startswith(("tmpfs", "overlay")):
            continue
        disks.append(
            {
                "device": device,
                "mountPoint": " ".join(fields[5:]),
                "totalBytes": size,
                "usedBytes": used,
                "usagePercent": 0.0 if size == 0 else used / size * 100.0,
            }
        )
    if not disks:
        raise _CollectorParseError("df produced no parseable rows")
    return disks


def _parse_network(outputs: Mapping[str, tuple[int, str]]) -> list[dict[str, Any]]:
    samples: list[dict[str, Any]] = []
    for line in outputs["network_info"][1].splitlines()[2:]:
        if ":" not in line:
            continue
        name, rest = line.split(":", 1)
        fields = rest.split()
        if len(fields) < 9:
            continue
        samples.append(
            {
                "name": name.strip(),
                "rxBytes": _unsigned_or_zero(fields[0]),
                "txBytes": _unsigned_or_zero(fields[8]),
            }
        )
    if not samples:
        raise _CollectorParseError("no interfaces in /proc/net/dev")
    return samples


def _parse_docker(outputs: Mapping[str, tuple[int, str]]) -> dict[str, Any]:
    if outputs["docker_info"][0] in (124, 137):
        raise _CollectorParseError("Docker detection timed out")
    available = outputs["docker_info"][0] == 0
    if not available:
        return {"available": False, "containers": []}
    if outputs["docker_ps"][0] != 0:
        raise _CollectorParseError("Docker container listing failed")
    containers: list[dict[str, Any]] = []
    for line in outputs["docker_ps"][1].splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except (json.JSONDecodeError, TypeError):
            continue
        if not isinstance(row, dict):
            continue
        restart_count = row.get("RestartCount", 0)
        if (
            isinstance(restart_count, bool)
            or not isinstance(restart_count, int)
            or not 0 <= restart_count <= _U64_MAX
        ):
            restart_count = 0
        containers.append(
            {
                "name": row.get("Names") if isinstance(row.get("Names"), str) else "",
                "image": row.get("Image") if isinstance(row.get("Image"), str) else "",
                "state": row.get("State") if isinstance(row.get("State"), str) else "",
                "status": row.get("Status") if isinstance(row.get("Status"), str) else "",
                "restartCount": restart_count % (1 << 32),
            }
        )
    return {"available": True, "containers": containers}


def _unsigned_or_zero(value: str) -> int:
    try:
        parsed = int(value)
        return parsed if 0 <= parsed <= _U64_MAX else 0
    except ValueError:
        return 0


def _usage(value: Any) -> float | None:
    usage = value.get("usagePercent") if isinstance(value, dict) else None
    return usage if isinstance(usage, (int, float)) and not isinstance(usage, bool) else None


def _overall_health(cpu: float | None, memory: float | None, disk: float | None) -> str:
    values = (
        (cpu, _HEALTH_THRESHOLDS["cpu"]),
        (memory, _HEALTH_THRESHOLDS["memory"]),
        (disk, _HEALTH_THRESHOLDS["disk"]),
    )
    seen = False
    result = "healthy"
    for usage, thresholds in values:
        if usage is None:
            continue
        seen = True
        if usage >= thresholds["critical"]:
            return "critical"
        if usage >= thresholds["warning"]:
            result = "warning"
    return result if seen else "unknown"
