"""
Provider Key 写操作后的副作用处理。
"""

from __future__ import annotations

from collections.abc import Callable

from sqlalchemy import delete as sa_delete
from sqlalchemy import update as sa_update
from sqlalchemy.orm import Session

from src.core.logger import logger
from src.models.database import (
    GeminiFileMapping,
    ProviderAPIKey,
    Usage,
    VideoTask,
)
from src.services.cache.model_list_cache import invalidate_models_list_cache
from src.services.cache.provider_cache import ProviderCacheService

_SQLITE_BATCH_SIZE = 900
_DEFAULT_BATCH_SIZE = 2000

_CLEANUP_STAGES = (
    (
        "gemini_file_mappings",
        lambda batch: sa_delete(GeminiFileMapping).where(GeminiFileMapping.key_id.in_(batch)),
    ),
    (
        "usage",
        lambda batch: sa_update(Usage)
        .where(Usage.provider_api_key_id.in_(batch))
        .values(provider_api_key_id=None),
    ),
    (
        "video_tasks",
        lambda batch: sa_update(VideoTask).where(VideoTask.key_id.in_(batch)).values(key_id=None),
    ),
)


def cleanup_key_references(
    db: Session,
    key_ids: list[str],
    *,
    batch_size: int | None = None,
    stage_callback: Callable[[str, int], None] | None = None,
) -> None:
    """在删除 ProviderAPIKey 前，先显式处理关联表引用，降低级联删除/置空成本。

    - gemini_file_mappings: 直接删除
    - usage / video_tasks: 先置空外键，保留快照与历史记录

    PostgreSQL 下直接按 key_id/provider_api_key_id 批量处理；
    SQLite 下按 key_id 批次拆分，避免超出变量数限制。
    """
    if not key_ids:
        return
    effective_batch_size = batch_size if batch_size is not None else _resolve_batch_size(db)
    for batch in iter_key_batches(key_ids, effective_batch_size):
        for stage_name, statement_factory in _CLEANUP_STAGES:
            if stage_callback is not None:
                stage_callback(stage_name, len(batch))
            db.execute(statement_factory(batch))


def _resolve_batch_size(db: Session) -> int:
    try:
        bind = db.get_bind()
        dialect_name = str(getattr(getattr(bind, "dialect", None), "name", "") or "").lower()
    except Exception:
        dialect_name = ""
    if dialect_name == "sqlite":
        return _SQLITE_BATCH_SIZE
    return _DEFAULT_BATCH_SIZE


def iter_key_batches(items: list[str], batch_size: int) -> list[list[str]]:
    """将 key_ids 列表按 batch_size 拆分为子列表。"""
    if not items:
        return []
    if batch_size <= 0:
        return [list(items)]
    return [list(items[i : i + batch_size]) for i in range(0, len(items), batch_size)]


