"""decouple request_candidates.key_id foreign key from provider_api_keys lifecycle

Revision ID: b7c8d9e0f1a2
Revises: c1d2e3f4a5b6
Create Date: 2026-03-12 19:15:00.000000+00:00

"""

from __future__ import annotations

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision = "b7c8d9e0f1a2"
down_revision = "c1d2e3f4a5b6"
branch_labels = None
depends_on = None


def _fk_exists(constraint_name: str, table_name: str) -> bool:
    bind = op.get_bind()
    result = bind.execute(
        sa.text(
            "SELECT 1 FROM pg_constraint c "
            "JOIN pg_class r ON c.conrelid = r.oid "
            "JOIN pg_namespace n ON r.relnamespace = n.oid "
            "WHERE c.conname = :name AND r.relname = :table "
            "  AND n.nspname = current_schema() AND c.contype = 'f'"
        ),
        {"name": constraint_name, "table": table_name},
    )
    return result.scalar() is not None


def upgrade() -> None:
    if _fk_exists("request_candidates_key_id_fkey", "request_candidates"):
        op.drop_constraint(
            "request_candidates_key_id_fkey", "request_candidates", type_="foreignkey"
        )


def downgrade() -> None:
    bind = op.get_bind()
    bind.execute(
        sa.text(
            "UPDATE request_candidates rc "
            "SET key_id = NULL "
            "WHERE key_id IS NOT NULL "
            "  AND NOT EXISTS ("
            "    SELECT 1 FROM provider_api_keys pak WHERE pak.id = rc.key_id"
            "  )"
        )
    )
    if not _fk_exists("request_candidates_key_id_fkey", "request_candidates"):
        op.create_foreign_key(
            "request_candidates_key_id_fkey",
            "request_candidates",
            "provider_api_keys",
            ["key_id"],
            ["id"],
            ondelete="CASCADE",
        )
