"""tighten wallet transaction snapshots and remove wallet version

Revision ID: 8e71f2a4c9b0
Revises: 7c91d2e4f8a1
Create Date: 2026-03-07 13:00:00.000000+00:00
"""

from __future__ import annotations

from collections.abc import Sequence

import sqlalchemy as sa
from sqlalchemy import inspect

from alembic import op

# revision identifiers, used by Alembic.
revision: str = "8e71f2a4c9b0"
down_revision: str | None = "7c91d2e4f8a1"
branch_labels: str | Sequence[str] | None = None
depends_on: str | Sequence[str] | None = None

_WALLET_TX_BEFORE_CHECK = "ck_wallet_tx_balance_before_consistent"
_WALLET_TX_AFTER_CHECK = "ck_wallet_tx_balance_after_consistent"
_WALLET_LIMIT_MODE_INDEX = "idx_wallets_limit_mode"


def _table_exists(table_name: str) -> bool:
    bind = op.get_bind()
    insp = inspect(bind)
    insp.clear_cache()
    return table_name in insp.get_table_names()


def _column_exists(table_name: str, column_name: str) -> bool:
    bind = op.get_bind()
    insp = inspect(bind)
    insp.clear_cache()
    return column_name in [c["name"] for c in insp.get_columns(table_name)]


def _index_exists(table_name: str, index_name: str) -> bool:
    bind = op.get_bind()
    insp = inspect(bind)
    insp.clear_cache()
    return any(index.get("name") == index_name for index in insp.get_indexes(table_name))


def _check_constraint_exists(table_name: str, constraint_name: str) -> bool:
    bind = op.get_bind()
    insp = inspect(bind)
    insp.clear_cache()
    return any(c.get("name") == constraint_name for c in insp.get_check_constraints(table_name))


def _tighten_wallet_transaction_snapshots() -> None:
    if not _table_exists("wallet_transactions"):
        return

    required_columns = {
        "balance_before",
        "balance_after",
        "recharge_balance_before",
        "recharge_balance_after",
        "gift_balance_before",
        "gift_balance_after",
    }
    existing_columns = {
        column["name"] for column in inspect(op.get_bind()).get_columns("wallet_transactions")
    }
    if not required_columns.issubset(existing_columns):
        return

    op.execute(
        sa.text(
            """
            UPDATE wallet_transactions
            SET recharge_balance_before = balance_before
            WHERE recharge_balance_before IS NULL
            """
        )
    )
    op.execute(
        sa.text(
            """
            UPDATE wallet_transactions
            SET recharge_balance_after = balance_after
            WHERE recharge_balance_after IS NULL
            """
        )
    )
    op.execute(
        sa.text(
            """
            UPDATE wallet_transactions
            SET gift_balance_before = 0
            WHERE gift_balance_before IS NULL
            """
        )
    )
    op.execute(
        sa.text(
            """
            UPDATE wallet_transactions
            SET gift_balance_after = 0
            WHERE gift_balance_after IS NULL
            """
        )
    )
    op.execute(
        sa.text(
            """
            UPDATE wallet_transactions
            SET balance_before = recharge_balance_before + gift_balance_before,
                balance_after = recharge_balance_after + gift_balance_after
            """
        )
    )

    if not _check_constraint_exists("wallet_transactions", _WALLET_TX_BEFORE_CHECK):
        op.create_check_constraint(
            _WALLET_TX_BEFORE_CHECK,
            "wallet_transactions",
            "balance_before = recharge_balance_before + gift_balance_before",
        )
    if not _check_constraint_exists("wallet_transactions", _WALLET_TX_AFTER_CHECK):
        op.create_check_constraint(
            _WALLET_TX_AFTER_CHECK,
            "wallet_transactions",
            "balance_after = recharge_balance_after + gift_balance_after",
        )

    op.alter_column(
        "wallet_transactions",
        "recharge_balance_before",
        existing_type=sa.Numeric(20, 8),
        nullable=False,
    )
    op.alter_column(
        "wallet_transactions",
        "recharge_balance_after",
        existing_type=sa.Numeric(20, 8),
        nullable=False,
    )
    op.alter_column(
        "wallet_transactions",
        "gift_balance_before",
        existing_type=sa.Numeric(20, 8),
        nullable=False,
    )
    op.alter_column(
        "wallet_transactions",
        "gift_balance_after",
        existing_type=sa.Numeric(20, 8),
        nullable=False,
    )


def _drop_wallet_cleanup_artifacts() -> None:
    if not _table_exists("wallets"):
        return

    if _index_exists("wallets", _WALLET_LIMIT_MODE_INDEX):
        op.drop_index(_WALLET_LIMIT_MODE_INDEX, table_name="wallets")

    if _column_exists("wallets", "version"):
        op.drop_column("wallets", "version")


def upgrade() -> None:
    _tighten_wallet_transaction_snapshots()
    _drop_wallet_cleanup_artifacts()


def downgrade() -> None:
    if _table_exists("wallets"):
        if not _column_exists("wallets", "version"):
            op.add_column(
                "wallets",
                sa.Column("version", sa.Integer(), nullable=False, server_default="0"),
            )
        if not _index_exists("wallets", _WALLET_LIMIT_MODE_INDEX):
            op.create_index(_WALLET_LIMIT_MODE_INDEX, "wallets", ["limit_mode"])

    if not _table_exists("wallet_transactions"):
        return

    if _column_exists("wallet_transactions", "recharge_balance_before"):
        op.alter_column(
            "wallet_transactions",
            "recharge_balance_before",
            existing_type=sa.Numeric(20, 8),
            nullable=True,
        )
    if _column_exists("wallet_transactions", "recharge_balance_after"):
        op.alter_column(
            "wallet_transactions",
            "recharge_balance_after",
            existing_type=sa.Numeric(20, 8),
            nullable=True,
        )
    if _column_exists("wallet_transactions", "gift_balance_before"):
        op.alter_column(
            "wallet_transactions",
            "gift_balance_before",
            existing_type=sa.Numeric(20, 8),
            nullable=True,
        )
    if _column_exists("wallet_transactions", "gift_balance_after"):
        op.alter_column(
            "wallet_transactions",
            "gift_balance_after",
            existing_type=sa.Numeric(20, 8),
            nullable=True,
        )

    if _check_constraint_exists("wallet_transactions", _WALLET_TX_AFTER_CHECK):
        op.drop_constraint(_WALLET_TX_AFTER_CHECK, "wallet_transactions", type_="check")
    if _check_constraint_exists("wallet_transactions", _WALLET_TX_BEFORE_CHECK):
        op.drop_constraint(_WALLET_TX_BEFORE_CHECK, "wallet_transactions", type_="check")
