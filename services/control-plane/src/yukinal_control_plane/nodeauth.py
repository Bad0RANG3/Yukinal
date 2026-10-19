"""Node identity: Ed25519 request signatures.

A node registers an Ed25519 **public** key. Every node-authenticated request —
and the WebSocket upgrade — carries a signature over a canonical string, so the
control plane never stores a node secret: it stores the public half and verifies.
That keeps the credential boundary honest (no material at rest) and lets a node
prove possession on each call without a session table.

Canonical string (newline separated, node id first, hashed body):

    <node-id>\n<timestamp>\n<nonce>\n<METHOD>\n<path[?query]>\n<sha256-hex(body)>

The node id is *inside* the signature, not merely beside it. It is the row the
control plane looks the public key up in, and a key is registered once, so
signing the id is what stops a key registered in one place from being presented
under a different node's header: without it the id is unsigned routing input and
the signature only proves that *some* registered key signed the same bytes.

The timestamp must be within :data:`SIGNATURE_WINDOW_SECONDS` of the server
clock, and the nonce must be unused for that node, so a captured request cannot
be replayed inside the window.

:func:`sign_headers` is the client half: it is the exact contract a node driver
must implement, and the tests use it.
"""

from __future__ import annotations

import base64
import binascii
import hashlib
import secrets
import time

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import (
    Ed25519PrivateKey,
    Ed25519PublicKey,
)

# How far a signed timestamp may sit from the server clock, in seconds.
SIGNATURE_WINDOW_SECONDS = 300
# Raw Ed25519 public keys are always 32 bytes.
_KEY_BYTES = 32
_OPENSSH_ALGORITHM = b"ssh-ed25519"

HEADER_NODE = "X-Yukinal-Node"
HEADER_TIMESTAMP = "X-Yukinal-Timestamp"
HEADER_NONCE = "X-Yukinal-Nonce"
HEADER_SIGNATURE = "X-Yukinal-Signature"


def parse_public_key(text: str) -> bytes:
    """Return the raw 32-byte Ed25519 public key encoded by ``text``.

    Accepts the canonical base64 form (what the control plane stores), a hex
    string, or an OpenSSH ``ssh-ed25519 <base64> [comment]`` line. Raises
    ``ValueError`` for anything else, so registration cannot store a key that
    could never verify a signature.
    """
    value = (text or "").strip()
    if not value:
        raise ValueError("the node public key is empty")

    if value.startswith("ssh-ed25519"):
        parts = value.split()
        if len(parts) < 2 or parts[0] != "ssh-ed25519":
            raise ValueError("the OpenSSH public key is malformed")
        try:
            blob = base64.b64decode(parts[1], validate=True)
        except (binascii.Error, ValueError) as error:
            raise ValueError("the OpenSSH public key is not valid base64") from error
        if len(blob) < 4:
            raise ValueError("the OpenSSH public key is truncated")
        algorithm_length = int.from_bytes(blob[:4], "big")
        algorithm = blob[4 : 4 + algorithm_length]
        rest = blob[4 + algorithm_length :]
        if algorithm != _OPENSSH_ALGORITHM or len(rest) < 4:
            raise ValueError("the OpenSSH public key is not an ssh-ed25519 key")
        key_length = int.from_bytes(rest[:4], "big")
        key = rest[4 : 4 + key_length]
        if key_length != _KEY_BYTES or len(key) != _KEY_BYTES:
            raise ValueError("the Ed25519 public key must be 32 bytes")
        return key

    for decoder in (_decode_base64, _decode_hex):
        try:
            raw = decoder(value)
        except ValueError:
            continue
        # A 64-character hex string is also valid base64, so a form only counts
        # when it decodes to exactly one key; otherwise try the next form.
        if len(raw) == _KEY_BYTES:
            return raw
    raise ValueError("the node public key must be a 32-byte Ed25519 key in base64 or hex form")


def _decode_base64(value: str) -> bytes:
    try:
        return base64.b64decode(value, validate=True)
    except (binascii.Error, ValueError) as error:
        raise ValueError("not base64") from error


def _decode_hex(value: str) -> bytes:
    try:
        return bytes.fromhex(value)
    except ValueError as error:
        raise ValueError("not hex") from error


def encode_public_key(raw: bytes) -> str:
    """Canonical stored form: standard base64 of the raw key."""
    return base64.b64encode(raw).decode("ascii")


def key_fingerprint(raw: bytes) -> str:
    """OpenSSH-style fingerprint, for logs and the API without echoing the key."""
    digest = base64.b64encode(hashlib.sha256(raw).digest()).decode("ascii").rstrip("=")
    return f"SHA256:{digest}"


def body_digest(body: bytes) -> str:
    return hashlib.sha256(body).hexdigest()


def canonical_request(
    *, node_id: str, timestamp: str, nonce: str, method: str, path: str, body: bytes
) -> bytes:
    """The exact bytes a node signs and the control plane verifies.

    ``node_id`` is the first line, so the signed bytes name the node the
    signature is for. Every field is newline separated and the node id is not
    attacker-chosen on the verifying side (it is the header the caller sent, but
    the key it is checked against is that node's own), so a key registered under
    one node cannot be replayed against another node's id.
    """
    return "\n".join(
        [node_id, timestamp, nonce, method.upper(), path, body_digest(body)]
    ).encode("utf-8")


def timestamp_within_window(
    timestamp: str, *, now: int | None = None, window: int = SIGNATURE_WINDOW_SECONDS
) -> bool:
    try:
        value = int(timestamp)
    except (TypeError, ValueError):
        return False
    reference = int(time.time()) if now is None else now
    return abs(reference - value) <= window


def verify_signature(public_key_b64: str, signature_b64: str, canonical: bytes) -> bool:
    """True when ``signature_b64`` is a valid Ed25519 signature by the key."""
    try:
        raw = base64.b64decode(public_key_b64, validate=True)
        signature = base64.b64decode(signature_b64, validate=True)
        Ed25519PublicKey.from_public_bytes(raw).verify(signature, canonical)
        return True
    except (binascii.Error, ValueError, InvalidSignature):
        return False


def sign_headers(
    private_key: Ed25519PrivateKey,
    *,
    node_id: str,
    method: str,
    path: str,
    body: bytes = b"",
    timestamp: int | None = None,
    nonce: str | None = None,
) -> dict[str, str]:
    """Build the four node authentication headers for one request.

    This is the client-side contract: a node driver signs the canonical string
    with its private key and sends the public-key-derived headers, never the key
    itself beyond the identifier the operator already registered. ``node_id`` is
    the id the driver's own registration returned; it is signed, so a driver
    cannot sign for a node it is not.
    """
    stamp = str(int(time.time()) if timestamp is None else timestamp)
    value = secrets.token_hex(16) if nonce is None else nonce
    canonical = canonical_request(
        node_id=node_id,
        timestamp=stamp,
        nonce=value,
        method=method,
        path=path,
        body=body,
    )
    signature = private_key.sign(canonical)
    return {
        HEADER_TIMESTAMP: stamp,
        HEADER_NONCE: value,
        HEADER_SIGNATURE: base64.b64encode(signature).decode("ascii"),
    }
