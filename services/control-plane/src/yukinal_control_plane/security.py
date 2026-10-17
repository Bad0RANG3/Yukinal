"""Secret-boundary helpers.

The control plane stores credential *references*, never credential material. A
request that carries an obviously secret-shaped field is rejected before any
write, so a careless caller cannot turn the registry into a secret store.
"""

from __future__ import annotations

from .errors import SecretRejected

# Field names (normalized to lowercase alphanumerics) that indicate credential
# material rather than a reference to it.
_SECRET_HINTS = (
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "privatekey",
    "apikey",
    "accesskey",
    "credential",
)


def _normalized(name: str) -> str:
    return "".join(character for character in name.lower() if character.isalnum())


def find_secret_fields(value: object, *, path: str = "") -> list[str]:
    """Recursively collect dotted paths whose key looks like secret material."""
    found: list[str] = []
    if isinstance(value, dict):
        for key, child in value.items():
            name = str(key)
            dotted = f"{path}.{name}" if path else name
            if any(hint in _normalized(name) for hint in _SECRET_HINTS):
                found.append(dotted)
            found.extend(find_secret_fields(child, path=dotted))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            found.extend(find_secret_fields(child, path=f"{path}[{index}]"))
    return found


def assert_no_secret_fields(value: object) -> None:
    """Raise ``SecretRejected`` when ``value`` carries secret-shaped keys."""
    offenders = find_secret_fields(value)
    if offenders:
        raise SecretRejected(
            "secret-shaped fields are not accepted; store a credential reference instead: "
            + ", ".join(sorted(offenders))
        )
