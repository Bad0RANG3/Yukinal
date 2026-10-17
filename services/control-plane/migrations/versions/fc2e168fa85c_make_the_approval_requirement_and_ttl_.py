"""make the approval requirement and TTL per tenant

Revision ID: fc2e168fa85c
Revises: c2ee024868fa
Create Date: 2026-10-17 13:56:53.543913

"""
from __future__ import annotations

from collections.abc import Sequence

import sqlalchemy as sa
from alembic import op

revision: str = 'fc2e168fa85c'
down_revision: str | None = 'c2ee024868fa'
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None


def upgrade() -> None:
    # Two new tables; no existing row is rewritten. `tier_modes` is stored
    # resolved rather than derived on read, so changing a built-in default later
    # cannot silently re-open a tenant that had already chosen.
    op.create_table('tenant_policies',
    sa.Column('tenant_id', sa.String(length=64), nullable=False),
    sa.Column('environment', sa.String(length=16), nullable=False),
    sa.Column('tier_modes', sa.JSON(), nullable=False),
    sa.Column('approval_ttl_seconds', sa.Integer(), nullable=False),
    sa.Column('updated_by', sa.String(length=200), nullable=True),
    sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False),
    sa.ForeignKeyConstraint(['tenant_id'], ['tenants.id'], ondelete='CASCADE'),
    sa.PrimaryKeyConstraint('tenant_id')
    )
    op.create_table('tool_policies',
    sa.Column('id', sa.String(length=64), nullable=False),
    sa.Column('tenant_id', sa.String(length=64), nullable=False),
    sa.Column('kind', sa.String(length=128), nullable=False),
    sa.Column('risk', sa.String(length=16), nullable=False),
    sa.Column('updated_by', sa.String(length=200), nullable=True),
    sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False),
    sa.ForeignKeyConstraint(['tenant_id'], ['tenants.id'], ondelete='CASCADE'),
    sa.PrimaryKeyConstraint('id'),
    sa.UniqueConstraint('tenant_id', 'kind', name='uq_tool_policies_tenant_kind')
    )
    with op.batch_alter_table('tool_policies', schema=None) as batch_op:
        batch_op.create_index(batch_op.f('ix_tool_policies_tenant_id'), ['tenant_id'], unique=False)


def downgrade() -> None:
    with op.batch_alter_table('tool_policies', schema=None) as batch_op:
        batch_op.drop_index(batch_op.f('ix_tool_policies_tenant_id'))

    op.drop_table('tool_policies')
    op.drop_table('tenant_policies')
