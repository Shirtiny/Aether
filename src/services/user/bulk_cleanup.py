from __future__ import annotations

import time
from typing import Any

from sqlalchemy.orm import Session

from src.core.logger import logger
from src.models.database import RequestCandidate, Usage

_POSTGRES_BATCH_SIZE = 2000
_SQLITE_BATCH_SIZE = 900


def _resolve_batch_size(db: Session) -> int:
    try:
        bind = db.get_bind()
        dialect_name = str(getattr(getattr(bind, "dialect", None), "name", "") or "").lower()
    except Exception:
        dialect_name = ""

    if dialect_name == "sqlite":
        return _SQLITE_BATCH_SIZE
    return _POSTGRES_BATCH_SIZE


def batch_nullify_fk(
    db: Session,
    model: type[Any],
    column_name: str,
    entity_id: str | None,
) -> int:
    """分批将大表外键置空，避免单个长事务阻塞删除流程。"""
    if not entity_id:
        return 0

    column = getattr(model, column_name)
    primary_key_column = next(iter(model.__table__.primary_key.columns))
    batch_size = _resolve_batch_size(db)
    total_updated = 0
    batch_index = 0
    started_at = time.monotonic()

    while True:
        batch_ids = [
            row[0]
            for row in db.query(primary_key_column)
            .filter(column == entity_id)
            .limit(batch_size)
            .all()
        ]
        if not batch_ids:
            break

        batch_index += 1
        batch_started_at = time.monotonic()
        updated = int(
            db.query(model)
            .filter(primary_key_column.in_(batch_ids))
            .update({column: None}, synchronize_session=False)
            or 0
        )
        db.commit()

        total_updated += updated
        elapsed_ms = int((time.monotonic() - batch_started_at) * 1000)
        logger.info(
            "批量清理 {}.{}: batch={}, updated={}, entity_id={}, elapsed_ms={}",
            model.__tablename__,
            column_name,
            batch_index,
            updated,
            entity_id,
            elapsed_ms,
        )

        if len(batch_ids) < batch_size:
            break

    if total_updated > 0:
        total_elapsed_ms = int((time.monotonic() - started_at) * 1000)
        logger.info(
            "批量清理完成 {}.{}: total_updated={}, entity_id={}, elapsed_ms={}",
            model.__tablename__,
            column_name,
            total_updated,
            entity_id,
            total_elapsed_ms,
        )

    return total_updated


def pre_clean_api_key(db: Session, api_key_id: str | None) -> int:
    """预清理 API Key 在大表中的外键引用，减少后续删除锁竞争。"""
    if not api_key_id:
        return 0

    usage_rows = batch_nullify_fk(db, Usage, "api_key_id", api_key_id)
    candidate_rows = batch_nullify_fk(db, RequestCandidate, "api_key_id", api_key_id)
    total_rows = usage_rows + candidate_rows

    if total_rows > 0:
        logger.info(
            "API Key 预清理完成: api_key_id={}, usage={}, request_candidates={}",
            api_key_id,
            usage_rows,
            candidate_rows,
        )

    return total_rows
