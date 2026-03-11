from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from src.services.scheduling.aware_scheduler import CacheAwareScheduler


def _make_db() -> MagicMock:
    db = MagicMock()
    db.new = []
    db.dirty = []
    db.deleted = []
    db.in_transaction.return_value = False
    return db


def _make_global_model(*, gid: str, name: str) -> SimpleNamespace:
    return SimpleNamespace(
        id=gid,
        name=name,
        is_active=True,
        config={},
        supported_capabilities=[],
    )


@pytest.mark.asyncio
async def test_list_all_candidates_returns_provider_batch_count_even_when_candidates_empty() -> (
    None
):
    """契约：候选为空不代表 Provider 页为空（用于分页继续拉取下一页）。"""

    scheduler = CacheAwareScheduler()
    scheduler.scheduling_mode = CacheAwareScheduler.SCHEDULING_MODE_FIXED_ORDER

    db = _make_db()

    providers = [
        SimpleNamespace(
            id="p1",
            name="p1",
            is_active=True,
            endpoints=[],
            models=[],
            provider_priority=1,
        ),
        SimpleNamespace(
            id="p2",
            name="p2",
            is_active=True,
            endpoints=[],
            models=[],
            provider_priority=2,
        ),
    ]

    # allowed_providers 会把本页的 provider 全过滤掉，导致 candidates 为空；但 provider_batch_count 应保留过滤前数量。
    user_api_key = SimpleNamespace(
        id="ak1",
        allowed_providers=["not-matching"],
        allowed_models=None,
        allowed_api_formats=None,
        user=None,
    )

    global_model = _make_global_model(gid="gm1", name="gpt-4o")

    with patch.object(scheduler, "_ensure_initialized", new=AsyncMock(return_value=None)):
        with patch.object(
            scheduler._candidate_builder,
            "_query_provider_refs",
            return_value=[("p1", "p1"), ("p2", "p2")],
        ):
            with patch.object(scheduler._candidate_builder, "_query_providers") as query_providers:
                with patch(
                    "src.services.scheduling.aware_scheduler.ModelCacheService.get_global_model_by_name",
                    new=AsyncMock(return_value=global_model),
                ):
                    with patch(
                        "src.services.scheduling.aware_scheduler.SystemConfigService.is_format_conversion_enabled",
                        return_value=True,
                    ):
                        candidates, global_model_id, provider_batch_count = (
                            await scheduler.list_all_candidates(
                                db=db,
                                api_format="openai:chat",
                                model_name="gpt-4o",
                                affinity_key=None,
                                user_api_key=user_api_key,  # type: ignore[arg-type]
                                provider_offset=0,
                                provider_limit=20,
                            )
                        )

    query_providers.assert_not_called()

    assert candidates == []
    assert global_model_id == "gm1"
    assert provider_batch_count == 2


@pytest.mark.asyncio
async def test_list_all_candidates_returns_zero_provider_batch_count_when_provider_page_empty() -> (
    None
):
    scheduler = CacheAwareScheduler()
    scheduler.scheduling_mode = CacheAwareScheduler.SCHEDULING_MODE_FIXED_ORDER

    db = _make_db()
    global_model = _make_global_model(gid="gm1", name="gpt-4o")

    with patch.object(scheduler, "_ensure_initialized", new=AsyncMock(return_value=None)):
        with patch.object(scheduler._candidate_builder, "_query_providers", return_value=[]):
            with patch(
                "src.services.scheduling.aware_scheduler.ModelCacheService.get_global_model_by_name",
                new=AsyncMock(return_value=global_model),
            ):
                with patch(
                    "src.services.scheduling.aware_scheduler.SystemConfigService.is_format_conversion_enabled",
                    return_value=True,
                ):
                    candidates, global_model_id, provider_batch_count = (
                        await scheduler.list_all_candidates(
                            db=db,
                            api_format="openai:chat",
                            model_name="gpt-4o",
                            affinity_key=None,
                            user_api_key=None,
                            provider_offset=0,
                            provider_limit=20,
                        )
                    )

    assert candidates == []
    assert global_model_id == "gm1"
    assert provider_batch_count == 0
