"""Independent, read-only snapshot extensions; one unavailable probe cannot hide another."""
from __future__ import annotations

import csv
import io
import ipaddress
import math
from collections.abc import Callable, Mapping
from typing import Any

from yukinal_ssh.toolkit_catalog import CATALOG, TOOLKIT_VERSION

Outputs = Mapping[str, tuple[int, str]]


def _probe_message(code: int) -> str:
    if code in (124, 137):
        return "检测超时"
    if code == 127:
        return "检测工具不可用"
    if code == 125:
        return "检测输出超出限制"
    return "无法读取，可能缺少权限"


def _metric(value: str, maximum: float) -> float | None:
    if value.strip() in {"N/A", "[N/A]", "[Not Supported]", "Not Supported"}:
        return None
    parsed = float(value)
    if not math.isfinite(parsed) or not 0 <= parsed <= maximum:
        raise ValueError("GPU 读数无效")
    return parsed


def _gpu(outputs: Outputs) -> dict[str, Any]:
    code, raw = outputs["gpu_nvidia"]
    if code != 0:
        return {"status": "unavailable" if code == 127 else "error", "devices": [], "message": _probe_message(code)}
    devices = []
    for fields in csv.reader(io.StringIO(raw), skipinitialspace=True):
        if not fields:
            continue
        if len(fields) != 6 or len(devices) >= 64:
            raise ValueError("GPU 列表无效")
        index, name, utilization, used, total, temperature = (s.strip() for s in fields)
        if not index.isascii() or not index.isdigit() or int(index) > 65535 or not name or len(name) > 256:
            raise ValueError("GPU 标识无效")
        if any(d["index"] == int(index) for d in devices):
            raise ValueError("GPU 标识重复")
        device: dict[str, Any] = {"index": int(index), "name": name}
        for key, value, maximum, scale in (
            ("usagePercent", utilization, 100, 1),
            ("memoryUsedBytes", used, 2**32, 1048576),
            ("memoryTotalBytes", total, 2**32, 1048576),
            ("temperatureCelsius", temperature, 200, 1),
        ):
            metric = _metric(value, maximum)
            if metric is not None:
                device[key] = int(metric * scale) if scale != 1 else metric
        if device.get("memoryUsedBytes", 0) > device.get("memoryTotalBytes", math.inf):
            raise ValueError("GPU 显存读数无效")
        devices.append(device)
    return {"status": "available" if devices else "unavailable", "devices": devices,
            **({"message": "未返回 NVIDIA 设备"} if not devices else {})}


def _identity(raw: str) -> tuple[str, str]:
    if not raw.strip().isascii() or not raw.strip().isdigit():
        raise ValueError("用户标识无效")
    return ("warning", "当前以 root 连接") if int(raw) == 0 else ("info", "当前使用普通用户")


def _sshd(raw: str) -> tuple[str, str]:
    values = dict(line.split(maxsplit=1) for line in raw.splitlines() if line.strip())
    if values.get("permitrootlogin") not in {"yes", "no", "prohibit-password", "without-password", "forced-commands-only"}:
        raise ValueError("SSH 配置不完整")
    if values.get("passwordauthentication") not in {"yes", "no"}:
        raise ValueError("SSH 配置不完整")
    root = values["permitrootlogin"]
    password = values["passwordauthentication"] == "yes"
    return ("warning" if root == "yes" or password else "info",
            f"root: {root} · 密码登录{'开启' if password else '关闭'}（默认配置）")


def _firewall(raw: str) -> tuple[str, str]:
    lines = raw.strip().splitlines()
    if not lines:
        raise ValueError("防火墙状态未知")
    if lines[0] == "ufw":
        if "Status: active" in lines:
            return "info", "UFW 已启用"
        if "Status: inactive" in lines:
            return "warning", "UFW 未启用"
    if lines == ["firewalld", "running"]:
        return "info", "firewalld 运行中"
    # Missing/denied or other firewall implementations do not imply no firewall.
    raise ValueError("无法确认防火墙状态")


def _listeners(raw: str) -> tuple[str, str]:
    wildcard: set[tuple[str, int]] = set()
    for line in raw.splitlines():
        fields = line.split()
        if len(fields) < 6 or fields[0] not in {"tcp", "udp"}:
            raise ValueError("监听列表无效")
        host, port = fields[4].rsplit(":", 1)
        if not port.isascii() or not port.isdigit() or not 0 < int(port) < 65536:
            raise ValueError("监听端口无效")
        host = host.strip("[]")
        if host != "*":
            ipaddress.ip_address(host.split("%", 1)[0])
        if host in {"*", "0.0.0.0", "::"}:
            wildcard.add((fields[0], int(port)))
    ports = ", ".join(f"{proto}/{port}" for proto, port in sorted(wildcard)[:16])
    return "info", f"{len(wildcard)} 个通配地址监听" + (f" · {ports}" if ports else "")


def _security(outputs: Outputs) -> dict[str, Any]:
    checks = []
    for name, label, parse in (
        ("identity", "连接身份", _identity),
        ("sshd", "SSH 登录", _sshd),
        ("firewall", "防火墙", _firewall),
        ("listeners", "监听端口", _listeners),
    ):
        code, raw = outputs.get("security_" + name, (127, ""))
        if code != 0:
            status, detail = "unknown", _probe_message(code)
        else:
            try:
                status, detail = parse(raw)
            except (ValueError, OverflowError):
                status, detail = "unknown", "返回数据无法识别"
        checks.append({"id": name, "label": label, "status": status, "detail": detail})
    return {"checks": checks}


# Add a module here and its fixed probes to toolkit_catalog.json. No remote code loading.
_MODULES: dict[str, Callable[[Outputs], dict[str, Any]]] = {"gpu": _gpu, "security": _security}


def extend_snapshot(snapshot: dict[str, Any], outputs: Outputs) -> None:
    modules = []
    for module, parse in _MODULES.items():
        ids = [p["id"] for p in CATALOG["probes"] if p["module"] == module]
        if not any(name in outputs for name in ids):
            continue  # Older services did not report extensions.
        modules.append(module)
        sample: dict[str, Any] = {"collectorId": module, "ok": True}
        try:
            snapshot[module] = parse(outputs)
            if snapshot[module].get("status") == "error":
                sample.update(ok=False, error=snapshot[module]["message"])
        except (ValueError, OverflowError, KeyError, csv.Error):
            sample.update(ok=False, error="检测数据无效")
        snapshot["collectors"].append(sample)
    if modules:
        snapshot["toolkit"] = {"version": TOOLKIT_VERSION, "modules": modules}
