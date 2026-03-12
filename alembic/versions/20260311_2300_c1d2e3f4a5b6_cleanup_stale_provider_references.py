"""cleanup stale provider references after provider deletion

Revision ID: c1d2e3f4a5b6
Revises: 9b7c6d5e4f3a
Create Date: 2026-03-11 23:00:00.000000+00:00

"""

from __future__ import annotations

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision = "c1d2e3f4a5b6"
down_revision = "9b7c6d5e4f3a"
branch_labels = None
depends_on = None

_users = sa.table(
    "users",
    sa.column("id", sa.String(36)),
    sa.column("allowed_providers", sa.JSON()),
)
_api_keys = sa.table(
    "api_keys",
    sa.column("id", sa.String(36)),
    sa.column("allowed_providers", sa.JSON()),
)
_user_preferences = sa.table(
    "user_preferences",
    sa.column("id", sa.String(36)),
    sa.column("default_provider_id", sa.String(36)),
)
_video_tasks = sa.table(
    "video_tasks",
    sa.column("id", sa.String(36)),
    sa.column("provider_id", sa.String(36)),
    sa.column("endpoint_id", sa.String(36)),
)
_providers = sa.table("providers", sa.column("id", sa.String(36)))
_provider_endpoints = sa.table("provider_endpoints", sa.column("id", sa.String(36)))


def _load_valid_ids(conn: sa.Connection, table: sa.Table) -> set[str]:
    return {str(row[0]) for row in conn.execute(sa.select(table.c.id)).fetchall() if row[0]}


def _cleanup_allowed_providers(
    conn: sa.Connection,
    table: sa.Table,
    valid_provider_ids: set[str],
) -> None:
    rows = conn.execute(
        sa.select(table.c.id, table.c.allowed_providers).where(
            table.c.allowed_providers.isnot(None)
        )
    ).fetchall()
    for row_id, allowed_providers in rows:
        if not isinstance(allowed_providers, list):
            continue
        filtered = [
            provider_id for provider_id in allowed_providers if provider_id in valid_provider_ids
        ]
        if filtered == allowed_providers:
            continue
        conn.execute(table.update().where(table.c.id == row_id).values(allowed_providers=filtered))


def _nullify_missing_fk(
    conn: sa.Connection,
    table: sa.Table,
    id_column: sa.ColumnElement[str],
    fk_column: sa.ColumnElement[str],
    valid_ids: set[str],
) -> None:
    rows = conn.execute(sa.select(id_column, fk_column).where(fk_column.isnot(None))).fetchall()
    invalid_row_ids = [row_id for row_id, fk_value in rows if fk_value not in valid_ids]
    if not invalid_row_ids:
        return
    conn.execute(table.update().where(id_column.in_(invalid_row_ids)).values({fk_column.key: None}))


def upgrade() -> None:
    conn = op.get_bind()
    valid_provider_ids = _load_valid_ids(conn, _providers)
    valid_endpoint_ids = _load_valid_ids(conn, _provider_endpoints)

    _cleanup_allowed_providers(conn, _users, valid_provider_ids)
    _cleanup_allowed_providers(conn, _api_keys, valid_provider_ids)
    _nullify_missing_fk(
        conn,
        _user_preferences,
        _user_preferences.c.id,
        _user_preferences.c.default_provider_id,
        valid_provider_ids,
    )
    _nullify_missing_fk(
        conn,
        _video_tasks,
        _video_tasks.c.id,
        _video_tasks.c.provider_id,
        valid_provider_ids,
    )
    _nullify_missing_fk(
        conn,
        _video_tasks,
        _video_tasks.c.id,
        _video_tasks.c.endpoint_id,
        valid_endpoint_ids,
    )


def downgrade() -> None:
    pass
