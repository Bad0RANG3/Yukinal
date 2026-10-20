"""Bundled, fixed read-only probes. Never load executable probes from server input."""
from importlib.resources import files
import json

CATALOG = json.loads(files(__package__).joinpath("toolkit_catalog.json").read_text(encoding="utf-8"))
TOOLKIT_VERSION = CATALOG["version"]
SYSTEM_SNAPSHOT_COMMAND_IDS = tuple(p["id"] for p in CATALOG["probes"] if p["required"])
OPTIONAL_SNAPSHOT_COMMAND_IDS = tuple(p["id"] for p in CATALOG["probes"] if not p["required"])
NETWORK_SNAPSHOT_COMMAND_IDS = tuple(p["id"] for p in CATALOG["probes"] if p["module"] == "network" and not p["required"])
