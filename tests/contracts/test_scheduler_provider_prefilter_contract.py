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


def _make_global_model() -> SimpleNamespace:
    return SimpleNamespace(
        id="gm1",
        name="gpt-4o",
        is_active=True,
        config={},
        supported_capabilities=[],
    )


@pytest.mark.asyncio
async def test_list_all_candidates_prefilters_provider_graph_by_allowed_providers() -> None:
    scheduler = CacheAwareScheduler()
    scheduler.scheduling_mode = CacheAwareScheduler.SCHEDULING_MODE_FIXED_ORDER

    db = _make_db()
    global_model = _make_global_model()
    user_api_key = SimpleNamespace(
        id="ak1",
        allowed_providers=["provider-b"],
        allowed_models=None,
        allowed_api_formats=None,
        user=None,
    )

    filtered_provider = SimpleNamespace(
        id="provider-b",
        name="provider-b",
        endpoints=[],
        models=[],
        provider_priority=2,
    )

    with patch.object(scheduler, "_ensure_initialized", new=AsyncMock(return_value=None)):
        with patch.object(
            scheduler._candidate_builder,
            "_query_provider_refs",
            return_value=[("provider-a", "provider-a"), ("provider-b", "provider-b")],
        ) as refs_mock:
            with patch.object(
                scheduler._candidate_builder,
                "_query_providers",
                return_value=[filtered_provider],
            ) as providers_mock:
                with patch(
                    "src.services.scheduling.aware_scheduler.ModelCacheService.get_global_model_by_name",
                    new=AsyncMock(return_value=global_model),
                ):
                    with patch(
                        "src.services.scheduling.aware_scheduler.SystemConfigService.is_format_conversion_enabled",
                        return_value=True,
                    ):
                        with patch.object(
                            scheduler._candidate_builder,
                            "_build_candidates",
                            new=AsyncMock(return_value=[]),
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

    assert candidates == []
    assert global_model_id == "gm1"
    assert provider_batch_count == 2
    refs_mock.assert_called_once()
    providers_mock.assert_called_once_with(db=db, provider_ids=["provider-b"])
