"""backfill_codex_default_body_rules

Backfill default body_rules for codex providers with openai:cli endpoints
that currently have body_rules IS NULL.

Rules:
- drop max_output_tokens
- drop temperature
- drop top_p
- set store = false
- set instructions = "You are GPT-5." (when instructions not exists)

Revision ID: dd0278c0a28c
Revises: 1d2e3f4a5b6c
Create Date: 2026-03-02 15:00:00.000000+00:00
"""

from __future__ import annotations

import json

import sqlalchemy as sa

from alembic import op

# revision identifiers, used by Alembic.
revision = "dd0278c0a28c"
down_revision = "1d2e3f4a5b6c"
branch_labels = None
depends_on = None

_TARGET_FORMATS = ("openai:cli",)

_DEFAULT_BODY_RULES = [
    {"action": "drop", "path": "max_output_tokens"},
    {"action": "drop", "path": "temperature"},
    {"action": "drop", "path": "top_p"},
    {"action": "set", "path": "store", "value": False},
    {
        "action": "set",
        "path": "instructions",
        "value": "You are GPT-5.",
        "condition": {"path": "instructions", "op": "not_exists"},
    },
]


def upgrade() -> None:
    conn = op.get_bind()

    # 幂等性: 仅回填 codex 提供商中 body_rules 为空（SQL NULL 或 JSON null）的记录
    rules_json = json.dumps(_DEFAULT_BODY_RULES, ensure_ascii=False)
    result = conn.execute(
        sa.text("""
            UPDATE provider_endpoints pe
            SET body_rules = CAST(:rules AS json),
                updated_at = CURRENT_TIMESTAMP
            FROM providers p
            WHERE pe.provider_id = p.id
              AND p.provider_type = :ptype
              AND pe.api_format = :fmt
              AND (pe.body_rules IS NULL OR pe.body_rules::text = 'null')
        """),
        {"rules": rules_json, "ptype": "codex", "fmt": _TARGET_FORMATS[0]},
    )
    if result.rowcount:
        print(f"  backfilled body_rules for {result.rowcount} endpoint(s)")


def downgrade() -> None:
    # Data backfill: no-op to avoid removing user-customized rules.
    return
