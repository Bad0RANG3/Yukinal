#!/usr/bin/env python3
"""Collect Python distribution license texts from a frozen Agent payload.

The inventory is enumerated from the *payload* (the PyInstaller onedir that will
actually ship), not from the interpreter running this script. Reading the build
venv instead overstated the bundled set: the venv held 85 distributions while the
payload shipped 21, so the notice declared 64 components that were not present,
including metadata for a package that no longer exists. Enumerating `*.dist-info`
under the payload makes the notice a description of the shipped bytes, and makes a
stale file impossible to inherit from an earlier build.

Paths inside the notice are relative to the payload root. The canonical
`build-python-agent.mjs` invocation passes an absolute `--payload`, so an absolute
path rendered here would ship the build machine's directory layout (and make two
otherwise identical builds disagree).
"""

from __future__ import annotations

import hashlib
import sys
from email.parser import Parser
from pathlib import Path


LICENSE_PREFIXES = ("license", "copying", "notice", "copyright")
MAX_LICENSE_BYTES = 2 * 1024 * 1024


def metadata_of(dist_info: Path) -> tuple[str, str, str, list[str], list[str]]:
    """Returns (name, version, license declaration, project URLs, declared license files)."""
    metadata_path = dist_info / "METADATA"
    name = dist_info.name[: -len(".dist-info")]
    version = "unknown"
    license_name = "License metadata not declared"
    urls: list[str] = []
    declared: list[str] = []
    try:
        message = Parser().parsestr(metadata_path.read_text(encoding="utf-8", errors="replace"))
    except OSError:
        return name, version, license_name, urls, declared

    name = message.get("Name") or name
    version = message.get("Version") or version
    license_name = (
        message.get("License-Expression") or message.get("License") or license_name
    )
    urls = [
        value
        for value in message.get_all("Project-URL", [])
        if "source" in value.casefold() or "homepage" in value.casefold()
    ]
    declared = list(message.get_all("License-File") or [])
    return name, version, license_name, urls, declared


def license_files(dist_info: Path, declared: list[str]) -> list[Path]:
    """License texts inside one `.dist-info`, sorted for a reproducible notice.

    A file is included when either:
      * its *name* starts with a license word (`LICENSE`, `COPYING`, `NOTICE`,
        `COPYRIGHT`) — modern wheels put the text under a `licenses/` subdirectory,
        so the search is recursive and the suffix is deliberately NOT filtered. An
        earlier suffix whitelist silently dropped `LICENSE.APACHE` and
        `LICENSE.BSD` from `cryptography`, shipping only the 197-byte pointer file
        while the metadata declared all three; or
      * the distribution's `License-File` metadata names it outright, which is the
        wheel spec's own declaration and outranks any naming convention.
    """
    declared_names = {Path(value).name.casefold() for value in declared}
    found = []
    for path in sorted(dist_info.rglob("*")):
        if not path.is_file():
            continue
        named = path.name.casefold().startswith(LICENSE_PREFIXES)
        if not named and path.name.casefold() not in declared_names:
            continue
        found.append(path)
    return found


def main() -> int:
    arguments = sys.argv[1:]
    payload: Path | None = None
    destination: Path | None = None
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        value = arguments[index + 1] if index + 1 < len(arguments) else None
        if argument == "--payload" and value is not None:
            payload = Path(value)
            index += 2
        elif argument == "--destination" and value is not None:
            destination = Path(value)
            index += 2
        else:
            print(
                "usage: collect-python-licenses.py --payload <frozen onedir> --destination <output-file>",
                file=sys.stderr,
            )
            return 2

    if payload is None or destination is None:
        print(
            "usage: collect-python-licenses.py --payload <frozen onedir> --destination <output-file>",
            file=sys.stderr,
        )
        return 2
    # Resolve so the paths below can be made relative to the payload: an absolute
    # `--payload` would otherwise bake the build machine's layout (D:\yukinal\...) into
    # a file that ships with the payload, and two byte-identical builds on different
    # machines would disagree.
    payload = payload.resolve()
    if not payload.is_dir():
        print(f"frozen Agent payload was not found: {payload}", file=sys.stderr)
        return 2

    distributions = sorted(
        (path for path in payload.rglob("*.dist-info") if path.is_dir()),
        key=lambda path: path.as_posix(),
    )
    if not distributions:
        # An empty notice would silently ship an unattributed bundle, so refuse.
        print(
            f"no *.dist-info metadata was found under {payload}; the frozen Agent "
            "payload looks incomplete",
            file=sys.stderr,
        )
        return 1

    def display(path: Path) -> str:
        try:
            return path.relative_to(payload).as_posix()
        except ValueError:
            return path.name

    destination.parent.mkdir(parents=True, exist_ok=True)
    sections: list[str] = [
        "Third-party Python distribution licenses included with the frozen Yukinal Agent.",
        "Each component remains under its own license. License metadata and available license texts are listed below.",
    ]
    seen_texts: set[str] = set()

    for dist_info in distributions:
        name, version, license_name, urls, declared = metadata_of(dist_info)
        sections.append(f"\n## {name} {version}\n\nLicense: {license_name}")
        sections.extend(f"Project URL: {url}" for url in urls)

        for path in license_files(dist_info, declared):
            try:
                raw = path.read_bytes()
            except OSError:
                continue
            relative = display(path)
            if len(raw) > MAX_LICENSE_BYTES:
                sections.append(f"\nLicense file omitted because it exceeds {MAX_LICENSE_BYTES} bytes: {relative}")
                continue
            digest = hashlib.sha256(raw).hexdigest()
            if digest in seen_texts:
                sections.append(f"\nLicense text shared with another package: {relative}")
                continue
            seen_texts.add(digest)
            text = raw.decode("utf-8", errors="replace").strip()
            sections.append(f"\n### {relative}\n\n{text}")

    destination.write_text("\n".join(sections) + "\n", encoding="utf-8")
    print(f"Collected Python license notices for {len(distributions)} distribution(s): {destination}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
