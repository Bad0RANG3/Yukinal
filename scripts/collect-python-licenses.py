#!/usr/bin/env python3
"""Collect installed Python distribution license texts for the desktop resource bundle."""

from __future__ import annotations

import hashlib
import sys
from importlib.metadata import distributions
from pathlib import Path


LICENSE_PREFIXES = ("license", "copying", "notice", "copyright")
MAX_LICENSE_BYTES = 2 * 1024 * 1024


def main() -> int:
    if len(sys.argv) != 3 or sys.argv[1] != "--destination":
        print("usage: collect-python-licenses.py --destination <output-file>", file=sys.stderr)
        return 2

    destination = Path(sys.argv[2])
    destination.parent.mkdir(parents=True, exist_ok=True)
    sections: list[str] = [
        "Third-party Python distribution licenses included with the frozen Yukinal Agent.",
        "Each component remains under its own license. License metadata and available license texts are listed below.",
    ]
    seen_texts: set[str] = set()

    for distribution in sorted(distributions(), key=lambda item: (item.metadata.get("Name", "").casefold(), item.version)):
        name = distribution.metadata.get("Name", "unknown package")
        metadata = distribution.metadata
        license_name = metadata.get("License-Expression") or metadata.get("License") or "License metadata not declared"
        urls = [value for value in metadata.get_all("Project-URL", []) if "source" in value.casefold() or "homepage" in value.casefold()]
        sections.append(f"\n## {name} {distribution.version}\n\nLicense: {license_name}")
        if urls:
            sections.extend(f"Project URL: {url}" for url in urls)

        files = distribution.files or ()
        license_files = [
            item for item in files
            if Path(item.name).name.casefold().startswith(LICENSE_PREFIXES)
            and Path(item.name).suffix.casefold() in {"", ".txt", ".md", ".rst", ".html"}
        ]
        for item in license_files:
            path = distribution.locate_file(item)
            try:
                raw = path.read_bytes()
            except OSError:
                continue
            if len(raw) > MAX_LICENSE_BYTES:
                sections.append(f"\nLicense file omitted because it exceeds {MAX_LICENSE_BYTES} bytes: {item}")
                continue
            digest = hashlib.sha256(raw).hexdigest()
            if digest in seen_texts:
                sections.append(f"\nLicense text shared with another package: {item}")
                continue
            seen_texts.add(digest)
            text = raw.decode("utf-8", errors="replace").strip()
            sections.append(f"\n### {item}\n\n{text}")

    destination.write_text("\n".join(sections) + "\n", encoding="utf-8")
    print(f"Collected Python license notices: {destination}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