async def run_update_key_side_effects(
    db: Session,
    key: ProviderAPIKey,
    key_id: str,
    auto_fetch_enabled_before: bool,
    auto_fetch_enabled_after: bool,
    include_patterns_before: list[str] | None,
    exclude_patterns_before: list[str] | None,
    allowed_models_before: set[str],
) -> None:
    """执行更新 Key 后的副作用。"""
    if not auto_fetch_enabled_before and auto_fetch_enabled_after:
        # 刚刚开启了 auto_fetch_models，同步执行模型获取
        logger.info("[AUTO_FETCH] Key {} 开启自动获取模型，同步执行模型获取", key_id)
        try:
            from src.services.model.fetch_scheduler import get_model_fetch_scheduler

            scheduler = get_model_fetch_scheduler()
            # 同步等待模型获取完成，确保前端刷新时能看到最新数据
            await scheduler._fetch_models_for_key_by_id(key_id)
            # fetch_scheduler 可能在独立 session 更新 allowed_models，需刷新当前对象避免后续比较使用旧值。
            db.refresh(key)
        except Exception as e:
            logger.error(f"触发模型获取失败: {e}")
            # 不抛出异常，避免影响 Key 更新操作
    elif auto_fetch_enabled_before and not auto_fetch_enabled_after:
        # 关闭了 auto_fetch_models，只保留锁定的模型，清除自动获取的模型
        locked = key.locked_models or []
        if locked:
            key.allowed_models = locked
            logger.info(
                "[AUTO_FETCH] Key {} 关闭自动获取模型，保留 {} 个锁定模型",
                key_id,
                len(locked),
            )
        else:
            key.allowed_models = None
            logger.info(
                "[AUTO_FETCH] Key {} 关闭自动获取模型，无锁定模型，清空 allowed_models",
                key_id,
            )
        db.commit()
        db.refresh(key)
    elif auto_fetch_enabled_after:
        # auto_fetch_models 保持开启状态，检查过滤规则是否变更
        include_patterns_after = key.model_include_patterns
        exclude_patterns_after = key.model_exclude_patterns
        patterns_changed = (
            include_patterns_before != include_patterns_after
            or exclude_patterns_before != exclude_patterns_after
        )
        if patterns_changed:
            # 过滤规则变更，重新应用过滤（使用缓存的上游模型数据）
            logger.info("[AUTO_FETCH] Key {} 过滤规则变更，重新应用过滤", key_id)
            try:
                from src.services.model.fetch_scheduler import get_model_fetch_scheduler

                scheduler = get_model_fetch_scheduler()
                await scheduler._fetch_models_for_key_by_id(key_id)
                # 重新应用过滤后，刷新当前对象以读取最新 allowed_models。
                db.refresh(key)
            except Exception as e:
                logger.error(f"重新应用过滤规则失败: {e}")

    # 任何字段更新都清除缓存，确保缓存一致性
    # 包括 is_active、allowed_models、capabilities 等影响权限和行为的字段
    await ProviderCacheService.invalidate_provider_api_key_cache(key_id)

    # 检查 allowed_models 是否有变化，触发缓存失效和自动关联
    allowed_models_after = set(key.allowed_models or [])
    if allowed_models_before != allowed_models_after and key.provider_id:
        from src.services.model.global_model import on_key_allowed_models_changed

        await on_key_allowed_models_changed(
            db=db,
            provider_id=key.provider_id,
            allowed_models=list(key.allowed_models or []),
        )
    else:
        # allowed_models 未变化时，仍需清除 /v1/models 缓存（is_active、api_formats 变更会影响模型可用性）
        await invalidate_models_list_cache()


async def run_create_key_side_effects(
    db: Session,
    provider_id: str,
    key: ProviderAPIKey,
) -> None:
    """执行创建 Key 后的副作用。"""
    # 如果开启了 auto_fetch_models，同步执行模型获取
    if key.auto_fetch_models:
        logger.info("[AUTO_FETCH] 新 Key {} 开启自动获取模型，同步执行模型获取", key.id)
        try:
            from src.services.model.fetch_scheduler import get_model_fetch_scheduler

            scheduler = get_model_fetch_scheduler()
            # 同步等待模型获取完成，确保前端刷新时能看到最新数据
            await scheduler._fetch_models_for_key_by_id(key.id)
        except Exception as e:
            logger.error(f"触发模型获取失败: {e}")
            # 不抛出异常，避免影响 Key 创建操作

    # 如果创建时指定了 allowed_models，触发自动关联检查（内部会清除 /v1/models 缓存）
    if key.allowed_models:
        from src.services.model.global_model import on_key_allowed_models_changed

        await on_key_allowed_models_changed(
            db=db,
            provider_id=provider_id,
            allowed_models=list(key.allowed_models),
        )
    else:
        # 没有 allowed_models 时，仍需清除 /v1/models 缓存
        await invalidate_models_list_cache()


async def run_delete_key_side_effects(
    db: Session,
    provider_id: str | None,
    deleted_key_allowed_models: list[str] | None,
) -> None:
    """执行删除 Key 后的副作用。"""
    _ = deleted_key_allowed_models
    if provider_id:
        from src.services.model.global_model import on_key_allowed_models_changed

        await on_key_allowed_models_changed(
            db=db,
            provider_id=provider_id,
            skip_disassociate=True,
        )
    else:
        # 无 provider_id 时仅清除缓存
        await invalidate_models_list_cache()
