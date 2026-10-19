"""one public key, one node

Revision ID: b7c41d0e9a2f
Revises: fc2e168fa85c
Create Date: 2026-10-18 09:12:04.118253

The node id is now part of the signed canonical string, and the signature is
verified against the key on the row that id names. That only binds a key to one
node if the key can only back one row, so ``nodes.public_key`` becomes globally
unique. ``uq_nodes_tenant_name`` is per tenant and would not be enough: the node
id header is unsigned routing input, so a key registered once per tenant is the
same key twice and a signature for either id verifies against either row.

Deployments can already hold duplicate keys, and the API allowed them, so the
upgrade does not fail halfway through an ``ALTER TABLE``: it counts the
duplicates first and refuses with the offending fingerprints. The operator then
re-points the extra node (register its own key, or delete it), and re-runs. The
constraint is still installed — the migration is not skipped, the data has to be
made consistent first.
"""
from __future__ import annotations

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

revision: str = 'b7c41d0e9a2f'
down_revision: str | None = 'fc2e168fa85c'
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None

# Group duplicates by the key itself, never by id: the fingerprint of a key is
# what an operator can match against the private key they hold.
_DUPLICATE_KEYS = sa.text(
    "SELECT public_key, COUNT(*) AS copies FROM nodes "
    "GROUP BY public_key HAVING COUNT(*) > 1"
)


def _duplicates(connection) -> list[tuple[str, int]]:
    return [(row.public_key, row.copies) for row in connection.execute(_DUPLICATE_KEYS)]


def _fingerprint(public_key: str) -> str:
    """A short, non-reversible handle for one key, for the refusal message."""
    import base64
    import binascii
    import hashlib

    try:
        raw = base64.b64decode(public_key, validate=True)
    except (binascii.Error, ValueError):
        return "unparseable"
    return "SHA256:" + base64.b64encode(hashlib.sha256(raw).digest()).decode("ascii")[:16]


def upgrade() -> None:
    connection = op.get_bind()
    duplicates = _duplicates(connection)
    if duplicates:
        detail = ", ".join(
            f"{_fingerprint(key)} x{copies}" for key, copies in duplicates[:10]
        )
        raise RuntimeError(
            "cannot make nodes.public_key unique: "
            f"{len(duplicates)} public key(s) are registered to more than one node "
            f"({detail}). Re-point or delete the extra node rows, then re-run this "
            "migration. The constraint is required: the node id is signed, so a key "
            "that backs two nodes lets a signature for one satisfy the other."
        )
    with op.batch_alter_table('nodes', schema=None) as batch_op:
        batch_op.create_unique_constraint('uq_nodes_public_key', ['public_key'])


def downgrade() -> None:
    with op.batch_alter_table('nodes', schema=None) as batch_op:
        batch_op.drop_constraint('uq_nodes_public_key', type_='unique')
