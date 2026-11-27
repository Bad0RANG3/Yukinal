"""Bundled, fixed read-only probes. Never load executable probes from server input."""
from importlib.resources import files
import json

CATALOG = json.loads(files(__package__).joinpath("toolkit_catalog.json").read_text(encoding="utf-8"))
TOOLKIT_VERSION = CATALOG["version"]
SYSTEM_SNAPSHOT_COMMAND_IDS = tuple(p["id"] for p in CATALOG["probes"] if p["required"])
OPTIONAL_SNAPSHOT_COMMAND_IDS = tuple(p["id"] for p in CATALOG["probes"] if not p["required"])
NETWORK_SNAPSHOT_COMMAND_IDS = tuple(p["id"] for p in CATALOG["probes"] if p["module"] == "network" and not p["required"])

# The single source of truth for the fixed read-only Agent tools. The C++ SSH
# service compiles the same list, so the command text that reaches a target is
# always a reviewed constant from this file — never a model-supplied command.
_READ_PROBES = {entry["id"]: entry for entry in CATALOG.get("readProbes", ())}
READ_PROBE_IDS = tuple(_READ_PROBES)
READ_PROBE_COMMANDS = {probe_id: entry["command"] for probe_id, entry in _READ_PROBES.items()}
READ_PROBE_SUMMARIES = {probe_id: entry.get("summary", "") for probe_id, entry in _READ_PROBES.items()}
READ_PROBE_TIMEOUTS = {probe_id: entry.get("timeoutSeconds", 10) for probe_id, entry in _READ_PROBES.items()}
READ_PROBE_PATH_PREFIXES = {
    probe_id: tuple(entry.get("allowedPrefixes", ()))
    for probe_id, entry in _READ_PROBES.items()
    if entry.get("parameter")
}
