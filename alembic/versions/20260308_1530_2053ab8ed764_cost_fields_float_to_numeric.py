"""cost fields: Float -> Numeric(20,8) + provider_api_keys composite index

Revision ID: 2053ab8ed764
Revises: 13a4c8f6d9e0
Create Date: 2026-03-08 15:30:00.000000+00:00

"""

from alembic import op
from helpers import batch_alter_type, index_exists, new_cache

# revision identifiers, used by Alembic.
revision = "2053ab8ed764"
down_revision = "13a4c8f6d9e0"
branch_labels = None
depends_on = None


# (table_name, column_name, nullable, server_default)
_COST_COLUMNS: list[tuple[str, str, bool, str | None]] = [
    # api_keys
    ("api_keys", "total_cost_usd", True, "0.0"),
    # usage
    ("usage", "input_cost_usd", True, "0.0"),
    ("usage", "output_cost_usd", True, "0.0"),
    ("usage", "cache_cost_usd", True, "0.0"),
    ("usage", "cache_creation_cost_usd", True, "0.0"),
    ("usage", "cache_read_cost_usd", True, "0.0"),
    ("usage", "request_cost_usd", True, "0.0"),
    ("usage", "total_cost_usd", True, "0.0"),
    ("usage", "actual_input_cost_usd", True, "0.0"),
    ("usage", "actual_output_cost_usd", True, "0.0"),
    ("usage", "actual_cache_creation_cost_usd", True, "0.0"),
    ("usage", "actual_cache_read_cost_usd", True, "0.0"),
    ("usage", "actual_request_cost_usd", True, "0.0"),
    ("usage", "actual_total_cost_usd", True, "0.0"),
    ("usage", "rate_multiplier", True, "1.0"),
    ("usage", "input_price_per_1m", True, None),
    ("usage", "output_price_per_1m", True, None),
    ("usage", "cache_creation_price_per_1m", True, None),
    ("usage", "cache_read_price_per_1m", True, None),
    ("usage", "price_per_request", True, None),
    # providers
    ("providers", "monthly_quota_usd", True, None),
    ("providers", "monthly_used_usd", True, "0.0"),
    # global_models
    ("global_models", "default_price_per_request", True, None),
    # models
    ("models", "price_per_request", True, None),
    # stats_hourly
    ("stats_hourly", "total_cost", False, "0.0"),
    ("stats_hourly", "actual_total_cost", False, "0.0"),
    # stats_hourly_user
    ("stats_hourly_user", "total_cost", False, "0.0"),
    # stats_hourly_model
    ("stats_hourly_model", "total_cost", False, "0.0"),
    # stats_hourly_provider
    ("stats_hourly_provider", "total_cost", False, "0.0"),
    # stats_daily
    ("stats_daily", "total_cost", False, "0.0"),
    ("stats_daily", "actual_total_cost", False, "0.0"),
    ("stats_daily", "input_cost", False, "0.0"),
    ("stats_daily", "output_cost", False, "0.0"),
    ("stats_daily", "cache_creation_cost", False, "0.0"),
    ("stats_daily", "cache_read_cost", False, "0.0"),
    # stats_daily_model
    ("stats_daily_model", "total_cost", False, "0.0"),
    # stats_daily_provider
    ("stats_daily_provider", "total_cost", False, "0.0"),
    # stats_daily_api_key
    ("stats_daily_api_key", "total_cost", False, "0.0"),
    # stats_summary
    ("stats_summary", "all_time_cost", False, "0.0"),
    ("stats_summary", "all_time_actual_cost", False, "0.0"),
    # stats_user_daily
    ("stats_user_daily", "total_cost", False, "0.0"),
]

_ALL_TABLES = list({t for t, *_ in _COST_COLUMNS})


def _type_spec(col: str) -> str:
    """Return the SQL type literal for a given column name."""
    return "NUMERIC(10,6)" if col == "rate_multiplier" else "NUMERIC(20,8)"


def upgrade() -> None:
    c = new_cache()
    c.load_columns(_ALL_TABLES)

    # -- 1. cost fields: Float -> Numeric  (batched per table)
    cols_to_convert = [
        (t, col, n, d)
        for t, col, n, d in _COST_COLUMNS
        if c.column_exists(t, col) and not c.is_numeric(t, col)
    ]
    batch_alter_type(c, cols_to_convert, cast_suffix="numeric", type_fn=_type_spec)

    # -- 2. provider_api_keys composite index
    if not index_exists("idx_provider_api_keys_provider_active"):
        op.create_index(
            "idx_provider_api_keys_provider_active",
            "provider_api_keys",
            ["provider_id", "is_active"],
        )


def downgrade() -> None:
    # -- 2. drop composite index
    if index_exists("idx_provider_api_keys_provider_active"):
        op.drop_index(
            "idx_provider_api_keys_provider_active",
            table_name="provider_api_keys",
        )

    # -- 1. Numeric -> Float  (batched per table)
    c = new_cache()
    c.load_columns(_ALL_TABLES)

    cols_to_revert = [
        (t, col, n, d)
        for t, col, n, d in _COST_COLUMNS
        if c.column_exists(t, col) and c.is_numeric(t, col)
    ]
    batch_alter_type(
        c,
        cols_to_revert,
        cast_suffix="double precision",
        type_fn=lambda _col: "DOUBLE PRECISION",
    )
