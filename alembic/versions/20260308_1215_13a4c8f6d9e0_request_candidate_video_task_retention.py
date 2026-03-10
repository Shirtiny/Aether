"""request_candidates/video_tasks retention: SET NULL and add snapshots

Revision ID: 13a4c8f6d9e0
Revises: 45b118150a78
Create Date: 2026-03-08 12:15:00.000000+00:00

"""

import sqlalchemy as sa
from helpers import new_cache, replace_fk_if_needed

from alembic import op

# revision identifiers, used by Alembic.
revision = "13a4c8f6d9e0"
down_revision = "45b118150a78"
branch_labels = None
depends_on = None

_TABLES = ["request_candidates", "video_tasks"]


def upgrade() -> None:
    c = new_cache()
    c.load_columns(_TABLES)
    c.load_fk_rules(_TABLES)

    # --- request_candidates: add snapshot columns ---
    if not c.column_exists("request_candidates", "username"):
        op.add_column(
            "request_candidates",
            sa.Column("username", sa.String(length=100), nullable=True, comment="用户名快照"),
        )
    if not c.column_exists("request_candidates", "api_key_name"):
        op.add_column(
            "request_candidates",
            sa.Column(
                "api_key_name",
                sa.String(length=200),
                nullable=True,
                comment="API Key 名称快照",
            ),
        )

    # --- request_candidates: CASCADE -> SET NULL ---
    replace_fk_if_needed(
        c,
        "request_candidates_user_id_fkey",
        "request_candidates",
        "users",
        ["user_id"],
        ["id"],
        "SET NULL",
    )
    replace_fk_if_needed(
        c,
        "request_candidates_api_key_id_fkey",
        "request_candidates",
        "api_keys",
        ["api_key_id"],
        ["id"],
        "SET NULL",
    )

    # --- video_tasks: add snapshot columns ---
    if not c.column_exists("video_tasks", "username"):
        op.add_column(
            "video_tasks",
            sa.Column("username", sa.String(length=100), nullable=True, comment="用户名快照"),
        )
    if not c.column_exists("video_tasks", "api_key_name"):
        op.add_column(
            "video_tasks",
            sa.Column(
                "api_key_name",
                sa.String(length=200),
                nullable=True,
                comment="API Key 名称快照",
            ),
        )

    # --- video_tasks: CASCADE -> SET NULL, user_id nullable ---
    op.alter_column("video_tasks", "user_id", existing_type=sa.String(length=36), nullable=True)
    replace_fk_if_needed(
        c,
        "video_tasks_user_id_fkey",
        "video_tasks",
        "users",
        ["user_id"],
        ["id"],
        "SET NULL",
    )
    replace_fk_if_needed(
        c,
        "video_tasks_api_key_id_fkey",
        "video_tasks",
        "api_keys",
        ["api_key_id"],
        ["id"],
        "SET NULL",
    )


def downgrade() -> None:
    c = new_cache()
    c.load_columns(_TABLES)
    c.load_fk_rules(_TABLES)

    # --- video_tasks: SET NULL -> default (no action), restore NOT NULL ---
    replace_fk_if_needed(
        c,
        "video_tasks_api_key_id_fkey",
        "video_tasks",
        "api_keys",
        ["api_key_id"],
        ["id"],
        "NO ACTION",
    )
    replace_fk_if_needed(
        c,
        "video_tasks_user_id_fkey",
        "video_tasks",
        "users",
        ["user_id"],
        ["id"],
        "NO ACTION",
    )
    op.alter_column("video_tasks", "user_id", existing_type=sa.String(length=36), nullable=False)
    if c.column_exists("video_tasks", "api_key_name"):
        op.drop_column("video_tasks", "api_key_name")
    if c.column_exists("video_tasks", "username"):
        op.drop_column("video_tasks", "username")

    # --- request_candidates: SET NULL -> CASCADE ---
    replace_fk_if_needed(
        c,
        "request_candidates_api_key_id_fkey",
        "request_candidates",
        "api_keys",
        ["api_key_id"],
        ["id"],
        "CASCADE",
    )
    replace_fk_if_needed(
        c,
        "request_candidates_user_id_fkey",
        "request_candidates",
        "users",
        ["user_id"],
        ["id"],
        "CASCADE",
    )
    if c.column_exists("request_candidates", "api_key_name"):
        op.drop_column("request_candidates", "api_key_name")
    if c.column_exists("request_candidates", "username"):
        op.drop_column("request_candidates", "username")
