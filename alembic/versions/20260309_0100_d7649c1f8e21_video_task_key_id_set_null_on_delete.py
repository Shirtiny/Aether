"""video_tasks.key_id: add ondelete SET NULL

Revision ID: d7649c1f8e21
Revises: 2053ab8ed764
Create Date: 2026-03-09 01:00:00.000000+00:00

"""

from helpers import new_cache, replace_fk_if_needed

# revision identifiers, used by Alembic.
revision = "d7649c1f8e21"
down_revision = "2053ab8ed764"
branch_labels = None
depends_on = None

_TABLE = "video_tasks"
_FK_NAME = "video_tasks_key_id_fkey"


def upgrade() -> None:
    c = new_cache()
    c.load_fk_rules([_TABLE])
    replace_fk_if_needed(
        c,
        _FK_NAME,
        _TABLE,
        "provider_api_keys",
        ["key_id"],
        ["id"],
        "SET NULL",
    )


def downgrade() -> None:
    c = new_cache()
    c.load_fk_rules([_TABLE])
    replace_fk_if_needed(
        c,
        _FK_NAME,
        _TABLE,
        "provider_api_keys",
        ["key_id"],
        ["id"],
        "NO ACTION",
    )
