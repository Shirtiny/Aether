"""Shared idempotent helpers for Alembic migrations.

All metadata lookups are batched: one query loads an entire table's column info
or all FK delete rules, then results are cached for the migration's lifetime.
"""

from __future__ import annotations

from collections import defaultdict
from collections.abc import Callable

import sqlalchemy as sa

from alembic import op

# ---------------------------------------------------------------------------
# Batch metadata cache
# ---------------------------------------------------------------------------


class _SchemaCache:
    """Lazy, per-migration cache for information_schema lookups.

    Call ``load_columns(tables)`` / ``load_fk_rules(tables)`` once at the top
    of ``upgrade()`` or ``downgrade()`` to prime the cache.  Subsequent
    ``column_exists`` / ``column_type`` / ``fk_ondelete`` calls are pure
    dict lookups -- zero extra DB round-trips.
    """

    def __init__(self) -> None:
        # {table_name: {column_name: data_type}}
        self._columns: dict[str, dict[str, str]] = {}
        # {(table_name, constraint_name): delete_rule}
        self._fk_rules: dict[tuple[str, str], str] = {}
        self._fk_loaded_tables: set[str] = set()

    # -- loaders (one query per call) --------------------------------------

    def load_columns(self, tables: list[str]) -> None:
        """Fetch column names + data types for *tables* in one query."""
        need = [t for t in tables if t not in self._columns]
        if not need:
            return
        bind = op.get_bind()
        rows = bind.execute(
            sa.text(
                "SELECT table_name, column_name, data_type "
                "FROM information_schema.columns "
                "WHERE table_name = ANY(:tables) "
                "  AND table_schema = current_schema()"
            ),
            {"tables": need},
        ).fetchall()
        # Initialise even empty tables so we don't re-query
        for t in need:
            self._columns.setdefault(t, {})
        for table, col, dtype in rows:
            self._columns[table][col] = dtype

    def load_fk_rules(self, tables: list[str]) -> None:
        """Fetch FK delete rules for *tables* in one query."""
        need = [t for t in tables if t not in self._fk_loaded_tables]
        if not need:
            return
        bind = op.get_bind()
        rows = bind.execute(
            sa.text(
                "SELECT tc.table_name, tc.constraint_name, rc.delete_rule "
                "FROM information_schema.referential_constraints rc "
                "JOIN information_schema.table_constraints tc "
                "  ON rc.constraint_name = tc.constraint_name "
                " AND rc.constraint_schema = tc.constraint_schema "
                "WHERE tc.table_name = ANY(:tables) "
                "  AND tc.table_schema = current_schema()"
            ),
            {"tables": need},
        ).fetchall()
        for table, name, rule in rows:
            self._fk_rules[(table, name)] = rule
        self._fk_loaded_tables.update(need)

    # -- lookups (pure dict, zero DB) --------------------------------------

    def column_exists(self, table: str, column: str) -> bool:
        return column in self._columns.get(table, {})

    def column_type(self, table: str, column: str) -> str | None:
        return self._columns.get(table, {}).get(column)

    def is_numeric(self, table: str, column: str) -> bool:
        return self.column_type(table, column) == "numeric"

    def fk_ondelete(self, table: str, constraint: str) -> str | None:
        return self._fk_rules.get((table, constraint))

    def invalidate_columns(self, table: str) -> None:
        """Force re-load on next load_columns() for *table* (after ADD/DROP COLUMN)."""
        self._columns.pop(table, None)


def new_cache() -> _SchemaCache:
    """Create a fresh schema cache for a single migration run."""
    return _SchemaCache()


# ---------------------------------------------------------------------------
# Idempotent DDL helpers
# ---------------------------------------------------------------------------


def _fk_exists(constraint_name: str, table_name: str) -> bool:
    """Check if a FK constraint exists via pg_constraint (definitive)."""
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


def replace_fk_if_needed(
    cache: _SchemaCache,
    constraint_name: str,
    table_name: str,
    ref_table: str,
    local_cols: list[str],
    remote_cols: list[str],
    desired_ondelete: str,
) -> None:
    """Drop and recreate a FK only if the current ON DELETE rule differs."""
    current = cache.fk_ondelete(table_name, constraint_name)
    if current and current.upper() == desired_ondelete.upper():
        return
    # Cache may miss existing constraints; fall back to pg_constraint lookup
    if current or _fk_exists(constraint_name, table_name):
        op.drop_constraint(constraint_name, table_name, type_="foreignkey")
    op.create_foreign_key(
        constraint_name,
        table_name,
        ref_table,
        local_cols,
        remote_cols,
        ondelete=desired_ondelete,
    )


def index_exists(index_name: str) -> bool:
    bind = op.get_bind()
    result = bind.execute(
        sa.text(
            "SELECT 1 FROM pg_indexes "
            "WHERE indexname = :name AND schemaname = current_schema()::text"
        ),
        {"name": index_name},
    )
    return result.scalar() is not None


# ---------------------------------------------------------------------------
# Batch ALTER TYPE helper
# ---------------------------------------------------------------------------


def _numeric_max(type_spec: str) -> float | None:
    """Parse NUMERIC(p,s) and return the maximum absolute value, or None."""
    # e.g. "NUMERIC(20,8)" -> precision=20, scale=8 -> max = 10^(20-8) - 10^(-8)
    import re

    m = re.match(r"NUMERIC\((\d+),(\d+)\)", type_spec, re.IGNORECASE)
    if not m:
        return None
    precision, scale = int(m.group(1)), int(m.group(2))
    return 10 ** (precision - scale) - 10 ** (-scale)


def batch_alter_type(
    cache: _SchemaCache,
    columns: list[tuple[str, str, bool, str | None]],
    cast_suffix: str,
    type_fn: Callable[[str], str],
) -> None:
    """Group columns by table and issue ONE ``ALTER TABLE`` per table.

    Skips columns that don't exist.  Each tuple is
    ``(table_name, column_name, nullable, server_default)``.

    When converting to NUMERIC(p,s), values exceeding the target precision
    are clamped before the type cast to prevent overflow errors.
    """
    by_table: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for table, col, _nullable, _default in columns:
        if not cache.column_exists(table, col):
            continue
        by_table[table].append((col, type_fn(col)))

    bind = op.get_bind()
    for table, col_types in by_table.items():
        # Clamp out-of-range values before ALTER TYPE
        for col, target in col_types:
            cap = _numeric_max(target)
            if cap is not None:
                bind.execute(
                    sa.text(
                        f"UPDATE {table} SET {col} = :cap "
                        f"WHERE {col} IS NOT NULL AND abs({col}) > :cap"
                    ),
                    {"cap": cap},
                )
        parts = [
            f"ALTER COLUMN {col} TYPE {target} USING {col}::{cast_suffix}"
            for col, target in col_types
        ]
        if parts:
            bind.execute(sa.text(f"ALTER TABLE {table} " + ", ".join(parts)))
